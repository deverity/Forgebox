//! Access log rotation with the real servers (M2-E5-T12, docs/14 section 3.8): nginx 1.30.6,
//! Apache httpd 2.4.69 and Caddy 2.11.7 from the shared cache, each running a minimal site
//! with the access log format of the Forgebox templates.
//!
//! What is real: the server processes (started in a Job Object with a hidden console group,
//! as the supervisor does), the manifests of `registry/tools/*.yaml` (access mapping,
//! rotation policy and `reopen`), the daemon's rotation step (`metrics::run_rotation`, with
//! the manifest's reopen command or Ctrl+Break), the tailer and the metrics service. What
//! is replaced: the instance reload path (the test runs the reopen command itself) and the
//! size, set to 1 MiB so that 50,000 requests rotate the log many times.
//!
//! Each test sends 50,000 requests over keep-alive connections and requires
//!
//! * the metrics service to have counted exactly that many (no line lost, none counted
//!   twice, across every rotation), and
//! * the log files to stay bounded: the live file never far past `max_size`, at most `keep`
//!   rotated files.
//!
//! `cargo test -p forge-daemon --test metrics_rotation_online -- --ignored --nocapture`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use forge_core::ids::{InstanceId, SiteId};
use forge_core::platform::{
    GroupOptions, ProcessCommand, ProcessControl, ProcessControlOptions, ProcessGroup,
    process_control,
};
use forge_core::store::Store;
use forge_daemon::metrics::{self, ReopenFuture, RotationTarget};
use forge_manifest::schema::Manifest;
use forge_manifest::schema::tool::{ConsoleSignal, Reload};
use forge_metrics::rotate::{Rotator, Strategy};
use forge_metrics::service::{self, ServiceConfig};
use forge_metrics::{LogSource, MetricsHandle, SiteRoute, Sites, Span, TailConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use common::*;

const REQUESTS: u64 = 50_000;
const CLIENTS: usize = 8;
/// Requests per second the clients stay under: a rotation every ~2 s at 1 MiB, the pace of a
/// busy site (unthrottled, nginx answers 25,000/s and the whole run is two seconds).
const RATE: u64 = 5_000;
const MAX_SIZE: u64 = 1 << 20;
const HOST: &str = "rot.test";
const ASK: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Server {
    Nginx,
    Apache,
    Caddy,
}

impl Server {
    fn id(self) -> &'static str {
        match self {
            Self::Nginx => "nginx",
            Self::Apache => "apache",
            Self::Caddy => "caddy",
        }
    }
}

fn registry_file(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../registry/tools")
        .join(name)
}

fn template(path: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/forge-render/templates")
            .join(path),
    )
    .unwrap()
}

/// The line of a template that starts with `prefix` (the log format, no template syntax).
fn template_line(path: &str, prefix: &str) -> String {
    let text = template(path);
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with(prefix))
        .unwrap_or_else(|| panic!("{path} has no line starting with {prefix}"));
    assert!(!line.contains("{{"), "{line}");
    line.to_owned()
}

fn posix(path: &Path) -> String {
    forge_core::paths::posix_string(path)
}

/// The server's `logs[].access` of the real manifest.
fn manifest_access(server: Server) -> forge_manifest::schema::tool::AccessSpec {
    let Manifest::Tool(manifest) =
        forge_manifest::parse::load_file(&registry_file(&format!("{}.yaml", server.id()))).unwrap()
    else {
        panic!("a tool manifest");
    };
    manifest
        .spec
        .service
        .daemon
        .as_ref()
        .unwrap()
        .logs
        .iter()
        .find_map(|l| l.access.clone())
        .expect("an access log")
}

struct Running {
    exit: forge_core::platform::ProcessExit,
    group: Arc<dyn ProcessGroup>,
    pid: u32,
    port: u16,
    /// Where the server's files are (and the folder `{{ paths.version }}` stands for).
    version_dir: PathBuf,
    conf: PathBuf,
    log: PathBuf,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.group.terminate_all(0);
    }
}

fn spawn_server(
    server: Server,
    processes: &Arc<dyn ProcessControl>,
    work: &Path,
    logs: &Path,
) -> Running {
    let port = free_port();
    let log = logs.join("access.json");
    let (exe, args, version_dir, conf, cwd): (PathBuf, Vec<String>, PathBuf, PathBuf, PathBuf) =
        match server {
            Server::Nginx => {
                let version_dir = work.join("nginx");
                online::copy_tree(&online::extracted(&online::NGINX_1_30), &version_dir);
                let conf = work.join("nginx.conf");
                std::fs::write(
                    &conf,
                    format!(
                        "worker_processes 1;\nerror_log \"{logs}/error.log\";\npid \"{logs}/nginx.pid\";\n\
                         events {{ worker_connections 1024; }}\nhttp {{\n    {format}\n    \
                         access_log \"{log}\" forgebox;\n    server {{ listen 127.0.0.1:{port}; \
                         location / {{ return 200 \"ok\"; }} }}\n}}\n",
                        logs = posix(logs),
                        format = template_line("nginx/nginx.conf.j2", "log_format forgebox"),
                        log = posix(&log),
                    ),
                )
                .unwrap();
                let args = ["-p", &posix(&version_dir), "-c", &posix(&conf)]
                    .map(str::to_owned)
                    .to_vec();
                (
                    version_dir.join("nginx.exe"),
                    args,
                    version_dir.clone(),
                    conf,
                    version_dir,
                )
            }
            Server::Apache => {
                let root = online::extracted(&online::APACHE_2_4);
                let conf = work.join("httpd.conf");
                let modules: String = ["authz_core", "log_config", "alias", "mime", "dir"]
                    .iter()
                    .map(|m| format!("LoadModule {m}_module modules/mod_{m}.so\n"))
                    .collect();
                std::fs::write(
                    &conf,
                    format!(
                        "ServerRoot \"{root}\"\nPidFile \"{logs}/httpd.pid\"\nListen 127.0.0.1:{port}\n\
                         ServerName localhost\n{modules}ErrorLog \"{logs}/error.log\"\n\
                         DocumentRoot \"{root}/htdocs\"\n<Directory \"{root}/htdocs\">\nRequire all granted\n\
                         </Directory>\n{format}\nCustomLog \"{log}\" forgebox_json\n",
                        root = posix(&root),
                        logs = posix(logs),
                        format = template_line("apache/httpd.conf.j2", "LogFormat \"{"),
                        log = posix(&log),
                    ),
                )
                .unwrap();
                let args = ["-d", &posix(&root), "-f", &posix(&conf)]
                    .map(str::to_owned)
                    .to_vec();
                (root.join("bin/httpd.exe"), args, root.clone(), conf, root)
            }
            Server::Caddy => {
                let root = online::extracted(&online::CADDY_2_11);
                let conf = work.join("Caddyfile");
                // The roll values of `Caddyfile.j2`, with the size of this test.
                std::fs::write(
                    &conf,
                    format!(
                        "{{\n    admin 127.0.0.1:{admin}\n    auto_https off\n}}\n\
                         :{port} {{\n    bind 127.0.0.1\n    log {{\n        output file \"{log}\" {{\n            \
                         roll_size 1MiB\n            roll_keep 2\n        }}\n        format json\n    }}\n    \
                         respond \"ok\"\n}}\n",
                        admin = free_port(),
                        log = posix(&log),
                    ),
                )
                .unwrap();
                let args = ["run", "--config", &posix(&conf), "--adapter", "caddyfile"]
                    .map(str::to_owned)
                    .to_vec();
                (
                    root.join("caddy.exe"),
                    args,
                    root.clone(),
                    conf,
                    work.to_path_buf(),
                )
            }
        };
    assert!(exe.is_file(), "{}", exe.display());
    let group = processes.create_group(GroupOptions::ROOT).unwrap();
    let mut command = ProcessCommand::new(&exe, &cwd);
    command.args = args.into_iter().map(Into::into).collect();
    command.env = processes.base_env();
    let path = processes
        .system_path()
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(";");
    command.env.push(("PATH".into(), path.into()));
    // Caddy keeps its storage and autosave under these; keep them out of the user profile.
    for var in ["XDG_DATA_HOME", "XDG_CONFIG_HOME"] {
        command.env.push((var.into(), work.join("xdg").into()));
    }
    command.stdout = forge_core::platform::StdioTarget::File(logs.join("server.out"));
    command.stderr = forge_core::platform::StdioTarget::File(logs.join("server.err"));
    let spawned = group.spawn(&command).unwrap();
    Running {
        exit: spawned.exit,
        group,
        pid: spawned.pid,
        port,
        version_dir,
        conf,
        log,
    }
}

/// One HTTP/1.1 keep-alive client. `get` returns once the whole response was read.
struct Client {
    port: u16,
    stream: Option<TcpStream>,
}

impl Client {
    async fn get(&mut self) -> std::io::Result<()> {
        let mut last = None;
        for _ in 0..3 {
            match self.try_get().await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    self.stream = None;
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap())
    }

    async fn try_get(&mut self) -> std::io::Result<()> {
        if self.stream.is_none() {
            let stream = TcpStream::connect(("127.0.0.1", self.port)).await?;
            stream.set_nodelay(true)?;
            self.stream = Some(stream);
        }
        let stream = self.stream.as_mut().unwrap();
        stream
            .write_all(format!("GET / HTTP/1.1\r\nHost: {HOST}\r\n\r\n").as_bytes())
            .await?;
        let mut buf = Vec::with_capacity(1024);
        let mut chunk = [0u8; 2048];
        let (head_end, close) = loop {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..at]).to_ascii_lowercase();
                break (at + 4, head.contains("connection: close"));
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
        let length: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        let mut have = buf.len() - head_end;
        while have < length {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            have += n;
        }
        if close {
            self.stream = None;
        }
        Ok(())
    }
}

/// Sizes seen while the load runs.
#[derive(Default)]
struct Sizes {
    max_live: u64,
    max_rotated: u64,
    max_rotated_files: usize,
    backups_seen: std::collections::BTreeSet<String>,
}

fn sample_sizes(log: &Path, sizes: &mut Sizes) {
    let dir = log.parent().unwrap();
    let name = log.file_name().unwrap().to_string_lossy().into_owned();
    let stem = name.trim_end_matches(".json").to_owned();
    let mut rotated = 0;
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = entry.metadata() else { continue };
        if file == name {
            sizes.max_live = sizes.max_live.max(meta.len());
        } else if file.starts_with(&name) || file.starts_with(&format!("{stem}-")) {
            // `access.json.1` (rename-reopen) or `access-<time>-size.json.gz` (Caddy).
            rotated += 1;
            sizes.max_rotated = sizes.max_rotated.max(meta.len());
            sizes.backups_seen.insert(file);
        }
    }
    sizes.max_rotated_files = sizes.max_rotated_files.max(rotated);
}

fn helper_path() -> PathBuf {
    // `forgebox-ctrl` is built next to `forgeboxd`: <target>/debug/deps/<test>.exe -> <target>/debug.
    let exe = std::env::current_exe().unwrap();
    let helper = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("forgebox-ctrl.exe");
    assert!(
        helper.is_file(),
        "{} missing: run `cargo build -p forge-supervisor` first",
        helper.display()
    );
    helper
}

/// The daemon's reopen, run here: the manifest's `command` (variables substituted, run in
/// the version folder) or the Ctrl+Break `signal`.
fn reopen_with(
    running: &Running,
    processes: Arc<dyn ProcessControl>,
    count: Arc<AtomicU64>,
) -> impl Fn(RotationTarget) -> ReopenFuture + use<> {
    let (version_dir, conf, pid) = (
        running.version_dir.clone(),
        running.conf.clone(),
        running.pid,
    );
    move |target: RotationTarget| -> ReopenFuture {
        let reopen: Reload = target.reopen.clone().expect("rename-reopen has a reopen");
        let (version_dir, conf) = (version_dir.clone(), conf.clone());
        let processes = Arc::clone(&processes);
        let count = Arc::clone(&count);
        Box::pin(async move {
            count.fetch_add(1, Ordering::SeqCst);
            tokio::task::spawn_blocking(move || -> forge_core::error::Result<bool> {
                if let Some(command) = &reopen.command {
                    let line = command
                        .replace("{{ paths.version }}", &posix(&version_dir))
                        .replace("{{ instance.config.nginx_conf }}", &posix(&conf));
                    let mut parts = line.split_whitespace();
                    let exe = version_dir.join(parts.next().unwrap());
                    let output = std::process::Command::new(exe)
                        .args(parts)
                        .current_dir(&version_dir)
                        .output()
                        .map_err(|e| forge_core::error::ForgeError::io("reopen command", e))?;
                    assert!(output.status.success(), "{output:?}");
                } else {
                    assert_eq!(reopen.signal, Some(ConsoleSignal::CtrlBreak));
                    processes.send_ctrl_break(pid, Duration::from_secs(5))?;
                }
                Ok(true)
            })
            .await
            .unwrap()
        })
    }
}

async fn run(server: Server) {
    // Long path names only: nginx mishandles the 8.3 `~1` of the short temp folder.
    let temp = forge_core::paths::plain_path(&std::fs::canonicalize(std::env::temp_dir()).unwrap());
    let work = temp.join(format!("forge-rot-{}-{}", server.id(), std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let logs = work.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let processes = process_control(ProcessControlOptions::with_ctrl_break_helper(helper_path()));
    let running = spawn_server(server, &processes, &work, &logs);

    // The manifest decides the strategy; only the size is the test's.
    let access = manifest_access(server);
    let mut policy = metrics::rotation_policy(access.rotate.as_ref());
    let expected = match server {
        Server::Nginx | Server::Apache => Strategy::RenameReopen,
        Server::Caddy => Strategy::SelfRotating,
    };
    assert_eq!(
        policy.strategy,
        expected,
        "strategy of {} in the registry",
        server.id()
    );
    assert_eq!(
        (policy.max_size, policy.keep),
        match server {
            Server::Caddy => (50 << 20, 5),
            _ => (64 << 20, 1),
        },
        "{} defaults",
        server.id()
    );
    policy.max_size = MAX_SIZE;
    let target = RotationTarget {
        key: "logs/access.json".into(),
        path: running.log.clone(),
        policy,
        instance: InstanceId::new(),
        reopen: access.rotate.as_ref().and_then(|r| r.reopen.clone()),
    };

    // Metrics service on the log, as the daemon feeds it.
    let store = Store::open_in_memory().unwrap();
    let site = SiteId::new();
    let handle: MetricsHandle = service::spawn(
        ServiceConfig {
            tick: Duration::from_millis(100),
            flush_interval: Duration::from_secs(3600),
            tail: TailConfig {
                tick: Duration::from_millis(20),
                idle_tick: Duration::from_millis(50),
                ..TailConfig::default()
            },
            ..ServiceConfig::default()
        },
        store.reader().clone(),
        store.writer().clone(),
        true,
    );
    assert!(handle.set_routes(vec![SiteRoute {
        id: site,
        host: HOST.into(),
        aliases: Vec::new(),
        metrics_enabled: true,
    }]));
    assert!(handle.set_sources(vec![LogSource {
        key: target.key.clone(),
        path: running.log.clone(),
        format: metrics::access_format(&access),
    }]));

    // Wait for the server to answer.
    let mut probe = Client {
        port: running.port,
        stream: None,
    };
    let started = Instant::now();
    while probe.get().await.is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "{} does not answer (exit {:?}); stderr: {}",
            server.id(),
            running.exit.try_exit_code(),
            std::fs::read_to_string(logs.join("server.err")).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // Requests answered so far (the probe's), counted like the rest.
    let sent = Arc::new(AtomicU64::new(1));

    // The daemon's rotation step every 200 ms.
    let reopens = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let rotation = {
        let reopen = reopen_with(&running, Arc::clone(&processes), Arc::clone(&reopens));
        let (target, stop) = (target.clone(), Arc::clone(&stop));
        tokio::spawn(async move {
            let rotator = Arc::new(Mutex::new(Rotator::default()));
            while !stop.load(Ordering::SeqCst) {
                metrics::run_rotation(&rotator, std::slice::from_ref(&target), &reopen).await;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
    };

    // File sizes, sampled while the load runs.
    let (size_tx, size_rx) = mpsc::channel::<Sizes>();
    let sampler = {
        let (log, stop) = (running.log.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut sizes = Sizes::default();
            while !stop.load(Ordering::SeqCst) {
                sample_sizes(&log, &mut sizes);
                std::thread::sleep(Duration::from_millis(50));
            }
            sample_sizes(&log, &mut sizes);
            size_tx.send(sizes).unwrap();
        })
    };

    // The load.
    let load_started = Instant::now();
    let next = Arc::new(AtomicU64::new(1));
    let mut clients = Vec::new();
    for _ in 0..CLIENTS {
        let (next, sent, port) = (Arc::clone(&next), Arc::clone(&sent), running.port);
        clients.push(tokio::spawn(async move {
            let mut client = Client { port, stream: None };
            loop {
                let n = next.fetch_add(1, Ordering::SeqCst);
                if n >= REQUESTS {
                    break;
                }
                let due = load_started + Duration::from_micros(n * 1_000_000 / RATE);
                tokio::time::sleep_until(tokio::time::Instant::from_std(due)).await;
                client.get().await.expect("request");
                sent.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }
    for client in clients {
        client.await.unwrap();
    }
    let load_time = load_started.elapsed();
    let sent = sent.load(Ordering::SeqCst);
    assert_eq!(sent, REQUESTS);

    // The service counts them all, and not one more when it settles.
    let count = |handle: MetricsHandle| async move {
        let snapshot = handle
            .query(
                Span::Window(Duration::from_secs(600)),
                Sites::One(site),
                ASK,
            )
            .await
            .unwrap();
        snapshot.samples[0].summary.counters.requests
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut counted = count(handle.clone()).await;
    while counted < sent && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        counted = count(handle.clone()).await;
    }
    tokio::time::sleep(Duration::from_secs(4)).await;
    let settled = count(handle.clone()).await;

    stop.store(true, Ordering::SeqCst);
    rotation.await.unwrap();
    sampler.join().unwrap();
    let sizes = size_rx.recv().unwrap();
    let status = handle.status(ASK).await.unwrap();
    drop(running);

    println!(
        "{}: {sent} requests in {load_time:?} ({:.0} req/s); counted {counted}, settled {settled}; \
         reopens {}; live file max {} B, rotated files max {} ({} B each at most), \
         distinct backups seen {}; tailer: {:?}",
        server.id(),
        sent as f64 / load_time.as_secs_f64(),
        reopens.load(Ordering::SeqCst),
        sizes.max_live,
        sizes.max_rotated_files,
        sizes.max_rotated,
        sizes.backups_seen.len(),
        status.sources,
    );
    let _ = std::fs::remove_dir_all(&work);

    assert_eq!(
        counted,
        sent,
        "{}: the service counted {counted} of {sent}",
        server.id()
    );
    assert_eq!(
        settled,
        sent,
        "{}: the count moved after settling",
        server.id()
    );
    // Bounded: the live file within a few MiB of max_size (the rotation looks every 200 ms
    // and the load writes MiB per second), the rotated ones within `keep`.
    assert!(
        sizes.max_live <= 4 * MAX_SIZE,
        "{}: live file reached {} B",
        server.id(),
        sizes.max_live
    );
    match server {
        Server::Nginx | Server::Apache => {
            assert!(
                reopens.load(Ordering::SeqCst) >= 3,
                "{}: only {} rotations for {sent} requests",
                server.id(),
                reopens.load(Ordering::SeqCst)
            );
            assert!(
                sizes.max_rotated_files <= 1,
                "keep = 1: {}",
                sizes.max_rotated_files
            );
            assert!(sizes.max_rotated <= 4 * MAX_SIZE, "{} B", sizes.max_rotated);
        }
        Server::Caddy => {
            assert_eq!(
                reopens.load(Ordering::SeqCst),
                0,
                "the daemon never touches Caddy's log"
            );
            assert!(
                sizes.backups_seen.len() >= 3,
                "Caddy rolled only {} times",
                sizes.backups_seen.len()
            );
            assert!(
                sizes.max_rotated_files <= 3,
                "roll_keep 2: {}",
                sizes.max_rotated_files
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the nginx 1.30.6 zip (shared cache) and forgebox-ctrl built"]
async fn nginx_rotates_by_rename_and_reopen_without_losing_a_request_online() {
    run(Server::Nginx).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the httpd 2.4.69 zip (shared cache), VC++ 2022+ and forgebox-ctrl built"]
async fn apache_rotates_by_rename_and_ctrl_break_without_losing_a_request_online() {
    run(Server::Apache).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the caddy 2.11.7 zip (shared cache)"]
async fn caddy_rolls_by_itself_and_the_tailer_follows_it_online() {
    run(Server::Caddy).await;
}
