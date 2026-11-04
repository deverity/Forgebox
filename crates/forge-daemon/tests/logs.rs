//! Log targets (M2-E3-T8) against the real `forgeboxd` on a temporary root:
//!
//! * `logs.targets` lists the daemon, each instance and each app process, with the kind,
//!   group, format and the root-relative file;
//! * `app:<host>/<process>` returns the lines the AppManager instance wrote, with
//!   `Authorization:` / `password=` masked; an unknown process or host is `not_found`;
//! * `site:<host>` is `not_found` while no web server has an access log to read (the
//!   filtering of a real access log is covered by the unit tests of `api/logs.rs`);
//! * `logs.read {before_seq}` pages back with contiguous, non-overlapping ranges;
//! * a root with a fake MySQL, a PHP 8.4 series with a log folder and a fake nginx
//!   (`fixtures/fake_nginx.rs`) with a site: `logs.targets` lists `mysql/default`,
//!   `mysql.error` (format `mysql`), `php@8.4` (format `php`, the `error_log` file, not the
//!   newest worker log), `nginx.error` / `nginx.access` and `site:<host>`; a real MySQL 8.4
//!   and PHP error line read through them has its level and time, and `site:` keeps one
//!   site's hosts of a 4-host access log.
#![cfg(windows)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use forge_ipc::api::methods::{jobs_get, logs_read, logs_targets, services_status, sites_up};
use forge_ipc::api::{
    JobDto, JobIdParams, JobState, LogChunkDto, LogTargetDto, LogTargetKind, LogsReadParams,
    NoParams, ProjectParams, ServiceState,
};
use forge_ipc::{Client, ClientError};

use common::*;

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

fn read_params(target: &str, limit: u32, before_seq: Option<u64>) -> LogsReadParams {
    LogsReadParams {
        target: target.into(),
        since_secs: None,
        grep: None,
        limit: Some(limit),
        before_seq,
    }
}

async fn read(client: &Client, target: &str, limit: u32, before: Option<u64>) -> LogChunkDto {
    client
        .call::<logs_read>(read_params(target, limit, before))
        .await
        .unwrap()
}

fn code_of(err: ClientError) -> String {
    match err {
        ClientError::Api(api) => api.code,
        other => panic!("not an API error: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn app_and_site_targets_list_read_redact_and_page_back() {
    let root = TestRoot::new();
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    std::fs::write(
        root.root.forgebox_toml(),
        format!("parked_dirs = []\n{toml}"),
    )
    .unwrap();
    let dir = root.path().join("projects").join("queue");
    std::fs::create_dir_all(&dir).unwrap();
    // The first process prints a secret and then keeps the process alive.
    std::fs::write(
        dir.join("forgebox.yaml"),
        "apiVersion: forgebox.dev/v1\nkind: Project\nmetadata: { name: queue }\nspec:\n  site: { host: queue.test, secure: false }\n  processes:\n    web: { cmd: 'echo Authorization: Bearer abc123 password=hunter2 & ping -n 300 127.0.0.1', autostart: true }\n",
    )
    .unwrap();
    let dir = forge_core::platform::long_path(&dir);
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;

    let job = client
        .call::<sites_up>(ProjectParams {
            dir: forge_core::paths::posix_string(&dir),
        })
        .await
        .unwrap();
    let done = finish(&client, job).await;
    assert_eq!(done.state, JobState::Done, "{:?}", done.error);

    // app:queue.test/web: the AppManager's lines, redacted.
    let deadline = Instant::now() + Duration::from_secs(30);
    let chunk = loop {
        let chunk = read(&client, "app:queue.test/web", 50, None).await;
        if chunk.lines.iter().any(|l| l.text.contains("Authorization")) {
            break chunk;
        }
        assert!(
            Instant::now() < deadline,
            "no app line yet: {:?}\n{}",
            chunk.lines,
            daemon.output()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let secret = chunk
        .lines
        .iter()
        .find(|l| l.text.contains("Authorization"))
        .unwrap();
    assert!(
        secret.text.contains("Authorization: ***"),
        "{}",
        secret.text
    );
    assert!(secret.text.contains("password=***"), "{}", secret.text);
    assert!(
        !chunk
            .lines
            .iter()
            .any(|l| l.text.contains("abc123") || l.text.contains("hunter2")),
        "{:?}",
        chunk.lines
    );

    // Unknown process, unknown host, a site without an access log.
    for target in [
        "app:queue.test/nope",
        "app:nope.test/web",
        "site:nope.test",
        "site:queue.test",
    ] {
        let err = client
            .call::<logs_read>(read_params(target, 5, None))
            .await
            .expect_err(target);
        assert_eq!(code_of(err), "not_found", "{target}");
    }

    // logs.targets.
    let list = client.call::<logs_targets>(NoParams {}).await.unwrap();
    let by = |target: &str| list.targets.iter().find(|t| t.target == target);
    let daemon_target = by("daemon").expect("daemon target");
    assert_eq!(daemon_target.kind, LogTargetKind::Daemon);
    assert_eq!(daemon_target.format.as_deref(), Some("json"));
    assert!(
        daemon_target
            .file
            .as_deref()
            .is_some_and(|f| f.starts_with("logs/forgeboxd.log.")),
        "{daemon_target:?}"
    );
    let app = by("app:queue.test/web").expect("app target");
    assert_eq!(app.kind, LogTargetKind::App);
    assert_eq!(app.group, "queue.test");
    assert_eq!(
        app.file.as_deref(),
        Some("logs/apps/queue.test/web.log"),
        "{app:?}"
    );
    assert!(app.live);
    assert!(by("site:queue.test").is_none(), "no access log to read");

    // before_seq: three pages of the daemon log, contiguous and disjoint.
    let deadline = Instant::now() + Duration::from_secs(20);
    while read(&client, "daemon", 1, None).await.last_seq & 0xffff_ffff < 16 {
        assert!(Instant::now() < deadline, "the daemon log stays short");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let p1 = read(&client, "daemon", 5, None).await;
    let first_of = |c: &LogChunkDto| c.lines.first().unwrap().seq;
    let last_of = |c: &LogChunkDto| c.lines.last().unwrap().seq;
    assert_eq!(p1.lines.len(), 5);
    let p2 = read(&client, "daemon", 5, Some(first_of(&p1))).await;
    assert_eq!(p2.lines.len(), 5);
    assert_eq!(last_of(&p2) + 1, first_of(&p1), "contiguous");
    let p3 = read(&client, "daemon", 5, Some(first_of(&p2))).await;
    assert_eq!(last_of(&p3) + 1, first_of(&p2), "contiguous");
    let all: Vec<u64> = [&p3, &p2, &p1]
        .iter()
        .flat_map(|c| c.lines.iter().map(|l| l.seq))
        .collect();
    assert!(all.windows(2).all(|w| w[1] == w[0] + 1), "{all:?}");
    // Every page keeps pointing at the newest line (resume point).
    assert!(p2.last_seq >= p1.last_seq);
    // The daemon's own JSON lines have level and time.
    assert!(p1.lines.iter().all(|l| l.ts.is_some() && l.level.is_some()));

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}\n{}", daemon.output());
}

fn fake(name: &str) -> PathBuf {
    static MYSQLD: OnceLock<PathBuf> = OnceLock::new();
    static PHP: OnceLock<PathBuf> = OnceLock::new();
    static NGINX: OnceLock<PathBuf> = OnceLock::new();
    let cell = match name {
        "fake_mysqld" => &MYSQLD,
        "fake_php" => &PHP,
        _ => &NGINX,
    };
    cell.get_or_init(|| compile_fixture(name)).clone()
}

fn put(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

async fn wait_running(client: &Client, tool: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let status = client.call::<services_status>(NoParams {}).await.unwrap();
        let found = status.instances.iter().find(|i| i.tool.as_str() == tool);
        if found.is_some_and(|i| i.state == ServiceState::Running) {
            return;
        }
        assert!(Instant::now() < deadline, "{tool} not running: {found:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

type Row = (String, LogTargetKind, String, String, String, String);

/// `(target, kind, group, format, label, file)` of the entries `want` names.
fn summary(list: &[LogTargetDto], want: &[&str]) -> Vec<Row> {
    want.iter()
        .map(|name| {
            let t = list
                .iter()
                .find(|t| t.target == *name)
                .unwrap_or_else(|| panic!("no target {name} in {list:#?}"));
            (
                t.target.clone(),
                t.kind,
                t.group.clone(),
                t.format.clone().unwrap_or_default(),
                t.label.clone(),
                t.file.clone().unwrap_or_default(),
            )
        })
        .collect()
}

fn row(
    target: &str,
    kind: LogTargetKind,
    group: &str,
    format: &str,
    label: &str,
    file: &str,
) -> Row {
    (
        target.to_owned(),
        kind,
        group.to_owned(),
        format.to_owned(),
        label.to_owned(),
        file.to_owned(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn targets_cover_instances_manifest_logs_pools_and_sites() {
    let root = TestRoot::new();
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    // Laragon may hold 80/443/8080: the web servers get a private range (the file ends
    // inside `[ports.ranges]`).
    let web = free_port().min(60_000);
    std::fs::write(
        root.root.forgebox_toml(),
        format!("parked_dirs = []\n{toml}web = [{web}, {}]\n", web + 20),
    )
    .unwrap();
    // Fake MySQL 8.4, PHP 8.4 (with its log folder, as the install makes it) and nginx.
    let bin = root.root.bin();
    let mysql = bin.join("mysql").join("8.4.13").join("bin");
    std::fs::create_dir_all(&mysql).unwrap();
    std::fs::copy(fake("fake_mysqld"), mysql.join("mysqld.exe")).unwrap();
    std::fs::copy(fake("fake_mysqld"), mysql.join("mysqladmin.exe")).unwrap();
    let php = bin.join("php").join("8.4.27");
    std::fs::create_dir_all(&php).unwrap();
    std::fs::copy(fake("fake_php"), php.join("php.exe")).unwrap();
    std::fs::copy(fake("fake_php"), php.join("php-cgi.exe")).unwrap();
    let nginx = bin.join("nginx").join("1.30.0");
    std::fs::create_dir_all(&nginx).unwrap();
    std::fs::copy(fake("fake_nginx"), nginx.join("nginx.exe")).unwrap();

    let logs = root.root.logs();
    // The pool's `error_log` (written first) and a newer worker log: the target is the
    // declared file, not the newest one.
    put(
        &logs.join("php").join("8.4").join("error.log"),
        "[03-Nov-2026 10:15:22 UTC] PHP Fatal error:  Uncaught Exception: boom in C:\\www\\x.php:3\n\
         Stack trace:\n#0 {main}\n  thrown in C:\\www\\x.php on line 3\n",
    );
    std::thread::sleep(Duration::from_millis(50));
    put(
        &logs.join("php").join("8.4").join("worker-9001.out.log"),
        "worker noise\n",
    );

    let dir = root.path().join("projects").join("queue");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("forgebox.yaml"),
        "apiVersion: forgebox.dev/v1\nkind: Project\nmetadata: { name: queue }\nspec:\n  site: { host: queue.test, aliases: [www.queue.test], secure: false }\n  processes:\n    web: { cmd: 'ping -n 300 127.0.0.1', autostart: true }\n",
    )
    .unwrap();
    let dir = forge_core::platform::long_path(&dir);

    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    wait_running(&client, "mysql").await;
    wait_running(&client, "nginx").await;

    put(
        &logs.join("mysql").join("default").join("error.log"),
        "2026-11-03T10:15:22.123456Z 0 [ERROR] [MY-010119] [Server] Aborting\n\
         2026-11-03T10:15:23.000001Z 0 [Warning] [MY-010068] [Server] CA certificate is self signed.\n",
    );
    let access = |t: &str, host: &str, status: u16, uri: &str| {
        format!(
            r#"{{"t":"{t}","host":"{host}","s":{status},"rt":0.012,"ut":"0.010","b":512,"m":"GET","u":"{uri}","up":"127.0.0.1:19083"}}"#
        )
    };
    let lines = [
        access("1761652800.500", "queue.test", 200, "/index.php"),
        access("1761652801.000", "other.test", 200, "/x"),
        access("1761652802.000", "www.queue.test", 404, "/missing"),
        "not json".to_owned(),
        access("1761652803.000", "blog.test", 500, "/boom"),
        access("1761652804.000", "queue.test", 503, "/down"),
    ];
    put(
        &logs.join("nginx").join("access.json"),
        &lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
    );

    let job = client
        .call::<sites_up>(ProjectParams {
            dir: forge_core::paths::posix_string(&dir),
        })
        .await
        .unwrap();
    let done = finish(&client, job).await;
    assert_eq!(done.state, JobState::Done, "{:?}", done.error);

    // The snapshot: target, kind, group, format, label, file.
    let list = client.call::<logs_targets>(NoParams {}).await.unwrap();
    let got = summary(
        &list.targets,
        &[
            "mysql/default",
            "mysql.error",
            "nginx/default",
            "nginx.error",
            "nginx.access",
            "php@8.4",
            "site:queue.test",
        ],
    );
    use LogTargetKind::{Instance, InstanceFile, Pool, Site};
    assert_eq!(
        got,
        vec![
            row(
                "mysql/default",
                Instance,
                "mysql",
                "",
                "mysql \u{b7} default",
                "logs/mysql/default.out.log"
            ),
            row(
                "mysql.error",
                InstanceFile,
                "mysql",
                "mysql",
                "mysql \u{b7} error log",
                "logs/mysql/default/error.log"
            ),
            row(
                "nginx/default",
                Instance,
                "nginx",
                "",
                "nginx \u{b7} default",
                "logs/nginx/default.out.log"
            ),
            row(
                "nginx.error",
                InstanceFile,
                "nginx",
                "nginx-error",
                "nginx \u{b7} error log",
                "logs/nginx/error.log"
            ),
            row(
                "nginx.access",
                InstanceFile,
                "nginx",
                "forgebox-json",
                "nginx \u{b7} access log (json)",
                "logs/nginx/access.json"
            ),
            row(
                "php@8.4",
                Pool,
                "php",
                "php",
                "php 8.4 \u{b7} pool",
                "logs/php/8.4/error.log"
            ),
            row(
                "site:queue.test",
                Site,
                "queue.test",
                "forgebox-json",
                "queue.test \u{b7} access",
                "logs/nginx/access.json"
            ),
        ]
    );
    assert!(
        list.targets
            .iter()
            .find(|t| t.target == "mysql/default")
            .unwrap()
            .live
    );

    // A real MySQL 8.4 line, through the `format: mysql` of the manifest log.
    let chunk = read(&client, "mysql.error", 10, None).await;
    assert_eq!(chunk.lines.len(), 2, "{:?}", chunk.lines);
    assert_eq!(chunk.lines[0].level.as_deref(), Some("error"));
    assert_eq!(
        chunk.lines[0].ts,
        Some("2026-11-03T10:15:22.123456Z".parse().unwrap())
    );
    assert_eq!(chunk.lines[1].level.as_deref(), Some("warn"));

    // The pool target: the declared error_log (not the newest worker log) with the `php`
    // parser.
    let chunk = read(&client, "php@8.4", 10, None).await;
    assert!(
        chunk.lines.iter().all(|l| !l.text.contains("worker noise")),
        "{:?}",
        chunk.lines
    );
    assert_eq!(
        chunk.lines[0].level.as_deref(),
        Some("error"),
        "{:?}",
        chunk.lines
    );
    assert!(chunk.lines[0].ts.is_some());

    // The site keeps its host and its alias, with the access fields and the status level.
    let chunk = read(&client, "site:queue.test", 10, None).await;
    let seen: Vec<(u64, &str, u16, Option<&str>)> = chunk
        .lines
        .iter()
        .map(|l| {
            let a = l.access.as_ref().expect("access fields");
            (l.seq, a.host.as_str(), a.status, l.level.as_deref())
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            (1, "queue.test", 200, Some("info")),
            (3, "www.queue.test", 404, Some("warn")),
            (6, "queue.test", 503, Some("error")),
        ]
    );
    assert_eq!(chunk.lines[0].access.as_ref().unwrap().uri, "/index.php");

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}\n{}", daemon.output());
}
