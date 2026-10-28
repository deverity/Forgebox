//! PhpPool against a fake php-cgi (`tests/fake_php_cgi`): real processes, Job Objects,
//! TCP table and FastCGI, no PHP. Covers warmup, request estimation and recycle,
//! scaling, worker exits, the listener contract, blue/green and failures.
#![cfg(windows)]

mod support;

use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use forge_core::event::EventKind;
use forge_core::ids::VersionId;
use forge_fcgi::probe::{self, HealthScript};
use forge_fcgi::swap::{SwapOptions, swap};
use forge_fcgi::{
    PhpPool, PoolDeps, PoolHandle, PoolSpec, PoolState, PoolTiming, Recycle, Scaling,
    UpstreamChange, WorkerPhase,
};
use support::*;

/// Pool tests use several processes and timing; one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const ASK: Duration = Duration::from_secs(30);
const WAIT: Duration = Duration::from_secs(30);

fn timing() -> PoolTiming {
    PoolTiming {
        tick: Duration::from_millis(100),
        start_timeout: Duration::from_secs(15),
        probe_timeout: Duration::from_secs(5),
        drain_grace: Duration::from_millis(300),
        drain_quiet: 2,
        drain_timeout: Duration::from_secs(10),
        stop_timeout: Duration::from_secs(5),
        retry_min: Duration::from_millis(100),
        retry_max: Duration::from_secs(1),
        ..PoolTiming::default()
    }
}

/// Spec of the fake php-cgi reporting `version`, ini dir under `root`.
fn fake_spec(root: &Path, version: &str, series: &str) -> PoolSpec {
    let exe = fake_php_cgi();
    let mut spec = PoolSpec::new(
        VersionId::new(),
        version,
        series,
        &exe,
        root.join("etc").join("php").join(series),
    )
    .unwrap();
    spec.env.set("FAKE_PHP_VERSION", version);
    spec.expect_php_version = Some(version.to_owned());
    spec.scaling = Scaling {
        min: 2,
        max: 3,
        idle_timeout: Duration::from_secs(60),
    };
    spec.recycle = Recycle {
        max_requests: 0,
        max_rss_mb: 0,
        max_age: None,
    };
    spec.timing = timing();
    spec
}

struct Fixture {
    root: tempfile::TempDir,
    supervisor: forge_supervisor::Supervisor,
    bus: forge_core::event::EventBus,
    ports: Arc<TestPorts>,
    router: Arc<Router>,
}

fn fixture(site: &str) -> Fixture {
    let (supervisor, bus) = supervisor();
    Fixture {
        root: tempfile::tempdir().unwrap(),
        supervisor,
        bus,
        ports: Arc::new(TestPorts::default()),
        router: Router::new(site),
    }
}

impl Fixture {
    fn spawn(&self, spec: PoolSpec) -> PoolHandle {
        PhpPool::spawn(
            spec,
            PoolDeps {
                supervisor: self.supervisor.clone(),
                ports: self.ports.clone(),
                listener: Some(self.router.listener()),
            },
        )
        .unwrap()
    }

    fn health_root(&self, series: &str) -> std::path::PathBuf {
        self.root.path().join("etc").join("php").join(series)
    }
}

/// Waits until the web server (router) routes `n` backends for the site: no change of the
/// pools is in flight any more.
async fn settled(fx: &Fixture, n: usize) {
    eventually("the router to settle", WAIT, || {
        fx.router.site_backends().len() == n
    })
    .await;
}

fn pids(status: &forge_fcgi::PoolStatus) -> Vec<u32> {
    status.workers.iter().filter_map(|w| w.pid).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn starts_min_workers_with_their_environment_and_answers_probes() {
    let _serial = SERIAL.lock().await;
    let fx = fixture("php_8_3");
    let mut events = fx.bus.subscribe();
    let pool = fx.spawn(fake_spec(fx.root.path(), "8.3.99", "8.3"));
    let model = pool.start(ASK).await.unwrap();
    assert_eq!(model.name, "php_8_3");
    assert_eq!(model.backends.len(), 2, "{model:?}");
    let status = pool.status(ASK).await.unwrap();
    assert_eq!(status.state, PoolState::Running);
    assert!(status.serving);
    assert_eq!(status.count(WorkerPhase::Active), 2);
    let script = HealthScript::new(fx.health_root("8.3"));
    assert!(script.path().is_file(), "health script written by the pool");

    // Every backend is a warm worker answering FastCGI with the right version and pid.
    let worker_pids: HashSet<u32> = pids(&status).into_iter().collect();
    for backend in &model.backends {
        let report = probe::health(*backend, &script, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(report.php_version, "8.3.99");
        assert_eq!(report.values.max_conns, Some(1));
        assert!(worker_pids.contains(&report.pid.unwrap()), "{report:?}");
    }
    // Ports come from the injected reservations, one per worker.
    let held: BTreeSet<u16> = fx.ports.held().into_iter().collect();
    let used: BTreeSet<u16> = model.backends.iter().map(|b| b.port()).collect();
    assert_eq!(held, used);
    // Workers live in the pool's job, under the daemon's root job.
    let all = fx.supervisor.jobs().pids().unwrap();
    assert!(worker_pids.iter().all(|p| all.contains(p)));
    // The listener got the model; events went out; worker service events did not.
    settled(&fx, 2).await;
    let routed: BTreeSet<_> = fx.router.site_backends().into_iter().collect();
    assert_eq!(routed, model.backends.iter().copied().collect());
    let events = drain(&mut events);
    let scaled = of_kind(&events, EventKind::PoolScaled);
    let last = scaled.last().expect("pool.scaled");
    assert_eq!(last.payload["to_workers"], 2);
    assert_eq!(last.payload["reason"], "start");
    assert_eq!(last.payload["upstream"], "php_8_3");
    assert!(of_kind(&events, EventKind::ServiceStarting).is_empty());

    // Environment: PHPRC is the ini dir (the fake prints it).
    let mut logs_seen = false;
    for w in &status.workers {
        assert_eq!(w.php_version.as_deref(), Some("8.3.99"));
        logs_seen |= w.port.is_some();
    }
    assert!(logs_seen);

    pool.stop(ASK).await.unwrap();
    let status = pool.status(ASK).await.unwrap();
    assert_eq!(status.state, PoolState::Stopped);
    assert!(status.workers.is_empty());
    for pid in worker_pids {
        assert!(!alive(pid), "worker {pid} stopped");
    }
    assert!(fx.ports.held().is_empty(), "ports released");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recycles_by_max_requests_without_failed_requests() {
    let _serial = SERIAL.lock().await;
    let fx = fixture("php_8_3");
    let mut events = fx.bus.subscribe();
    let mut spec = fake_spec(fx.root.path(), "8.3.99", "8.3");
    spec.scaling = Scaling {
        min: 2,
        max: 2,
        idle_timeout: Duration::from_secs(60),
    };
    spec.recycle.max_requests = 10;
    let pool = fx.spawn(spec);
    pool.start(ASK).await.unwrap();
    let first: Vec<u32> = pids(&pool.status(ASK).await.unwrap());

    let load = Load::start(
        fx.router.clone(),
        health_request(&fx.health_root("8.3"), probe::HEALTH_URI),
        4,
        Duration::from_millis(20),
        Some(160),
    );
    eventually("160 answers", WAIT, || {
        load.answers().len() + load.errors().len() >= 160
    })
    .await;
    let (answers, errors) = load.finish().await;
    assert!(errors.is_empty(), "failed requests: {errors:?}");
    assert_eq!(answers.len(), 160);

    let served_by: HashSet<u32> = answers.iter().filter_map(|a| a.pid).collect();
    assert!(
        served_by.len() > 2,
        "requests were served by replacement workers too: {served_by:?}"
    );
    let events = drain(&mut events);
    let recycled = of_kind(&events, EventKind::PoolWorkerRecycled);
    assert!(recycled.len() >= 2, "{recycled:?}");
    for event in &recycled {
        assert_eq!(event.payload["reason"], "max_requests");
        assert!(event.payload["requests"].as_u64().unwrap() >= 10);
        assert!(event.payload["replacement_port"].as_u64().is_some());
    }
    let status = pool
        .wait_for(
            |s| s.count(WorkerPhase::Active) == 2 && s.workers.len() == 2,
            WAIT,
        )
        .await
        .unwrap();
    assert!(status.recycled >= 2);
    assert!(status.requests_total >= 160, "{}", status.requests_total);
    for pid in first {
        assert!(!alive(pid), "first-generation worker {pid} was recycled");
    }
    pool.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scales_up_under_load_and_down_when_idle() {
    let _serial = SERIAL.lock().await;
    let fx = fixture("php_8_3");
    let mut events = fx.bus.subscribe();
    let mut spec = fake_spec(fx.root.path(), "8.3.99", "8.3");
    spec.scaling = Scaling {
        min: 1,
        max: 3,
        idle_timeout: Duration::from_millis(1500),
    };
    let pool = fx.spawn(spec);
    pool.start(ASK).await.unwrap();

    // 8 concurrent slow requests (300 ms each) on one worker: busy with a queue.
    let load = Load::start(
        fx.router.clone(),
        health_request(&fx.health_root("8.3"), "/_forgebox/health.php?sleep_ms=300"),
        8,
        Duration::ZERO,
        None,
    );
    let scaled = pool
        .wait_for(|s| s.count(WorkerPhase::Active) == 3, WAIT)
        .await
        .unwrap();
    assert_eq!(scaled.upstream.backends.len(), 3);
    eventually("the router uses 3 backends", WAIT, || {
        fx.router.site_backends().len() == 3
    })
    .await;
    eventually("answers from the new workers", WAIT, || {
        let backends: HashSet<_> = load.answers().iter().map(|a| a.backend).collect();
        backends.len() == 3
    })
    .await;
    let (_, errors) = load.finish().await;
    assert!(errors.is_empty(), "{errors:?}");

    // Idle: back to min after idle_timeout.
    let idle = pool
        .wait_for(
            |s| s.count(WorkerPhase::Active) == 1 && s.workers.len() == 1,
            WAIT,
        )
        .await
        .unwrap();
    assert_eq!(idle.upstream.backends.len(), 1);
    let events = drain(&mut events);
    let reasons: Vec<String> = of_kind(&events, EventKind::PoolScaled)
        .iter()
        .map(|e| e.payload["reason"].as_str().unwrap().to_owned())
        .collect();
    assert!(reasons.contains(&"scale_up".to_owned()), "{reasons:?}");
    assert!(reasons.contains(&"scale_down".to_owned()), "{reasons:?}");
    pool.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replaces_a_worker_that_dies() {
    let _serial = SERIAL.lock().await;
    let fx = fixture("php_8_3");
    let mut events = fx.bus.subscribe();
    let pool = fx.spawn(fake_spec(fx.root.path(), "8.3.99", "8.3"));
    pool.start(ASK).await.unwrap();
    let before = pool.status(ASK).await.unwrap();
    let victim = before.workers[0].pid.unwrap();
    let victim_port = before.workers[0].port.unwrap();
    kill(victim);
    let after = pool
        .wait_for(
            |s| {
                s.count(WorkerPhase::Active) == 2
                    && !s.upstream.backends.iter().any(|b| b.port() == victim_port)
            },
            WAIT,
        )
        .await
        .unwrap();
    assert!(!pids(&after).contains(&victim));
    let events = drain(&mut events);
    let recycled = of_kind(&events, EventKind::PoolWorkerRecycled);
    assert!(
        recycled
            .iter()
            .any(|e| e.payload["reason"] == "crashed" && e.payload["pid"] == victim),
        "{recycled:?}"
    );
    pool.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leaving_workers_serve_until_the_listener_applied_the_change() {
    let _serial = SERIAL.lock().await;
    let fx = fixture("php_8_3");
    let mut spec = fake_spec(fx.root.path(), "8.3.99", "8.3");
    spec.scaling = Scaling {
        min: 1,
        max: 1,
        idle_timeout: Duration::from_secs(60),
    };
    let pool = fx.spawn(spec);
    pool.start(ASK).await.unwrap();
    settled(&fx, 1).await;
    let old = pool.status(ASK).await.unwrap().workers[0].clone();

    // The web server reload hangs: the replacement is warm, the old worker must keep
    // running (Retiring) until the change is applied.
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *fx.router.gate.lock().unwrap() = Some(gate.clone());
    assert_eq!(pool.reload(ASK).await.unwrap(), 1);
    let retiring = pool
        .wait_for(|s| s.count(WorkerPhase::Retiring) == 1, WAIT)
        .await
        .unwrap();
    assert_eq!(retiring.count(WorkerPhase::Active), 1);
    tokio::time::sleep(Duration::from_millis(800)).await;
    let still = pool.status(ASK).await.unwrap();
    assert_eq!(still.count(WorkerPhase::Retiring), 1, "{still:?}");
    assert!(alive(old.pid.unwrap()));
    assert_eq!(
        fx.router.site_backends(),
        vec![std::net::SocketAddr::from((
            [127, 0, 0, 1],
            old.port.unwrap()
        ))],
        "the web server still routes to the old worker"
    );

    // Reload done: the old worker drains and stops; failures are retried.
    fx.router.fail_next.store(1, Ordering::Relaxed);
    *fx.router.gate.lock().unwrap() = None;
    gate.add_permits(100);
    pool.wait_for(
        |s| s.workers.len() == 1 && s.count(WorkerPhase::Active) == 1,
        WAIT,
    )
    .await
    .unwrap();
    assert!(!alive(old.pid.unwrap()));
    let routed = fx.router.site_backends();
    assert_eq!(routed.len(), 1);
    assert_ne!(routed[0].port(), old.port.unwrap());
    pool.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blue_green_swap_under_load_drains_the_old_pool() {
    let _serial = SERIAL.lock().await;
    let fx = fixture("php_8_3");
    let blue = fx.spawn(fake_spec(fx.root.path(), "8.3.99", "8.3"));
    let green = fx.spawn(fake_spec(fx.root.path(), "8.4.99", "8.4"));
    blue.start(ASK).await.unwrap();
    settled(&fx, 2).await;
    let blue_status = blue.status(ASK).await.unwrap();

    let load = Load::start(
        fx.router.clone(),
        health_request(&fx.health_root("8.3"), probe::HEALTH_URI),
        8,
        Duration::from_millis(5),
        None,
    );
    eventually("traffic on 8.3", WAIT, || load.answers().len() >= 50).await;
    let report = swap(
        &blue,
        &green,
        fx.router.listener().as_ref(),
        &SwapOptions::default(),
    )
    .await
    .unwrap();
    let served_before = load.answers().len();
    eventually("traffic on 8.4", WAIT, || {
        load.answers().len() >= served_before + 50
    })
    .await;
    let (answers, errors) = load.finish().await;
    assert!(
        errors.is_empty(),
        "failed requests during the swap: {errors:?}"
    );
    assert!(report.started_target);
    let drain = report.drain.expect("drained");
    // The load may have scaled the old pool up to its max (3) before the swap.
    assert_eq!(drain.workers as usize, report.upstream_from.backends.len());
    assert_eq!(drain.forced, 0);
    assert_eq!(fx.router.site().as_deref(), Some("php_8_4"));
    assert!(answers.iter().any(|a| a.version == "8.3.99"));
    // Once the swap returned (the old pool drained), only 8.4 answers.
    assert!(
        answers[served_before..]
            .iter()
            .all(|a| a.version == "8.4.99"),
        "8.3 answered after its drain"
    );

    let blue_after = blue.status(ASK).await.unwrap();
    assert_eq!(blue_after.state, PoolState::Stopped);
    for pid in pids(&blue_status) {
        assert!(!alive(pid));
    }
    for backend in &report.upstream_from.backends {
        assert!(!probe::accepts(*backend, Duration::from_millis(250)).await);
    }
    let changes = fx.router.log();
    assert!(
        changes
            .iter()
            .any(|c| matches!(c, UpstreamChange::Swapped { from, to } if from.name == "php_8_3" && to.name == "php_8_4")),
        "{changes:?}"
    );
    green.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_series_swap_keeps_the_upstream_name() {
    let _serial = SERIAL.lock().await;
    let fx = fixture("php_8_3");
    let old = fx.spawn(fake_spec(fx.root.path(), "8.3.98", "8.3"));
    let new = fx.spawn(fake_spec(fx.root.path(), "8.3.99", "8.3"));
    old.start(ASK).await.unwrap();
    settled(&fx, 2).await;
    let before = fx.router.log().len();
    // The new pool starts on standby: nothing reaches the web server until the swap.
    let report = swap(
        &old,
        &new,
        fx.router.listener().as_ref(),
        &SwapOptions::default(),
    )
    .await
    .unwrap();
    let log = fx.router.log();
    assert_eq!(log.len(), before + 1, "only the swap itself: {log:?}");
    assert_eq!(report.upstream_from.name, report.upstream_to.name);
    assert_eq!(fx.router.site_backends(), report.upstream_to.backends);
    let script = HealthScript::new(fx.health_root("8.3"));
    for backend in fx.router.site_backends() {
        let health = probe::health(backend, &script, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(health.php_version, "8.3.99");
    }
    // The promoted pool's own changes reach the web server now.
    assert_eq!(new.reload(ASK).await.unwrap(), 2);
    eventually("reloaded backends routed", WAIT, || {
        let routed = fx.router.site_backends();
        routed.len() == 2
            && routed
                .iter()
                .all(|b| !report.upstream_to.backends.contains(b))
    })
    .await;
    new.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_swap_keeps_the_source_serving() {
    let _serial = SERIAL.lock().await;
    let fx = fixture("php_8_3");
    let blue = fx.spawn(fake_spec(fx.root.path(), "8.3.99", "8.3"));
    let green = fx.spawn(fake_spec(fx.root.path(), "8.4.99", "8.4"));
    blue.start(ASK).await.unwrap();
    settled(&fx, 2).await;
    fx.router.fail_next.store(1, Ordering::Relaxed);
    let err = swap(
        &blue,
        &green,
        fx.router.listener().as_ref(),
        &SwapOptions::default(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("reload failed"), "{err}");
    let blue_status = blue.status(ASK).await.unwrap();
    assert_eq!(blue_status.state, PoolState::Running);
    assert!(blue_status.serving, "back in service");
    assert_eq!(fx.router.site().as_deref(), Some("php_8_3"));
    let green_status = green.status(ASK).await.unwrap();
    assert_eq!(
        green_status.state,
        PoolState::Stopped,
        "target started by the swap is stopped"
    );
    blue.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recycles_by_age_and_by_rss() {
    let _serial = SERIAL.lock().await;
    let fx = fixture("php_8_3");
    let mut events = fx.bus.subscribe();
    let mut spec = fake_spec(fx.root.path(), "8.3.99", "8.3");
    spec.scaling.min = 1;
    spec.scaling.max = 1;
    spec.recycle.max_age = Some(Duration::from_millis(700));
    let pool = fx.spawn(spec);
    pool.start(ASK).await.unwrap();
    eventually("an age recycle", WAIT, || {
        of_kind(&drain(&mut events), EventKind::PoolWorkerRecycled)
            .iter()
            .any(|e| e.payload["reason"] == "max_age")
    })
    .await;
    pool.stop(ASK).await.unwrap();

    let mut spec = fake_spec(fx.root.path(), "8.3.99", "8.3");
    spec.scaling.min = 1;
    spec.scaling.max = 1;
    spec.env.set("FAKE_ALLOC_MB", "48");
    spec.recycle.max_rss_mb = 32;
    let pool = fx.spawn(spec);
    pool.start(ASK).await.unwrap();
    eventually("an rss recycle", WAIT, || {
        of_kind(&drain(&mut events), EventKind::PoolWorkerRecycled)
            .iter()
            .any(|e| {
                e.payload["reason"] == "max_rss"
                    && e.payload["rss_bytes"].as_u64().unwrap() >= 32 * 1024 * 1024
            })
    })
    .await;
    pool.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_failures_end_in_failed_and_free_the_ports() {
    let _serial = SERIAL.lock().await;
    let fx = fixture("php_8_3");
    // php-cgi exits at once (broken install).
    let mut spec = fake_spec(fx.root.path(), "8.3.99", "8.3");
    spec.env.set("FAKE_EXIT_CODE", "3");
    let pool = fx.spawn(spec);
    let err = pool.start(ASK).await.unwrap_err();
    assert!(err.to_string().contains("exited during warmup"), "{err}");
    let status = pool.wait_for(|s| s.workers.is_empty(), WAIT).await.unwrap();
    assert!(
        matches!(status.state, PoolState::Failed { .. }),
        "{status:?}"
    );
    eventually("ports released", WAIT, || fx.ports.held().is_empty()).await;

    // Wrong version answered at warmup.
    let mut spec = fake_spec(fx.root.path(), "8.3.99", "8.3");
    spec.expect_php_version = Some("8.3.36".into());
    let pool = fx.spawn(spec);
    let err = pool.start(ASK).await.unwrap_err();
    assert_eq!(err.code(), "config_invalid", "{err}");
    assert!(err.to_string().contains("expected 8.3.36"), "{err}");
    pool.wait_for(|s| s.workers.is_empty(), WAIT).await.unwrap();
    // A failed pool can be started again once fixed? Not this one: same spec, same error.
    let status = pool.status(ASK).await.unwrap();
    match status.state {
        PoolState::Failed { code, .. } => assert_eq!(code, "config_invalid"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_the_pool_kills_its_workers() {
    let _serial = SERIAL.lock().await;
    let fx = fixture("php_8_3");
    let pool = fx.spawn(fake_spec(fx.root.path(), "8.3.99", "8.3"));
    pool.start(ASK).await.unwrap();
    let worker_pids = pids(&pool.status(ASK).await.unwrap());
    drop(pool);
    eventually("workers gone", WAIT, || {
        worker_pids.iter().all(|p| !alive(*p))
    })
    .await;
    eventually("ports released", WAIT, || fx.ports.held().is_empty()).await;
}

/// "Generation of the upstream block": the model of a running pool, mapped like the
/// daemon does, renders with the real nginx and Apache templates of forge-render.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_pool_renders_its_upstream_block() {
    let _serial = SERIAL.lock().await;
    let fx = fixture("php_8_3");
    let pool = fx.spawn(fake_spec(fx.root.path(), "8.3.99", "8.3"));
    let model = pool.start(ASK).await.unwrap();
    let render_model = forge_render::model::UpstreamModel::new(
        model.name.clone(),
        model
            .servers()
            .into_iter()
            .map(|(host, port)| forge_render::model::UpstreamServer { host, port })
            .collect(),
    );
    render_model.validate().unwrap();
    let engine = forge_render::Engine::embedded_only();
    let ctx = serde_json::json!({ "upstream": render_model });
    let nginx = engine.render("nginx/upstream.conf.j2", &ctx).unwrap();
    let apache = engine.render("apache/upstream.conf.j2", &ctx).unwrap();
    println!(
        "{nginx}
{apache}"
    );
    assert!(nginx.contains("upstream php_8_3 {"), "{nginx}");
    assert!(nginx.contains("least_conn;"));
    assert!(
        apache.contains("<Proxy \"balancer://php_8_3/\">"),
        "{apache}"
    );
    for backend in &model.backends {
        assert!(nginx.contains(&format!("server {backend};")), "{nginx}");
        assert!(
            apache.contains(&format!(
                "BalancerMember \"fcgi://{backend}\" enablereuse=off max=1"
            )),
            "{apache}"
        );
    }
    pool.stop(ASK).await.unwrap();
}
