//! `forge logs <instance> -f` after daemon restarts (M2-E2-T2): the backlog and the live
//! lines of an instance share one `seq`, so the client no longer drops the live lines
//! of a restarted daemon as duplicates, and a follower kept open through a restart
//! resumes without repeating or skipping lines.
//!
//! The service is the daemon tests' fake Mailpit (`crates/forge-daemon/tests/fixtures`)
//! in its `polite` mode: `tick <n>` every 50 ms, quick to stop.
#![cfg(windows)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::Value;

use common::*;

/// `fake_mailpit.exe`, compiled once per test binary.
fn fake_mailpit() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli_fake_mailpit");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("fake_mailpit.exe");
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../forge-daemon/tests/fixtures/fake_mailpit.rs");
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let output = Command::new(rustc)
            .args(["--edition", "2024", "-O", "-o"])
            .arg(&exe)
            .arg(&source)
            .output()
            .expect("running rustc");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        exe
    })
    .clone()
}

fn root_with_fake_mailpit() -> TestRoot {
    let root = TestRoot::new(true);
    let toml_path = root.path.join("etc").join("forgebox.toml");
    let toml = std::fs::read_to_string(&toml_path).unwrap();
    // The file ends inside `[ports.ranges]`.
    let toml = format!("{toml}mail = [{}, {}]\n", free_port(), free_port());
    std::fs::write(&toml_path, toml).unwrap();
    let dir = root.path.join("bin").join("mailpit").join("1.32.0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fake_mailpit(), dir.join("mailpit.exe")).unwrap();
    std::fs::write(dir.join("fake-mode.txt"), "polite").unwrap();
    // An instance with history: its log already has SEEDED lines.
    let logs = root.path.join("logs").join("mailpit");
    std::fs::create_dir_all(&logs).unwrap();
    let seed: String = (1..=SEEDED).map(|i| format!("seed {i}\n")).collect();
    std::fs::write(logs.join("default.out.log"), seed).unwrap();
    root
}

/// Lines in the instance log before the first start.
const SEEDED: u64 = 5_000;

/// Waits until `forge logs mailpit` shows at least `n` lines.
fn wait_lines(root: &TestRoot, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let run = root.run(&["--json", "logs", "mailpit", "-n", "50"]);
        if run.code == 0 && run.json()["lines"].as_array().is_some_and(|l| l.len() >= n) {
            return;
        }
        assert!(Instant::now() < deadline, "no log lines: {run:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `forge daemon stop`, then waits for the process to exit.
fn stop_daemon(root: &TestRoot, mut daemon: DaemonProcess) {
    root.run(&["daemon", "stop"]).assert_success();
    let deadline = Instant::now() + Duration::from_secs(60);
    while daemon.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "forgeboxd did not stop");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The next `n` JSON lines of a follower as `(seq, source, text)`.
fn take(stream: &Streaming, n: usize, what: &str) -> Vec<(u64, String, String)> {
    let mut out = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while out.len() < n {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = stream
            .next_line(left)
            .unwrap_or_else(|| panic!("{what}: only {out:?}"));
        let doc: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("{what}: not a JSON line ({e}): {line}"));
        out.push((
            doc["seq"].as_u64().unwrap(),
            doc["source"].as_str().unwrap().to_owned(),
            doc["text"].as_str().unwrap().to_owned(),
        ));
    }
    out
}

fn assert_contiguous(what: &str, lines: &[(u64, String, String)]) {
    assert!(
        lines.windows(2).all(|w| w[1].0 == w[0].0 + 1),
        "{what}: seq must grow by one: {lines:?}"
    );
}

#[test]
fn logs_follow_of_an_instance_sees_live_lines_after_daemon_restarts() {
    let root = root_with_fake_mailpit();
    let first = start_daemon(&root);
    wait_lines(&root, 10);
    stop_daemon(&root, first);

    // Regression: after a restart the backlog was numbered by file line and the live
    // lines from 1, so `forge logs -f` printed the backlog and then nothing.
    let second = start_daemon(&root);
    let mut follow = root
        .forge()
        .spawn(&["--json", "logs", "mailpit", "-f", "-n", "5"]);
    let lines = take(&follow, 25, "after the first restart");
    assert_contiguous("after the first restart", &lines);
    let live = lines
        .iter()
        .filter(|(_, source, _)| source == "stdout")
        .count();
    assert!(live >= 15, "live lines after the backlog: {lines:?}");
    assert!(
        lines[0].0 > SEEDED,
        "seq continues the file's lines: {lines:?}"
    );
    assert!(lines.iter().all(|(_, _, text)| text.starts_with("tick ")));

    // The same follower through another restart: it resumes from its last line. What the
    // new run wrote before the client reconnected is replayed from the files (as many
    // lines as the reconnect took, source "file"), then the follower goes on live.
    stop_daemon(&root, second);
    let _third = start_daemon(&root);
    let mut more = take(&follow, 1, "through the second restart");
    let deadline = Instant::now() + Duration::from_secs(30);
    while more
        .iter()
        .filter(|(_, source, _)| source == "stdout")
        .count()
        < 10
    {
        assert!(
            Instant::now() < deadline,
            "never live again after the restart: {more:?}"
        );
        more.extend(take(&follow, 1, "through the second restart"));
    }
    let mut all = lines.clone();
    all.extend(more.iter().cloned());
    assert_contiguous("one follower across a restart", &all);
    let first_live = more
        .iter()
        .position(|(_, source, _)| source == "stdout")
        .unwrap();
    assert!(
        more[first_live..]
            .iter()
            .all(|(_, source, _)| source == "stdout"),
        "the replay comes first, then only live lines: {more:?}"
    );
    assert!(
        more.iter().all(|(_, _, text)| text.starts_with("tick ")),
        "{more:?}"
    );
    assert!(follow.kill(), "the follower keeps running");
    root.stop_daemon();
}
