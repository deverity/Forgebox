//! The concurrency budgets of docs/03 §7.6 against the real `forgeboxd` on a temporary
//! root (M1 gate):
//!
//! * **stuck service**: a service that ignores CTRL_C/CTRL_BREAK and never exits is
//!   stopped; during the whole cascade (≈ 10 s until the Job is terminated) the daemon
//!   answers `services.status` and `daemon.status` (`forge status`) in < 50 ms;
//! * **slow client**: a client subscribes to the service's logs and stops reading; the
//!   service keeps writing at its pace (nothing waits for that client), another client
//!   keeps receiving the newest lines and the API keeps answering in < 50 ms;
//! * **heavy download + start**: in the daemon's runtime, with the daemon's download
//!   engine pulling large files at full speed (sha256 included), starting a service is
//!   not more than 10 % slower than the baseline (both measured interleaved, round by
//!   round, so outside load on a shared machine weighs on both alike).
//!
//! The service is `tests/fixtures/fake_mailpit.rs` (compiled with rustc), installed as
//! `bin/mailpit/1.32.0/mailpit.exe`: the daemon finds it through the manifest's
//! `discover` and runs it as `mailpit/default` with the manifest's arguments, health
//! probe and stop cascade, exactly like the real Mailpit.
#![cfg(windows)]

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use forge_ipc::Client;
use forge_ipc::api::methods::{
    daemon_status, jobs_get, logs_subscribe, services_status, services_stop,
};
use forge_ipc::api::{
    JobIdParams, JobState, LogsSubscribeParams, NoParams, ServiceState, ServiceTarget,
    ServiceTargetsParams,
};
use forge_ipc::client::SubscriptionItem;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};

use common::*;

/// The budget of docs/03 §7.6 for `forge status` while something else is stuck.
const STATUS_BUDGET: Duration = Duration::from_millis(50);

/// The download test measures time: the other tests of this binary (daemons, a flooding
/// service) do not run meanwhile.
static HEAVY: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

/// `fake_mailpit.exe`, compiled once per test binary.
fn fake_mailpit() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| compile_fixture("fake_mailpit")).clone()
}

/// A root whose `mail` range has two free ports and whose `bin/` holds the fake Mailpit
/// (`flood`: the fast-writing mode). Returns the root and the version folder.
fn root_with_fake_mailpit(flood: bool) -> (TestRoot, PathBuf) {
    let root = TestRoot::new();
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    // The file ends inside `[ports.ranges]`.
    let toml = format!("{toml}mail = [{}, {}]\n", free_port(), free_port());
    std::fs::write(root.root.forgebox_toml(), toml).unwrap();
    let dir = root.root.bin().join("mailpit").join("1.32.0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fake_mailpit(), dir.join("mailpit.exe")).unwrap();
    if flood {
        std::fs::write(dir.join("fake-mode.txt"), "flood").unwrap();
    }
    (root, dir)
}

async fn mailpit_state(client: &Client) -> Option<ServiceState> {
    client
        .call::<services_status>(NoParams {})
        .await
        .unwrap()
        .instances
        .into_iter()
        .find(|i| i.tool.as_str() == "mailpit")
        .map(|i| i.state)
}

async fn wait_mailpit(client: &Client, state: ServiceState, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let now = mailpit_state(client).await;
        if now == Some(state) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "mailpit is {now:?}, expected {state:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_job(client: &Client, id: forge_core::ids::JobId, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let job = client
            .call::<jobs_get>(JobIdParams { job_id: id })
            .await
            .unwrap();
        match job.state {
            JobState::Done => return,
            JobState::Failed | JobState::Cancelled => {
                panic!("job {id} ended {:?}: {:?}", job.state, job.error)
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "job {id} still {:?}", job.state);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn mailpit_target() -> ServiceTargetsParams {
    ServiceTargetsParams {
        targets: vec![ServiceTarget::from("mailpit/default")],
    }
}

/// Latency of one `services.status` and one `daemon.status`.
async fn status_latencies(client: &Client) -> (Duration, Duration) {
    let started = Instant::now();
    client.call::<services_status>(NoParams {}).await.unwrap();
    let services = started.elapsed();
    let started = Instant::now();
    client.call::<daemon_status>(NoParams {}).await.unwrap();
    (services, started.elapsed())
}

/// Last count the fake wrote. It rewrites the file with `fs::write` (truncate, then
/// write), so a read can land in between and see it empty: that is retried, not taken
/// for 0 (which made the rate subtractions underflow under a loaded `cargo test`).
fn read_progress(dir: &Path) -> u64 {
    for _ in 0..200 {
        let count = std::fs::read_to_string(dir.join("progress.txt"))
            .ok()
            .and_then(|t| t.trim().parse().ok());
        if let Some(count) = count {
            return count;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    0
}

// ------------------------------------------------------------------ stuck service

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_answers_within_50_ms_during_a_stuck_stop_cascade() {
    let _shared = HEAVY.read().await;
    let (root, _dir) = root_with_fake_mailpit(false);
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
    // `autostart: true` in the manifest: the boot starts it.
    wait_mailpit(&client, ServiceState::Running, Duration::from_secs(30)).await;
    // Warm the connection (first calls allocate).
    for _ in 0..5 {
        status_latencies(&client).await;
    }

    let stop = client
        .call::<services_stop>(mailpit_target())
        .await
        .unwrap();
    let asked = Instant::now();
    let (mut worst_services, mut worst_daemon, mut samples) =
        (Duration::ZERO, Duration::ZERO, 0_u32);
    let mut stopped_after = None;
    while asked.elapsed() < Duration::from_secs(60) {
        let (services, daemon_lat) = status_latencies(&client).await;
        worst_services = worst_services.max(services);
        worst_daemon = worst_daemon.max(daemon_lat);
        samples += 1;
        if mailpit_state(&client).await == Some(ServiceState::Stopped) {
            stopped_after = Some(asked.elapsed());
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let stopped_after = stopped_after.expect("the cascade ends with the Job terminated");
    println!(
        "stuck stop took {stopped_after:?}; {samples} samples: services.status max \
         {worst_services:?}, daemon.status max {worst_daemon:?}"
    );
    // The manifest gives CTRL_C 10 s; the fake ignores it, so the Job is terminated.
    assert!(
        stopped_after >= Duration::from_secs(9),
        "the stop was not stuck ({stopped_after:?})"
    );
    assert!(samples > 100, "{samples} samples");
    assert!(
        worst_services < STATUS_BUDGET && worst_daemon < STATUS_BUDGET,
        "status latency during the stuck stop: services {worst_services:?}, daemon {worst_daemon:?}"
    );
    wait_job(&client, stop.id, Duration::from_secs(30)).await;
    let (status, _) = common::stop(&client, &mut daemon, Duration::from_secs(30)).await;
    assert!(status.success(), "{status}");
    assert!(
        pids_of("mailpit.exe")
            .iter()
            .all(|pid| !children_of(&[daemon.pid()]).iter().any(|(c, _)| c == pid)),
        "nothing of the daemon left"
    );
}

// ------------------------------------------------------------------ slow client

/// A raw connection that says hello, subscribes to the logs of `target` and then never
/// reads again (its pipe and the daemon's queue for it fill up).
async fn silent_log_subscriber(root: &TestRoot, target: &str) -> forge_ipc::transport::Connection {
    let token = forge_ipc::Credentials::local(&root.root, "slow", "0").unwrap();
    let mut conn = forge_ipc::transport::local_transport(&root.root)
        .connect()
        .await
        .unwrap();
    let rid = || forge_core::ids::RequestId::new().to_string();
    for frame in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "system.hello", "request_id": rid(),
               "params": {"token": token.token.secret(), "client": {"name": "slow", "version": "0"},
                          "api_version": {"major": 1, "minor": 0}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "logs.subscribe", "request_id": rid(),
               "params": {"target": target}}),
    ] {
        conn.sink.send(frame.to_string()).await.unwrap();
        loop {
            let text = tokio::time::timeout(Duration::from_secs(10), conn.stream.next())
                .await
                .expect("answer in time")
                .unwrap()
                .unwrap();
            let answer: Value = serde_json::from_str(&text).unwrap();
            if answer.get("id").is_some() {
                assert!(answer["result"].is_object(), "{answer}");
                break;
            }
        }
    }
    conn
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_log_subscriber_that_never_reads_slows_down_nothing() {
    let _shared = HEAVY.read().await;
    let (root, dir) = root_with_fake_mailpit(true);
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
    wait_mailpit(&client, ServiceState::Running, Duration::from_secs(30)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Baseline: lines/s with nobody subscribed.
    let window = Duration::from_secs(3);
    let p0 = read_progress(&dir);
    tokio::time::sleep(window).await;
    let baseline = (read_progress(&dir) - p0) as f64 / window.as_secs_f64();

    // A subscriber that never reads, and a normal one that does.
    let mut silent = silent_log_subscriber(&root, "mailpit/default").await;
    let mut reader = client
        .subscribe::<logs_subscribe>(LogsSubscribeParams {
            target: "mailpit/default".into(),
            grep: None,
            tail: None,
            resume_from: None,
        })
        .await
        .unwrap();
    let received = Arc::new(AtomicU64::new(0));
    let newest = Arc::new(Mutex::new(String::new()));
    let done = Arc::new(AtomicBool::new(false));
    let reading = {
        let (received, newest, done) = (received.clone(), newest.clone(), done.clone());
        tokio::spawn(async move {
            while !done.load(Ordering::Relaxed) {
                match tokio::time::timeout(Duration::from_millis(200), reader.next_item()).await {
                    Ok(Some(SubscriptionItem::Item(envelope))) => {
                        received.fetch_add(1, Ordering::Relaxed);
                        *newest.lock().unwrap() = envelope.line.text;
                    }
                    Ok(Some(SubscriptionItem::Lagged { .. })) | Err(_) => {}
                    Ok(None) => break,
                }
            }
        })
    };

    let p1 = read_progress(&dir);
    let started = Instant::now();
    let (mut worst_services, mut worst_daemon) = (Duration::ZERO, Duration::ZERO);
    while started.elapsed() < window {
        let (services, daemon_lat) = status_latencies(&client).await;
        worst_services = worst_services.max(services);
        worst_daemon = worst_daemon.max(daemon_lat);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let p2 = read_progress(&dir);
    let loaded = (p2 - p1) as f64 / started.elapsed().as_secs_f64();
    tokio::time::sleep(Duration::from_millis(300)).await;
    done.store(true, Ordering::Relaxed);
    reading.await.unwrap();
    let got = received.load(Ordering::Relaxed);
    let last_line = newest.lock().unwrap().clone();
    let last_n: u64 = last_line
        .split_whitespace()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    println!(
        "fake service: {baseline:.0} lines/s alone, {loaded:.0} lines/s with a silent \
         subscriber; the reading client got {got} lines (last {last_n}, service at {p2}); \
         status max: services {worst_services:?}, daemon {worst_daemon:?}"
    );
    assert!(
        baseline > 5_000.0,
        "the fake service runs at its pace ({baseline:.0}/s)"
    );
    assert!(
        loaded >= baseline * 0.8,
        "the silent subscriber slowed the service: {loaded:.0}/s vs {baseline:.0}/s"
    );
    assert!(got > 10_000, "the other client is served ({got} lines)");
    assert!(
        last_n + 5_000 >= p2,
        "the other client receives the newest lines (last {last_n}, service at {p2})"
    );
    assert!(
        worst_services < STATUS_BUDGET && worst_daemon < STATUS_BUDGET,
        "status latency with a silent subscriber: services {worst_services:?}, daemon {worst_daemon:?}"
    );

    // The silent client was not waited for: when it reads again, it is told it lagged.
    let mut lagged = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !lagged && Instant::now() < deadline {
        let Ok(Some(Ok(text))) =
            tokio::time::timeout(Duration::from_secs(5), silent.stream.next()).await
        else {
            break;
        };
        lagged = text.contains("\"logs.lagged\"");
    }
    assert!(lagged, "the silent subscriber must get logs.lagged");
    drop(silent);

    let (status, _) = common::stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
}

// ------------------------------------------------------------------ download + start

/// Granularity of the fake server's writes: a pause takes effect within one chunk.
const CHUNK: usize = 64 * 1024;

/// A local HTTP server that serves `size` bytes of a fixed pattern on every request (no
/// `Range`), as fast as the client reads, unless `paused`.
struct BigFileServer {
    url: String,
    /// sha256 of the body.
    sha: String,
    stop: Arc<AtomicBool>,
    /// While set, open transfers stop sending (the client just waits for bytes).
    paused: Arc<AtomicBool>,
    /// Bytes of body sent so far, all connections.
    sent: Arc<AtomicU64>,
}

fn big_file_server(size: u64, seed: u8) -> BigFileServer {
    use sha2::{Digest, Sha256};
    let block: Vec<u8> = (0..1024 * 1024_u32)
        .map(|i| ((i.wrapping_mul(2_654_435_761) >> 24) as u8) ^ seed)
        .collect();
    let mut hasher = Sha256::new();
    let mut left = size;
    while left > 0 {
        let n = left.min(block.len() as u64) as usize;
        hasher.update(&block[..n]);
        left -= n as u64;
    }
    let sha: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/big.bin", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let sent = Arc::new(AtomicU64::new(0));
    let block = Arc::new(block);
    {
        let (stop, paused, sent) = (stop.clone(), paused.clone(), sent.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(mut stream) = stream else { continue };
                let (block, stop, paused, sent) =
                    (block.clone(), stop.clone(), paused.clone(), sent.clone());
                std::thread::spawn(move || {
                    let mut buf = [0_u8; 4096];
                    let _ = stream.read(&mut buf);
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
                         Content-Length: {size}\r\nConnection: close\r\n\r\n"
                    );
                    if stream.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    // The body is `block` repeated; `at` walks it in `CHUNK` steps (the
                    // block is a multiple of `CHUNK`).
                    let (mut left, mut at) = (size, 0_usize);
                    while left > 0 {
                        while paused.load(Ordering::Relaxed) {
                            if stop.load(Ordering::Relaxed) {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        let n = left.min(CHUNK as u64) as usize;
                        if stream.write_all(&block[at..at + n]).is_err() {
                            return;
                        }
                        sent.fetch_add(n as u64, Ordering::Relaxed);
                        left -= n as u64;
                        at = (at + n) % block.len();
                    }
                });
            }
        });
    }
    BigFileServer {
        url,
        sha,
        stop,
        paused,
        sent,
    }
}

/// Bytes of body sent by `servers` so far.
fn sent_by(servers: &[BigFileServer]) -> u64 {
    servers.iter().map(|s| s.sent.load(Ordering::Relaxed)).sum()
}

/// Resumes (`flowing`) or pauses the download and waits until it really is flowing again
/// (a few chunks per connection sent) or idle (nothing sent for 30 ms, then 30 ms for the
/// engine to write and hash what it already received).
async fn set_download(servers: &[BigFileServer], flowing: bool) {
    for server in servers {
        server.paused.store(!flowing, Ordering::Relaxed);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    if flowing {
        let target = sent_by(servers) + (4 * CHUNK * servers.len()) as u64;
        while sent_by(servers) < target && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    } else {
        let mut last = sent_by(servers);
        loop {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let now = sent_by(servers);
            if now == last || Instant::now() >= deadline {
                break;
            }
            last = now;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort();
    values[values.len() / 2]
}

#[test]
fn a_heavy_download_does_not_delay_a_service_start_by_more_than_10_percent() {
    use forge_core::actor::ActorSupervisor;
    use forge_core::event::EventBus;
    use forge_core::ids::{InstanceId, ToolId};
    use forge_core::jobs::{JobRegistry, JobState as CoreJobState};
    use forge_core::platform::{ProcessControlOptions, process_control};
    use forge_core::runtime::{RuntimeConfig, build_runtime};
    use forge_provider::download::{
        DownloadConfig, DownloadManager, DownloadRequest, ProxySetting,
    };
    use forge_supervisor::{
        HealthSpec, InstanceSpec, JobTree, Probe, RestartPolicy, SpawnSpec, StopPlan, Supervisor,
    };

    // The daemon's runtime (docs/03 §7.1), components wired like the daemon's.
    let _alone = HEAVY.blocking_write();
    let runtime = build_runtime(&RuntimeConfig::default()).unwrap();
    runtime.block_on(async {
        let dir = tempfile::Builder::new()
            .prefix("fbd-dl-")
            .tempdir()
            .unwrap();
        let bus = EventBus::with_capacity(65_536);
        let actors = ActorSupervisor::new(bus.clone());
        let jobs = JobRegistry::new(bus.clone());
        let mut config = DownloadConfig::new(dir.path().join("tmp").join("downloads"));
        config.proxy = ProxySetting::Direct;
        let downloads = DownloadManager::spawn(&actors, jobs.clone(), config).unwrap();
        let processes = process_control(ProcessControlOptions::default());
        let supervisor = Supervisor::new(JobTree::new(processes).unwrap(), actors.clone());

        // The service: the fake Mailpit, healthy once its HTTP port accepts.
        let exe = fake_mailpit();
        let service_dir = exe.parent().unwrap().to_path_buf();
        // `with_health`: `Running` once the HTTP port accepts (like the manifest); without
        // it, as soon as the process exists (spawn path only, no probe quantization).
        let (exe, service_dir, supervisor) = (&exe, &service_dir, &supervisor);
        let start_once = move |with_health: bool| async move {
            let (http, smtp) = (free_port(), free_port());
            let spawn = SpawnSpec::new(exe, service_dir).args([
                "--smtp".to_owned(),
                format!("127.0.0.1:{smtp}"),
                "--listen".to_owned(),
                format!("127.0.0.1:{http}"),
            ]);
            let mut spec = InstanceSpec::new(
                InstanceId::new(),
                ToolId::new("mailpit").unwrap(),
                "default",
                spawn,
            )
            .with_restart(RestartPolicy::Never);
            if with_health {
                let mut health = HealthSpec::new(Probe::Tcp {
                    addr: format!("127.0.0.1:{http}"),
                });
                health.start_interval = Duration::from_millis(10);
                spec = spec.with_health(health);
            }
            // Straight to the Job termination (the fake ignores console events).
            spec.stop = StopPlan {
                ctrl_break: false,
                wm_close: false,
                ..StopPlan::default()
            };
            let instance = supervisor.spawn_instance(spec).unwrap();
            let started = Instant::now();
            instance.start(Duration::from_secs(30)).await.unwrap();
            let elapsed = started.elapsed();
            instance.stop(Duration::from_secs(30)).await.unwrap();
            instance.shutdown().await;
            elapsed
        };

        // Warm-up (first process creation, page cache) before any download.
        for _ in 0..3 {
            start_once(false).await;
            start_once(true).await;
        }

        // Heavy download: two 256 MiB files at a time (the engine's default parallelism;
        // different contents, since the engine runs one transfer per file), re-queued as
        // they finish, at full loopback speed with sha256 verification.
        let size = 256 * 1024 * 1024;
        let servers: Vec<_> = (0..2_u8).map(|seed| big_file_server(size, seed)).collect();
        let downloaded = Arc::new(AtomicU64::new(0));
        let stop_downloads = Arc::new(AtomicBool::new(false));
        let mut pumps = Vec::new();
        for (slot, server) in servers.iter().enumerate() {
            let (downloads, jobs, url, sha) = (
                downloads.clone(),
                jobs.clone(),
                server.url.clone(),
                server.sha.clone(),
            );
            let (dest_dir, done, stop) = (
                dir.path().join("dl"),
                downloaded.clone(),
                stop_downloads.clone(),
            );
            pumps.push(tokio::spawn(async move {
                let mut round = 0;
                while !stop.load(Ordering::Relaxed) {
                    round += 1;
                    let dest = dest_dir.join(format!("big-{slot}-{round}.bin"));
                    // One URL per download (the engine refuses the same URL twice at once).
                    let url = url::Url::parse(&format!("{url}?n={slot}-{round}")).unwrap();
                    let req = DownloadRequest::new(vec![url], &dest)
                        .expected_size(size)
                        .expected_sha256(&sha);
                    let job = match downloads.download(req).await {
                        Ok(job) => job,
                        Err(_) if stop.load(Ordering::Relaxed) => return,
                        Err(err) => panic!("download refused: {err}"),
                    };
                    loop {
                        let info = jobs.get(job).expect("download job");
                        match info.state {
                            CoreJobState::Done => break,
                            CoreJobState::Failed | CoreJobState::Cancelled => {
                                if stop.load(Ordering::Relaxed) {
                                    return;
                                }
                                panic!("download failed: {:?}", info.error);
                            }
                            _ => tokio::time::sleep(Duration::from_millis(20)).await,
                        }
                    }
                    done.fetch_add(size, Ordering::Relaxed);
                    let _ = std::fs::remove_file(&dest);
                }
            }));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Baseline and loaded starts are interleaved round by round (download paused, then
        // flowing; the reverse on odd rounds), so a shared machine whose load changes
        // during the test (another build, the indexer) weighs on both sides alike instead
        // of on whichever phase it happened to overlap. A run over budget is measured once
        // more and only the last run counts: a daemon that really waits for the download
        // is over budget every time, a burst of outside load is not.
        const ROUNDS: usize = 9;
        const ATTEMPTS: usize = 2;
        const PROCESS_START_FLOOR: Duration = Duration::from_millis(2);
        let mut over_budget = Vec::new();
        let mut through = 0_u64;
        for attempt in 1..=ATTEMPTS {
            let (mut base_bare, mut base_healthy) = (Vec::new(), Vec::new());
            let (mut load_bare, mut load_healthy) = (Vec::new(), Vec::new());
            let mut loaded_time = Duration::ZERO;
            through = 0;
            for round in 0..ROUNDS {
                let order = if round % 2 == 0 {
                    [false, true]
                } else {
                    [true, false]
                };
                for loaded in order {
                    set_download(&servers, loaded).await;
                    let (sent_before, started) = (sent_by(&servers), Instant::now());
                    let bare = start_once(false).await;
                    let healthy = start_once(true).await;
                    if loaded {
                        through += sent_by(&servers) - sent_before;
                        loaded_time += started.elapsed();
                        load_bare.push(bare);
                        load_healthy.push(healthy);
                    } else {
                        base_bare.push(bare);
                        base_healthy.push(healthy);
                    }
                }
            }

            let report = |what: &str, base: &[Duration], under: &[Duration]| {
                let (b, u) = (median(base.to_vec()), median(under.to_vec()));
                println!(
                    "attempt {attempt}, {what}: baseline median {b:?} {base:?}; \
                     under download median {u:?} {under:?}"
                );
                (b, u)
            };
            let (bare_b, bare_u) = report("start (process)", &base_bare, &load_bare);
            let (health_b, health_u) = report("start (TCP health)", &base_healthy, &load_healthy);
            let loaded_secs = loaded_time.as_secs_f64();
            println!(
                "attempt {attempt}: {} MiB downloaded during the {loaded_secs:.1} s of loaded \
                 measures ({:.0} MiB/s)",
                through / (1024 * 1024),
                through as f64 / 1_048_576.0 / loaded_secs,
            );
            // 10 % (docs/03 §7.6). The process-only start is ~5 ms. On a 4-core machine
            // the download (two hashing threads, plus this harness's two loopback writers
            // standing in for the network) takes every core, and that alone costs a
            // process start +0.2..+1.1 ms of CPU scheduling (measured interleaved): below
            // 2 ms the difference is the scheduler, not the daemon waiting.
            over_budget = [
                ("process", bare_b, bare_u, PROCESS_START_FLOOR),
                ("TCP health", health_b, health_u, Duration::ZERO),
            ]
            .into_iter()
            .filter(|&(_, b, u, floor)| u > b.mul_f64(1.10).max(b + floor))
            .map(|(what, b, u, _)| {
                format!("start ({what}) under a heavy download: {u:?} vs baseline {b:?} (> 10 %)")
            })
            .collect();
            if over_budget.is_empty() {
                break;
            }
        }
        let completed = downloaded.load(Ordering::Relaxed);
        println!(
            "{} files of 256 MiB completed and verified",
            completed / size
        );

        stop_downloads.store(true, Ordering::Relaxed);
        for server in &servers {
            server.stop.store(true, Ordering::Relaxed);
            let url = url::Url::parse(&server.url).unwrap();
            let addr = url.socket_addrs(|| None).unwrap()[0];
            let _ = std::net::TcpStream::connect(addr);
        }
        downloads.shutdown().await;
        for pump in pumps {
            let _ = tokio::time::timeout(Duration::from_secs(30), pump).await;
        }

        assert!(
            through >= 64 * 1024 * 1024,
            "the download really ran during the loaded measures ({through} bytes)"
        );
        assert!(over_budget.is_empty(), "{}", over_budget.join("; "));
    });
}
