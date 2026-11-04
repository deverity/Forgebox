//! The metrics service (M2-E5-T8, docs/14 section 3.6): a real `Store` (in-memory SQLite with
//! its writer thread and reader pool), real tailers on a temporary access log, the real clock.
//!
//! * routed lines (host, alias, `*.host` wildcard) are counted and their percentiles are the
//!   brute force's; lines of a site whose metrics are off and of unknown hosts are not;
//! * subscriptions get a snapshot every cadence;
//! * disabled: no tailer runs, samples say `metrics_enabled = false`;
//! * a stop flushes, and a new service on the same store resumes without counting a line
//!   twice;
//! * a range older than the in-memory retention merges the stored minutes;
//! * a series older than the in-memory retention reads the stored minutes and hours
//!   (M2-E5-T9).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use forge_core::ids::SiteId;
use forge_core::store::{MetricsRepo, MetricsRow, MetricsTier, Store};
use forge_metrics::hist::FORMAT_VERSION;
use forge_metrics::service::{self, ServiceConfig};
use forge_metrics::{
    AccessFormat, FieldMap, LatencyHist, LogSource, MetricsHandle, Multi, SiteRoute, SiteSample,
    Sites, Span, Step, TailConfig, Unit, UpstreamMap,
};
use jiff::Timestamp;

const ASK: Duration = Duration::from_secs(5);

fn nginx() -> AccessFormat {
    AccessFormat {
        ts: FieldMap::new("/t", Unit::Seconds),
        ts_is_start: false,
        host: "/host".into(),
        status: "/s".into(),
        duration: FieldMap::new("/rt", Unit::Seconds),
        upstream_duration: Some(UpstreamMap {
            field: FieldMap::new("/ut", Unit::Seconds),
            multi: Multi::Sum,
        }),
        bytes: Some("/b".into()),
        method: Some("/m".into()),
        uri: Some("/u".into()),
        lenient_escapes: false,
    }
}

fn config() -> ServiceConfig {
    ServiceConfig {
        tick: Duration::from_millis(100),
        // Flushes are asked for explicitly.
        flush_interval: Duration::from_secs(3600),
        tail: TailConfig {
            tick: Duration::from_millis(20),
            idle_tick: Duration::from_millis(50),
            ..TailConfig::default()
        },
        ..ServiceConfig::default()
    }
}

struct Ids {
    a: SiteId,
    b: SiteId,
}

fn routes(sites: &Ids) -> Vec<SiteRoute> {
    vec![
        SiteRoute {
            id: sites.a,
            host: "a.test".into(),
            aliases: vec!["a.local".into()],
            metrics_enabled: true,
        },
        SiteRoute {
            id: sites.b,
            host: "b.test".into(),
            aliases: Vec::new(),
            metrics_enabled: false,
        },
    ]
}

fn source(path: &Path) -> LogSource {
    LogSource {
        key: "logs/web/access.json".into(),
        path: path.to_path_buf(),
        format: nginx(),
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

    /// Milliseconds, skewed: mostly 1-80, some 100-900, a few 1-3 s.
    fn ms(&mut self) -> u64 {
        let n = self.next();
        match n % 100 {
            0 => 1000 + n / 7 % 2000,
            1..=9 => 100 + n / 7 % 800,
            _ => 1 + n / 7 % 80,
        }
    }
}

/// One nginx `forgebox` line ending now.
fn line(host: &str, status: u16, rt_ms: u64) -> String {
    let now = Timestamp::now();
    format!(
        "{{\"t\":\"{}.{:03}\",\"host\":\"{host}\",\"s\":{status},\"rt\":{}.{:03},\"ut\":\"{}.{:03}\",\"b\":100,\"m\":\"GET\",\"u\":\"/p/1?q=x\",\"up\":\"127.0.0.1:1\"}}\n",
        now.as_second(),
        now.subsec_millisecond(),
        rt_ms / 1000,
        rt_ms % 1000,
        rt_ms / 2 / 1000,
        rt_ms / 2 % 1000,
    )
}

fn append(path: &Path, text: &str) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

async fn sample(handle: &MetricsHandle, site: SiteId, span: Span) -> SiteSample {
    handle
        .query(span, Sites::One(site), ASK)
        .await
        .unwrap()
        .samples
        .remove(0)
}

async fn wait_requests(handle: &MetricsHandle, site: SiteId, want: u64) -> SiteSample {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let s = sample(handle, site, Span::Window(Duration::from_secs(300))).await;
        if s.summary.counters.requests >= want || Instant::now() > deadline {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The `highest_equivalent` brute force of p50/p90/p99 (rank `max(1, ceil(q n))`).
fn exact(values: &mut [u64]) -> [u64; 3] {
    values.sort_unstable();
    [0.5, 0.9, 0.99].map(|q: f64| {
        let rank = ((q * values.len() as f64).ceil() as usize).max(1);
        values[rank - 1]
    })
}

fn within(got: u64, want: u64, tolerance: f64) -> bool {
    (got as f64 - want as f64).abs() <= want as f64 * tolerance
}

struct Fixture {
    _dir: tempfile::TempDir,
    log: PathBuf,
    store: Store,
    sites: Ids,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("access.json");
    Fixture {
        log,
        _dir: dir,
        store: Store::open_in_memory().unwrap(),
        sites: Ids {
            a: SiteId::new(),
            b: SiteId::new(),
        },
    }
}

fn start(fx: &Fixture, enabled: bool) -> MetricsHandle {
    let handle = service::spawn(
        config(),
        fx.store.reader().clone(),
        fx.store.writer().clone(),
        enabled,
    );
    assert!(handle.set_routes(routes(&fx.sites)));
    assert!(handle.set_sources(vec![source(&fx.log)]));
    handle
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routed_lines_match_the_brute_force_and_the_rest_is_counted_apart() {
    let fx = fixture();
    std::fs::write(&fx.log, "").unwrap();
    let handle = start(&fx, true);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut rt, mut ut) = (Vec::new(), Vec::new());
    let (mut errors, mut text) = (0u64, String::new());
    for i in 0..3000u64 {
        let host = ["a.test", "A.LOCAL:8080", "api.a.test"][(i % 3) as usize];
        let status = [200, 200, 404, 200, 503, 302][(i % 6) as usize];
        if status >= 400 {
            errors += 1;
        }
        let ms = rng.ms();
        rt.push(ms * 1000);
        ut.push(ms / 2 * 1000);
        text.push_str(&line(host, status, ms));
    }
    for i in 0..200u64 {
        text.push_str(&line("b.test", 200, 5 + i % 10));
        text.push_str(&line("nobody.example", 200, 5));
    }
    append(&fx.log, &text);

    let a = wait_requests(&handle, fx.sites.a, 3000).await;
    assert_eq!(a.summary.counters.requests, 3000, "{a:?}");
    assert_eq!(
        a.summary.counters.err4xx() + a.summary.counters.err5xx(),
        errors
    );
    assert_eq!(a.host, "a.test");
    assert!(a.metrics_enabled && !a.summary.sampled);
    // Exact against the histogram oracle, and within 0.1 % of the sorted values.
    let oracle = LatencyHist::from_values(rt.iter().copied())
        .percentiles()
        .unwrap();
    let got = a.summary.rt.unwrap();
    assert_eq!(got, oracle);
    let brute = exact(&mut rt);
    for (g, w) in [got.p50_us, got.p90_us, got.p99_us].into_iter().zip(brute) {
        assert!(within(g, w, 0.001), "{g} vs {w}");
    }
    let got = a.summary.ut.unwrap();
    let brute = exact(&mut ut);
    for (g, w) in [got.p50_us, got.p90_us, got.p99_us].into_iter().zip(brute) {
        assert!(within(g, w, 0.001), "ut {g} vs {w}");
    }
    // 1m and 5m agree (everything is a few seconds old).
    let one = sample(&handle, fx.sites.a, Span::Window(Duration::from_secs(60))).await;
    assert_eq!(one.summary.counters, a.summary.counters);

    let b = sample(&handle, fx.sites.b, Span::Window(Duration::from_secs(300))).await;
    assert!(!b.metrics_enabled);
    assert_eq!(b.summary.counters.requests, 0);
    let status = handle.status(ASK).await.unwrap();
    assert_eq!((status.disabled, status.unrouted), (200, 200), "{status:?}");
    assert_eq!(status.tailers, 1);
    assert_eq!(status.sites, 2);

    // Every site at once, sorted by host.
    let all = handle
        .query(Span::Window(Duration::from_secs(60)), Sites::All, ASK)
        .await
        .unwrap();
    let hosts: Vec<&str> = all.samples.iter().map(|s| s.host.as_str()).collect();
    assert_eq!(hosts, ["a.test", "b.test"]);

    // The caller's own view of the sites wins (a site created a moment ago).
    let fresh = SiteRoute {
        id: SiteId::new(),
        host: "new.test".into(),
        aliases: Vec::new(),
        metrics_enabled: true,
    };
    let these = handle
        .query(
            Span::Window(Duration::from_secs(60)),
            Sites::These(vec![fresh.clone()]),
            ASK,
        )
        .await
        .unwrap();
    assert_eq!(these.samples.len(), 1);
    assert_eq!(these.samples[0].host, "new.test");
    assert_eq!(these.samples[0].summary.counters.requests, 0);

    // A bad range is refused.
    let now = Timestamp::now();
    let err = handle
        .query(
            Span::Range {
                from: now,
                to: Some(now - jiff::SignedDuration::from_secs(60)),
            },
            Sites::All,
            ASK,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_input");
    handle.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriptions_get_a_snapshot_every_cadence() {
    let fx = fixture();
    let handle = start(&fx, true);
    let mut rx = handle
        .subscribe(Duration::from_secs(60), Duration::from_millis(300), ASK)
        .await
        .unwrap();
    let mut seen = Vec::new();
    while seen.len() < 4 {
        tokio::time::timeout(Duration::from_secs(5), rx.changed())
            .await
            .unwrap()
            .unwrap();
        let snapshot = rx.borrow_and_update().clone().unwrap();
        assert_eq!(snapshot.samples.len(), 2);
        seen.push(Instant::now());
    }
    for pair in seen.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(
            gap >= Duration::from_millis(200) && gap <= Duration::from_millis(600),
            "{gap:?}"
        );
    }
    drop(rx);
    handle.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_runs_no_tailer_and_says_so() {
    let fx = fixture();
    std::fs::write(&fx.log, line("a.test", 200, 5)).unwrap();
    let handle = start(&fx, false);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let status = handle.status(ASK).await.unwrap();
    assert!(!status.enabled);
    assert_eq!(status.tailers, 0);
    let a = sample(&handle, fx.sites.a, Span::Window(Duration::from_secs(60))).await;
    assert!(!a.metrics_enabled);
    assert_eq!(a.summary.counters.requests, 0);

    assert!(handle.set_enabled(true));
    let a = wait_requests(&handle, fx.sites.a, 1).await;
    assert_eq!(a.summary.counters.requests, 1);
    assert_eq!(handle.status(ASK).await.unwrap().tailers, 1);

    assert!(handle.set_enabled(false));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(handle.status(ASK).await.unwrap().tailers, 0);
    // Back on: the tailer resumes after the line it consumed (nothing counted twice).
    append(&fx.log, &line("a.test", 200, 5));
    assert!(handle.set_enabled(true));
    let a = wait_requests(&handle, fx.sites.a, 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let again = sample(&handle, fx.sites.a, Span::Window(Duration::from_secs(60))).await;
    assert_eq!(
        (a.summary.counters.requests, again.summary.counters.requests),
        (2, 2)
    );
    handle.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_service_resumes_without_counting_twice() {
    let fx = fixture();
    let handle = start(&fx, true);
    let text: String = (0..500).map(|i| line("a.test", 200, 1 + i % 50)).collect();
    append(&fx.log, &text);
    let before = wait_requests(&handle, fx.sites.a, 500).await;
    assert_eq!(before.summary.counters.requests, 500);
    let report = handle.stop(ASK).await.unwrap();
    assert!(report.error.is_none(), "{report:?}");
    assert!(handle.is_closed() || handle.status(ASK).await.is_err());

    // Lines written while no service runs are read by the next one.
    let more: String = (0..100).map(|_| line("a.test", 500, 7)).collect();
    append(&fx.log, &more);
    let handle = start(&fx, true);
    let after = wait_requests(&handle, fx.sites.a, 600).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let settled = sample(&handle, fx.sites.a, Span::Window(Duration::from_secs(300))).await;
    assert_eq!(after.summary.counters.requests, 600, "{after:?}");
    assert_eq!(settled.summary.counters.requests, 600);
    assert_eq!(settled.summary.counters.err5xx(), 100);
    handle.stop(ASK).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_ranges_merge_the_stored_minutes_with_memory() {
    let fx = fixture();
    let handle = start(&fx, true);
    let now = Timestamp::now();
    let start = (now.as_second() - 2 * 24 * 3600).div_euclid(60) * 60;
    let stored = LatencyHist::from_values([10_000, 20_000, 30_000, 40_000, 50_000]);
    let row = MetricsRow {
        site: fx.sites.a,
        start,
        requests: 5,
        status: [0, 5, 0, 0, 0],
        bytes: 500,
        rt_sum_us: 150_000,
        ut_sum_us: 0,
        ut_count: 0,
        sampled: false,
        rt_hist: stored.to_v2().unwrap(),
        ut_hist: Vec::new(),
        format_version: FORMAT_VERSION,
    };
    fx.store
        .writer()
        .execute(move |conn| MetricsRepo::new(conn).put(MetricsTier::Minute, &row))
        .await
        .unwrap();
    append(&fx.log, &line("a.test", 200, 1));
    wait_requests(&handle, fx.sites.a, 1).await;

    let from = now - jiff::SignedDuration::from_secs(3 * 24 * 3600);
    let merged = sample(&handle, fx.sites.a, Span::Range { from, to: None }).await;
    assert_eq!(merged.summary.counters.requests, 6, "{merged:?}");
    assert!(merged.summary.from <= Timestamp::from_second(start).unwrap());
    let p = merged.summary.rt.unwrap();
    assert_eq!(
        p,
        stored
            .merge(&LatencyHist::from_values([1000]))
            .percentiles()
            .unwrap()
    );
    // Entirely in the past: only the stored minute.
    let old = sample(
        &handle,
        fx.sites.a,
        Span::Range {
            from,
            to: Some(now - jiff::SignedDuration::from_secs(24 * 3600 + 3600)),
        },
    )
    .await;
    assert_eq!(old.summary.counters.requests, 5);
    handle.stop(ASK).await.unwrap();
}

fn row(site: SiteId, start: i64, requests: u64, hist: &LatencyHist) -> MetricsRow {
    MetricsRow {
        site,
        start,
        requests,
        status: [0, requests, 0, 0, 0],
        bytes: requests * 100,
        rt_sum_us: requests * 1000,
        ut_sum_us: 0,
        ut_count: 0,
        sampled: false,
        rt_hist: hist.to_v2().unwrap(),
        ut_hist: Vec::new(),
        format_version: FORMAT_VERSION,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn series_beyond_memory_come_from_the_stored_minutes_and_hours() {
    let fx = fixture();
    let handle = start(&fx, true);
    let site = fx.sites.a;
    let now = Timestamp::now().as_second();
    let hour = (now / 3600) * 3600;
    // Two minutes of one hour (merged into one hourly point), another hour days earlier,
    // and an hour row older than the minute retention.
    let h48 = hour - 48 * 3600;
    let h100 = hour - 100 * 3600;
    let h10d = hour - 10 * 24 * 3600;
    let fast = LatencyHist::from_values([10_000, 20_000]);
    let slow = LatencyHist::from_values([900_000]);
    let rows = vec![
        (MetricsTier::Minute, row(site, h48 + 60, 2, &fast)),
        (MetricsTier::Minute, row(site, h48 + 120, 1, &slow)),
        (MetricsTier::Minute, row(site, h100, 2, &fast)),
        (MetricsTier::Hour, row(site, h10d, 3, &slow)),
    ];
    fx.store
        .writer()
        .execute(move |conn| {
            let repo = MetricsRepo::new(conn);
            for (tier, row) in &rows {
                repo.put(*tier, row)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    append(&fx.log, &line("a.test", 200, 1));
    wait_requests(&handle, site, 1).await;
    let route = routes(&fx.sites).remove(0);
    assert_eq!(route.id, site);
    let at = |s: i64| Timestamp::from_second(s).unwrap();

    // Hours, 11 days back: the old hour row, the two merged minutes, the lone minute and
    // the hour in memory, oldest first, each hour once.
    let report = handle
        .series(route.clone(), at(now - 11 * 86_400), None, Step::Hour, ASK)
        .await
        .unwrap();
    let starts: Vec<i64> = report.points.iter().map(|p| p.start.as_second()).collect();
    assert_eq!(starts, [h10d, h100, h48, hour], "{:?}", report.points);
    let counts: Vec<u64> = report.points.iter().map(|p| p.requests).collect();
    assert_eq!(counts, [3, 2, 3, 1]);
    let merged = fast.merge(&slow).percentiles().unwrap();
    assert_eq!(report.points[2].rt, Some(merged));
    assert_eq!(report.points[2].err4xx, 0);
    assert_eq!(report.points[2].bytes, 300);

    // Minutes, 3 days back: the two stored minutes, no hour rows.
    let report = handle
        .series(route.clone(), at(now - 3 * 86_400), None, Step::Minute, ASK)
        .await
        .unwrap();
    let starts: Vec<i64> = report.points.iter().map(|p| p.start.as_second()).collect();
    assert_eq!(starts.len(), 3, "{starts:?}");
    assert_eq!(&starts[..2], [h48 + 60, h48 + 120]);
    assert_eq!(report.points[0].rt, fast.percentiles());
    assert_eq!(report.points[1].rt, slow.percentiles());

    // Fine steps never reach the store; metrics off answers nothing.
    let report = handle
        .series(
            route.clone(),
            at(now - 3 * 86_400),
            None,
            Step::TenSeconds,
            ASK,
        )
        .await
        .unwrap();
    assert!(
        report
            .points
            .iter()
            .all(|p| p.start.as_second() > now - 87_000)
    );
    let mut off = route;
    off.metrics_enabled = false;
    let report = handle
        .series(off.clone(), at(now - 3 * 86_400), None, Step::Minute, ASK)
        .await
        .unwrap();
    assert!(report.points.is_empty());
    assert!(handle.routes(off, ASK).await.unwrap().report.is_none());
    let err = handle
        .series(
            routes(&fx.sites).remove(0),
            at(now + 600),
            None,
            Step::Hour,
            ASK,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_input");
    handle.stop(ASK).await.unwrap();
}
