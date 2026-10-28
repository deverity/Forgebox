//! Pre-checks with real processes, sockets and the real TCP table (docs/03 §1.5; roadmap
//! M1-E3-T3 acceptance: a port taken by another process ends in
//! `Failed(PortInUse{pid, exe})`). Configuration checks run the fake service of
//! `tests/fake_service` in one-shot mode (`--say`, `--say-err`, `--fail-if-exists`,
//! `--exit-code`).
#![cfg(windows)]

mod support;

use std::collections::{BTreeSet, HashMap};
use std::net::{Ipv4Addr, TcpListener};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use forge_core::event::EventKind;
use forge_core::ids::ToolId;
use forge_core::platform::{
    RuntimeDep, RuntimeDeps, VCREDIST_X64_URL, VcRedistFlavor, VcRuntimeVersion, port_table,
    runtime_deps,
};
use forge_supervisor::precheck::{self, ConfigCheck, DependencyFuture, DependencyRef};
use forge_supervisor::{
    FailureReason, HealthSpec, InstanceHandle, InstanceState, PrecheckSpec, Probe,
    QUICK_ASK_TIMEOUT,
};
use support::*;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// File name of this test binary (`precheck_windows-<hash>.exe`).
fn exe_name() -> String {
    std::env::current_exe()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn tcp_health(port: u16) -> HealthSpec {
    HealthSpec::new(Probe::Tcp {
        addr: format!("127.0.0.1:{port}"),
    })
}

/// Configuration check running the fake service with `args`.
fn check(args: &[&str]) -> ConfigCheck {
    ConfigCheck::new(fake_command(args))
}

fn kinds_only(events: &[forge_core::event::Event]) -> Vec<EventKind> {
    events.iter().map(|e| e.kind).collect()
}

async fn failure(instance: &InstanceHandle) -> FailureReason {
    match instance.status(QUICK_ASK_TIMEOUT).await.unwrap().state {
        InstanceState::Failed { reason } => reason,
        other => panic!("expected failed, got {other:?}"),
    }
}

/// Waits until no process is left in the daemon's root job (configuration checks run in
/// jobs nested in it).
async fn wait_root_empty(supervisor: &forge_supervisor::Supervisor, timeout: Duration) {
    let root = Arc::clone(supervisor.jobs().root());
    let deadline = Instant::now() + timeout;
    loop {
        let root = Arc::clone(&root);
        let pids = tokio::task::spawn_blocking(move || root.pids())
            .await
            .unwrap()
            .unwrap();
        if pids.is_empty() {
            return;
        }
        assert!(Instant::now() < deadline, "processes left: {pids:?}");
        tokio::time::sleep(ms(25)).await;
    }
}

/// Acceptance: a declared port held by another process fails the start with that
/// process' PID and executable, before the service is created.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn occupied_port_fails_the_start_with_pid_and_exe() {
    let squatter = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = squatter.local_addr().unwrap().port();
    let (supervisor, bus) = supervisor();
    let spec =
        fake_spec("web", &["--polite"]).with_precheck(PrecheckSpec::default().with_ports([port]));
    let instance = supervisor.spawn_instance(spec).unwrap();
    let mut events = bus.subscribe();

    let err = instance.start(ASK).await.unwrap_err();
    assert_eq!(err.code(), "port_in_use", "{err}");
    let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!(
        (status.pid, status.runs),
        (None, 1),
        "no process was created"
    );
    let FailureReason::PortInUse {
        port: reported,
        pid,
        exe,
    } = failure(&instance).await
    else {
        panic!("expected port_in_use");
    };
    assert_eq!(reported, port);
    assert_eq!(pid, Some(std::process::id()));
    let exe = exe.expect("the holder's executable is known");
    assert!(exe.contains(&exe_name()), "{exe}");
    println!("port {port} held by {exe} (PID {})", std::process::id());

    let published = drain_events(&mut events, &instance);
    assert_eq!(
        kinds_only(&published),
        vec![EventKind::ServiceStarting, EventKind::ServiceFailed]
    );
    let failed = &published[1].payload;
    assert_eq!(failed["error"]["code"], "port_in_use");
    assert_eq!(failed["reason"]["kind"], "port_in_use");
    assert_eq!(failed["reason"]["port"], port);
    assert_eq!(failed["reason"]["pid"], std::process::id());

    // Once the port is free the same instance starts.
    drop(squatter);
    let pid = instance.start(ASK).await.unwrap();
    assert!(alive(pid));
    instance.stop(ASK).await.unwrap();
}

/// The real TCP table: our own listener is reported with our PID and executable, an own
/// PID is never a conflict, a free port passes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn port_check_against_the_real_table() {
    let table = port_table();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let err = precheck::check_ports(&[port], &table, &BTreeSet::new())
        .await
        .unwrap_err();
    match err {
        FailureReason::PortInUse { port: p, pid, exe } => {
            assert_eq!((p, pid), (port, Some(std::process::id())));
            assert!(exe.unwrap().contains(&exe_name()));
        }
        other => panic!("{other:?}"),
    }
    let own = BTreeSet::from([std::process::id()]);
    precheck::check_ports(&[port], &table, &own).await.unwrap();
    drop(listener);
    precheck::check_ports(&[port], &table, &BTreeSet::new())
        .await
        .unwrap();
}

/// The instance's own listener never blocks its restart (the port is checked again
/// before the new run).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn own_port_does_not_block_a_restart() {
    let port = free_port();
    let (supervisor, _bus) = supervisor();
    let spec = fake_spec("own", &["--listen", &port.to_string(), "--polite"])
        .with_health(tcp_health(port))
        .with_precheck(PrecheckSpec::default().with_ports([port]));
    let instance = supervisor.spawn_instance(spec).unwrap();
    let first = instance.start(ASK).await.unwrap();
    let second = instance.restart(ASK).await.unwrap();
    assert_ne!(first, second);
    assert!(wait_dead(first, ASK).await.is_some());
    let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!((status.state, status.runs), (InstanceState::Running, 2));
    instance.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failing_config_check_reports_its_output() {
    let (supervisor, bus) = supervisor();
    let spec = fake_spec("conf", &["--polite"]).with_precheck(PrecheckSpec::default().with_check(
        check(&[
            "--say",
            "testing configuration file nginx.conf",
            "--say-err",
            "nginx: [emerg] unknown directive \"lsten\" in nginx.conf:12",
            "--exit-code",
            "1",
        ]),
    ));
    let instance = supervisor.spawn_instance(spec).unwrap();
    let mut events = bus.subscribe();
    let err = instance.start(ASK).await.unwrap_err();
    assert_eq!(err.code(), "config_invalid", "{err}");
    assert_eq!(
        failure(&instance).await,
        FailureReason::ConfigInvalid {
            stderr: "nginx: [emerg] unknown directive \"lsten\" in nginx.conf:12\n\
                     testing configuration file nginx.conf"
                .into()
        }
    );
    let published = drain_events(&mut events, &instance);
    assert_eq!(published[1].payload["reason"]["kind"], "config_invalid");
    assert_eq!(instance.status(ASK).await.unwrap().pid, None);
    wait_root_empty(&supervisor, ASK).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn passing_config_check_lets_the_service_start() {
    let (supervisor, _bus) = supervisor();
    let spec = fake_spec("conf-ok", &["--polite"]).with_precheck(
        PrecheckSpec::default().with_check(check(&["--say", "syntax is ok", "--exit-code", "0"])),
    );
    let instance = supervisor.spawn_instance(spec).unwrap();
    let pid = instance.start(ASK).await.unwrap();
    assert!(alive(pid));
    instance.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn config_check_has_a_deadline_and_leaves_nothing_behind() {
    let (supervisor, _bus) = supervisor();
    let slow = check(&["--interval-ms", "1000"]).timeout(ms(300));
    let spec = fake_spec("conf-slow", &["--polite"])
        .with_precheck(PrecheckSpec::default().with_check(slow));
    let instance = supervisor.spawn_instance(spec).unwrap();
    let started = Instant::now();
    let err = instance.start(ASK).await.unwrap_err();
    let elapsed = started.elapsed();
    assert_eq!(err.code(), "timeout", "{err}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    match failure(&instance).await {
        FailureReason::Other { code, message } => {
            assert_eq!(code, "timeout");
            assert!(message.contains("fake_service.exe"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    wait_root_empty(&supervisor, ASK).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_dll_in_the_check_is_reported_as_such() {
    let (supervisor, _bus) = supervisor();
    let spec = fake_spec("conf-dll", &["--polite"])
        .with_precheck(PrecheckSpec::default().with_check(check(&["--exit-code", "0xC0000135"])));
    let instance = supervisor.spawn_instance(spec).unwrap();
    instance.start(ASK).await.unwrap_err();
    assert_eq!(
        failure(&instance).await,
        FailureReason::MissingDll {
            exit_code: 0xC000_0135_u32 as i32
        }
    );
}

/// docs/03 §1.5: a restart with a broken configuration is refused and the running
/// process is left alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broken_config_never_takes_down_a_running_service() {
    let dir = tempfile::tempdir().unwrap();
    let flag = dir.path().join("broken");
    let flag_arg = flag.to_string_lossy().into_owned();
    let (supervisor, bus) = supervisor();
    let spec = fake_spec("guarded", &["--polite"]).with_precheck(
        PrecheckSpec::default().with_check(check(&[
            "--fail-if-exists",
            &flag_arg,
            "--exit-code",
            "0",
        ])),
    );
    let instance = supervisor.spawn_instance(spec).unwrap();
    let first = instance.start(ASK).await.unwrap();
    let mut events = bus.subscribe();

    std::fs::write(&flag, b"x").unwrap();
    let err = instance.restart(ASK).await.unwrap_err();
    assert_eq!(err.code(), "config_invalid", "{err}");
    assert!(err.to_string().contains("config broken"), "{err}");
    let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!(
        (status.state, status.pid),
        (InstanceState::Running, Some(first))
    );
    assert!(alive(first), "the running process was not touched");
    assert!(
        drain_events(&mut events, &instance).is_empty(),
        "no stop was attempted"
    );

    std::fs::remove_file(&flag).unwrap();
    let second = instance.restart(ASK).await.unwrap();
    assert_ne!(first, second);
    assert!(wait_dead(first, ASK).await.is_some());
    instance.stop(ASK).await.unwrap();
}

#[derive(Debug)]
struct FixedRuntime(Option<VcRuntimeVersion>);

impl RuntimeDeps for FixedRuntime {
    fn vcredist_version(&self) -> forge_core::Result<Option<VcRuntimeVersion>> {
        Ok(self.0)
    }
}

/// The runtime is checked first: the configuration check (which would run the tool's
/// executable) does not even start.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_runtime_fails_before_anything_runs() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("check-ran");
    let (supervisor, _bus) = supervisor();
    let old = VcRuntimeVersion::new(14, 29, 30133, 0);
    let supervisor = supervisor.with_runtime_deps(Arc::new(FixedRuntime(Some(old))));
    let spec = fake_spec("php", &["--polite"]).with_precheck(
        PrecheckSpec::default()
            .with_runtime(RuntimeDep::VcRedist(VcRedistFlavor::Vs17))
            .with_check(check(&["--touch", &marker.to_string_lossy()])),
    );
    let instance = supervisor.spawn_instance(spec).unwrap();
    let err = instance.start(ASK).await.unwrap_err();
    assert_eq!(err.code(), "not_found", "{err}");
    assert!(err.to_string().contains(VCREDIST_X64_URL), "{err}");
    assert_eq!(
        failure(&instance).await,
        FailureReason::MissingRuntime {
            flavor: "vs17".into(),
            download_url: VCREDIST_X64_URL.into(),
            installed: Some("14.29.30133.0".into()),
        }
    );
    assert!(!marker.exists(), "the configuration check did not run");
}

/// The runtime of this machine, detected for real. Tolerates a machine without any v14
/// runtime (logs it).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runtime_of_this_machine() {
    let installed = tokio::task::spawn_blocking(|| runtime_deps().vcredist_version())
        .await
        .unwrap()
        .unwrap();
    println!("VC++ v14 x64 runtime installed: {installed:?}");
    let (supervisor, _bus) = supervisor();
    for flavor in VcRedistFlavor::ALL {
        let spec = fake_spec(&format!("rt-{flavor}"), &["--polite"])
            .with_precheck(PrecheckSpec::default().with_runtime(RuntimeDep::VcRedist(flavor)));
        let instance = supervisor.spawn_instance(spec).unwrap();
        let satisfied = installed.is_some_and(|v| v.satisfies(flavor));
        match instance.start(ASK).await {
            Ok(_) => {
                assert!(satisfied, "{flavor} started without a suitable runtime");
                instance.stop(ASK).await.unwrap();
            }
            Err(err) => {
                assert!(!satisfied, "{flavor}: {err}");
                assert_eq!(
                    failure(&instance).await,
                    FailureReason::MissingRuntime {
                        flavor: flavor.to_string(),
                        download_url: VCREDIST_X64_URL.into(),
                        installed: installed.map(|v| v.to_string()),
                    }
                );
            }
        }
        println!("{flavor}: satisfied = {satisfied}");
    }
}

/// `depends_on` is answered by the dependency's actor, as the daemon will do.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dependencies_must_be_running_first() {
    let handles: Arc<Mutex<HashMap<String, InstanceHandle>>> = Arc::default();
    let known = Arc::clone(&handles);
    let lookup = move |dep: &DependencyRef| -> DependencyFuture {
        let handle = known.lock().unwrap().get(&dep.to_string()).cloned();
        Box::pin(async move {
            let handle = handle?;
            handle.status(QUICK_ASK_TIMEOUT).await.ok().map(|s| s.state)
        })
    };
    let (supervisor, _bus) = supervisor();
    let supervisor = supervisor.with_dependencies(Arc::new(lookup));
    let fake = ToolId::new("fake").unwrap();
    let db = supervisor
        .spawn_instance(fake_spec("db", &["--polite"]))
        .unwrap();
    handles
        .lock()
        .unwrap()
        .insert("fake/db".to_owned(), db.clone());
    let app = supervisor
        .spawn_instance(fake_spec("app", &["--polite"]).with_precheck(
            PrecheckSpec::default().with_dependency(DependencyRef::instance(fake.clone(), "db")),
        ))
        .unwrap();

    let err = app.start(ASK).await.unwrap_err();
    assert_eq!(err.code(), "conflict", "{err}");
    assert_eq!(
        failure(&app).await,
        FailureReason::DependencyNotRunning {
            dependency: "fake/db".into(),
            state: "stopped".into(),
        }
    );
    db.start(ASK).await.unwrap();
    let pid = app.start(ASK).await.unwrap();
    assert!(alive(pid));

    let orphan = supervisor
        .spawn_instance(fake_spec("orphan", &["--polite"]).with_precheck(
            PrecheckSpec::default().with_dependency(DependencyRef::instance(fake, "ghost")),
        ))
        .unwrap();
    orphan.start(ASK).await.unwrap_err();
    assert_eq!(
        failure(&orphan).await,
        FailureReason::DependencyNotRunning {
            dependency: "fake/ghost".into(),
            state: "unknown".into(),
        }
    );
    app.stop(ASK).await.unwrap();
    db.stop(ASK).await.unwrap();
}

/// A stop (or maintenance) during the pre-checks ends the run at once without creating
/// the process, and kills the configuration check.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_during_prechecks_creates_no_process() {
    let (supervisor, bus) = supervisor();
    let slow = check(&["--interval-ms", "1000"]).timeout(Duration::from_secs(30));
    let spec = fake_spec("slowcheck", &["--polite"])
        .with_precheck(PrecheckSpec::default().with_check(slow));
    let instance = supervisor.spawn_instance(spec).unwrap();
    let mut events = bus.subscribe();

    let starter = {
        let instance = instance.clone();
        tokio::spawn(async move { instance.start(ASK).await })
    };
    instance
        .wait_for_state(|s| *s == InstanceState::Starting, ASK)
        .await
        .unwrap();
    tokio::time::sleep(ms(300)).await;
    let asked = Instant::now();
    let report = instance.stop(ASK).await.unwrap();
    assert!(
        asked.elapsed() < Duration::from_secs(2),
        "{:?}",
        asked.elapsed()
    );
    assert_eq!(report.exit_code, None, "no process to stop");
    let err = starter.await.unwrap().unwrap_err();
    assert_eq!(err.code(), "cancelled", "{err}");
    let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!((status.state, status.pid), (InstanceState::Stopped, None));
    wait_root_empty(&supervisor, ASK).await;
    let published = drain_events(&mut events, &instance);
    assert_eq!(
        kinds_only(&published),
        vec![EventKind::ServiceStarting, EventKind::ServiceStopped]
    );
    assert!(published[1].payload["pid"].is_null());

    // Maintenance requested during the pre-checks.
    let starter = {
        let instance = instance.clone();
        tokio::spawn(async move { instance.start(ASK).await })
    };
    instance
        .wait_for_state(|s| *s == InstanceState::Starting, ASK)
        .await
        .unwrap();
    instance.enter_maintenance(ASK).await.unwrap();
    assert_eq!(starter.await.unwrap().unwrap_err().code(), "cancelled");
    let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!(status.state, InstanceState::Maintenance);
    wait_root_empty(&supervisor, ASK).await;
}
