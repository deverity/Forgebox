//! Acceptance gate of the metrics epic (M2-E5-T13, docs/14 section 3.6 "Orçamento de CPU"
//! and "Aceite medido"): what the metrics pipeline costs the daemon under real traffic, and
//! whether its percentiles agree with an external load generator.
//!
//! Each round builds a temporary root with the official nginx 1.30.6 and PHP 8.4.27 (shared
//! cache), stores three sites and the nginx instance (as after a restart) and starts the real
//! `forgeboxd`, which renders nginx, starts it and follows its JSON access log:
//!
//! * `perf.test`, a proxy site whose upstream is an axum server inside this test answering
//!   after a log-normal delay (median 20 ms, p99 ≈ 120 ms; the sleep uses the high-resolution
//!   waitable timer of `std::thread::sleep`, not the 15.6 ms tick of the tokio timer);
//! * `a.test` and `b.test`, two PHP sites sharing the 8.4 pool.
//!
//! Measured in each round:
//!
//! 1. an idle baseline: the daemon's CPU over [`IDLE`] with no traffic;
//! 2. `oha` against `perf.test` at [`RATE`] req/s with [`CONNECTIONS`] keep-alive
//!    connections for [`LOAD`] (waiting for the requests in flight at the deadline); the
//!    daemon's CPU over the steady [`STEADY`] in the middle of it, as Task Manager shows it
//!    (Δcpu / Δwall / logical CPUs) and as a share of one core; `services.status` latency
//!    every [`STATUS_EVERY`] during the whole load;
//! 3. `stats.get {site: perf.test, from, to}` over the load: exact request count, no
//!    sampling, only 2xx, p50/p90/p99 against oha's;
//! 4. the secondary check: `a.test` and `b.test` at 3:1 for [`PHP_LOAD`]; the attributed CPU
//!    of the shared pool (10 s points of `stats.series`) splits 75/25.
//!
//! The rounds' medians are asserted against the budgets of the roadmap: ≥ 990 req/s with no
//! error, daemon CPU < 2 % of the machine, |Forgebox − oha| / oha ≤ 5 % for p50/p90/p99,
//! `services.status` p99 < 50 ms and an attribution of 75/25 ± 15 pp. The daemon's CPU only
//! means something in a release build on a quiet machine (no other build running): a debug
//! build reports it without asserting the budget.
//!
//! `oha` comes from `FORGEBOX_OHA` (default `C:\apps_src\Forgebox-wt\cache\tools\bin\oha.exe`,
//! installed with `cargo install oha --locked --root C:/apps_src/Forgebox-wt/cache/tools`).
//! `FORGEBOX_PERF_ROUNDS` changes the number of rounds (default 3).
//!
//! `cargo test -p forge-daemon --release --test metrics_perf_online -- --ignored --nocapture`.
#![cfg(windows)]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use forge_core::Timestamp;
use forge_core::ids::ToolId;
use forge_core::model::port::{OwnerKind, PortReservation, ReservationOwner};
use forge_core::platform::{MachineSample, process_stats, system_stats};
use forge_core::store::{
    InstanceRepo, InstanceRow, NewToolVersion, PortRepo, Site, SiteKind, SiteRepo, Store,
    ToolVersionRepo, VersionSource, VersionState,
};
use forge_ipc::Client;
use forge_ipc::api::methods::{services_status, stats_get, stats_series};
use forge_ipc::api::{
    EventFilter, EventsListParams, NoParams, ServiceState, SiteRef, SiteStatsDto, StatsParams,
    StatsSeriesParams, StatsWindow,
};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use common::*;

/// Requests per second of the main load.
const RATE: u32 = 1_000;
/// Keep-alive connections of the main load.
const CONNECTIONS: u32 = 64;
/// Length of the main load.
const LOAD: Duration = Duration::from_secs(70);
/// Start of the CPU window after the load starts (ramp-up left out).
const STEADY_SKIP: Duration = Duration::from_secs(5);
/// Length of the CPU window.
const STEADY: Duration = Duration::from_secs(60);
/// Idle baseline before the load.
const IDLE: Duration = Duration::from_secs(20);
/// Quiet time after the boot, before the baseline (pools and tailers settle).
const SETTLE: Duration = Duration::from_secs(5);
/// One `services.status` every so often during the load (and the baseline).
const STATUS_EVERY: Duration = Duration::from_millis(200);
/// Requests per second to `a.test` and `b.test` (3:1).
const PHP_RATES: [u32; 2] = [60, 20];
/// Length of the PHP load: at least two whole 10 s attribution buckets inside it.
const PHP_LOAD: Duration = Duration::from_secs(40);

/// Achieved rate, daemon CPU, percentile agreement, status latency and attribution budgets.
const MIN_RPS: f64 = 990.0;
const CPU_BUDGET_PCT: f64 = 2.0;
const PERCENTILE_TOLERANCE: f64 = 0.05;
const STATUS_BUDGET: Duration = Duration::from_millis(50);
const SHARE_WANTED: f64 = 75.0;
const SHARE_TOLERANCE_PP: f64 = 15.0;

/// Log-normal upstream delay: median 20 ms; σ = ln(120 / 20) / z(0.99) puts p99 at 120 ms.
const UPSTREAM_MEDIAN_MS: f64 = 20.0;
const UPSTREAM_P99_MS: f64 = 120.0;
const Z_99: f64 = 2.326_347_874;

const PERF_HOST: &str = "perf.test";
const PHP_HOSTS: [&str; 2] = ["a.test", "b.test"];

// ------------------------------------------------------------------------------ upstream

/// SplitMix64 over a shared counter: independent delays for concurrent requests.
fn next_u64(state: &AtomicU64) -> u64 {
    let mut z = state
        .fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed)
        .wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A uniform number in (0, 1).
fn uniform(state: &AtomicU64) -> f64 {
    ((next_u64(state) >> 11) as f64 + 0.5) / (1u64 << 53) as f64
}

/// A log-normal delay (Box-Muller), capped at 2 s.
fn lognormal_delay(state: &AtomicU64) -> Duration {
    let sigma = (UPSTREAM_P99_MS / UPSTREAM_MEDIAN_MS).ln() / Z_99;
    let z = (-2.0 * uniform(state).ln()).sqrt() * (std::f64::consts::TAU * uniform(state)).cos();
    let ms = (UPSTREAM_MEDIAN_MS.ln() + sigma * z).exp().min(2_000.0);
    Duration::from_secs_f64(ms / 1000.0)
}

/// The upstream of `perf.test`: every request is answered after a log-normal delay.
async fn spawn_upstream() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(AtomicU64::new(0x5eed_f0f0_1234_5678));
    let app = axum::Router::new().fallback(move || {
        let state = Arc::clone(&state);
        async move {
            let delay = lognormal_delay(&state);
            // `std::thread::sleep` waits on a high-resolution waitable timer; the tokio
            // timer would round every delay to the 15.6 ms system tick.
            tokio::task::spawn_blocking(move || std::thread::sleep(delay))
                .await
                .unwrap();
            "ok"
        }
    });
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    port
}

// ------------------------------------------------------------------------------ the root

/// The CPU work of each PHP request (a few milliseconds), so the shared pool has CPU to
/// attribute.
const INDEX_PHP: &str = "<?php $x = 0; for ($i = 0; $i < 150000; $i++) { $x += $i % 7; } \
                         echo 'site ', $_SERVER['HTTP_HOST'] ?? '-', ' ', $x;\n";

struct PerfRoot {
    root: TestRoot,
    http: u16,
}

fn perf_root(upstream: u16) -> PerfRoot {
    let php = online::extracted(&online::PHP_8_4);
    let nginx = online::extracted(&online::NGINX_1_30);
    let root = TestRoot::new();
    let nginx_dir = root.root.bin().join("nginx").join("1.30.6");
    online::copy_tree(
        &php,
        &root.root.bin().join("php").join("8.4.27-nts-vs17-x64"),
    );
    online::copy_tree(&nginx, &nginx_dir);
    // What `post_install` of the manifests creates on a real installation.
    for dir in [
        "logs/nginx",
        "tmp/nginx",
        "logs/php/8.4",
        "tmp/php/8.4/sessions",
        "tmp/php/8.4/uploads",
    ] {
        std::fs::create_dir_all(root.path().join(dir)).unwrap();
    }
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    std::fs::write(
        root.root.forgebox_toml(),
        format!("parked_dirs = []\n{toml}"),
    )
    .unwrap();

    let (http, https) = (free_port(), free_port());
    std::fs::create_dir_all(root.root.state()).unwrap();
    let store = Store::open(&root.root.db()).unwrap();
    for host in PHP_HOSTS {
        let dir = root.root.www().join(host.trim_end_matches(".test"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.php"), INDEX_PHP).unwrap();
        let mut site = Site::new(host, host.replace('.', "-"));
        site.kind = SiteKind::Php;
        site.root = Some(forge_core::paths::posix_string(&dir));
        site.driver = Some("generic-php".into());
        site.php.series = Some("8.4".into());
        site.secure = false;
        site.http_redirect = false;
        SiteRepo::new(store.conn()).insert(&site).unwrap();
    }
    let mut proxy = Site::new(PERF_HOST, "perf-test");
    proxy.kind = SiteKind::Proxy;
    proxy.upstream = Some(format!("http://127.0.0.1:{upstream}"));
    proxy.secure = false;
    proxy.http_redirect = false;
    SiteRepo::new(store.conn()).insert(&proxy).unwrap();

    let nginx_tool = ToolId::new("nginx").unwrap();
    let version = ToolVersionRepo::new(store.conn())
        .register_if_absent(&NewToolVersion {
            tool_id: nginx_tool.clone(),
            tool_name: "Nginx".into(),
            version: "1.30.6".into(),
            canonical_name: "1.30.6".into(),
            variant: BTreeMap::new(),
            path: forge_core::paths::posix_string(&nginx_dir),
            state: VersionState::Installed,
            source: VersionSource::Manual,
        })
        .unwrap()
        .unwrap();
    let mut instance = InstanceRow::new(nginx_tool, version, "default");
    instance.autostart = true;
    InstanceRepo::new(store.conn()).insert(&instance).unwrap();
    for (name, port) in [("http", http), ("https", https)] {
        let owner =
            ReservationOwner::new(OwnerKind::Instance, instance.id.to_string(), name).unwrap();
        PortRepo::new(store.conn())
            .insert(&PortReservation::new_tcp(port, Some("web".into()), owner))
            .unwrap();
    }
    store.close().unwrap();
    PerfRoot { root, http }
}

async fn http_get(port: u16, host: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let request = format!("GET / HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    out
}

async fn wait_running(client: &Client, daemon: &DaemonProcess) {
    let started = Instant::now();
    loop {
        let status = client.call::<services_status>(NoParams {}).await.unwrap();
        let nginx_up = status
            .instances
            .iter()
            .any(|i| i.tool.as_str() == "nginx" && i.state == ServiceState::Running);
        let pool_up = status
            .pools
            .iter()
            .any(|p| p.state == ServiceState::Running && p.workers >= 2);
        if nginx_up && pool_up {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "not running: {status:#?}\n{}",
            daemon.output()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// ------------------------------------------------------------------------------ measuring

/// Daemon CPU time and machine counters at one instant.
struct CpuMark {
    at: Instant,
    daemon: Duration,
    machine: MachineSample,
}

async fn cpu_mark(pid: u32) -> CpuMark {
    tokio::task::spawn_blocking(move || {
        let machine = system_stats().sample().unwrap();
        let daemon = process_stats().sample(pid).unwrap().cpu_total;
        CpuMark {
            at: Instant::now(),
            daemon,
            machine,
        }
    })
    .await
    .unwrap()
}

/// The daemon's CPU between two marks.
#[derive(Debug, Clone, Copy, Default)]
struct CpuUse {
    /// Share of the whole machine (Task Manager): Δcpu / Δwall / logical CPUs.
    machine_pct: f64,
    /// Share of one core: Δcpu / Δwall.
    core_pct: f64,
    /// How busy the whole machine was (everything, the load generator included).
    machine_busy_pct: f64,
}

fn cpu_use(a: &CpuMark, b: &CpuMark) -> CpuUse {
    let wall = (b.at - a.at).as_secs_f64();
    let core_pct = (b.daemon - a.daemon).as_secs_f64() / wall * 100.0;
    CpuUse {
        machine_pct: core_pct / f64::from(b.machine.logical_cpus.max(1)),
        core_pct,
        machine_busy_pct: b.machine.cpu_pct_since(&a.machine).unwrap_or(f64::NAN),
    }
}

/// `services.status` every [`STATUS_EVERY`] until `stop`; the answer times.
fn poll_status(
    client: Client,
    stop: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<Vec<Duration>> {
    tokio::spawn(async move {
        let mut samples = Vec::new();
        let mut tick = tokio::time::interval(STATUS_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        while !*stop.borrow() {
            tick.tick().await;
            let asked = Instant::now();
            client.call::<services_status>(NoParams {}).await.unwrap();
            samples.push(asked.elapsed());
        }
        samples
    })
}

fn oha_path() -> PathBuf {
    let path = std::env::var_os("FORGEBOX_OHA").map_or_else(
        || PathBuf::from(r"C:\apps_src\Forgebox-wt\cache\tools\bin\oha.exe"),
        PathBuf::from,
    );
    assert!(
        path.is_file(),
        "oha not found at {} (FORGEBOX_OHA): cargo install oha --locked --root \
         C:/apps_src/Forgebox-wt/cache/tools",
        path.display()
    );
    path
}

/// What oha measured.
#[derive(Debug, Clone)]
struct OhaRun {
    requests: u64,
    errors: u64,
    error_kinds: BTreeMap<String, u64>,
    non_200: u64,
    rps: f64,
    /// p50, p90, p99 in milliseconds.
    percentiles: [f64; 3],
}

/// How oha talks to nginx.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Conn {
    /// `CONNECTIONS` keep-alive connections (the main load).
    KeepAlive,
    /// A new connection per request: an nginx reload (a PHP worker recycled or scaled
    /// re-renders the upstream) closes idle keep-alive connections, and a client that sends
    /// on one of them right then gets a connection error (M2-E5-T11).
    PerRequest,
}

/// Runs oha (blocking, in the blocking pool) against `host` through nginx on `port`, waiting
/// for the requests still in flight at the deadline (`-w`).
async fn oha(
    port: u16,
    host: &str,
    rate: u32,
    connections: u32,
    duration: Duration,
    conn: Conn,
) -> OhaRun {
    let oha = oha_path();
    let host = host.to_owned();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = Command::new(oha);
        cmd.args(["--no-tui", "--output-format", "json", "-w"])
            .args(["-q", &rate.to_string()])
            .args(["-c", &connections.to_string()])
            .args(["-z", &format!("{}s", duration.as_secs())])
            .args(["--host", &host]);
        if conn == Conn::PerRequest {
            cmd.arg("--disable-keepalive");
        }
        cmd.arg(format!("http://127.0.0.1:{port}/"))
            .output()
            .expect("running oha")
    })
    .await
    .unwrap();
    assert!(
        output.status.success(),
        "oha failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "oha output ({e}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    let count = |key: &str| -> BTreeMap<String, u64> {
        json[key]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_u64().unwrap_or(0)))
                    .collect()
            })
            .unwrap_or_default()
    };
    let statuses = count("statusCodeDistribution");
    let errors = count("errorDistribution");
    let pct = |key: &str| json["latencyPercentiles"][key].as_f64().unwrap() * 1000.0;
    OhaRun {
        requests: statuses.values().sum(),
        errors: errors.values().sum(),
        error_kinds: errors,
        non_200: statuses
            .iter()
            .filter(|(k, _)| k.as_str() != "200")
            .map(|(_, v)| v)
            .sum(),
        rps: json["summary"]["requestsPerSec"].as_f64().unwrap(),
        percentiles: [pct("p50"), pct("p90"), pct("p99")],
    }
}

async fn stats_range(client: &Client, host: &str, from: Timestamp, to: Timestamp) -> SiteStatsDto {
    let mut dto = client
        .call::<stats_get>(StatsParams {
            site: Some(SiteRef::from(host)),
            window: StatsWindow::FiveMinutes,
            from: Some(from),
            to: Some(to),
        })
        .await
        .unwrap();
    assert_eq!(dto.sites.len(), 1, "{dto:?}");
    dto.sites.remove(0)
}

/// The final numbers of `[from, to)` for `host` once the daemon has every one of `want`
/// requests (it never gets more than that) and `to` is in the past.
async fn settled_range(
    client: &Client,
    host: &str,
    from: Timestamp,
    to: Timestamp,
    want: u64,
) -> SiteStatsDto {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let s = stats_range(client, host, from, to).await;
        if (s.requests >= want && Timestamp::now() > to) || Instant::now() > deadline {
            // One more read after a pause: nothing may arrive later.
            tokio::time::sleep(Duration::from_secs(3)).await;
            return stats_range(client, host, from, to).await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn secs(d: u64) -> jiff::SignedDuration {
    jiff::SignedDuration::from_secs(i64::try_from(d).unwrap())
}

/// p50/p90/p99 (ms, `highest_equivalent` ranks) of the `rt` of `host`'s lines in the access
/// log whose `$msec` is at or after `since`, with their count: what nginx itself recorded,
/// to tell the pipeline's error from the gap between nginx and the client.
fn access_log_percentiles(root: &TestRoot, host: &str, since: Timestamp) -> ([f64; 3], u64) {
    let since = since.as_millisecond() as f64 / 1000.0;
    let text = std::fs::read_to_string(root.root.logs().join("nginx").join("access.json"))
        .unwrap_or_default();
    let mut rt: Vec<f64> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["host"] == host)
        .filter(|v| {
            v["t"]
                .as_str()
                .and_then(|t| t.parse::<f64>().ok())
                .is_some_and(|t| t >= since)
        })
        .filter_map(|v| v["rt"].as_f64())
        .map(|s| s * 1000.0)
        .collect();
    assert!(!rt.is_empty(), "no {host} line in the access log");
    rt.sort_by(f64::total_cmp);
    let at = |q: f64| rt[((q * rt.len() as f64).ceil() as usize).max(1) - 1];
    ([at(0.5), at(0.9), at(0.99)], rt.len() as u64)
}

/// Share of the attributed CPU of `PHP_HOSTS[0]` over the 10 s points fully inside
/// `[from, to)`, in percent; with the request counts of the same points.
async fn attribution(client: &Client, from: Timestamp, to: Timestamp) -> (f64, [u64; 2], usize) {
    let mut cpu = [0.0f64; 2];
    let mut requests = [0u64; 2];
    let mut points = 0;
    for (i, host) in PHP_HOSTS.iter().enumerate() {
        let series = client
            .call::<stats_series>(StatsSeriesParams {
                site: SiteRef::from(*host),
                range: StatsWindow::OneHour,
                from: None,
                to: None,
            })
            .await
            .unwrap();
        assert_eq!(series.step_ms, 10_000);
        for p in &series.points {
            if p.at >= from && p.at + secs(10) <= to {
                if let Some(c) = p.cpu_pct {
                    cpu[i] += c;
                    requests[i] += p.requests;
                    if i == 0 {
                        points += 1;
                    }
                }
            }
        }
    }
    let total = cpu[0] + cpu[1];
    assert!(total > 0.0, "no CPU attributed: {cpu:?}");
    (cpu[0] / total * 100.0, requests, points)
}

/// Events that reload or restart the web server or change a pool (`config.rendered`,
/// `pool.*`, `service.*` without the metrics samples) in `[from, to]`, as
/// `kind (+seconds from from) payload`.
async fn web_changes(client: &Client, from: Timestamp, to: Timestamp) -> Vec<String> {
    let events = client
        .events_list(EventsListParams {
            after_seq: 0,
            limit: Some(5000),
            filter: EventFilter::default(),
        })
        .await
        .unwrap()
        .events;
    events
        .into_iter()
        .filter(|e| e.ts >= from && e.ts <= to)
        .filter(|e| {
            e.kind == "config.rendered"
                || e.kind.starts_with("pool.")
                || (e.kind.starts_with("service.") && e.kind != "service.metrics")
        })
        .map(|e| {
            let at = e.ts.duration_since(from).as_secs_f64();
            format!("{} (+{at:.1} s) {}", e.kind, e.payload)
        })
        .collect()
}

/// The results of one round.
#[derive(Debug, Clone)]
struct Round {
    idle: CpuUse,
    load: CpuUse,
    oha: OhaRun,
    forgebox: [f64; 3],
    /// What nginx itself logged for the load (brute force over the access log).
    nginx: [f64; 3],
    counted: u64,
    status_p99: Duration,
    status_max: Duration,
    status_samples: usize,
    share_a: f64,
    php_requests: [u64; 2],
    php_points: usize,
}

impl Round {
    /// |Forgebox − oha| / oha for p50, p90, p99.
    fn deviation(&self) -> [f64; 3] {
        [0, 1, 2]
            .map(|i| (self.forgebox[i] - self.oha.percentiles[i]).abs() / self.oha.percentiles[i])
    }

    /// |nginx − oha| / oha: the part of the deviation that lies between what nginx measures
    /// (`$request_time`) and what the client sees, outside the metrics pipeline.
    fn nginx_gap(&self) -> [f64; 3] {
        [0, 1, 2].map(|i| (self.nginx[i] - self.oha.percentiles[i]).abs() / self.oha.percentiles[i])
    }
}

async fn round(n: usize, upstream: u16) -> Round {
    let PerfRoot { root, http } = perf_root(upstream);
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, ready) = connect(&root, &mut daemon, Duration::from_secs(90)).await;
    wait_running(&client, &daemon).await;
    println!("round {n}: api after {ready:?}, nginx and the 8.4 pool running");
    let answer = http_get(http, PERF_HOST).await;
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    for host in PHP_HOSTS {
        let answer = http_get(http, host).await;
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        assert!(answer.contains(&format!("site {host}")), "{answer}");
    }
    // The access log is followed before anything is measured.
    let deadline = Instant::now() + Duration::from_secs(30);
    while stats_range(
        &client,
        PERF_HOST,
        Timestamp::now() - secs(300),
        Timestamp::now(),
    )
    .await
    .requests
        == 0
    {
        assert!(
            Instant::now() < deadline,
            "access log not followed:\n{}",
            daemon_log(&root)
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(SETTLE).await;
    let pid = daemon.pid();

    // Idle baseline, with the same status polling as the load.
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let poller = poll_status(client.clone(), stop_rx);
    let a = cpu_mark(pid).await;
    tokio::time::sleep(IDLE).await;
    let b = cpu_mark(pid).await;
    stop_tx.send(true).unwrap();
    let _ = poller.await.unwrap();
    let idle = cpu_use(&a, &b);

    // The main load.
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let poller = poll_status(client.clone(), stop_rx);
    let started = Timestamp::now();
    let load = tokio::spawn(oha(
        http,
        PERF_HOST,
        RATE,
        CONNECTIONS,
        LOAD,
        Conn::KeepAlive,
    ));
    tokio::time::sleep(STEADY_SKIP).await;
    let a = cpu_mark(pid).await;
    tokio::time::sleep(STEADY).await;
    let b = cpu_mark(pid).await;
    let oha_run = load.await.unwrap();
    let ended = Timestamp::now();
    stop_tx.send(true).unwrap();
    let status = poller.await.unwrap();
    let busy = cpu_use(&a, &b);
    let changes = web_changes(&client, started, ended).await;
    println!("round {n}: web server and pool events during the load: {changes:#?}");
    if oha_run.errors > 0 {
        let log = std::fs::read_to_string(root.root.logs().join("nginx").join("error.log"))
            .unwrap_or_default();
        println!(
            "round {n}: oha errors {:?}; nginx error.log:\n{log}",
            oha_run.error_kinds
        );
    }

    let from = started - secs(4);
    let to = ended + secs(4);
    let fb = settled_range(&client, PERF_HOST, from, to, oha_run.requests).await;
    assert!(!fb.sampled, "adaptive sampling kicked in: {fb:?}");
    assert_eq!(fb.requests, oha_run.requests, "{fb:?}");
    assert_eq!(fb.status_2xx, fb.requests, "{fb:?}");
    // The pipeline reports what nginx logged, within the HDR resolution (3 digits).
    let (nginx, logged) = access_log_percentiles(&root, PERF_HOST, from);
    assert_eq!(logged, fb.requests);
    let forgebox = [fb.p50_ms, fb.p90_ms, fb.p99_ms];
    for i in 0..3 {
        assert!(
            (forgebox[i] - nginx[i]).abs() <= nginx[i] * 0.001 + 0.001,
            "percentile {i}: Forgebox {forgebox:?} vs the access log {nginx:?}"
        );
    }

    // Secondary: the shared PHP pool at 3:1.
    let php_started = Timestamp::now();
    let loads: Vec<_> = PHP_HOSTS
        .iter()
        .zip(PHP_RATES)
        .map(|(host, rate)| tokio::spawn(oha(http, host, rate, 4, PHP_LOAD, Conn::PerRequest)))
        .collect();
    let mut php_counts = Vec::new();
    let mut php_errors = Vec::new();
    for load in loads {
        let run = load.await.unwrap();
        php_counts.push(run.requests);
        php_errors.push(run.error_kinds);
    }
    let php_ended = Timestamp::now();
    let php_changes = web_changes(&client, php_started, php_ended).await;
    // The last bucket inside the load is attributed on the next 10 s tick after it ends,
    // with the pool sample the feed sends every 10 s.
    tokio::time::sleep(Duration::from_secs(25)).await;
    let (share_a, php_requests, php_points) = attribution(&client, php_started, php_ended).await;
    println!(
        "round {n}: PHP requests sent {php_counts:?} ({php_errors:?} errors), in the attributed \
         points {php_requests:?}; web server and pool events {php_changes:#?}"
    );

    let (exit, took) = stop(&client, &mut daemon, Duration::from_secs(90)).await;
    client.close();
    assert!(exit.success(), "{}", daemon.output());
    let left: Vec<u32> = ["nginx.exe", "php-cgi.exe"]
        .iter()
        .flat_map(|image| pids_under(image, root.path()))
        .collect();
    assert!(left.is_empty(), "left running: {left:?}");
    assert!(children_of(&[pid]).is_empty());
    println!("round {n}: daemon stopped in {took:?}");
    show_log(&root);

    let round = Round {
        idle,
        load: busy,
        oha: oha_run,
        forgebox,
        nginx,
        counted: fb.requests,
        status_p99: p99(&status),
        status_max: status.iter().max().copied().unwrap_or_default(),
        status_samples: status.len(),
        share_a,
        php_requests,
        php_points,
    };
    println!(
        "round {n}: {round:#?}\nround {n}: deviation from oha {:?}, nginx's own gap {:?}",
        round.deviation(),
        round.nginx_gap()
    );
    round
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

/// CPU, logical processors, OS build and session (what docs/14 records with the numbers).
fn machine() -> String {
    let script = "$c = Get-CimInstance Win32_Processor | Select-Object -First 1; \
                  $o = Get-CimInstance Win32_OperatingSystem; \
                  $m = Get-CimInstance Win32_ComputerSystem; \
                  \"$($c.Name.Trim()), $($c.NumberOfCores) cores / $($c.NumberOfLogicalProcessors) \
                  logical, $([math]::Round($m.TotalPhysicalMemory / 1GB)) GiB, $($o.Caption) \
                  $($o.Version)\"";
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    // The session of this process right now (`SESSIONNAME` is inherited and can be stale).
    let session = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", std::process::id())])
        .args(["/FO", "CSV", "/NH"])
        .output()
        .ok()
        .and_then(|o| {
            let text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.split("\",\"").nth(2).map(str::to_owned)
        })
        .unwrap_or_else(|| "?".into());
    // This test runs under one cargo; more than that is another build.
    let builds = pids_of("rustc.exe").len() + pids_of("cargo.exe").len();
    format!(
        "{out}; session {session}; rustc/cargo processes at start {builds} (this run's cargo included)"
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "acceptance measurement: official nginx/PHP zips, oha, a quiet machine and --release"]
async fn metrics_cost_and_accuracy_at_1000_rps_online() {
    let release = !cfg!(debug_assertions);
    println!("machine: {}", machine());
    println!(
        "build: {}",
        if release {
            "release"
        } else {
            "debug (CPU budget not asserted)"
        }
    );
    let _ = oha_path();
    let upstream = spawn_upstream().await;
    let rounds: usize = std::env::var("FORGEBOX_PERF_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let mut results = Vec::new();
    for n in 1..=rounds {
        let r = round(n, upstream).await;
        // Correctness holds in every round.
        assert_eq!(r.oha.errors + r.oha.non_200, 0, "{r:#?}");
        assert_eq!(r.counted, r.oha.requests);
        results.push(r);
    }

    let med = |f: &dyn Fn(&Round) -> f64| median(results.iter().map(f).collect());
    let rps = med(&|r| r.oha.rps);
    let idle_machine = med(&|r| r.idle.machine_pct);
    let idle_core = med(&|r| r.idle.core_pct);
    let cpu_machine = med(&|r| r.load.machine_pct);
    let cpu_core = med(&|r| r.load.core_pct);
    let busy = med(&|r| r.load.machine_busy_pct);
    let oha_pct = [0, 1, 2].map(|i| med(&|r| r.oha.percentiles[i]));
    let fb_pct = [0, 1, 2].map(|i| med(&|r| r.forgebox[i]));
    let dev = [0, 1, 2].map(|i| med(&|r| r.deviation()[i]));
    let nginx_pct = [0, 1, 2].map(|i| med(&|r| r.nginx[i]));
    let gap = [0, 1, 2].map(|i| med(&|r| r.nginx_gap()[i]));
    let status_p99 = med(&|r| r.status_p99.as_secs_f64() * 1000.0);
    let status_max = med(&|r| r.status_max.as_secs_f64() * 1000.0);
    let share = med(&|r| r.share_a);
    let request_share = med(&|r| {
        r.php_requests[0] as f64 / (r.php_requests[0] + r.php_requests[1]).max(1) as f64 * 100.0
    });
    println!(
        "\nmedian of {rounds} rounds:\n\
         achieved {rps:.1} req/s, requests per round {:?} (counted exactly, sampled=false, 0 errors)\n\
         daemon CPU under load {cpu_machine:.2} % of the machine ({cpu_core:.1} % of one core); \
         idle {idle_machine:.2} % ({idle_core:.1} % of one core); machine busy {busy:.1} %\n\
         p50/p90/p99 oha {oha_pct:.2?} ms, Forgebox {fb_pct:.2?} ms, deviation {:.2?} %; \
         nginx access log {nginx_pct:.2?} ms (its gap to oha {:.2?} %)\n\
         services.status p99 {status_p99:.1} ms, max {status_max:.1} ms ({:?} samples)\n\
         PHP attribution a.test {share:.1} % / b.test {:.1} % (points {:?}; a.test has \
         {request_share:.1} % of their requests)",
        results.iter().map(|r| r.oha.requests).collect::<Vec<_>>(),
        dev.map(|d| d * 100.0),
        gap.map(|d| d * 100.0),
        results.iter().map(|r| r.status_samples).collect::<Vec<_>>(),
        100.0 - share,
        results.iter().map(|r| r.php_points).collect::<Vec<_>>(),
    );

    assert!(rps >= MIN_RPS, "achieved {rps} req/s");
    if release {
        assert!(
            cpu_machine < CPU_BUDGET_PCT,
            "daemon CPU {cpu_machine:.2} % of the machine"
        );
    }
    for (i, d) in dev.iter().enumerate() {
        assert!(
            *d <= PERCENTILE_TOLERANCE,
            "percentile {i}: Forgebox {} vs oha {} ms; nginx's own access log says {} ms (the \
             pipeline matches it, so the gap is between nginx and the client: docs/14 'Aceite \
             medido')",
            fb_pct[i],
            oha_pct[i],
            nginx_pct[i]
        );
    }
    assert!(
        Duration::from_secs_f64(status_p99 / 1000.0) < STATUS_BUDGET,
        "services.status p99 {status_p99} ms"
    );
    assert!(
        (share - SHARE_WANTED).abs() <= SHARE_TOLERANCE_PP,
        "attribution {share} % for a.test"
    );
}

#[test]
fn lognormal_delay_has_the_wanted_median_and_p99() {
    let state = AtomicU64::new(7);
    let mut ms: Vec<f64> = (0..200_000)
        .map(|_| lognormal_delay(&state).as_secs_f64() * 1000.0)
        .collect();
    ms.sort_by(f64::total_cmp);
    let at = |q: f64| ms[(q * ms.len() as f64) as usize];
    assert!(
        (at(0.5) - UPSTREAM_MEDIAN_MS).abs() < 0.5,
        "p50 {}",
        at(0.5)
    );
    assert!((at(0.99) - UPSTREAM_P99_MS).abs() < 6.0, "p99 {}", at(0.99));
}
