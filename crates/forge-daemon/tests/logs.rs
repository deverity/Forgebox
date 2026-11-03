//! Log targets (M2-E3-T8) against the real `forgeboxd` on a temporary root:
//!
//! * `logs.targets` lists the daemon, each instance and each app process, with the kind,
//!   group, format and the root-relative file;
//! * `app:<host>/<process>` returns the lines the AppManager instance wrote, with
//!   `Authorization:` / `password=` masked; an unknown process or host is `not_found`;
//! * `site:<host>` is `not_found` while no web server has an access log to read (the
//!   filtering of a real access log is covered by the unit tests of `api/logs.rs`);
//! * `logs.read {before_seq}` pages back with contiguous, non-overlapping ranges.
#![cfg(windows)]

mod common;

use std::time::{Duration, Instant};

use forge_ipc::api::methods::{jobs_get, logs_read, logs_targets, sites_up};
use forge_ipc::api::{
    JobDto, JobIdParams, JobState, LogChunkDto, LogTargetKind, LogsReadParams, NoParams,
    ProjectParams,
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
