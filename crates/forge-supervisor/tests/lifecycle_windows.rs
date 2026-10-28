//! Health probes, restart policies, maintenance and metrics with real processes
//! (docs/03 §1.4, §6; roadmap M1-E3-T2 acceptance). Uses the fake service of
//! `tests/fake_service` (`--listen`, `--burn`, `--alloc`, `--crash-code`).
#![cfg(windows)]

mod support;

use std::time::{Duration, Instant};

use forge_core::event::{Event, EventKind};
use forge_supervisor::{
    Backoff, FailureReason, HealthSpec, InstanceState, Probe, QUICK_ASK_TIMEOUT, RestartPolicy,
};
use support::*;

/// The metrics test measures CPU: nothing else of this binary runs meanwhile.
static HEAVY: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn kinds_only(events: &[Event]) -> Vec<EventKind> {
    events.iter().map(|e| e.kind).collect()
}

fn tcp_health(port: u16) -> HealthSpec {
    HealthSpec::new(Probe::Tcp {
        addr: format!("127.0.0.1:{port}"),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_is_restarted_up_to_max_then_fails() {
    let _shared = HEAVY.read().await;
    let (supervisor, bus) = supervisor();
    let spec = fake_spec("crashloop", &["--crash-after-ms", "300"]).with_restart(
        RestartPolicy::OnFailure {
            max: 3,
            window: Duration::from_secs(60),
            backoff: Backoff {
                min: ms(100),
                max: ms(300),
            },
        },
    );
    let instance = supervisor.spawn_instance(spec).unwrap();
    let mut events = bus.subscribe();
    let started = Instant::now();
    instance.start(ASK).await.unwrap();
    let status = instance
        .wait_for_state(
            |s| matches!(s, InstanceState::Failed { .. }),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    let elapsed = started.elapsed();
    println!("crash loop ended in {elapsed:?}: {status:?}");
    assert_eq!(
        status.state,
        InstanceState::Failed {
            reason: FailureReason::CrashLoop {
                restarts: 3,
                window_ms: 60_000,
                exit_code: Some(3),
            }
        }
    );
    assert_eq!((status.runs, status.restarts), (4, 3));
    assert_eq!(status.next_restart_at, None);
    // 4 runs of ≥ 300 ms plus 100 + 200 + 300 ms of backoff.
    assert!(elapsed >= ms(4 * 300 + 600), "{elapsed:?}");

    let events = drain_events(&mut events, &instance);
    use EventKind::*;
    assert_eq!(
        kinds_only(&events),
        vec![
            ServiceStarting,
            ServiceRunning,
            ServiceCrashed,
            ServiceStarting,
            ServiceRunning,
            ServiceCrashed,
            ServiceStarting,
            ServiceRunning,
            ServiceCrashed,
            ServiceStarting,
            ServiceRunning,
            ServiceCrashed,
            ServiceFailed,
        ]
    );
    let crashed: Vec<&Event> = events.iter().filter(|e| e.kind == ServiceCrashed).collect();
    let delays: Vec<Option<u64>> = crashed
        .iter()
        .map(|e| e.payload["restart_in_ms"].as_u64())
        .collect();
    assert_eq!(delays, vec![Some(100), Some(200), Some(300), None]);
    assert!(crashed.iter().all(|e| e.payload["exit_code"] == 3));
    let runs: Vec<u64> = events
        .iter()
        .filter(|e| e.kind == ServiceStarting)
        .map(|e| e.payload["run"].as_u64().unwrap())
        .collect();
    assert_eq!(runs, vec![1, 2, 3, 4]);
    let failed = events.last().unwrap();
    assert_eq!(failed.payload["reason"]["kind"], "crash_loop");
    assert_eq!(failed.payload["reason"]["restarts"], 3);
    assert_eq!(failed.payload["error"]["code"], "io");

    // A manual start gets a fresh budget.
    instance.start(ASK).await.unwrap();
    let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!(status.state, InstanceState::Running);
    instance.stop(Duration::from_secs(30)).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clean_exit_and_never_policy_stay_crashed() {
    let _shared = HEAVY.read().await;
    let (supervisor, _bus) = supervisor();
    // on-failure does not restart a clean exit.
    let clean = supervisor
        .spawn_instance(
            fake_spec("clean", &["--crash-after-ms", "200", "--crash-code", "0"])
                .with_restart(RestartPolicy::default()),
        )
        .unwrap();
    // never does not restart a failure.
    let never = supervisor
        .spawn_instance(fake_spec("never", &["--crash-after-ms", "200"]))
        .unwrap();
    clean.start(ASK).await.unwrap();
    never.start(ASK).await.unwrap();
    for (instance, code) in [(&clean, 0), (&never, 3)] {
        let status = instance
            .wait_for_state(
                |s| matches!(s, InstanceState::Crashed { .. }),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(status.state, InstanceState::Crashed { exit_code: code });
        assert_eq!(status.next_restart_at, None);
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    for instance in [&clean, &never] {
        let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
        assert_eq!(status.runs, 1, "{status:?}");
        assert!(matches!(status.state, InstanceState::Crashed { .. }));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_cancels_a_pending_automatic_restart() {
    let _shared = HEAVY.read().await;
    let (supervisor, _bus) = supervisor();
    let spec =
        fake_spec("backoff", &["--crash-after-ms", "200"]).with_restart(RestartPolicy::OnFailure {
            max: 3,
            window: Duration::from_secs(60),
            backoff: Backoff {
                min: Duration::from_secs(2),
                max: Duration::from_secs(2),
            },
        });
    let instance = supervisor.spawn_instance(spec).unwrap();
    instance.start(ASK).await.unwrap();
    let status = instance
        .wait_for_state(
            |s| matches!(s, InstanceState::Crashed { .. }),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert!(status.next_restart_at.is_some(), "{status:?}");
    let report = instance.stop(ASK).await.unwrap();
    assert_eq!(report.strategy, None, "nothing was running");
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!(status.runs, 1, "{status:?}");
    assert_eq!(status.next_restart_at, None);
    assert_eq!(status.state, InstanceState::Crashed { exit_code: 3 });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tcp_health_passes_when_the_port_opens() {
    let _shared = HEAVY.read().await;
    let (supervisor, bus) = supervisor();
    let port = free_port();
    let port_text = port.to_string();
    let spec = fake_spec(
        "listener",
        &[
            "--polite",
            "--listen",
            &port_text,
            "--listen-delay-ms",
            "1500",
        ],
    )
    // Short probe timeout: a connect to a closed loopback port takes ~2 s to be refused
    // on Windows (SYN retries), so failed probes show up while the port is closed.
    .with_health(HealthSpec {
        timeout: ms(400),
        ..tcp_health(port)
    });
    let instance = supervisor.spawn_instance(spec).unwrap();
    let mut events = bus.subscribe();
    let mut logs = instance.subscribe_logs(ASK).await.unwrap();
    let started = Instant::now();
    let start = {
        let instance = instance.clone();
        tokio::spawn(async move { instance.start(Duration::from_secs(30)).await })
    };
    // The process exists (pid known) but listens only 1.5 s after it started.
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
        if status.pid.is_some() || Instant::now() >= deadline {
            break status;
        }
        tokio::time::sleep(ms(10)).await;
    };
    assert_eq!(status.state, InstanceState::Starting, "port not open yet");
    assert!(status.pid.is_some(), "the process exists while Starting");

    let pid = start.await.unwrap().unwrap();
    let elapsed = started.elapsed();
    wait_line(
        &mut logs,
        &format!("listening {port}"),
        Duration::from_secs(5),
    )
    .await;
    println!("Running {elapsed:?} after the start request (port opened at ~1.5 s)");
    assert!(elapsed >= ms(1_400), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!(
        (status.state.clone(), status.pid),
        (InstanceState::Running, Some(pid))
    );
    let health = status.health.expect("health status");
    assert_eq!(health.probe, "tcp");
    assert_eq!(health.passing, Some(true));
    assert!(health.checks >= 2, "{health:?}");
    assert!(
        health.last_error.is_some(),
        "probes failed before the port opened"
    );
    std::net::TcpStream::connect(("127.0.0.1", port)).expect("the fake accepts connections");

    let events = drain_events(&mut events, &instance);
    assert_eq!(
        kinds_only(&events),
        vec![EventKind::ServiceStarting, EventKind::ServiceRunning]
    );
    instance.stop(Duration::from_secs(30)).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_timeout_stops_the_process_and_fails_with_health_timeout() {
    let _shared = HEAVY.read().await;
    let (supervisor, bus) = supervisor();
    let port = free_port(); // nobody listens there
    let spec = fake_spec("never-ready", &["--polite"]).with_health(HealthSpec {
        start_timeout: ms(3_000),
        timeout: ms(500),
        ..tcp_health(port)
    });
    let instance = supervisor.spawn_instance(spec).unwrap();
    let mut events = bus.subscribe();
    let started = Instant::now();
    let err = instance.start(Duration::from_secs(30)).await.unwrap_err();
    let elapsed = started.elapsed();
    println!("start failed after {elapsed:?}: {err}");
    assert_eq!(err.code(), "timeout", "{err}");
    assert!(
        elapsed >= ms(3_000) && elapsed < Duration::from_secs(15),
        "{elapsed:?}"
    );

    let status = instance
        .wait_for_state(
            |s| matches!(s, InstanceState::Failed { .. }),
            Duration::from_secs(20),
        )
        .await
        .unwrap();
    let InstanceState::Failed {
        reason:
            FailureReason::HealthTimeout {
                timeout_ms,
                last_error,
            },
    } = &status.state
    else {
        panic!("{status:?}");
    };
    assert_eq!(*timeout_ms, 3_000);
    // Present unless the spawn alone took the whole budget (very loaded machine).
    if let Some(last_error) = last_error {
        assert!(
            last_error.contains(&format!("127.0.0.1:{port}")),
            "{last_error}"
        );
    }
    assert_eq!(status.pid, None);

    let events = drain_events(&mut events, &instance);
    let pid = events
        .iter()
        .find(|e| e.kind == EventKind::ServiceStopping)
        .and_then(|e| e.payload["pid"].as_u64())
        .expect("the process was stopped with the cascade") as u32;
    assert!(!alive(pid), "nothing is left of the run");
    assert_eq!(
        kinds_only(&events),
        vec![
            EventKind::ServiceStarting,
            EventKind::ServiceStopping,
            EventKind::ServiceFailed
        ]
    );
    assert_eq!(events[1].payload["cause"], "start_timeout");
    assert_eq!(events[1].payload["strategy"], "ctrl_break");
    assert_eq!(events[2].payload["reason"]["kind"], "health_timeout");
    assert_eq!(events[2].payload["error"]["code"], "timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn command_probe_gates_running() {
    let _shared = HEAVY.read().await;
    let (supervisor, _bus) = supervisor();
    let ok = fake_spec("cmd-ok", &["--polite"]).with_health(HealthSpec::new(Probe::Command {
        command: fake_command(&["--exit-code", "0"]),
        exit_code: 0,
    }));
    let failing = fake_spec("cmd-bad", &["--polite"]).with_health(HealthSpec {
        start_timeout: ms(6_000),
        ..HealthSpec::new(Probe::Command {
            command: fake_command(&["--exit-code", "1"]),
            exit_code: 0,
        })
    });
    let ok = supervisor.spawn_instance(ok).unwrap();
    let failing = supervisor.spawn_instance(failing).unwrap();
    let (ok_result, failing_result) = tokio::join!(
        ok.start(Duration::from_secs(30)),
        failing.start(Duration::from_secs(30))
    );
    ok_result.unwrap();
    assert_eq!(failing_result.unwrap_err().code(), "timeout");
    let status = failing
        .wait_for_state(
            |s| matches!(s, InstanceState::Failed { .. }),
            Duration::from_secs(20),
        )
        .await
        .unwrap();
    match &status.state {
        InstanceState::Failed {
            reason: FailureReason::HealthTimeout { last_error, .. },
        } => {
            let last_error = last_error.as_deref().unwrap_or_default();
            assert!(
                last_error.contains("exited with 1, expected 0"),
                "{last_error}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        ok.status(QUICK_ASK_TIMEOUT).await.unwrap().state,
        InstanceState::Running
    );
    ok.stop(Duration::from_secs(30)).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unhealthy_instance_is_stopped_restarted_then_given_up() {
    let _shared = HEAVY.read().await;
    let (supervisor, bus) = supervisor();
    let port = free_port();
    let port_text = port.to_string();
    let spec = fake_spec(
        "flaky",
        &[
            "--polite",
            "--listen",
            &port_text,
            "--unlisten-after-ms",
            "2000",
        ],
    )
    .with_health(HealthSpec {
        interval: ms(300),
        timeout: ms(500),
        failure_threshold: 2,
        ..tcp_health(port)
    })
    .with_restart(RestartPolicy::OnFailure {
        max: 1,
        window: Duration::from_secs(60),
        backoff: Backoff {
            min: ms(100),
            max: ms(100),
        },
    });
    let instance = supervisor.spawn_instance(spec).unwrap();
    let mut events = bus.subscribe();
    instance.start(ASK).await.unwrap();
    let status = instance
        .wait_for_state(
            |s| matches!(s, InstanceState::Failed { .. }),
            Duration::from_secs(40),
        )
        .await
        .unwrap();
    assert_eq!(
        status.state,
        InstanceState::Failed {
            reason: FailureReason::CrashLoop {
                restarts: 1,
                window_ms: 60_000,
                exit_code: None,
            }
        }
    );
    let events = drain_events(&mut events, &instance);
    use EventKind::*;
    assert_eq!(
        kinds_only(&events),
        vec![
            ServiceStarting,
            ServiceRunning,
            ServiceStopping,
            ServiceCrashed,
            ServiceStarting,
            ServiceRunning,
            ServiceStopping,
            ServiceCrashed,
            ServiceFailed,
        ]
    );
    for stopping in events.iter().filter(|e| e.kind == ServiceStopping) {
        assert_eq!(stopping.payload["cause"], "unhealthy");
        assert_eq!(stopping.payload["strategy"], "ctrl_break");
    }
    let crashed: Vec<&Event> = events.iter().filter(|e| e.kind == ServiceCrashed).collect();
    assert!(crashed.iter().all(|e| e.payload["cause"] == "unhealthy"));
    assert_eq!(crashed[0].payload["restart_in_ms"], 100);
    assert!(crashed[1].payload.get("restart_in_ms").is_none());
    // The polite fake leaves on CTRL_BREAK with code 0: still a failure (unhealthy).
    assert_eq!(crashed[0].payload["exit_code"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unhealthy_with_never_policy_keeps_running() {
    let _shared = HEAVY.read().await;
    let (supervisor, _bus) = supervisor();
    let port = free_port();
    let port_text = port.to_string();
    let spec = fake_spec(
        "degraded",
        &[
            "--polite",
            "--listen",
            &port_text,
            "--unlisten-after-ms",
            "2000",
        ],
    )
    .with_health(HealthSpec {
        interval: ms(200),
        timeout: ms(300),
        failure_threshold: 2,
        ..tcp_health(port)
    });
    let instance = supervisor.spawn_instance(spec).unwrap();
    let pid = instance.start(ASK).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
        let health = status.health.clone().unwrap();
        if health.consecutive_failures >= 3 {
            assert_eq!(status.state, InstanceState::Running, "{status:?}");
            assert_eq!(health.passing, Some(false));
            break;
        }
        assert!(Instant::now() < deadline, "{status:?}");
        tokio::time::sleep(ms(100)).await;
    }
    assert!(alive(pid));
    instance.stop(Duration::from_secs(30)).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_dll_exit_codes_never_restart() {
    let _shared = HEAVY.read().await;
    let (supervisor, bus) = supervisor();
    // Crash while Running with STATUS_DLL_NOT_FOUND: Crashed, then Failed, no restart.
    let running = supervisor
        .spawn_instance(
            fake_spec(
                "dll-running",
                &["--crash-after-ms", "200", "--crash-code", "0xC0000135"],
            )
            .with_restart(RestartPolicy::default()),
        )
        .unwrap();
    // Exits at once while Starting (it has a health check).
    let starting = supervisor
        .spawn_instance(
            fake_spec("dll-starting", &["--exit-code", "0xC0000135"])
                .with_health(tcp_health(free_port())),
        )
        .unwrap();
    // Any other code while Starting.
    let other = supervisor
        .spawn_instance(
            fake_spec("exit7", &["--exit-code", "7"]).with_health(tcp_health(free_port())),
        )
        .unwrap();
    let mut events = bus.subscribe();

    running.start(ASK).await.unwrap();
    let status = running
        .wait_for_state(
            |s| matches!(s, InstanceState::Failed { .. }),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert_eq!(
        status.state,
        InstanceState::Failed {
            reason: FailureReason::MissingDll {
                exit_code: -1_073_741_515
            }
        }
    );
    assert_eq!((status.runs, status.restarts), (1, 0));
    let running_events = drain_events(&mut events, &running);
    assert_eq!(
        kinds_only(&running_events),
        vec![
            EventKind::ServiceStarting,
            EventKind::ServiceRunning,
            EventKind::ServiceCrashed,
            EventKind::ServiceFailed
        ]
    );
    assert!(running_events[2].payload.get("restart_in_ms").is_none());

    let err = starting.start(ASK).await.unwrap_err();
    assert_eq!(err.code(), "not_found", "{err}");
    let status = starting.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert!(
        matches!(
            status.state,
            InstanceState::Failed {
                reason: FailureReason::MissingDll { .. }
            }
        ),
        "{status:?}"
    );

    let err = other.start(ASK).await.unwrap_err();
    assert_eq!(err.code(), "io", "{err}");
    let status = other.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!(
        status.state,
        InstanceState::Failed {
            reason: FailureReason::ExitedDuringStart { exit_code: 7 }
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn maintenance_stops_holds_and_releases_the_instance() {
    let _shared = HEAVY.read().await;
    let (supervisor, bus) = supervisor();
    let instance = supervisor
        .spawn_instance(fake_spec("upgrading", &["--polite"]))
        .unwrap();
    let mut events = bus.subscribe();
    let mut logs = instance.subscribe_logs(ASK).await.unwrap();
    let pid = instance.start(ASK).await.unwrap();
    wait_line(&mut logs, "ready", Duration::from_secs(10)).await;

    instance
        .enter_maintenance(Duration::from_secs(30))
        .await
        .unwrap();
    assert!(!alive(pid));
    let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!(status.state, InstanceState::Maintenance);
    let err = instance.start(ASK).await.unwrap_err();
    assert_eq!(err.code(), "conflict", "{err}");
    let err = instance.restart(ASK).await.unwrap_err();
    assert_eq!(err.code(), "conflict", "{err}");
    assert_eq!(instance.stop(ASK).await.unwrap().strategy, None);
    instance.enter_maintenance(ASK).await.unwrap(); // idempotent

    instance.exit_maintenance(ASK).await.unwrap();
    assert_eq!(
        instance.exit_maintenance(ASK).await.unwrap_err().code(),
        "conflict"
    );
    let pid = instance.start(ASK).await.unwrap();
    assert!(alive(pid));
    // CTRL_BREAK reaches the fake once its console and handler are set up.
    wait_line(&mut logs, "ready", Duration::from_secs(10)).await;
    instance.stop(Duration::from_secs(30)).await.unwrap();

    let events = drain_events(&mut events, &instance);
    let stopping = events
        .iter()
        .find(|e| e.kind == EventKind::ServiceStopping)
        .unwrap();
    assert_eq!(stopping.payload["cause"], "maintenance");
    use EventKind::*;
    assert_eq!(
        kinds_only(&events),
        vec![
            ServiceStarting,
            ServiceRunning,
            ServiceStopping,
            ServiceStopped,
            ServiceMaintenance,
            ServiceStopped,
            ServiceStarting,
            ServiceRunning,
            ServiceStopping,
            ServiceStopped,
        ]
    );
    assert_eq!(events[4].payload["state"], "maintenance");
}

/// One `Get-Process` reading: (UTC ticks of 100 ns, CPU time ticks, working set).
#[derive(Debug, Clone, Copy)]
struct OsReading {
    at_ticks: i64,
    cpu_ticks: i64,
    working_set: u64,
}

/// Reads the process twice through PowerShell (`Get-Process`, the counters Task Manager
/// shows), `seconds` apart.
fn get_process_twice(pid: u32, seconds: u32) -> (OsReading, OsReading) {
    let script = format!(
        "$p = Get-Process -Id {pid}; \
         $a = '{{0}} {{1}} {{2}}' -f [DateTime]::UtcNow.Ticks, $p.TotalProcessorTime.Ticks, $p.WorkingSet64; \
         Start-Sleep -Seconds {seconds}; $p.Refresh(); \
         $b = '{{0}} {{1}} {{2}}' -f [DateTime]::UtcNow.Ticks, $p.TotalProcessorTime.Ticks, $p.WorkingSet64; \
         Write-Output $a; Write-Output $b"
    );
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .expect("running powershell.exe");
    assert!(
        output.status.success(),
        "Get-Process failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let readings: Vec<OsReading> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            OsReading {
                at_ticks: parts[0].parse().unwrap(),
                cpu_ticks: parts[1].parse().unwrap(),
                working_set: parts[2].parse().unwrap(),
            }
        })
        .collect();
    assert_eq!(readings.len(), 2, "{text}");
    (readings[0], readings[1])
}

fn within(measured: f64, reference: f64, tolerance: f64) -> bool {
    (measured - reference).abs() <= reference.abs() * tolerance
}

/// Acceptance of M1-E3-T2: the sampler's CPU % and working set match what Windows
/// reports (`Get-Process`, the source of Task Manager) within 10 %, for a process that
/// burns 50 % of one CPU and holds 200 MiB.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metrics_match_get_process_within_ten_percent() {
    let _exclusive = HEAVY.write().await;
    let (supervisor, _bus) = supervisor();
    let instance = supervisor
        .spawn_instance(fake_spec(
            "burner",
            &["--polite", "--burn", "50", "--alloc", "200"],
        ))
        .unwrap();
    let mut logs = instance.subscribe_logs(ASK).await.unwrap();
    let pid = instance.start(ASK).await.unwrap();
    let allocated = wait_line(&mut logs, "allocated ", Duration::from_secs(20)).await;
    let alloc_bytes: u64 = allocated.trim_start_matches("allocated ").parse().unwrap();
    assert_eq!(alloc_bytes, 200 * 1024 * 1024);

    // Our samples during the same 8 s as the two Get-Process readings.
    let mut samples = instance.subscribe_metrics(ASK).await.unwrap();
    // Let the burn settle and get a first full 2 s window.
    tokio::time::timeout(Duration::from_secs(10), samples.wait_for(|m| m.is_some()))
        .await
        .unwrap()
        .unwrap();
    samples.mark_unchanged();
    let reader = tokio::task::spawn_blocking(move || get_process_twice(pid, 8));
    let mut ours = Vec::new();
    while !reader.is_finished() {
        if tokio::time::timeout(Duration::from_millis(500), samples.changed())
            .await
            .is_ok()
        {
            if let Some(sample) = samples.borrow_and_update().clone() {
                ours.push(sample);
            }
        }
    }
    let (a, b) = reader.await.unwrap();
    let cpus = f64::from(forge_core::platform::process_stats().logical_cpus());
    let os_pct =
        (b.cpu_ticks - a.cpu_ticks) as f64 / (b.at_ticks - a.at_ticks) as f64 * 100.0 / cpus;
    // Samples fully inside the Get-Process window (it started before the first one we
    // kept, the last ones may straddle its end: they still measure the same steady load).
    assert!(ours.len() >= 3, "{} samples", ours.len());
    let main: Vec<(f64, u64)> = ours
        .iter()
        .map(|m| {
            let process = m
                .processes
                .iter()
                .find(|p| p.pid == pid)
                .expect("the main process is sampled");
            (process.cpu_pct, process.rss_bytes)
        })
        .collect();
    let our_pct = main.iter().map(|(pct, _)| pct).sum::<f64>() / main.len() as f64;
    let last = ours.last().unwrap();
    let (_, our_rss) = *main.last().unwrap();
    let os_rss = b.working_set as f64;
    println!(
        "CPU: ours {our_pct:.3} % of the machine ({:.1} % of one CPU), Get-Process {os_pct:.3} % ({:.1} % of one CPU); {} logical CPUs",
        our_pct * cpus,
        os_pct * cpus,
        cpus
    );
    println!(
        "RSS: ours {:.1} MiB (instance total {:.1} MiB over {} processes), Get-Process {:.1} MiB, expected ≈ 200 MiB + runtime",
        our_rss as f64 / 1_048_576.0,
        last.rss_bytes as f64 / 1_048_576.0,
        last.processes.len(),
        os_rss / 1_048_576.0
    );
    println!(
        "handles {}, private {:.1} MiB, uptime {} ms, window {} ms, {} samples: {:?}",
        last.handles,
        last.private_bytes as f64 / 1_048_576.0,
        last.uptime_ms,
        last.window_ms,
        main.len(),
        main.iter()
            .map(|(p, _)| format!("{:.2}", p * cpus))
            .collect::<Vec<_>>()
    );

    // Same as Windows, ±10 %.
    assert!(
        within(our_pct, os_pct, 0.10),
        "CPU ours {our_pct} vs Get-Process {os_pct}"
    );
    assert!(
        within(our_rss as f64, os_rss, 0.10),
        "RSS ours {our_rss} vs Get-Process {os_rss}"
    );
    // Same as the load we asked for: 50 % of one CPU ±10 points, 200 MiB ±10 %.
    let per_cpu = our_pct * cpus;
    assert!((40.0..=60.0).contains(&per_cpu), "{per_cpu} % of one CPU");
    assert!(
        within(our_rss as f64, alloc_bytes as f64, 0.10),
        "RSS {our_rss} vs 200 MiB"
    );
    // The instance total includes the console host of the hidden console.
    assert!(last.rss_bytes >= our_rss);
    assert_eq!(last.pid, Some(pid));
    assert!(last.handles > 0 && last.private_bytes >= alloc_bytes);
    assert_eq!(
        instance
            .metrics(QUICK_ASK_TIMEOUT)
            .await
            .unwrap()
            .map(|m| m.pid),
        Some(Some(pid))
    );

    instance.stop(Duration::from_secs(30)).await.unwrap();
    assert_eq!(
        instance.metrics(QUICK_ASK_TIMEOUT).await.unwrap(),
        None,
        "no run, no sample"
    );
}
