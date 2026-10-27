//! Real processes, real Job Objects, real console events (docs/03 §1, §7.6; roadmap
//! M1-E3-T1 acceptance). Uses the fake service of `tests/fake_service`.
#![cfg(windows)]

mod support;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio as StdStdio};
use std::time::{Duration, Instant};

use forge_core::event::{EventKind, EventStream};
use forge_supervisor::{
    InstanceHandle, InstanceState, LogItem, QUICK_ASK_TIMEOUT, StopPlan, StopStrategy,
};
use support::*;

/// Timing-sensitive tests run one at a time.
static HEAVY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const ROLE_ENV: &str = "FORGEBOX_SUPERVISOR_TEST_ROLE";

/// Not a test on its own: the body of the "daemon" subprocess of
/// `daemon_death_kills_children_*` (this test binary re-executed with `ROLE_ENV`). It
/// creates the root job, starts a stubborn fake service that spawns a stubborn child,
/// prints `PIDS <service> <grandchild>` and then dies without any cleanup.
#[test]
fn fake_daemon_role() {
    let Ok(mode) = std::env::var(ROLE_ENV) else {
        return; // normal test run: nothing to do
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (supervisor, _bus) = supervisor();
        let spec = fake_spec("daemon-role", &["--stubborn", "--spawn-child"]);
        let instance = supervisor.spawn_instance(spec).unwrap();
        let mut logs = instance.subscribe_logs(ASK).await.unwrap();
        let pid = instance.start(ASK).await.unwrap();
        let child = wait_line(&mut logs, "child ", Duration::from_secs(10)).await;
        let child: u32 = child.trim_start_matches("child ").parse().unwrap();
        println!("PIDS {pid} {child}");
        use std::io::Write;
        std::io::stdout().flush().unwrap();
        match mode.as_str() {
            // Dies like a crash: no destructor, no stop, no actor shutdown.
            "exit" => std::process::exit(0),
            // Waits to be killed by the parent (TerminateProcess).
            _ => loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
            },
        }
    });
}

/// Runs the "daemon" subprocess and returns it with the PIDs it printed.
fn start_fake_daemon(mode: &str) -> (std::process::Child, u32, u32) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "fake_daemon_role",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROLE_ENV, mode)
        .stdin(StdStdio::null())
        .stdout(StdStdio::piped())
        .stderr(StdStdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            eprintln!("[fake daemon] {line}");
            // libtest prints "test fake_daemon_role ... " right before the test output.
            if let Some((_, rest)) = line.split_once("PIDS ") {
                let pids: Vec<u32> = rest
                    .split_whitespace()
                    .map(|p| p.parse().unwrap())
                    .collect();
                let _ = tx.send((pids[0], pids[1]));
            }
        }
    });
    let (service, grandchild) = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("fake daemon did not report its PIDs");
    (child, service, grandchild)
}

async fn assert_children_die_with_daemon(mode: &str) {
    let processes = processes();
    let (mut daemon, service, grandchild) = start_fake_daemon(mode);
    // Watch by handle before the daemon dies: immune to PID reuse.
    let mut service_exit = processes.watch(service).unwrap();
    let mut grandchild_exit = processes.watch(grandchild).unwrap();
    assert!(processes.is_alive(service).unwrap());
    assert!(processes.is_alive(grandchild).unwrap());

    if mode == "kill" {
        daemon.kill().unwrap();
    }
    let status = tokio::task::spawn_blocking(move || daemon.wait())
        .await
        .unwrap()
        .unwrap();
    let died_at = Instant::now();
    assert!(
        mode == "kill" || status.success(),
        "daemon exit: {status:?}"
    );

    let service_code = service_exit
        .wait_timeout(Duration::from_secs(2))
        .await
        .unwrap()
        .expect("service survived the daemon for more than 2 s");
    let grandchild_code = grandchild_exit
        .wait_timeout(Duration::from_secs(2).saturating_sub(died_at.elapsed()))
        .await
        .unwrap()
        .expect("grandchild survived the daemon for more than 2 s");
    let elapsed = died_at.elapsed();
    println!(
        "[{mode}] service {service} exit {service_code:#x}, grandchild {grandchild} exit \
         {grandchild_code:#x}, both gone {elapsed:?} after the daemon"
    );
    assert!(elapsed < Duration::from_secs(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_death_kills_children_on_process_exit() {
    assert_children_die_with_daemon("exit").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_death_kills_children_on_terminate_process() {
    assert_children_die_with_daemon("kill").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn polite_service_stops_with_ctrl_break_at_step_two() {
    let (supervisor, bus) = supervisor();
    let mut spec = fake_spec("polite", &["--polite"]);
    spec.stop = StopPlan {
        // Succeeds but does not stop the service: the cascade waits `timeout` then moves on.
        command: Some(fake_command(&["--exit-code", "0"])),
        timeout: Duration::from_secs(1),
        ..StopPlan::default()
    };
    let instance = supervisor.spawn_instance(spec).unwrap();
    let mut events = bus.subscribe();
    let mut logs = instance.subscribe_logs(ASK).await.unwrap();
    let pid = instance.start(ASK).await.unwrap();
    wait_line(&mut logs, "ready", Duration::from_secs(10)).await;

    let report = instance.stop(Duration::from_secs(30)).await.unwrap();
    assert_eq!(report.strategy, Some(StopStrategy::CtrlBreak), "{report:?}");
    assert_eq!(
        report.attempted,
        vec![StopStrategy::Command, StopStrategy::CtrlBreak]
    );
    assert_eq!(
        report.exit_code,
        Some(0),
        "clean exit from the CTRL_BREAK handler"
    );
    assert_eq!(
        report.leftovers_killed, 0,
        "the console host exits by itself"
    );
    assert!(report.elapsed_ms < 5_000, "{report:?}");
    wait_line(&mut logs, "ctrl-break", Duration::from_secs(5)).await;
    assert!(!alive(pid));

    let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!(status.state, InstanceState::Stopped);
    let events = drain_events(&mut events, &instance);
    assert_eq!(
        kinds(&events),
        vec![
            (EventKind::ServiceStarting, String::new()),
            (EventKind::ServiceRunning, String::new()),
            (EventKind::ServiceStopping, "command".into()),
            (EventKind::ServiceStopping, "ctrl_break".into()),
            (EventKind::ServiceStopped, "ctrl_break".into()),
        ]
    );
    assert!(events.windows(2).all(|w| w[0].seq < w[1].seq));
    assert_eq!(events[2].payload["step"], 1);
    assert_eq!(events[3].payload["step"], 2);
    assert_eq!(events[2].payload["timeout_ms"], 1000);
    assert_eq!(events[3].payload["timeout_ms"], 5000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stuck_stop_reaches_terminate_and_actors_stay_responsive() {
    let _heavy = HEAVY.lock().await;
    let (supervisor, bus) = supervisor();
    let stubborn = supervisor
        .spawn_instance(fake_spec("stubborn", &["--stubborn"]))
        .unwrap();
    let other = supervisor
        .spawn_instance(fake_spec("other", &["--polite"]))
        .unwrap();
    let mut events = bus.subscribe();
    let mut stubborn_logs = stubborn.subscribe_logs(ASK).await.unwrap();
    let mut other_logs = other.subscribe_logs(ASK).await.unwrap();
    let stubborn_pid = stubborn.start(ASK).await.unwrap();
    other.start(ASK).await.unwrap();
    // "ready" is printed after the console handlers are installed.
    wait_line(&mut stubborn_logs, "ready", Duration::from_secs(10)).await;
    wait_line(&mut other_logs, "ready", Duration::from_secs(10)).await;
    let plan = StopPlan::default();
    assert_eq!(plan.timeout, Duration::from_secs(15));

    let stopping = {
        let stubborn = stubborn.clone();
        let budget = plan.worst_case() + Duration::from_secs(5);
        tokio::spawn(async move {
            let started = Instant::now();
            let report = stubborn.stop(budget).await;
            (report, started.elapsed())
        })
    };

    // 20 samples spread over the stuck stop (≈ 14 s).
    let mut other_latencies = Vec::new();
    let mut own_latencies = Vec::new();
    let mut strategies = Vec::new();
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(700)).await;
        let asked = Instant::now();
        let status = other.status(QUICK_ASK_TIMEOUT).await.unwrap();
        other_latencies.push(asked.elapsed());
        assert_eq!(status.state, InstanceState::Running);

        let asked = Instant::now();
        let status = stubborn.status(QUICK_ASK_TIMEOUT).await.unwrap();
        own_latencies.push(asked.elapsed());
        strategies.push(status.state);
    }
    let max_other = other_latencies.iter().max().unwrap();
    let max_own = own_latencies.iter().max().unwrap();
    println!(
        "status latency during the stuck stop: other actor max {max_other:?}, stopping actor max {max_own:?}"
    );
    println!("other actor samples: {other_latencies:?}");
    assert!(
        other_latencies
            .iter()
            .all(|d| *d < Duration::from_millis(50)),
        "{other_latencies:?}"
    );
    assert!(
        own_latencies.iter().all(|d| *d < Duration::from_millis(50)),
        "{own_latencies:?}"
    );
    let during_ctrl_break = InstanceState::Stopping {
        strategy: Some(StopStrategy::CtrlBreak),
    };
    assert!(strategies.contains(&during_ctrl_break), "{strategies:?}");

    let (report, elapsed) = stopping.await.unwrap();
    let report = report.unwrap();
    println!("stuck stop report: {report:?} after {elapsed:?}");
    assert_eq!(report.strategy, Some(StopStrategy::Terminate));
    assert_eq!(
        report.attempted,
        vec![StopStrategy::CtrlBreak, StopStrategy::Terminate],
        "no window: WM_CLOSE is skipped"
    );
    assert_eq!(
        report.exit_code,
        Some(forge_supervisor::TERMINATED_EXIT_CODE)
    );
    assert!(elapsed >= Duration::from_secs(15), "{elapsed:?}");
    assert!(
        elapsed <= plan.timeout + Duration::from_secs(2),
        "{elapsed:?}"
    );
    assert!(!alive(stubborn_pid));

    let events = drain_events(&mut events, &stubborn);
    assert_eq!(
        kinds(&events),
        vec![
            (EventKind::ServiceStarting, String::new()),
            (EventKind::ServiceRunning, String::new()),
            (EventKind::ServiceStopping, "ctrl_break".into()),
            (EventKind::ServiceStopping, "terminate".into()),
            (EventKind::ServiceStopped, "terminate".into()),
        ]
    );
    assert_eq!(events[2].payload["timeout_ms"], 15_000);

    let report = other.stop(Duration::from_secs(30)).await.unwrap();
    assert_eq!(report.strategy, Some(StopStrategy::CtrlBreak));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_log_subscriber_affects_neither_process_nor_other_clients() {
    let _heavy = HEAVY.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let log_file = dir.path().join("logs").join("fake").join("chatty.out.log");
    let (supervisor, _bus) = supervisor();
    let mut spec = fake_spec("chatty", &["--interval-ms", "1", "--lines-per-tick", "5"]);
    spec.logs.file = Some(log_file.clone());
    spec.logs.broadcast_capacity = 256;
    let instance = supervisor.spawn_instance(spec).unwrap();

    let mut slow = instance.subscribe_logs(ASK).await.unwrap(); // never reads for 5 s
    let mut fast = instance.subscribe_logs(ASK).await.unwrap();
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    let collector = tokio::spawn(async move {
        let mut seqs = Vec::new();
        let mut last_tick = 0_u64;
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                item = fast.recv_item() => match item {
                    Some(LogItem::Line(line)) => {
                        seqs.push(line.seq);
                        if let Some(n) = line.text.strip_prefix("tick ") {
                            last_tick = n.parse().unwrap();
                        }
                    }
                    Some(LogItem::Lagged { missed }) => panic!("fast subscriber lagged by {missed}"),
                    None => break,
                },
            }
        }
        (seqs, last_tick, fast.missed())
    });

    let pid = instance.start(ASK).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let size_1s = std::fs::metadata(&log_file).map(|m| m.len()).unwrap_or(0);
    tokio::time::sleep(Duration::from_secs(4)).await;
    let size_5s = std::fs::metadata(&log_file).unwrap().len();
    let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!(status.state, InstanceState::Running, "process kept running");
    assert!(alive(pid));
    stop_tx.send(()).unwrap();
    let (seqs, last_tick, fast_missed) = collector.await.unwrap();

    println!(
        "5 s with a stalled subscriber: file {size_1s} → {size_5s} bytes, fast subscriber got {} lines (last tick {last_tick}), missed {fast_missed}",
        seqs.len()
    );
    assert!(
        size_5s > size_1s * 2,
        "log file keeps growing: {size_1s} → {size_5s}"
    );
    assert!(
        last_tick > 2_000,
        "process kept writing: last tick {last_tick}"
    );
    assert_eq!(fast_missed, 0);
    assert!(
        seqs.windows(2).all(|w| w[1] == w[0] + 1),
        "fast subscriber saw every line in order"
    );
    match slow.recv_item().await.unwrap() {
        LogItem::Lagged { missed } => {
            println!("stalled subscriber: Lagged {{ missed: {missed} }}");
            assert!(missed > 0);
        }
        other => panic!("stalled subscriber should report lag first, got {other:?}"),
    }

    instance.stop(Duration::from_secs(30)).await.unwrap();
    // Every line reached the file, in order, despite the stalled subscriber.
    let text = std::fs::read_to_string(&log_file).unwrap();
    let ticks: Vec<u64> = text
        .lines()
        .filter_map(|l| l.strip_prefix("tick ").and_then(|n| n.parse().ok()))
        .collect();
    assert!(ticks.len() as u64 >= last_tick);
    assert!(
        ticks.iter().enumerate().all(|(i, n)| *n == i as u64 + 1),
        "file has every tick in order"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_command_stops_service_at_step_one() {
    let dir = tempfile::tempdir().unwrap();
    let flag = dir.path().join("stop.flag");
    let flag_text = flag.to_str().unwrap();
    let (supervisor, _bus) = supervisor();
    let mut spec = fake_spec("cmd", &["--stubborn", "--stop-file", flag_text]);
    spec.stop.command = Some(fake_command(&["--touch", flag_text]));
    let instance = supervisor.spawn_instance(spec).unwrap();
    instance.start(ASK).await.unwrap();
    let report = instance.stop(Duration::from_secs(30)).await.unwrap();
    assert_eq!(report.strategy, Some(StopStrategy::Command), "{report:?}");
    assert_eq!(report.attempted, vec![StopStrategy::Command]);
    assert_eq!(report.exit_code, Some(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failing_stop_command_escalates_at_once() {
    let (supervisor, _bus) = supervisor();
    let mut spec = fake_spec("failcmd", &["--polite"]);
    spec.stop.command = Some(fake_command(&["--exit-code", "3"]));
    spec.stop.timeout = Duration::from_secs(20);
    let instance = supervisor.spawn_instance(spec).unwrap();
    let mut logs = instance.subscribe_logs(ASK).await.unwrap();
    instance.start(ASK).await.unwrap();
    wait_line(&mut logs, "ready", Duration::from_secs(10)).await;
    let report = instance.stop(Duration::from_secs(40)).await.unwrap();
    assert_eq!(report.strategy, Some(StopStrategy::CtrlBreak), "{report:?}");
    assert!(
        report.elapsed_ms < 5_000,
        "did not wait the 20 s timeout: {report:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wm_close_stops_a_windowed_process_at_step_three() {
    let (supervisor, bus) = supervisor();
    let mut spec = fake_spec("gui", &["--stubborn", "--window"]);
    spec.stop.timeout = Duration::from_secs(1);
    let instance = supervisor.spawn_instance(spec).unwrap();
    let mut events = bus.subscribe();
    let mut logs = instance.subscribe_logs(ASK).await.unwrap();
    instance.start(ASK).await.unwrap();
    wait_line(&mut logs, "window", Duration::from_secs(10)).await;
    let report = instance.stop(Duration::from_secs(30)).await.unwrap();
    assert_eq!(report.strategy, Some(StopStrategy::WmClose), "{report:?}");
    assert_eq!(
        report.attempted,
        vec![StopStrategy::CtrlBreak, StopStrategy::WmClose]
    );
    assert_eq!(report.exit_code, Some(0));
    wait_line(&mut logs, "wm_close", Duration::from_secs(5)).await;
    let stopping: Vec<_> = kinds(&drain_events(&mut events, &instance))
        .into_iter()
        .filter(|(kind, _)| *kind == EventKind::ServiceStopping)
        .map(|(_, strategy)| strategy)
        .collect();
    assert_eq!(stopping, vec!["ctrl_break", "wm_close"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_is_reported_and_leftovers_are_killed() {
    let (supervisor, bus) = supervisor();
    let instance = supervisor
        .spawn_instance(fake_spec(
            "crashy",
            &["--spawn-child", "--crash-after-ms", "500"],
        ))
        .unwrap();
    let mut events = bus.subscribe();
    let mut logs = instance.subscribe_logs(ASK).await.unwrap();
    instance.start(ASK).await.unwrap();
    let child: u32 = wait_line(&mut logs, "child ", Duration::from_secs(10))
        .await
        .trim_start_matches("child ")
        .parse()
        .unwrap();
    let status = instance
        .wait_for_state(
            |s| matches!(s, InstanceState::Crashed { .. }),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert_eq!(status.state, InstanceState::Crashed { exit_code: 3 });
    assert_eq!(status.pid, None);
    assert!(
        wait_dead(child, Duration::from_secs(2)).await.is_some(),
        "grandchild killed"
    );
    let events = drain_events(&mut events, &instance);
    let crashed = events
        .iter()
        .find(|e| e.kind == EventKind::ServiceCrashed)
        .expect("service.crashed");
    assert_eq!(crashed.payload["exit_code"], 3);
    // The stubborn grandchild, plus the console host it kept alive.
    let leftovers = crashed.payload["leftovers_killed"].as_u64().unwrap();
    assert!((1..=2).contains(&leftovers), "{leftovers}");
    // Stop on a crashed instance is a no-op; start works again.
    let report = instance.stop(ASK).await.unwrap();
    assert_eq!(report.strategy, None);
    let pid = instance.start(ASK).await.unwrap();
    assert!(alive(pid));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn environment_and_cwd_are_deterministic() {
    assert!(
        std::env::var_os("CARGO_MANIFEST_DIR").is_some(),
        "cargo sets it for the test process"
    );
    let (supervisor, _bus) = supervisor();
    let mut spec = fake_spec(
        "env",
        &[
            "--print-env",
            "PATH",
            "--print-env",
            "PHPRC",
            "--print-env",
            "CARGO_MANIFEST_DIR",
            "--print-env",
            "SystemRoot",
            "--print-cwd",
        ],
    );
    spec.spawn.env.set("PHPRC", r"C:\Forgebox\etc\php\8.4");
    let dir = spec.spawn.cwd.clone();
    let instance = supervisor.spawn_instance(spec).unwrap();
    let mut logs = instance.subscribe_logs(ASK).await.unwrap();
    instance.start(ASK).await.unwrap();
    let path = wait_line(&mut logs, "env PATH", Duration::from_secs(10)).await;
    let system = processes().system_path();
    let expected = std::env::join_paths(std::iter::once(&dir).chain(&system)).unwrap();
    assert_eq!(path, format!("env PATH={}", expected.to_string_lossy()));
    assert_eq!(
        wait_line(&mut logs, "env PHPRC", Duration::from_secs(5)).await,
        r"env PHPRC=C:\Forgebox\etc\php\8.4"
    );
    assert_eq!(
        wait_line(&mut logs, "env CARGO_MANIFEST_DIR", Duration::from_secs(5)).await,
        "env CARGO_MANIFEST_DIR unset",
        "nothing outside the allowlist leaks from the daemon"
    );
    assert!(
        wait_line(&mut logs, "env SystemRoot", Duration::from_secs(5))
            .await
            .contains('=')
    );
    let cwd = wait_line(&mut logs, "cwd ", Duration::from_secs(5)).await;
    assert_eq!(
        cwd.replace('/', "\\"),
        format!("cwd {}", dir.display()).replace('/', "\\")
    );
    instance.stop(Duration::from_secs(30)).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_gives_a_new_pid_and_log_subscriptions_survive() {
    let (supervisor, _bus) = supervisor();
    let instance = supervisor
        .spawn_instance(fake_spec("restart", &["--polite", "--stderr"]))
        .unwrap();
    let mut logs = instance.subscribe_logs(ASK).await.unwrap();
    let first = instance.start(ASK).await.unwrap();
    assert_eq!(
        instance.start(ASK).await.unwrap(),
        first,
        "start is idempotent"
    );
    let ready = wait_line(&mut logs, "ready", Duration::from_secs(10)).await;
    assert_eq!(ready, "ready");
    wait_line(&mut logs, "err ", Duration::from_secs(10)).await;
    let second = instance.restart(Duration::from_secs(30)).await.unwrap();
    assert_ne!(first, second);
    assert!(!alive(first));
    wait_line(&mut logs, "ready", Duration::from_secs(10)).await;
    let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!(
        (status.state, status.pid, status.runs),
        (InstanceState::Running, Some(second), 2)
    );
    instance.stop(Duration::from_secs(30)).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_failure_is_reported() {
    let (supervisor, bus) = supervisor();
    let mut spec = fake_spec("missing", &[]);
    spec.spawn.exe = spec.spawn.cwd.join("no-such-service.exe");
    let instance = supervisor.spawn_instance(spec).unwrap();
    let mut events = bus.subscribe();
    let err = instance.start(ASK).await.unwrap_err();
    assert_eq!(err.code(), "not_found", "{err}");
    let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert!(
        matches!(&status.state, InstanceState::Failed { code, .. } if code == "not_found"),
        "{status:?}"
    );
    let events = drain_events(&mut events, &instance);
    assert_eq!(events.last().unwrap().kind, EventKind::ServiceFailed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_the_last_handle_kills_the_process() {
    let (supervisor, _bus) = supervisor();
    let instance = supervisor
        .spawn_instance(fake_spec("dropped", &["--stubborn"]))
        .unwrap();
    let pid = instance.start(ASK).await.unwrap();
    let mut exit = processes().watch(pid).unwrap();
    drop(instance);
    let code = exit.wait_timeout(Duration::from_secs(5)).await.unwrap();
    assert_eq!(code, Some(forge_supervisor::TERMINATED_EXIT_CODE));
}

/// Regression: the leftovers of a crash used to be terminated with a fire-and-forget
/// `TerminateJobObject`, and the crash was reported while the termination was still in
/// flight. Windows refuses `AssignProcessToJobObject` on such a job (`Access denied`) for
/// a few ms (longer under load), so a start issued right after `service.crashed` failed.
/// Each round starts the instance the moment the crash is published; four instances
/// crash-loop at the same time to load the machine like a full test run does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_right_after_a_crash_with_leftovers_always_succeeds() {
    let _heavy = HEAVY.lock().await;
    let (supervisor, bus) = supervisor();
    let loops: Vec<_> = (1..=4)
        .map(|n| {
            let instance = supervisor
                .spawn_instance(fake_spec(
                    &format!("crashloop-{n}"),
                    &["--spawn-child", "--crash-after-ms", "150"],
                ))
                .unwrap();
            let events = bus.subscribe();
            tokio::spawn(crash_loop(instance, events, 12))
        })
        .collect();
    for task in loops {
        task.await.unwrap();
    }
}

async fn crash_loop(instance: InstanceHandle, mut events: EventStream, rounds: u32) {
    let name = instance.name().to_owned();
    let mut pid = instance.start(ASK).await.unwrap();
    for round in 1..=rounds {
        let crashed = wait_event(
            &mut events,
            &instance,
            EventKind::ServiceCrashed,
            Duration::from_secs(20),
        )
        .await;
        assert_eq!(crashed.payload["exit_code"], 3, "{name} round {round}");
        let leftovers = crashed.payload["leftovers_killed"].as_u64().unwrap();
        assert!(
            leftovers >= 1,
            "{name} round {round}: the stubborn child was a leftover"
        );
        let next = instance.start(ASK).await.unwrap_or_else(|err| {
            panic!("{name} round {round}: start right after the crash failed: {err}")
        });
        assert_ne!(next, pid, "{name} round {round}");
        assert!(!alive(pid), "{name} round {round}: previous run is gone");
        pid = next;
    }
    instance
        .wait_for_state(
            |s| matches!(s, InstanceState::Crashed { .. }),
            Duration::from_secs(20),
        )
        .await
        .unwrap();
}

/// Regression: a restart of a service that ignores `CTRL_BREAK` goes through the
/// `Terminate` step, and the new run is spawned into the same instance job right after
/// it. It must never see the job still terminating.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stubborn_restart_through_terminate_always_succeeds() {
    let (supervisor, bus) = supervisor();
    let mut spec = fake_spec("stubborn-restart", &["--stubborn", "--spawn-child"]);
    spec.stop.timeout = Duration::from_millis(300);
    let instance = supervisor.spawn_instance(spec).unwrap();
    let mut events = bus.subscribe();
    let mut logs = instance.subscribe_logs(ASK).await.unwrap();
    let mut pid = instance.start(ASK).await.unwrap();
    for round in 1..=5 {
        // `child <pid>` comes after the console handler is installed.
        let child: u32 = wait_line(&mut logs, "child ", Duration::from_secs(10))
            .await
            .trim_start_matches("child ")
            .parse()
            .unwrap();
        let next = instance
            .restart(Duration::from_secs(30))
            .await
            .unwrap_or_else(|err| panic!("round {round}: restart failed: {err}"));
        let stopped = wait_event(
            &mut events,
            &instance,
            EventKind::ServiceStopped,
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(stopped.payload["strategy"], "terminate", "round {round}");
        assert_ne!(next, pid, "round {round}");
        assert!(!alive(pid), "round {round}: old service is gone");
        assert!(!alive(child), "round {round}: old grandchild is gone");
        assert!(alive(next), "round {round}: new service runs");
        pid = next;
    }
    let report = instance.stop(Duration::from_secs(30)).await.unwrap();
    assert_eq!(report.strategy, Some(StopStrategy::Terminate));
}

/// A stop that ends with `TerminateJobObject` followed at once by a start.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_right_after_a_terminated_stop_always_succeeds() {
    let (supervisor, _bus) = supervisor();
    let mut spec = fake_spec("terminated", &["--stubborn", "--spawn-child"]);
    spec.stop.timeout = Duration::from_millis(200);
    let instance = supervisor.spawn_instance(spec).unwrap();
    let mut logs = instance.subscribe_logs(ASK).await.unwrap();
    for round in 1..=5 {
        let pid = instance
            .start(ASK)
            .await
            .unwrap_or_else(|err| panic!("round {round}: start failed: {err}"));
        wait_line(&mut logs, "child ", Duration::from_secs(10)).await;
        let report = instance.stop(Duration::from_secs(30)).await.unwrap();
        assert_eq!(
            report.strategy,
            Some(StopStrategy::Terminate),
            "round {round}"
        );
        assert!(!alive(pid), "round {round}");
    }
}
