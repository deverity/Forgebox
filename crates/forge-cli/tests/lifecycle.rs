//! The daemon seen from the CLI: autostart (and recovery after a crash), `--no-daemon`,
//! `forge daemon start|status|stop|restart`.

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::*;

/// Stops whatever daemon serves the root when the test ends, pass or fail.
struct StopOnDrop<'a>(&'a TestRoot);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.stop_daemon();
    }
}

fn wait_until(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn kill(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn running(root: &TestRoot) -> bool {
    root.run(&["--no-daemon", "--json", "daemon", "status"])
        .code
        == 0
}

#[test]
fn forge_starts_the_daemon_and_starts_it_again_after_it_is_killed() {
    let root = TestRoot::new(true);
    let _stop = StopOnDrop(&root);
    assert!(root.daemon_pid().is_none() && !running(&root));

    // 1. No daemon: the command starts one, says so on stderr, and works.
    let first = root.run(&["status"]);
    first.assert_success();
    assert!(first.stderr.contains("Iniciando o daemon"), "{first:?}");
    assert!(first.stdout.contains("Nenhum serviço"), "{first:?}");
    assert!(
        first.elapsed < Duration::from_secs(15),
        "{:?}",
        first.elapsed
    );
    let pid1 = root.daemon_pid().expect("the daemon described itself");
    assert!(running(&root));

    // 2. A daemon is there: nothing is started.
    let second = root.run(&["status"]);
    second.assert_success();
    assert!(!second.stderr.contains("Iniciando"), "{second:?}");
    assert_eq!(root.daemon_pid(), Some(pid1));

    // 3. The daemon dies (kill -9 style): the stale token and lock file stay behind, and
    // the next command starts a new daemon.
    kill(pid1);
    wait_until("the daemon to disappear", Duration::from_secs(20), || {
        !running(&root)
    });
    assert!(
        root.path.join("state/ipc.token").is_file(),
        "stale token expected"
    );
    let third = root.run(&["status", "--json"]);
    third.assert_success();
    assert_eq!(third.stdout, "{\"instances\":[],\"pools\":[]}\n");
    assert!(third.stderr.contains("Iniciando o daemon"), "{third:?}");
    wait_until(
        "the new daemon to describe itself",
        Duration::from_secs(10),
        || root.daemon_pid().is_some_and(|p| p != pid1),
    );
    let status = root.json(&["daemon", "status"]);
    assert_eq!(status["pid"].as_u64().map(|p| p as u32), root.daemon_pid());
}

#[test]
fn autostart_with_the_json_flag_keeps_stdout_clean() {
    let root = TestRoot::new(true);
    let _stop = StopOnDrop(&root);
    let run = root.run(&["--json", "status"]);
    run.assert_success();
    // The notice is on stderr; stdout is exactly one JSON document.
    run.json();
    let quiet = TestRoot::new(true);
    let _stop2 = StopOnDrop(&quiet);
    let run = quiet.run(&["--quiet", "status"]);
    run.assert_success();
    assert_eq!(run.stderr, "", "--quiet hides the notice: {run:?}");
}

#[test]
fn concurrent_commands_share_one_daemon() {
    let root = TestRoot::new(true);
    let _stop = StopOnDrop(&root);
    let runs: Vec<Run> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..3)
            .map(|_| scope.spawn(|| root.run(&["--json", "status"])))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for run in &runs {
        run.assert_success();
    }
    // One daemon won the mutex; the others exited quietly (code 4 of forgeboxd) and
    // every command talked to the winner.
    let status = root.json(&["daemon", "status"]);
    assert_eq!(status["pid"].as_u64().map(|p| p as u32), root.daemon_pid());
}

#[test]
fn no_daemon_flag_does_not_start_one() {
    let root = TestRoot::new(true);
    let _stop = StopOnDrop(&root);
    let run = root.run(&["--no-daemon", "status"]);
    assert_eq!(run.code, 1, "{run:?}");
    assert!(run.stderr.contains("--no-daemon"), "{run:?}");
    let json = root.run(&["--no-daemon", "--json", "status"]);
    assert_eq!(json.code, 1);
    assert_eq!(json.error_code(), "io");
    // Give a wrongly started daemon time to appear; there must be none.
    std::thread::sleep(Duration::from_millis(500));
    assert!(root.daemon_pid().is_none());
    assert!(!root.path.join("state/ipc.token").exists());
}

#[test]
fn a_missing_daemon_executable_is_a_clear_error() {
    let root = TestRoot::new(true);
    let _stop = StopOnDrop(&root);
    let run = root
        .forge()
        .env("FORGE_DAEMON_EXE", r"C:\definitely\not\here\forgeboxd.exe")
        .run(&["--json", "status"]);
    assert_eq!(run.code, 3, "{run:?}");
    assert_eq!(run.error_code(), "not_found");
    assert!(
        run.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("forgeboxd")
    );
}

#[test]
fn daemon_start_status_restart_stop() {
    let root = TestRoot::new(true);
    let _stop = StopOnDrop(&root);

    // Status of a stopped daemon: reported, exit 1, and not started by asking.
    let stopped = root.run(&["daemon", "status"]);
    assert_eq!(stopped.code, 1, "{stopped:?}");
    assert!(stopped.stdout.contains("Daemon parado"), "{stopped:?}");
    let stopped_json = root.run(&["--json", "daemon", "status"]);
    assert_eq!(stopped_json.code, 1);
    assert_eq!(stopped_json.json()["running"], false);
    assert!(root.daemon_pid().is_none());
    // Stopping a stopped daemon is fine.
    let stop = root.run(&["daemon", "stop"]);
    stop.assert_success();
    assert!(stop.stdout.contains("já estava parado"), "{stop:?}");

    // start
    let start = root.run(&["daemon", "start"]);
    start.assert_success();
    assert!(start.stdout.contains("Daemon iniciado"), "{start:?}");
    let pid1 = root.daemon_pid().unwrap();
    let again = root.run(&["daemon", "start"]);
    again.assert_success();
    assert!(again.stdout.contains("já está rodando"), "{again:?}");
    assert_eq!(root.daemon_pid(), Some(pid1));

    // status
    let status = root.json(&["daemon", "status"]);
    assert_eq!(status["pid"], pid1);
    assert_eq!(status["portable"], true);
    assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
    let human = root.run(&["daemon", "status"]);
    human.assert_success();
    assert!(human.stdout.contains(&pid1.to_string()), "{human:?}");

    // restart: a new process serves the same root.
    let restart = root.run(&["daemon", "restart"]);
    restart.assert_success();
    assert!(restart.stdout.contains("reiniciado"), "{restart:?}");
    let status = root.json(&["daemon", "status"]);
    assert_ne!(status["pid"], pid1);
    wait_until(
        "the lock file of the new daemon",
        Duration::from_secs(10),
        || root.daemon_pid() == status["pid"].as_u64().map(|p| p as u32),
    );

    // stop: the pipe is gone when the command returns, and so is the lock file.
    let stop = root.run(&["daemon", "stop"]);
    stop.assert_success();
    assert!(stop.stdout.contains("Daemon parado"), "{stop:?}");
    assert!(!running(&root));
    wait_until("the lock file to go", Duration::from_secs(10), || {
        root.daemon_pid().is_none()
    });

    // restart of a stopped daemon just starts it.
    let restart = root.run(&["daemon", "restart"]);
    restart.assert_success();
    assert!(running(&root));
}
