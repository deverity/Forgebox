//! PhpPool with real PHP (roadmap M1-E3-T4 acceptance, docs/03 §2): downloads the
//! current PHP 8.3 and 8.4 NTS x64 builds (SHA-256 of `releases.json`, see
//! `support/php.rs`) and runs `php-cgi.exe` workers through the pool.
//!
//! * two pools (8.3 and 8.4) side by side, each worker answering its own `PHP_VERSION`;
//! * scaling to `max` under 16 concurrent slow requests, back to `min` once idle;
//! * recycle by `max_requests = 20` under load without a failed request;
//! * blue/green 8.3 → 8.4 while a burst of 200 concurrent requests and a continuous load
//!   run: zero errors, only 8.4 answers at the end, the 8.3 pool drained and stopped.
//!
//! There is no web server here (nginx/Apache come with M1-E4): the test's FastCGI
//! clients play its part, reading the upstream as the pool's listener applies it before
//! each request (like nginx after a reload). The end-to-end run with nginx + `oha` is the
//! M1 smoke test.
//!
//! Run with `cargo test -p forge-fcgi --test php_pool_online -- --ignored --nocapture`.
#![cfg(windows)]

mod support;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::event::EventKind;
use forge_core::ids::VersionId;
use forge_fcgi::probe::{self, HealthScript};
use forge_fcgi::swap::{SwapOptions, swap};
use forge_fcgi::{
    FcgiRequest, PhpPool, PoolDeps, PoolHandle, PoolSpec, PoolState, PoolStatus, PoolTiming,
    Recycle, Scaling, WorkerPhase,
};
use support::php::{PhpBuild, php_build};
use support::*;

/// Online tests share the machine with real PHP processes; one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const ASK: Duration = Duration::from_secs(60);
const WAIT: Duration = Duration::from_secs(60);

async fn build(series: &'static str) -> PhpBuild {
    tokio::task::spawn_blocking(move || php_build(series))
        .await
        .unwrap()
}

struct Fixture {
    root: tempfile::TempDir,
    supervisor: forge_supervisor::Supervisor,
    bus: forge_core::event::EventBus,
    ports: Arc<TestPorts>,
    router: Arc<Router>,
}

impl Fixture {
    fn new(site: &str) -> Self {
        let (supervisor, bus) = supervisor();
        let root = tempfile::tempdir().unwrap();
        let www = root.path().join("www");
        std::fs::create_dir_all(&www).unwrap();
        // A slow page that answers like the health script.
        std::fs::write(
            www.join("sleep.php"),
            "<?php\n$ms = isset($_GET['ms']) ? (int) $_GET['ms'] : 100;\nusleep($ms * 1000);\n\
             echo 'forgebox-health ', PHP_VERSION, ' ', PHP_SAPI, ' ', getmypid(), \"\\n\";\n",
        )
        .unwrap();
        Self {
            root,
            supervisor,
            bus,
            ports: Arc::new(TestPorts::default()),
            router: Router::new(site),
        }
    }

    fn www(&self) -> PathBuf {
        self.root.path().join("www")
    }

    fn sleep_request(&self, ms: u32) -> FcgiRequest {
        let www = self.www();
        FcgiRequest::get(&format!("/sleep.php?ms={ms}"), &www.join("sleep.php"), &www)
    }

    fn health_root(&self, series: &str) -> PathBuf {
        self.root.path().join("etc").join("php").join(series)
    }

    /// Pool of the real `php-cgi.exe` of `build`, ini dir `etc/php/<series>` (no
    /// php.ini: PHP defaults).
    fn spec(&self, build: &PhpBuild) -> PoolSpec {
        let mut spec = PoolSpec::new(
            VersionId::new(),
            &build.php_version,
            &build.series,
            build.php_cgi(),
            self.health_root(&build.series),
        )
        .unwrap();
        spec.expect_php_version = Some(build.php_version.clone());
        spec.scaling = Scaling {
            min: 2,
            max: 4,
            idle_timeout: Duration::from_secs(120),
        };
        spec.recycle = Recycle {
            max_requests: 500,
            max_rss_mb: 256,
            max_age: None,
        };
        spec.timing = PoolTiming {
            tick: Duration::from_millis(200),
            start_timeout: Duration::from_secs(30),
            drain_grace: Duration::from_millis(500),
            drain_timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(5),
            retry_min: Duration::from_millis(200),
            retry_max: Duration::from_secs(2),
            ..PoolTiming::default()
        };
        spec
    }

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
}

fn pids(status: &PoolStatus) -> Vec<u32> {
    status.workers.iter().filter_map(|w| w.pid).collect()
}

fn describe(status: &PoolStatus) -> String {
    let workers: Vec<String> = status
        .workers
        .iter()
        .map(|w| {
            format!(
                "{}:{:?}/pid {}/{} req/{:.1} MiB",
                w.port.unwrap_or_default(),
                w.phase,
                w.pid.unwrap_or_default(),
                w.requests,
                w.rss_bytes.unwrap_or_default() as f64 / 1_048_576.0
            )
        })
        .collect();
    format!(
        "pool {} {} [{}] {}",
        status.version,
        status.state.name(),
        status.upstream.name,
        workers.join(", ")
    )
}

/// Answers per version.
fn by_version(answers: &[Answer]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for a in answers {
        *counts.entry(a.version.clone()).or_default() += 1;
    }
    counts
}

/// Every backend answers the health script with `version` from one of `pids`.
async fn check_backends(
    root: &Path,
    backends: &[std::net::SocketAddr],
    version: &str,
    pids: &[u32],
) {
    let script = HealthScript::new(root);
    for backend in backends {
        let report = probe::health(*backend, &script, Duration::from_secs(10))
            .await
            .unwrap_or_else(|e| panic!("{backend}: {e}"));
        println!(
            "  {backend}: PHP {} ({}) pid {:?}, FCGI_MAX_CONNS {:?}, {:?}",
            report.php_version, report.sapi, report.pid, report.values.max_conns, report.elapsed
        );
        assert_eq!(report.php_version, version);
        assert_eq!(report.sapi, "cgi-fcgi");
        assert!(
            pids.contains(&report.pid.unwrap()),
            "{report:?} not in {pids:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "downloads PHP 8.3 and 8.4 from windows.php.net"]
async fn two_pools_8_3_and_8_4_side_by_side_online() {
    let _serial = SERIAL.lock().await;
    let (b83, b84) = (build("8.3").await, build("8.4").await);
    println!(
        "PHP {} ({}) and {} ({})",
        b83.release, b83.build, b84.release, b84.build
    );
    let fx = Fixture::new("php_8_3");
    let mut spec83 = fx.spec(&b83);
    let mut spec84 = fx.spec(&b84);
    spec83.scaling.max = 2;
    spec84.scaling.max = 2;
    let pool83 = fx.spawn(spec83);
    let pool84 = fx.spawn(spec84);
    let started = Instant::now();
    let (m83, m84) = tokio::join!(pool83.start(ASK), pool84.start(ASK));
    let (m83, m84) = (m83.unwrap(), m84.unwrap());
    println!("both pools warm in {:?}", started.elapsed());
    assert_eq!((m83.name.as_str(), m83.backends.len()), ("php_8_3", 2));
    assert_eq!((m84.name.as_str(), m84.backends.len()), ("php_8_4", 2));

    let s83 = pool83.status(ASK).await.unwrap();
    let s84 = pool84.status(ASK).await.unwrap();
    println!("{}\n{}", describe(&s83), describe(&s84));
    assert_eq!(
        (s83.state.clone(), s84.state.clone()),
        (PoolState::Running, PoolState::Running)
    );
    let all: Vec<u32> = pids(&s83).into_iter().chain(pids(&s84)).collect();
    assert_eq!(all.len(), 4);
    assert!(all.iter().all(|p| alive(*p)), "4 php-cgi running at once");
    check_backends(
        &fx.health_root("8.3"),
        &m83.backends,
        &b83.php_version,
        &pids(&s83),
    )
    .await;
    check_backends(
        &fx.health_root("8.4"),
        &m84.backends,
        &b84.php_version,
        &pids(&s84),
    )
    .await;

    pool83.stop(ASK).await.unwrap();
    pool84.stop(ASK).await.unwrap();
    assert!(all.iter().all(|p| !alive(*p)), "every worker stopped");
    assert!(fx.ports.held().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "downloads PHP 8.4 from windows.php.net"]
async fn scales_to_max_under_16_connections_and_back_online() {
    let _serial = SERIAL.lock().await;
    let b84 = build("8.4").await;
    let fx = Fixture::new("php_8_4");
    let mut events = fx.bus.subscribe();
    let mut spec = fx.spec(&b84);
    spec.scaling = Scaling {
        min: 2,
        max: 4,
        idle_timeout: Duration::from_secs(2),
    };
    let pool = fx.spawn(spec);
    pool.start(ASK).await.unwrap();
    eventually("the router to settle", WAIT, || {
        fx.router.site_backends().len() == 2
    })
    .await;

    let started = Instant::now();
    let load = Load::start(
        fx.router.clone(),
        fx.sleep_request(250),
        16,
        Duration::ZERO,
        None,
    );
    let up = pool
        .wait_for(|s| s.count(WorkerPhase::Active) == 4, WAIT)
        .await
        .unwrap();
    println!(
        "scaled to 4 workers after {:?}\n{}",
        started.elapsed(),
        describe(&up)
    );
    eventually("answers from all 4 workers", WAIT, || {
        load.answers()
            .iter()
            .map(|a| a.backend)
            .collect::<HashSet<_>>()
            .len()
            == 4
    })
    .await;
    let (answers, errors) = load.finish().await;
    println!(
        "{} requests under load, {} errors",
        answers.len(),
        errors.len()
    );
    assert!(errors.is_empty(), "{errors:?}");

    let idle_from = Instant::now();
    let down = pool
        .wait_for(
            |s| s.count(WorkerPhase::Active) == 2 && s.workers.len() == 2,
            WAIT,
        )
        .await
        .unwrap();
    println!(
        "back to 2 workers {:?} after the load\n{}",
        idle_from.elapsed(),
        describe(&down)
    );
    let scaled: Vec<String> = of_kind(&drain(&mut events), EventKind::PoolScaled)
        .iter()
        .map(|e| {
            format!(
                "{}->{} ({})",
                e.payload["from_workers"], e.payload["to_workers"], e.payload["reason"]
            )
        })
        .collect();
    println!("pool.scaled: {}", scaled.join(", "));
    assert!(scaled.iter().any(|s| s.contains("scale_up")));
    assert!(scaled.iter().any(|s| s.contains("scale_down")));
    pool.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "downloads PHP 8.4 from windows.php.net"]
async fn recycles_every_20_requests_without_errors_online() {
    let _serial = SERIAL.lock().await;
    let b84 = build("8.4").await;
    let fx = Fixture::new("php_8_4");
    let mut events = fx.bus.subscribe();
    let mut spec = fx.spec(&b84);
    spec.scaling = Scaling {
        min: 2,
        max: 2,
        idle_timeout: Duration::from_secs(120),
    };
    spec.recycle.max_requests = 20;
    spec.timing.tick = Duration::from_millis(100);
    let pool = fx.spawn(spec);
    pool.start(ASK).await.unwrap();
    eventually("the router to settle", WAIT, || {
        fx.router.site_backends().len() == 2
    })
    .await;
    let first = pids(&pool.status(ASK).await.unwrap());

    let started = Instant::now();
    let load = Load::start(
        fx.router.clone(),
        health_request(&fx.health_root("8.4"), probe::HEALTH_URI),
        8,
        Duration::from_millis(60),
        Some(200),
    );
    eventually("200 requests", WAIT, || {
        load.answers().len() + load.errors().len() >= 200
    })
    .await;
    let (answers, errors) = load.finish().await;
    let served_by: HashSet<u32> = answers.iter().filter_map(|a| a.pid).collect();
    let recycled = of_kind(&drain(&mut events), EventKind::PoolWorkerRecycled)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    println!(
        "{} requests in {:?}, {} errors, served by {} php-cgi processes, {} recycles",
        answers.len(),
        started.elapsed(),
        errors.len(),
        served_by.len(),
        recycled.len()
    );
    for event in &recycled {
        println!(
            "  recycled port {} pid {} after {} requests ({}) -> port {}",
            event.payload["port"],
            event.payload["pid"],
            event.payload["requests"],
            event.payload["reason"],
            event.payload["replacement_port"]
        );
    }
    let now = pool.status(ASK).await.unwrap();
    println!("{} (last error: {:?})", describe(&now), now.last_error);
    assert!(errors.is_empty(), "failed requests: {errors:?}");
    assert_eq!(answers.len(), 200);
    assert!(recycled.len() >= 3, "{recycled:?}");
    assert!(
        recycled
            .iter()
            .all(|e| e.payload["reason"] == "max_requests")
    );
    assert!(served_by.len() > 2);
    let status = pool
        .wait_for(
            |s| s.workers.len() == 2 && s.count(WorkerPhase::Active) == 2,
            WAIT,
        )
        .await
        .unwrap();
    println!("{}", describe(&status));
    assert!(
        first.iter().all(|p| !alive(*p)),
        "first workers recycled and gone"
    );
    pool.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "downloads PHP 8.3 and 8.4 from windows.php.net"]
async fn blue_green_8_3_to_8_4_under_200_concurrent_requests_online() {
    let _serial = SERIAL.lock().await;
    let (b83, b84) = (build("8.3").await, build("8.4").await);
    let fx = Fixture::new("php_8_3");
    let mut events = fx.bus.subscribe();
    let blue = fx.spawn(fx.spec(&b83));
    let green = fx.spawn(fx.spec(&b84));
    blue.start(ASK).await.unwrap();
    eventually("the router to settle", WAIT, || {
        fx.router.site_backends().len() == 2
    })
    .await;

    // Continuous load (health script) plus, during the swap, 200 concurrent requests to a
    // 50 ms page sent over 1.5 s: they queue on the 8.3 workers, cross the switch and
    // must all be answered.
    let load = Load::start(
        fx.router.clone(),
        health_request(&fx.health_root("8.3"), probe::HEALTH_URI),
        16,
        Duration::from_millis(5),
        None,
    );
    eventually("traffic on 8.3", WAIT, || load.answers().len() >= 100).await;
    let burst = Load::burst(
        fx.router.clone(),
        fx.sleep_request(50),
        200,
        Duration::from_millis(1500),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let blue_before = blue.status(ASK).await.unwrap();
    let started = Instant::now();
    let report = swap(
        &blue,
        &green,
        fx.router.listener().as_ref(),
        &SwapOptions::default(),
    )
    .await
    .unwrap();
    let swap_took = started.elapsed();
    let after_swap = load.answers().len();
    eventually("traffic on 8.4", WAIT, || {
        load.answers().len() >= after_swap + 100
    })
    .await;
    let (burst_answers, burst_errors) = burst.finish().await;
    let (answers, errors) = load.finish().await;

    println!(
        "swap 8.3 -> 8.4 in {swap_took:?} (target started: {}, drain: {:?})",
        report.started_target, report.drain
    );
    println!(
        "burst: {} answers {:?}, {} errors; continuous: {} answers {:?}, {} errors",
        burst_answers.len(),
        by_version(&burst_answers),
        burst_errors.len(),
        answers.len(),
        by_version(&answers),
        errors.len()
    );
    assert!(burst_errors.is_empty(), "burst errors: {burst_errors:?}");
    assert!(errors.is_empty(), "continuous errors: {errors:?}");
    assert_eq!(burst_answers.len(), 200);
    assert!(by_version(&answers).contains_key(&b83.php_version));
    assert!(by_version(&answers).contains_key(&b84.php_version));
    assert!(
        answers[after_swap..]
            .iter()
            .all(|a| a.version == b84.php_version),
        "8.3 answered after its drain"
    );

    // Only 8.4 backends answer; the 8.3 pool drained and stopped.
    assert_eq!(fx.router.site().as_deref(), Some("php_8_4"));
    let green_status = green.status(ASK).await.unwrap();
    println!("{}", describe(&green_status));
    check_backends(
        &fx.health_root("8.4"),
        &fx.router.site_backends(),
        &b84.php_version,
        &pids(&green_status),
    )
    .await;
    let blue_after = blue.status(ASK).await.unwrap();
    println!("{}", describe(&blue_after));
    assert_eq!(blue_after.state, PoolState::Stopped);
    let drained = report.drain.unwrap();
    assert_eq!(drained.forced, 0, "the 8.3 pool drained before its timeout");
    for pid in pids(&blue_before) {
        assert!(!alive(pid), "8.3 worker {pid} stopped");
    }
    for backend in &report.upstream_from.backends {
        assert!(!probe::accepts(*backend, Duration::from_millis(250)).await);
    }
    let kinds: Vec<String> = of_kind(&drain(&mut events), EventKind::PoolScaled)
        .iter()
        .map(|e| format!("{} {}", e.payload["version"], e.payload["reason"]))
        .collect();
    println!("pool.scaled: {}", kinds.join(", "));
    green.stop(ASK).await.unwrap();
}
