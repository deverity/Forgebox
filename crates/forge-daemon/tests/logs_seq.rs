//! `logs.subscribe` sequence domain (M2-E2-T2, ADR-0046) against the real `forgeboxd`:
//! the tail backlog, a resume replay and the live lines of an instance share one `seq`
//! per target — the line's index in the instance's file set — across daemon restarts
//! and rotations, and a resume either replays what was missed or reports it with
//! `logs.lagged{missed}`.
//!
//! The service is `tests/fixtures/fake_mailpit.rs` in its `polite` mode (`tick <n>`
//! every 50 ms, stops on the first console event), run by the daemon as
//! `mailpit/default` from the manifest, exactly like the real Mailpit.
#![cfg(windows)]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use forge_ipc::Client;
use forge_ipc::api::methods::{logs_read, logs_subscribe, services_status};
use forge_ipc::api::{LogsReadParams, LogsSubscribeParams, NoParams, ServiceState};
use forge_ipc::client::SubscriptionItem;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};

use common::*;

const TARGET: &str = "mailpit/default";

fn fake_mailpit() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| compile_fixture("fake_mailpit")).clone()
}

/// A root whose `bin/` holds the fake Mailpit in `polite` mode.
fn root_with_fake_mailpit() -> TestRoot {
    let root = TestRoot::new();
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    // The file ends inside `[ports.ranges]`.
    let toml = format!("{toml}mail = [{}, {}]\n", free_port(), free_port());
    std::fs::write(root.root.forgebox_toml(), toml).unwrap();
    let dir = root.root.bin().join("mailpit").join("1.32.0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fake_mailpit(), dir.join("mailpit.exe")).unwrap();
    std::fs::write(dir.join("fake-mode.txt"), "polite").unwrap();
    root
}

/// `logs/mailpit/default.out.log`.
fn log_file(root: &TestRoot) -> PathBuf {
    root.root.logs().join("mailpit").join("default.out.log")
}

async fn wait_running(client: &Client, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let state = client
            .call::<services_status>(NoParams {})
            .await
            .unwrap()
            .instances
            .into_iter()
            .find(|i| i.tool.as_str() == "mailpit")
            .map(|i| i.state);
        if state == Some(ServiceState::Running) {
            return;
        }
        assert!(Instant::now() < deadline, "mailpit is {state:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `seq → text` of the newest `limit` lines, through `logs.read`.
async fn read_map(client: &Client, limit: u32) -> (BTreeMap<u64, String>, u64) {
    let chunk = client
        .call::<logs_read>(LogsReadParams {
            target: TARGET.into(),
            since_secs: None,
            grep: None,
            limit: Some(limit),
        })
        .await
        .unwrap();
    (
        chunk.lines.into_iter().map(|l| (l.seq, l.text)).collect(),
        chunk.last_seq,
    )
}

/// Waits until the target's file set has at least `n` lines.
async fn wait_lines(client: &Client, n: u64) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (_, last) = read_map(client, 1).await;
        if last >= n {
            return last;
        }
        assert!(Instant::now() < deadline, "only {last} lines");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// What a raw subscription receives.
#[derive(Debug, Clone, PartialEq)]
enum Got {
    Line {
        seq: u64,
        text: String,
        source: String,
    },
    Lagged(u64),
}

/// A raw connection (hello + one `logs.subscribe`): shows the answer's `last_seq` and
/// every `logs.line` / `logs.lagged` exactly as the daemon sent them.
struct RawSub {
    conn: forge_ipc::transport::Connection,
    last_seq: u64,
}

impl RawSub {
    async fn open(root: &TestRoot, params: Value) -> Self {
        let token = forge_ipc::Credentials::local(&root.root, "logs-seq", "0").unwrap();
        let mut conn = forge_ipc::transport::local_transport(&root.root)
            .connect()
            .await
            .unwrap();
        let rid = || forge_core::ids::RequestId::new().to_string();
        let mut answer = Value::Null;
        for frame in [
            json!({"jsonrpc": "2.0", "id": 1, "method": "system.hello", "request_id": rid(),
                   "params": {"token": token.token.secret(), "client": {"name": "logs-seq", "version": "0"},
                              "api_version": {"major": 1, "minor": 0}}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "logs.subscribe", "request_id": rid(),
                   "params": params}),
        ] {
            conn.sink.send(frame.to_string()).await.unwrap();
            loop {
                let text = tokio::time::timeout(Duration::from_secs(10), conn.stream.next())
                    .await
                    .expect("answer in time")
                    .unwrap()
                    .unwrap();
                let value: Value = serde_json::from_str(&text).unwrap();
                if value.get("id").is_some() {
                    assert!(value["result"].is_object(), "{value}");
                    answer = value["result"].clone();
                    break;
                }
            }
        }
        Self {
            conn,
            last_seq: answer["last_seq"].as_u64().expect("last_seq"),
        }
    }

    async fn next(&mut self, timeout: Duration) -> Option<Got> {
        let text = tokio::time::timeout(timeout, self.conn.stream.next())
            .await
            .ok()??
            .ok()?;
        let value: Value = serde_json::from_str(&text).unwrap();
        match value["method"].as_str() {
            Some("logs.line") => Some(Got::Line {
                seq: value["params"]["seq"].as_u64().unwrap(),
                text: value["params"]["line"]["text"].as_str().unwrap().to_owned(),
                source: value["params"]["line"]["source"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            }),
            Some("logs.lagged") => Some(Got::Lagged(value["params"]["missed"].as_u64().unwrap())),
            _ => panic!("unexpected frame {value}"),
        }
    }

    /// Items until `lines` lines arrived (at most 20 s).
    async fn take(&mut self, lines: usize) -> Vec<Got> {
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while got.iter().filter(|g| matches!(g, Got::Line { .. })).count() < lines {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.next(left).await {
                Some(item) => got.push(item),
                None => panic!("only {got:?} after 20 s"),
            }
        }
        got
    }
}

fn seqs(got: &[Got]) -> Vec<u64> {
    got.iter()
        .filter_map(|g| match g {
            Got::Line { seq, .. } => Some(*seq),
            Got::Lagged(_) => None,
        })
        .collect()
}

fn assert_contiguous(what: &str, seqs: &[u64]) {
    assert!(
        seqs.windows(2).all(|w| w[1] == w[0] + 1),
        "{what}: seq must grow by one, got {seqs:?}"
    );
}

/// Every received line has the text the files have for its `seq`.
fn assert_same_as_files(what: &str, got: &[Got], files: &BTreeMap<u64, String>) {
    let mut checked = 0;
    for item in got {
        if let Got::Line { seq, text, .. } = item {
            if let Some(on_disk) = files.get(seq) {
                assert_eq!(on_disk, text, "{what}: seq {seq}");
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "{what}: nothing to compare with the files");
}

fn params(tail: Option<u32>, resume_from: Option<u64>) -> Value {
    json!({"target": TARGET, "tail": tail, "resume_from": resume_from})
}

// ------------------------------------------------------------------ daemon restart

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tail_and_live_lines_share_one_seq_across_a_daemon_restart() {
    let root = root_with_fake_mailpit();
    let mut first = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut first, Duration::from_secs(30)).await;
    wait_running(&client, Duration::from_secs(30)).await;
    wait_lines(&client, 20).await;

    // A client that stays subscribed through the restart (resumes by itself).
    let mut kept = client
        .subscribe::<logs_subscribe>(LogsSubscribeParams {
            target: TARGET.into(),
            grep: None,
            tail: Some(3),
            resume_from: None,
        })
        .await
        .unwrap();
    let mut kept_seqs = Vec::new();
    while kept_seqs.len() < 10 {
        match tokio::time::timeout(Duration::from_secs(10), kept.next_item()).await {
            Ok(Some(SubscriptionItem::Item(env))) => kept_seqs.push(env.seq),
            other => panic!("before the restart: {other:?}"),
        }
    }
    let (status, _) = stop(&client, &mut first, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
    let written_before = std::fs::read_to_string(log_file(&root))
        .unwrap()
        .lines()
        .count() as u64;

    // The restarted daemon numbers its live lines after everything in the files.
    let mut second = spawn_daemon(&root, &[]);
    let (client2, _) = connect(&root, &mut second, Duration::from_secs(30)).await;
    wait_running(&client2, Duration::from_secs(30)).await;
    let mut sub = RawSub::open(&root, params(Some(5), None)).await;
    assert!(
        sub.last_seq >= written_before,
        "last_seq {} must continue the {written_before} lines of the file",
        sub.last_seq
    );
    let got = sub.take(25).await;
    let received = seqs(&got);
    assert!(
        !got.iter().any(|g| matches!(g, Got::Lagged(_))),
        "nothing lost: {got:?}"
    );
    assert_eq!(received[0], sub.last_seq - 4, "the 5-line backlog first");
    assert_contiguous("tail + live after a restart", &received);
    let live: Vec<&Got> = got
        .iter()
        .filter(|g| matches!(g, Got::Line { seq, .. } if *seq > sub.last_seq))
        .collect();
    assert!(live.len() >= 20, "live lines are delivered: {got:?}");
    assert!(
        live.iter()
            .all(|g| matches!(g, Got::Line { source, text, .. } if source == "stdout" && text.starts_with("tick "))),
        "{live:?}"
    );
    let (files, _) = read_map(&client2, 2_000).await;
    assert_same_as_files("after a restart", &got, &files);

    // The subscription kept across the restart resumed: nothing repeated, nothing
    // skipped, and it reaches the new process's lines.
    let target = sub.last_seq + 10;
    while kept_seqs.last().is_none_or(|s| *s < target) {
        match tokio::time::timeout(Duration::from_secs(20), kept.next_item()).await {
            Ok(Some(SubscriptionItem::Item(env))) => kept_seqs.push(env.seq),
            Ok(Some(SubscriptionItem::Lagged { missed })) => {
                panic!("the resumed subscription lost {missed} lines: {kept_seqs:?}")
            }
            other => panic!("after the restart: {other:?} ({kept_seqs:?})"),
        }
    }
    assert_contiguous("subscription kept across the restart", &kept_seqs);

    drop(kept);
    let (status, _) = stop(&client2, &mut second, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
}

// ------------------------------------------------------------------ rotation

/// Fills `logs/mailpit/` like a long-running instance: `.4` … `.1` with 10 lines each
/// and the current file a few bytes below the 10 MiB rotation threshold.
fn seed_full_set(root: &TestRoot) -> u64 {
    let file = log_file(root);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    for index in 1..=4 {
        let text: String = (0..10).map(|i| format!("old {index} {i}\n")).collect();
        std::fs::write(file.with_extension(format!("log.{index}")), text).unwrap();
    }
    let line = |i: u64| format!("fill {i:09} {}\n", "x".repeat(84)); // 100 bytes
    let count = (10 * 1024 * 1024 - 40) / 100;
    let mut text = String::with_capacity((count * 100) as usize);
    for i in 0..count {
        text.push_str(&line(i));
    }
    std::fs::write(&file, text).unwrap();
    40 + count
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn seq_stays_strictly_monotonic_across_a_rotation() {
    let root = root_with_fake_mailpit();
    let seeded = seed_full_set(&root);
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
    wait_running(&client, Duration::from_secs(30)).await;

    let mut before = RawSub::open(&root, params(Some(2), None)).await;
    assert!(before.last_seq >= seeded);
    let got = before.take(60).await; // ~3 s of ticks: the file rotates after a few
    let received = seqs(&got);
    assert!(!got.iter().any(|g| matches!(g, Got::Lagged(_))), "{got:?}");
    assert_contiguous("across the rotation", &received);
    assert!(
        log_file(&root).with_extension("log.seq").is_file(),
        "the rotation recorded the dropped lines"
    );
    let state: Value = serde_json::from_str(
        &std::fs::read_to_string(log_file(&root).with_extension("log.seq")).unwrap(),
    )
    .unwrap();
    assert_eq!(state["dropped"], 10, "the old .4 was dropped: {state}");
    assert_eq!(state["rotations"].as_u64().unwrap() % 2, 0, "{state}");
    let one = std::fs::read_to_string(log_file(&root).with_extension("log.1")).unwrap();
    assert!(one.starts_with("fill 000000000"), "the full file became .1");

    // A subscription taken after the rotation: its backlog spans `.1` and the new file.
    let mut after = RawSub::open(&root, params(Some(150), None)).await;
    let got_after = after.take(170).await;
    let received_after = seqs(&got_after);
    assert!(
        !got_after.iter().any(|g| matches!(g, Got::Lagged(_))),
        "{got_after:?}"
    );
    assert_contiguous("tail across .1 and the new file", &received_after);
    assert!(
        got_after
            .iter()
            .any(|g| matches!(g, Got::Line { text, .. } if text.starts_with("fill "))),
        "the backlog reaches into .1: {got_after:?}"
    );
    assert!(
        received_after[0] <= *received.last().unwrap(),
        "both subscriptions number the same lines alike"
    );
    let (files, _) = read_map(&client, 400).await;
    assert_same_as_files("before", &got, &files);
    assert_same_as_files("after", &got_after, &files);

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
}

// ------------------------------------------------------------------ resume

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resume_replays_what_was_missed_or_reports_it_lagged() {
    let root = root_with_fake_mailpit();
    // 1000 lines were rotated away long ago; `.1` and the file hold 1001..=1010.
    let file = log_file(&root);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        file.with_extension("log.seq"),
        r#"{"dropped":1000,"rotations":8}"#,
    )
    .unwrap();
    std::fs::write(
        file.with_extension("log.1"),
        (1..=5).map(|i| format!("old {i}\n")).collect::<String>(),
    )
    .unwrap();
    std::fs::write(
        &file,
        (6..=10).map(|i| format!("old {i}\n")).collect::<String>(),
    )
    .unwrap();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
    wait_running(&client, Duration::from_secs(30)).await;

    // Connected, then gone for 1.5 s while the service keeps writing.
    let mut first = RawSub::open(&root, params(Some(1), None)).await;
    let got = first.take(5).await;
    let cursor = *seqs(&got).last().unwrap();
    drop(first);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let mut resumed = RawSub::open(&root, params(None, Some(cursor))).await;
    assert!(
        resumed.last_seq >= cursor + 15,
        "the service wrote while we were away ({cursor} → {})",
        resumed.last_seq
    );
    let got = resumed
        .take((resumed.last_seq - cursor) as usize + 10)
        .await;
    assert!(!got.iter().any(|g| matches!(g, Got::Lagged(_))), "{got:?}");
    let received = seqs(&got);
    assert_eq!(
        received[0],
        cursor + 1,
        "replay starts right after the cursor"
    );
    assert_contiguous("replay then live", &received);
    assert!(
        *received.last().unwrap() > resumed.last_seq,
        "and goes on live"
    );
    let (files, _) = read_map(&client, 2_000).await;
    assert_same_as_files("resume", &got, &files);

    // A cursor older than the retained lines: the gap is reported, then the files.
    let mut old = RawSub::open(&root, params(None, Some(500))).await;
    let got = old.take(12).await;
    assert_eq!(
        got[0],
        Got::Lagged(500),
        "lines 501..=1000 are gone: {got:?}"
    );
    match &got[1] {
        Got::Line { seq, text, source } => {
            assert_eq!(
                (*seq, text.as_str(), source.as_str()),
                (1001, "old 1", "file")
            )
        }
        other => panic!("{other:?}"),
    }
    assert_contiguous("after the lagged", &seqs(&got));

    // The client library's own resume after a dropped connection.
    let mut sub = client
        .subscribe::<logs_subscribe>(LogsSubscribeParams {
            target: TARGET.into(),
            grep: None,
            tail: Some(1),
            resume_from: None,
        })
        .await
        .unwrap();
    let mut seen = Vec::new();
    while seen.len() < 40 {
        if seen.len() == 5 || seen.len() == 20 {
            client.disconnect();
        }
        match tokio::time::timeout(Duration::from_secs(20), sub.next_item()).await {
            Ok(Some(SubscriptionItem::Item(env))) => seen.push(env.seq),
            other => panic!("{other:?} after {seen:?}"),
        }
    }
    assert_contiguous("client resume after disconnects", &seen);

    drop(sub);
    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
}
