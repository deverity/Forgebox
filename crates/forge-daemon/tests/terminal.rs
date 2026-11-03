//! Terminal sessions hosted by the daemon (M2-E3-T4, ADR-0040) on the real ConPTY:
//!
//! * against the real `forgeboxd` on a temporary root: `cmd` echoes `hi`, a retried input
//!   (same `writer`/`input_seq`, other connection) is written once, `exit` ends the stream
//!   with `exited {0}` and publishes `terminal.exited`; a session opened in a site starts
//!   in the site's project folder with the site's PHP first in `PATH`;
//! * resuming after a disconnect gets exactly the missing bytes; a cursor older than the
//!   1 MiB ring gets `terminal.lagged {missed}` and then the ring;
//! * `terminal.max_sessions` + 1 → `conflict`; `daemon.stop` with 3 sessions running
//!   `ping -t` leaves no process behind;
//! * in-process (the daemon's router behind servers that grant other roles): `viewer` and
//!   `operator` get `forbidden` for every `terminal.*` method.
#![cfg(windows)]

mod common;

use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use clap::Parser;
use forge_core::config::ForgeConfig;
use forge_core::ids::{TerminalId, TerminalWriterId};
use forge_core::paths::{ForgeRoot, InitOptions};
use forge_core::platform;
use forge_daemon::args::Args;
use forge_daemon::boot::{self, BootOptions};
use forge_daemon::instance_lock::InstanceLock;
use forge_ipc::api::methods::{
    events_subscribe, jobs_get, sites_add, terminal_close, terminal_input, terminal_list,
    terminal_open, terminal_resize, terminal_subscribe,
};
use forge_ipc::api::{
    EventFilter, EventsSubscribeParams, JobDto, JobIdParams, JobState, NoParams, Role,
    SiteAddParams, SiteDto, SiteRef, TerminalDto, TerminalEnvelope, TerminalIdParams,
    TerminalInputParams, TerminalItemDto, TerminalOpenParams, TerminalResizeParams,
    TerminalSubscribeParams,
};
use forge_ipc::auth::{Authenticator, IpcToken};
use forge_ipc::client::{Subscription, SubscriptionItem};
use forge_ipc::transport::{ConnectionInfo, InMemoryTransport, Transport};
use forge_ipc::{Client, Credentials, Server};
use serde_json::Value;

use common::*;

const WAIT: Duration = Duration::from_secs(30);

fn fake_php() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| compile_fixture("fake_php")).clone()
}

/// Removes VT sequences from the console rendering (as `forge-core/tests/conpty_windows`):
/// cursor-forward becomes spaces, cursor positioning a line break, `\r` is dropped.
fn strip_vt(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            if c != '\r' {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                let mut params = String::new();
                for n in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&n) {
                        match n {
                            'C' => {
                                let count = params.parse::<usize>().unwrap_or(1);
                                out.extend(std::iter::repeat_n(' ', count));
                            }
                            'H' => out.push('\n'),
                            _ => {}
                        }
                        break;
                    }
                    params.push(n);
                }
            }
            Some(']') => {
                while let Some(n) = chars.next() {
                    if n == '\x07' {
                        break;
                    }
                    if n == '\x1b' {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// One subscription and what it received.
struct Watch {
    sub: Subscription<TerminalEnvelope>,
    /// Output as sent (VT included).
    raw: String,
    last_seq: u64,
    lagged: Vec<u64>,
    exited: Option<(u64, Option<i32>)>,
}

impl Watch {
    async fn new(client: &Client, id: TerminalId, resume_from: Option<u64>) -> Self {
        let sub = client
            .subscribe::<terminal_subscribe>(TerminalSubscribeParams { id, resume_from })
            .await
            .unwrap();
        Self {
            sub,
            raw: String::new(),
            last_seq: resume_from.unwrap_or(0),
            lagged: Vec::new(),
            exited: None,
        }
    }

    fn text(&self) -> String {
        strip_vt(&self.raw)
    }

    /// Takes items until `done` holds (or panics after `WAIT`).
    async fn until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(self) {
            let left = deadline.saturating_duration_since(Instant::now());
            let item = tokio::time::timeout(left, self.sub.next_item())
                .await
                .unwrap_or_else(|_| panic!("{what}: nothing in time; got:\n{}", self.text()))
                .expect("the stream is open");
            match item {
                SubscriptionItem::Item(envelope) => match envelope.item {
                    TerminalItemDto::Output { data } => {
                        assert!(envelope.seq > self.last_seq, "seq grows");
                        self.raw.push_str(&data);
                        self.last_seq = envelope.seq;
                    }
                    TerminalItemDto::Exited { exit_code } => {
                        self.exited = Some((envelope.seq, exit_code));
                    }
                },
                SubscriptionItem::Lagged { missed } => self.lagged.push(missed),
            }
        }
    }

    async fn until_line(&mut self, what: &str, line: impl Fn(&str) -> bool) {
        self.until(what, |w| w.text().lines().any(|l| line(l.trim())))
            .await;
    }
}

/// Typed input of one tab: one writer, `input_seq` from 1.
struct Keyboard {
    id: TerminalId,
    writer: TerminalWriterId,
    seq: u64,
}

impl Keyboard {
    fn new(id: TerminalId) -> Self {
        Self {
            id,
            writer: TerminalWriterId::new(),
            seq: 0,
        }
    }

    fn params(&self, data: &str) -> TerminalInputParams {
        TerminalInputParams {
            id: self.id,
            writer: self.writer,
            data: data.to_owned(),
            input_seq: NonZeroU64::new(self.seq).unwrap(),
        }
    }

    async fn line(&mut self, client: &Client, line: &str) -> TerminalInputParams {
        self.seq += 1;
        let params = self.params(&format!("{line}\r"));
        client.call::<terminal_input>(params.clone()).await.unwrap();
        params
    }
}

fn open_params(cols: u16) -> TerminalOpenParams {
    TerminalOpenParams {
        dir: None,
        site: None,
        versions: BTreeMap::new(),
        shell: Some("cmd".into()),
        command: Vec::new(),
        env: BTreeMap::new(),
        cols,
        rows: 30,
        title: None,
    }
}

async fn open_cmd(client: &Client) -> TerminalDto {
    client
        .call::<terminal_open>(open_params(120))
        .await
        .unwrap()
}

/// `terminal.exited` events, collected in the background.
#[derive(Clone, Default)]
struct Exits(Arc<Mutex<Vec<Value>>>);

impl Exits {
    async fn collect(client: &Client) -> Self {
        let exits = Self::default();
        let mut sub = client
            .subscribe::<events_subscribe>(EventsSubscribeParams {
                filter: EventFilter::kinds(["terminal.exited"]),
                resume_from: None,
            })
            .await
            .unwrap();
        let sink = exits.clone();
        tokio::spawn(async move {
            while let Some(item) = sub.next_item().await {
                if let SubscriptionItem::Item(envelope) = item {
                    sink.0.lock().unwrap().push(envelope.event.payload);
                }
            }
        });
        exits
    }

    async fn wait_for(&self, id: TerminalId) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let found = self
                .0
                .lock()
                .unwrap()
                .iter()
                .find(|p| p["terminal_id"] == id.to_string())
                .cloned();
            if let Some(found) = found {
                return found;
            }
            assert!(Instant::now() < deadline, "terminal.exited for {id}");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

async fn finish(client: &Client, job: JobDto) -> JobDto {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let now = client
            .call::<jobs_get>(JobIdParams { job_id: job.id })
            .await
            .unwrap();
        if now.state != JobState::Running {
            return now;
        }
        assert!(Instant::now() < deadline, "job {} still running", job.id);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A root with the fake PHP 8.4.27 in `bin/php/` and no parked folder.
fn root_with_php() -> TestRoot {
    let root = TestRoot::new();
    let dir = root.root.bin().join("php").join("8.4.27");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fake_php(), dir.join("php.exe")).unwrap();
    std::fs::copy(fake_php(), dir.join("php-cgi.exe")).unwrap();
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    std::fs::write(
        root.root.forgebox_toml(),
        format!("parked_dirs = []\n{toml}"),
    )
    .unwrap();
    root
}

/// A project folder with a marker (`composer.json`) and its document root `public/`.
fn project(root: &TestRoot, name: &str) -> PathBuf {
    let dir = root.path().join("projects").join(name);
    std::fs::create_dir_all(dir.join("public")).unwrap();
    std::fs::write(dir.join("composer.json"), "{}").unwrap();
    std::fs::write(dir.join("public").join("index.php"), "<?php echo 'ok';").unwrap();
    dir
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Whether `pid` is still a running process.
fn alive(pid: u32) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cmd_echoes_dedups_retries_exits_and_opens_in_a_site() {
    let root = root_with_php();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, WAIT).await;
    let exits = Exits::collect(&client).await;

    let dto = open_cmd(&client).await;
    assert_eq!((dto.shell.as_str(), dto.title.as_str()), ("cmd", "cmd"));
    assert!(dto.pid.is_some() && dto.exit_code.is_none());
    assert!(same_dir(Path::new(&dto.cwd), root.path()), "{}", dto.cwd);
    let mut watch = Watch::new(&client, dto.id, None).await;
    let mut keys = Keyboard::new(dto.id);
    let first = keys.line(&client, "echo hi").await;
    watch.until_line("echo hi", |l| l == "hi").await;

    // The same input retried through another connection is not written again.
    let (other, _) = connect(&root, &mut daemon, WAIT).await;
    other.call::<terminal_input>(first).await.unwrap();
    keys.line(&client, "echo %FORGEBOX_TERMINAL%-done").await;
    watch.until_line("the marker", |l| l == "1-done").await;
    let his = watch.text().lines().filter(|l| l.trim() == "hi").count();
    assert_eq!(his, 1, "the retry was written again:\n{}", watch.text());

    client
        .call::<terminal_resize>(TerminalResizeParams {
            id: dto.id,
            cols: 100,
            rows: 40,
        })
        .await
        .unwrap();
    let listed = client.call::<terminal_list>(NoParams {}).await.unwrap();
    assert_eq!(listed.terminals.len(), 1);
    assert_eq!(
        (listed.terminals[0].cols, listed.terminals[0].rows),
        (100, 40)
    );

    keys.line(&client, "exit").await;
    watch.until("exited", |w| w.exited.is_some()).await;
    let (seq, code) = watch.exited.unwrap();
    assert_eq!(code, Some(0));
    assert_eq!(seq, watch.last_seq + 1, "exited comes after the last byte");
    let event = exits.wait_for(dto.id).await;
    assert_eq!(event["exit_code"], 0);
    let listed = client.call::<terminal_list>(NoParams {}).await.unwrap();
    assert_eq!(listed.terminals[0].exit_code, Some(0));
    assert_eq!(listed.terminals[0].last_seq, watch.last_seq);
    client
        .call::<terminal_close>(TerminalIdParams { id: dto.id })
        .await
        .unwrap();
    assert!(
        client
            .call::<terminal_list>(NoParams {})
            .await
            .unwrap()
            .terminals
            .is_empty()
    );

    // In a site: its project folder and its PHP.
    let dir = project(&root, "loja");
    let job = client
        .call::<sites_add>(SiteAddParams {
            dir: forge_core::paths::posix_string(&dir.join("public")),
            host: Some("loja.test".into()),
            php: Some("8.4".into()),
            server: None,
            secure: Some(false),
            driver: None,
        })
        .await
        .unwrap();
    let job = finish(&client, job).await;
    assert_eq!(job.state, JobState::Done, "{:?}", job.error);
    let site: SiteDto = serde_json::from_value(job.result.unwrap()).unwrap();
    let in_site = client
        .call::<terminal_open>(TerminalOpenParams {
            site: Some(SiteRef::from("loja.test")),
            // No wrapping of the long PATH line.
            cols: 2000,
            ..open_params(2000)
        })
        .await
        .unwrap();
    assert_eq!(in_site.title, "loja.test");
    assert_eq!(in_site.site_id, Some(site.id));
    let mut watch = Watch::new(&client, in_site.id, None).await;
    let mut keys = Keyboard::new(in_site.id);
    keys.line(&client, "echo [%CD%]").await;
    keys.line(&client, "echo [%FORGEBOX_PHP_VERSION%]").await;
    keys.line(&client, "echo [%PATH%]").await;
    watch
        .until_line("PATH", |l| l.starts_with("[") && l.contains(';'))
        .await;
    let text = watch.text();
    // `[value]` lines (the long PATH may still wrap: only its start matters).
    let values: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('['))
        .map(|l| l[1..].trim_end_matches(']'))
        .collect();
    assert_eq!(values.len(), 3, "{text}");
    assert!(same_dir(Path::new(values[0]), &dir), "cwd {}", values[0]);
    assert_eq!(values[1], "8.4.27");
    let first_entry = values[2].split(';').next().unwrap().to_ascii_lowercase();
    assert!(
        first_entry.ends_with(r"bin\php\8.4.27"),
        "the site's PHP comes first in PATH: {}",
        values[2]
    );

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
    assert!(!alive(in_site.pid.unwrap()), "the site's shell is gone");
    show_log(&root);
}

/// A file of `lines` numbered lines (`line-000001 xxxx…`).
fn numbered_file(dir: &Path, name: &str, lines: usize, width: usize) -> PathBuf {
    use std::io::Write;
    let path = dir.join(name);
    let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    for n in 0..lines {
        writeln!(file, "line-{n:06} {}\r", "x".repeat(width)).unwrap();
    }
    file.flush().unwrap();
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_gets_exactly_the_missing_bytes_and_old_cursors_lag() {
    let root = TestRoot::new();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, WAIT).await;
    let files = tempfile::tempdir().unwrap();
    let small = numbered_file(files.path(), "small.txt", 2_000, 40);

    let dto = client
        .call::<terminal_open>(open_params(200))
        .await
        .unwrap();
    let mut keys = Keyboard::new(dto.id);
    // A first client sees part of the output and goes away.
    let (first, _) = connect(&root, &mut daemon, WAIT).await;
    let mut a = Watch::new(&first, dto.id, None).await;
    keys.line(&client, &format!("type \"{}\"", small.display()))
        .await;
    a.until("the first lines", |w| w.text().contains("line-000100"))
        .await;
    let cursor = a.last_seq;
    first.close();
    drop(a);
    drop(first);
    keys.line(&client, "echo END-%COMPUTERNAME:~0,0%MARK").await;

    // Reattaching from the cursor gets the rest, and the rest only.
    let mut b = Watch::new(&client, dto.id, Some(cursor)).await;
    b.until_line("the end marker", |l| l == "END-MARK").await;
    assert!(b.lagged.is_empty());
    // A third client reads the whole ring (< 1 MiB) up to the same byte.
    let mut c = Watch::new(&client, dto.id, None).await;
    let end = b.last_seq;
    c.until("the same bytes", |w| w.last_seq >= end).await;
    // Items of different subscribers are cut differently; the bytes are the same.
    let whole = &c.raw[..usize::try_from(end).unwrap()];
    let a_part = &whole[..usize::try_from(cursor).unwrap()];
    assert_eq!(
        format!("{a_part}{}", b.raw),
        whole,
        "resume is exact (no byte lost or repeated)"
    );
    assert!(strip_vt(whole).contains("line-001999"));

    // More than the ring (1 MiB): an old cursor lags, then gets the ring from its start.
    let big = numbered_file(files.path(), "big.txt", 12_000, 120);
    keys.line(&client, &format!("type \"{}\"", big.display()))
        .await;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let listed = client.call::<terminal_list>(NoParams {}).await.unwrap();
        if listed.terminals[0].last_seq > end + (1 << 20) + (64 << 10) {
            break;
        }
        assert!(Instant::now() < deadline, "the output passes 1 MiB");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut d = Watch::new(&client, dto.id, Some(cursor)).await;
    d.until("output after the gap", |w| !w.raw.is_empty()).await;
    assert_eq!(d.lagged.len(), 1, "one terminal.lagged first");
    let missed = d.lagged[0];
    let ring_start = cursor + missed;
    let first_len = d.raw.len() as u64;
    assert!(missed > 0);
    assert_eq!(
        d.last_seq - first_len,
        ring_start,
        "the first item starts where the gap ends"
    );

    client
        .call::<terminal_close>(TerminalIdParams { id: dto.id })
        .await
        .unwrap();
    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
    show_log(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_session_limit_holds_and_daemon_stop_leaves_no_process() {
    let root = TestRoot::new();
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    std::fs::write(
        root.root.forgebox_toml(),
        format!("{toml}\n[terminal]\nmax_sessions = 3\n"),
    )
    .unwrap();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, WAIT).await;

    let mut shells = Vec::new();
    for _ in 0..3 {
        let dto = open_cmd(&client).await;
        Keyboard::new(dto.id)
            .line(&client, "ping -t 127.0.0.1")
            .await;
        shells.push(dto.pid.unwrap());
    }
    let err = client
        .call::<terminal_open>(open_params(80))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "conflict");
    assert!(err.to_string().contains("terminal.max_sessions"), "{err}");

    // Each shell runs a ping.
    let deadline = Instant::now() + WAIT;
    let pings = loop {
        let pings: Vec<u32> = children_of(&shells)
            .into_iter()
            .filter(|(_, name)| name.eq_ignore_ascii_case("ping.exe"))
            .map(|(pid, _)| pid)
            .collect();
        if pings.len() == 3 {
            break pings;
        }
        assert!(Instant::now() < deadline, "3 pings start: {pings:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let daemon_pid = daemon.pid();
    let (status, took) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
    println!("daemon.stop with 3 sessions took {took:?}");
    let left: Vec<u32> = shells
        .iter()
        .chain(&pings)
        .copied()
        .filter(|pid| alive(*pid))
        .collect();
    assert!(left.is_empty(), "processes left: {left:?}");
    let children = children_of(&[daemon_pid])
        .into_iter()
        .filter(|(pid, _)| alive(*pid))
        .collect::<Vec<_>>();
    assert!(
        children.is_empty(),
        "children of the daemon left: {children:?}"
    );
    show_log(&root);
}

// ------------------------------------------------------------------ roles (in-process)

/// Grants a fixed role to every connection.
struct FixedRole(Role);

impl Authenticator for FixedRole {
    fn authenticate(
        &self,
        _: &forge_ipc::api::HelloParams,
        _: &ConnectionInfo,
    ) -> forge_core::error::Result<Role> {
        Ok(self.0)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn viewer_and_operator_are_forbidden() {
    let test_root = TestRoot::new();
    let root = ForgeRoot::new(platform::long_path(test_root.path())).unwrap();
    let lock = InstanceLock::acquire(&root).expect("instance lock");
    let config = ForgeConfig::load_or_default(&root.forgebox_toml()).unwrap();
    let report = root
        .init(InitOptions {
            case_sensitive: config.case_sensitive,
        })
        .unwrap();
    let args = Args::parse_from([
        "forgeboxd",
        "--root",
        &root.path().display().to_string(),
        "--registry",
        &repo_registry().display().to_string(),
    ]);
    let booted = boot::boot_with(args, root, &lock, config, report, BootOptions::default())
        .await
        .expect("boot");

    let id = TerminalId::new();
    for role in [Role::Viewer, Role::Operator, Role::Admin] {
        let transport = Arc::new(InMemoryTransport::new());
        let server = Server::builder(Arc::new(FixedRole(role)))
            .router(forge_daemon::api::router(&booted.daemon))
            .build();
        let (handle, _task) = server.bind_and_spawn(transport.as_ref()).await.unwrap();
        let client = Client::new(
            transport as Arc<dyn Transport>,
            Credentials::new(IpcToken::generate().unwrap(), "role-test", "0"),
            options(),
        );
        let calls = [
            (
                "terminal.open",
                serde_json::to_value(open_params(80)).unwrap(),
            ),
            ("terminal.list", serde_json::json!({})),
            (
                "terminal.input",
                serde_json::json!({"id": id, "writer": TerminalWriterId::new(), "data": "x", "input_seq": 1}),
            ),
            (
                "terminal.resize",
                serde_json::json!({"id": id, "cols": 80, "rows": 24}),
            ),
            ("terminal.close", serde_json::json!({"id": id})),
        ];
        for (method, params) in calls {
            let result = client
                .call_raw(method, params, forge_ipc::CallOptions::default())
                .await;
            match role {
                // `open` works; the others name an unknown terminal: past the role check.
                Role::Admin if method == "terminal.open" || method == "terminal.list" => {
                    result.unwrap_or_else(|e| panic!("{method} as admin: {e}"));
                }
                Role::Admin => {
                    assert_eq!(result.unwrap_err().code(), "not_found", "{method}");
                }
                _ => assert_eq!(
                    result.unwrap_err().code(),
                    "forbidden",
                    "{method} as {role}"
                ),
            }
        }
        let sub = client
            .subscribe::<terminal_subscribe>(TerminalSubscribeParams {
                id,
                resume_from: None,
            })
            .await;
        let code = sub.err().map(|e| e.code().to_owned());
        let expected = if role == Role::Admin {
            "not_found"
        } else {
            "forbidden"
        };
        assert_eq!(
            code.as_deref(),
            Some(expected),
            "terminal.subscribe as {role}"
        );
        client.close();
        handle.shutdown();
    }
    // The admin's session is real; the shutdown closes it.
    assert_eq!(booted.daemon.terminals.len(), 1);

    booted
        .daemon
        .shutdown
        .request(forge_daemon::daemon::ShutdownRequest {
            reason: "user".into(),
            restart: false,
            keep_services: false,
        });
    forge_daemon::shutdown::run(booted, lock).await;
}
