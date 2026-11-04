//! Per-site metrics through the real `forgeboxd` (M2-E5-T8, docs/14 §3, ADR-0044).
//!
//! The web server is `tests/fixtures/fake_nginx.rs` installed as `bin/nginx/1.30.6/nginx.exe`:
//! the daemon discovers it, renders the real nginx configuration, starts it and follows the
//! access log the registry's nginx manifest declares (`logs/nginx/access.json`, the
//! `logs[].access` mapping). The test writes the synthetic access lines itself, with the
//! current time, exactly in nginx's `log_format forgebox`. Sites come from parked folders.
//!
//! * `stats.get` 1m/5m and an exact `from`: requests, err% and p50/p90/p99 (total and PHP)
//!   within 0.1 % of the brute force; an alias and a `*.host` wildcard reach their site;
//! * `metrics.subscribe`: first item ≤ 2.5 s, cadence 2 s ± 10 %, a resume gets a fresh
//!   snapshot and no `seq` twice; a client that stops reading gets `metrics.lagged` while
//!   `services.status` stays under 50 ms;
//! * a 50 MiB backlog: 200 `stats.get` with p99 < 50 ms;
//! * a restart keeps the numbers (24 h and 5 min) without counting a line twice;
//! * `[metrics] enabled = false`: no tailer, `metrics_enabled = false`; a portable root is off
//!   until `metrics.enabled` is set, which applies live.
#![cfg(windows)]

mod common;

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use forge_ipc::Client;
use forge_ipc::api::methods::{
    jobs_get, metrics_subscribe, services_status, sites_alias, stats_get, system_config_set,
};
use forge_ipc::api::{
    ConfigSetParams, JobIdParams, JobState, ListAction, MetricsSubscribeParams, NoParams,
    ServiceState, SiteAliasParams, SiteRef, SiteStatsDto, StatsParams, StatsWindow,
};
use forge_ipc::client::SubscriptionItem;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};

use common::*;

const STATUS_BUDGET: Duration = Duration::from_millis(50);

fn fake_nginx() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| compile_fixture("fake_nginx")).clone()
}

/// A root with parked sites `<name>.test` and, with `nginx`, the fake nginx installed.
/// `extra` is appended to `forgebox.toml` (after `[ports.ranges]`).
fn root_with(sites: &[&str], nginx: bool, extra: &str) -> TestRoot {
    let root = TestRoot::new();
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    // The file ends inside `[ports.ranges]`.
    let toml = format!("{toml}web = [{}, {}]\n{extra}", free_port(), free_port());
    std::fs::write(root.root.forgebox_toml(), toml).unwrap();
    if nginx {
        let dir = root.root.bin().join("nginx").join("1.30.6");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(fake_nginx(), dir.join("nginx.exe")).unwrap();
    }
    for site in sites {
        let dir = root.root.www().join(site);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "<h1>ok</h1>").unwrap();
    }
    root
}

fn access_log(root: &TestRoot) -> PathBuf {
    root.root.logs().join("nginx").join("access.json")
}

async fn wait_nginx(client: &Client) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = client
            .call::<services_status>(NoParams {})
            .await
            .unwrap()
            .instances
            .into_iter()
            .find(|i| i.tool.as_str() == "nginx")
            .map(|i| i.state);
        if state == Some(ServiceState::Running) {
            return;
        }
        assert!(Instant::now() < deadline, "nginx is {state:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Milliseconds, skewed like real traffic: mostly 1-80, some 100-900, a few 1-3 s.
    fn ms(&mut self) -> u64 {
        let n = self.next();
        match n % 100 {
            0 => 1000 + n / 7 % 2000,
            1..=9 => 100 + n / 7 % 800,
            _ => 1 + n / 7 % 80,
        }
    }
}

/// One line of nginx's `log_format forgebox` that ends now.
fn line(host: &str, status: u16, rt_ms: u64, ut_ms: u64) -> String {
    let now = jiff::Timestamp::now();
    format!(
        "{{\"t\":\"{}.{:03}\",\"host\":\"{host}\",\"s\":{status},\"rt\":{}.{:03},\"ut\":\"{}.{:03}\",\"b\":512,\"m\":\"GET\",\"u\":\"/produtos/42\",\"up\":\"127.0.0.1:19000\"}}\n",
        now.as_second(),
        now.subsec_millisecond(),
        rt_ms / 1000,
        rt_ms % 1000,
        ut_ms / 1000,
        ut_ms % 1000,
    )
}

fn append(path: &std::path::Path, text: &str) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

async fn stats(client: &Client, site: &str, window: StatsWindow) -> SiteStatsDto {
    let mut dto = client
        .call::<stats_get>(StatsParams {
            site: Some(SiteRef::from(site)),
            window,
            ..StatsParams::default()
        })
        .await
        .unwrap();
    assert_eq!(dto.sites.len(), 1, "{dto:?}");
    dto.sites.remove(0)
}

/// Polls `site` (5 min window) until it has `want` requests (or 30 s).
async fn wait_requests(client: &Client, site: &str, want: u64) -> SiteStatsDto {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let s = stats(client, site, StatsWindow::FiveMinutes).await;
        if s.requests >= want || Instant::now() > deadline {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Waits until the daemon follows the access log: a probe line of `probe` counts.
async fn wait_following(client: &Client, root: &TestRoot, probe: &str) {
    append(&access_log(root), &line(probe, 200, 1, 1));
    let s = wait_requests(client, probe, 1).await;
    assert_eq!(
        s.requests,
        1,
        "the access log is not followed:\n{}",
        daemon_log(root)
    );
}

/// `highest_equivalent` brute force (rank `max(1, ceil(q n))`), in milliseconds.
fn exact(values: &mut [u64]) -> [f64; 3] {
    values.sort_unstable();
    [0.5, 0.9, 0.99].map(|q: f64| {
        let rank = ((q * values.len() as f64).ceil() as usize).max(1);
        values[rank - 1] as f64
    })
}

fn assert_close(got: f64, want: f64, what: &str) {
    assert!(
        (got - want).abs() <= want * 0.001,
        "{what}: {got} ms vs {want} ms (more than 0.1 %)"
    );
}

async fn wait_job(client: &Client, id: forge_core::ids::JobId) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let job = client
            .call::<jobs_get>(JobIdParams { job_id: id })
            .await
            .unwrap();
        if job.state == JobState::Done {
            return;
        }
        assert!(
            !matches!(job.state, JobState::Failed | JobState::Cancelled),
            "{job:?}"
        );
        assert!(Instant::now() < deadline, "{job:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct Up {
    root: TestRoot,
    daemon: DaemonProcess,
    client: Client,
}

async fn up(root: TestRoot, nginx: bool) -> Up {
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
    if nginx {
        wait_nginx(&client).await;
    }
    Up {
        root,
        daemon,
        client,
    }
}

impl Up {
    async fn down(mut self) {
        let (status, _) =
            common::stop(&self.client, &mut self.daemon, Duration::from_secs(60)).await;
        assert!(status.success(), "{status}");
        show_log(&self.root);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stats_get_matches_the_brute_force_and_routes_aliases_and_wildcards() {
    let t = up(root_with(&["loja", "blog", "probe"], true, ""), true).await;
    let c = &t.client;
    let job = c
        .call::<sites_alias>(SiteAliasParams {
            site: SiteRef::from("loja.test"),
            action: ListAction::Add,
            alias: "loja.local".into(),
        })
        .await
        .unwrap();
    wait_job(c, job.id).await;
    wait_following(c, &t.root, "probe.test").await;

    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let (mut rt, mut ut) = (Vec::new(), Vec::new());
    let (mut errors, mut blog) = (0u64, 0u64);
    let started = jiff::Timestamp::now();
    for burst in 0..10 {
        let mut text = String::new();
        for i in 0..240u64 {
            // Host, alias (with a port, mixed case) and a wildcard of the host.
            let host = ["loja.test", "Loja.Local:8080", "api.loja.test"][(i % 3) as usize];
            let status = [200, 200, 404, 200, 503, 304, 201, 200][(i % 8) as usize];
            errors += u64::from(status >= 400);
            let (r, u) = (rng.ms(), rng.ms() / 2 + 1);
            rt.push(r);
            ut.push(u);
            text.push_str(&line(host, status, r, u));
            if i % 4 == 0 {
                text.push_str(&line("blog.test", 200, 3, 1));
                text.push_str(&line("unknown.example", 200, 3, 1));
                blog += 1;
            }
        }
        append(&access_log(&t.root), &text);
        if burst % 3 == 0 {
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
    }
    let total = rt.len() as u64;
    let loja = wait_requests(c, "loja.test", total).await;
    assert_eq!(loja.requests, total, "{loja:?}");
    let [p50, p90, p99] = exact(&mut rt);
    let [u50, u90, u99] = exact(&mut ut);
    let err_pct = errors as f64 * 100.0 / total as f64;
    for window in [StatsWindow::OneMinute, StatsWindow::FiveMinutes] {
        let s = stats(c, "loja.test", window).await;
        assert_eq!(s.window, window);
        assert_eq!(s.requests, total, "{window:?}");
        assert!(
            (s.error_pct - err_pct).abs() < 1e-9,
            "{} vs {err_pct}",
            s.error_pct
        );
        assert_eq!(s.status_4xx + s.status_5xx, errors);
        assert_eq!(s.bytes, total * 512);
        assert_close(s.p50_ms, p50, "p50");
        assert_close(s.p90_ms, p90, "p90");
        assert_close(s.p99_ms, p99, "p99");
        assert_close(s.php_p50_ms.unwrap(), u50, "php p50");
        assert_close(s.php_p90_ms.unwrap(), u90, "php p90");
        assert_close(s.php_p99_ms.unwrap(), u99, "php p99");
        assert!(s.metrics_enabled && !s.sampled);
        assert!(s.last_request_at.is_some());
        assert!(s.from.unwrap() < s.at);
    }
    assert_eq!(
        stats(c, "blog.test", StatsWindow::OneMinute).await.requests,
        blog
    );

    // The exact range `[from, now)` covers the same lines.
    let ranged = c
        .call::<stats_get>(StatsParams {
            site: Some(SiteRef::from("loja.test")),
            window: StatsWindow::OneHour,
            from: Some(started - jiff::SignedDuration::from_secs(5)),
            to: None,
        })
        .await
        .unwrap();
    assert_eq!(ranged.sites[0].requests, total);
    assert_close(ranged.sites[0].p99_ms, p99, "range p99");

    // Every site at once.
    let all = c.call::<stats_get>(StatsParams::default()).await.unwrap();
    let mut hosts: Vec<&str> = all.sites.iter().map(|s| s.host.as_str()).collect();
    hosts.sort_unstable();
    assert_eq!(hosts, ["blog.test", "loja.test", "probe.test"]);
    // An unknown site is not_found.
    let err = c
        .call::<stats_get>(StatsParams {
            site: Some(SiteRef::from("nope.test")),
            ..StatsParams::default()
        })
        .await
        .unwrap_err();
    assert!(format!("{err:?}").contains("not_found"), "{err:?}");
    t.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscribe_snapshots_at_once_keeps_the_cadence_and_resumes_fresh() {
    let t = up(root_with(&["loja", "blog"], false, ""), false).await;
    let c = &t.client;
    let params = |resume_from| MetricsSubscribeParams {
        site: None,
        window: StatsWindow::FiveMinutes,
        resume_from,
    };
    let asked = Instant::now();
    let mut sub = c
        .subscribe::<metrics_subscribe>(params(None))
        .await
        .unwrap();
    let mut arrivals: Vec<(Instant, u64, String)> = Vec::new();
    while arrivals.len() < 2 + 2 * 6 {
        let item = tokio::time::timeout(Duration::from_secs(5), sub.next_item())
            .await
            .expect("an item within 5 s")
            .expect("open stream");
        match item {
            SubscriptionItem::Item(env) => {
                arrivals.push((Instant::now(), env.seq, env.sample.host.clone()));
            }
            SubscriptionItem::Lagged { missed } => panic!("lagged {missed}"),
        }
    }
    let first = arrivals[0].0 - asked;
    assert!(
        first <= Duration::from_millis(2500),
        "first item after {first:?}"
    );
    // The snapshot has every site; `seq` counts from 1 without gaps.
    let mut snapshot: Vec<&str> = arrivals[..2].iter().map(|a| a.2.as_str()).collect();
    snapshot.sort_unstable();
    assert_eq!(snapshot, ["blog.test", "loja.test"]);
    let seqs: Vec<u64> = arrivals.iter().map(|a| a.1).collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
    // Cadence of the periodic samples of one site.
    let loja: Vec<Instant> = arrivals[2..]
        .iter()
        .filter(|a| a.2 == "loja.test")
        .map(|a| a.0)
        .collect();
    for pair in loja.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(
            gap >= Duration::from_millis(1800) && gap <= Duration::from_millis(2200),
            "cadence {gap:?} (2 s ± 10 %)"
        );
    }
    let last = *seqs.last().unwrap();
    sub.unsubscribe().await;

    // Resumed: a fresh snapshot right away, numbered after the cursor.
    let asked = Instant::now();
    let mut resumed = c
        .subscribe::<metrics_subscribe>(params(Some(last)))
        .await
        .unwrap();
    let mut got = Vec::new();
    while got.len() < 4 {
        let item = tokio::time::timeout(Duration::from_secs(5), resumed.next_item())
            .await
            .expect("an item within 5 s")
            .expect("open stream");
        if let SubscriptionItem::Item(env) = item {
            if got.is_empty() {
                assert!(asked.elapsed() <= Duration::from_millis(2500));
            }
            got.push(env.seq);
        }
    }
    assert_eq!(got, (last + 1..=last + 4).collect::<Vec<_>>());
    resumed.unsubscribe().await;
    t.down().await;
}

/// A raw connection that subscribes to `metrics.subscribe` and then never reads.
async fn silent_metrics_subscriber(root: &TestRoot) -> forge_ipc::transport::Connection {
    let token = forge_ipc::Credentials::local(&root.root, "slow", "0").unwrap();
    let mut conn = forge_ipc::transport::local_transport(&root.root)
        .connect()
        .await
        .unwrap();
    let rid = || forge_core::ids::RequestId::new().to_string();
    for frame in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "system.hello", "request_id": rid(),
               "params": {"token": token.token.secret(), "client": {"name": "slow", "version": "0"},
                          "api_version": {"major": 1, "minor": 1}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "metrics.subscribe", "request_id": rid(),
               "params": {"window": "1m"}}),
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
async fn a_silent_subscriber_lags_while_the_api_stays_fast() {
    // 400 sites: 400 samples every 2 s fill the silent client's pipe and queue in seconds.
    let names: Vec<String> = (0..400).map(|i| format!("site{i:03}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let t = up(root_with(&refs, false, ""), false).await;
    let c = &t.client;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let all = c.call::<stats_get>(StatsParams::default()).await.unwrap();
        if all.sites.len() == 400 {
            break;
        }
        assert!(Instant::now() < deadline, "{} sites", all.sites.len());
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let mut silent = silent_metrics_subscriber(&t.root).await;
    let started = Instant::now();
    let mut took = Vec::new();
    while started.elapsed() < Duration::from_secs(14) {
        let asked = Instant::now();
        c.call::<services_status>(NoParams {}).await.unwrap();
        took.push(asked.elapsed());
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let (calls, worst) = (took.len(), *took.iter().max().unwrap());
    let p = p99(took);
    println!(
        "services.status with a silent metrics subscriber: {calls} calls, p99 {p:?}, max {worst:?}"
    );
    // p99, like the terminal flood of concurrency.rs: the gate runs other daemons alongside.
    assert!(p < STATUS_BUDGET, "services.status p99 {p:?}");
    let mut lagged = false;
    let deadline = Instant::now() + Duration::from_secs(30);
    while !lagged && Instant::now() < deadline {
        let Ok(Some(Ok(text))) =
            tokio::time::timeout(Duration::from_secs(5), silent.stream.next()).await
        else {
            break;
        };
        lagged = text.contains("\"metrics.lagged\"");
    }
    assert!(lagged, "the silent subscriber must get metrics.lagged");
    drop(silent);
    t.down().await;
}

fn p99(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[(samples.len() * 99).div_ceil(100) - 1]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_50_mib_backlog_keeps_stats_get_under_50_ms() {
    let t = up(root_with(&["loja", "probe"], true, ""), true).await;
    let c = &t.client;
    wait_following(c, &t.root, "probe.test").await;
    let mut out = std::io::BufWriter::with_capacity(
        1 << 20,
        std::fs::OpenOptions::new()
            .append(true)
            .open(access_log(&t.root))
            .unwrap(),
    );
    let (mut written, mut lines) = (0usize, 0u64);
    let mut rng = Rng(7);
    while written < 50 * 1024 * 1024 {
        let text = line("loja.test", 200, rng.ms(), 1);
        written += text.len();
        lines += 1;
        out.write_all(text.as_bytes()).unwrap();
    }
    out.flush().unwrap();
    drop(out);
    let mut took = Vec::with_capacity(200);
    for _ in 0..200 {
        let asked = Instant::now();
        c.call::<stats_get>(StatsParams::default()).await.unwrap();
        took.push(asked.elapsed());
    }
    let worst = *took.iter().max().unwrap();
    let p = p99(took);
    // Caught up: the scaled count is the number of lines (sampling is exact to < 1 %).
    let deadline = Instant::now() + Duration::from_secs(60);
    let caught = loop {
        let s = stats(c, "loja.test", StatsWindow::FiveMinutes).await;
        if s.requests as f64 >= lines as f64 * 0.99 || Instant::now() > deadline {
            break s;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    println!(
        "50 MiB backlog ({lines} lines): stats.get p99 {p:?}, max {worst:?}; counted {} (sampled {})",
        caught.requests, caught.sampled
    );
    assert!(
        p < STATUS_BUDGET,
        "stats.get p99 {p:?} with a 50 MiB backlog"
    );
    let error = (caught.requests as f64 - lines as f64).abs() / lines as f64;
    assert!(error < 0.01, "counted {} of {lines} lines", caught.requests);
    t.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_keeps_the_numbers_without_counting_twice() {
    let t = up(root_with(&["loja", "probe"], true, ""), true).await;
    wait_following(&t.client, &t.root, "probe.test").await;
    let text: String = (0..1000u64)
        .map(|i| {
            line(
                "loja.test",
                if i % 10 == 0 { 500 } else { 200 },
                1 + i % 90,
                1,
            )
        })
        .collect();
    append(&access_log(&t.root), &text);
    let before = wait_requests(&t.client, "loja.test", 1000).await;
    assert_eq!(before.requests, 1000);
    let day = stats(&t.client, "loja.test", StatsWindow::OneDay).await;
    assert_eq!(day.requests, 1000);
    let root = {
        let Up {
            root,
            mut daemon,
            client,
        } = t;
        let (status, _) = common::stop(&client, &mut daemon, Duration::from_secs(60)).await;
        assert!(status.success(), "{status}");
        root
    };
    let t = up(root, true).await;
    let after = wait_requests(&t.client, "loja.test", 1000).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    for window in [StatsWindow::FiveMinutes, StatsWindow::OneDay] {
        let s = stats(&t.client, "loja.test", window).await;
        assert_eq!(
            s.requests,
            1000,
            "{window:?} after the restart:\n{}",
            daemon_log(&t.root)
        );
        assert_eq!(s.status_5xx, 100);
        assert_eq!(s.p99_ms, day.p99_ms);
    }
    assert_eq!(after.requests, 1000);
    t.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disabled_metrics_run_no_tailer_and_portable_roots_start_off() {
    // `[metrics] enabled = false`.
    let t = up(
        root_with(&["loja"], true, "\n[metrics]\nenabled = false\n"),
        true,
    )
    .await;
    append(&access_log(&t.root), &line("loja.test", 200, 5, 1));
    tokio::time::sleep(Duration::from_secs(3)).await;
    let s = stats(&t.client, "loja.test", StatsWindow::OneMinute).await;
    assert!(!s.metrics_enabled, "{s:?}");
    assert_eq!(s.requests, 0);
    let log = daemon_log(&t.root);
    assert!(
        !log.contains("metrics: following access log"),
        "a tailer ran while metrics are off:\n{log}"
    );
    t.down().await;

    // A portable root: off until the user sets it, then on at once.
    let root = root_with(&["loja", "probe"], true, "");
    std::fs::write(root.root.portable_flag(), "").unwrap();
    let t = up(root, true).await;
    let s = stats(&t.client, "loja.test", StatsWindow::OneMinute).await;
    assert!(!s.metrics_enabled, "portable roots default to off: {s:?}");
    t.client
        .call::<system_config_set>(ConfigSetParams {
            key: "metrics.enabled".into(),
            value: Value::Bool(true),
        })
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !stats(&t.client, "loja.test", StatsWindow::OneMinute)
        .await
        .metrics_enabled
    {
        assert!(
            Instant::now() < deadline,
            "metrics.enabled = true did not apply"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    wait_following(&t.client, &t.root, "probe.test").await;
    let toml = std::fs::read_to_string(t.root.root.forgebox_toml()).unwrap();
    assert!(
        toml.contains("[metrics]") && toml.contains("enabled = true"),
        "{toml}"
    );
    t.down().await;
}
