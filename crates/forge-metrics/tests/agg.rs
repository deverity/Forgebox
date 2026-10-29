//! Aggregator acceptance (M2-E5-T5, docs/14 section 3.4, ADR-0048).
//!
//! Window/range percentiles against brute force, exact merges, late and future records,
//! series, the persistence hooks, top routes and recent errors. Memory is measured with a
//! counting global allocator that keeps one counter per thread, so tests running in
//! parallel do not see each other's allocations (the aggregator is single-owner).
//!
//! The 24 h memory runs and the cost per record are `#[ignore]`d; run them in release:
//! `cargo test -p forge-metrics --release --test agg -- --ignored --nocapture`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use forge_core::ids::SiteId;
use forge_metrics::agg::MAX_RETENTION;
use forge_metrics::hist::HIGHEST_US;
use forge_metrics::{
    AccessRecord, AggConfig, Aggregator, Bucket, LatencyHist, MinuteDelta, Percentiles, PoolMode,
    PoolRoute, PoolSample, SiteResources, Step, Summary, attribute,
};
use hdrhistogram::Histogram;
use jiff::{SignedDuration, Timestamp};

// ------------------------------------------------------------------------------------------
// Counting allocator (per thread)
// ------------------------------------------------------------------------------------------

struct Counting;

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

fn track(delta: isize) {
    let _ = LIVE.try_with(|c| c.set(c.get() + delta));
}

// SAFETY: forwards to the system allocator; the bookkeeping never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            track(layout.size() as isize);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            track(layout.size() as isize);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        track(-(layout.size() as isize));
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            track(new_size as isize - layout.size() as isize);
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Bytes allocated and not freed by this thread.
fn live() -> isize {
    LIVE.with(|c| c.get())
}

const KIB: isize = 1024;
const MIB: isize = 1024 * 1024;

// ------------------------------------------------------------------------------------------
// Deterministic workloads
// ------------------------------------------------------------------------------------------

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    fn f64(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn normal(&mut self) -> f64 {
        let u1 = self.f64().max(1e-300);
        let u2 = self.f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// Log-normal latency in µs with this median (µs) and sigma.
    fn lognormal(&mut self, median_us: f64, sigma: f64) -> u64 {
        (median_us * (sigma * self.normal()).exp()).round() as u64
    }
}

/// Zipf(s) over `n` ranks: cumulative weights for a binary search.
struct Zipf(Vec<f64>);

impl Zipf {
    fn new(n: usize, s: f64) -> Self {
        let mut acc = 0.0;
        let cdf = (1..=n)
            .map(|k| {
                acc += 1.0 / (k as f64).powf(s);
                acc
            })
            .collect::<Vec<_>>();
        let total = acc;
        Self(cdf.into_iter().map(|c| c / total).collect())
    }

    fn sample(&self, rng: &mut Rng) -> usize {
        let u = rng.f64();
        self.0.partition_point(|c| *c < u).min(self.0.len() - 1)
    }
}

fn base() -> Timestamp {
    // 2026-09-21T13:46:40Z: not aligned to a minute or an hour, on purpose.
    Timestamp::from_second(1_790_000_000).unwrap()
}

fn ts_us(us: i64) -> Timestamp {
    Timestamp::from_microsecond(us).unwrap()
}

fn us(ts: Timestamp) -> i64 {
    ts.as_microsecond()
}

fn record(end_us: i64, rt: u64, ut: Option<u64>, status: u16, route: &str) -> AccessRecord {
    AccessRecord {
        end_ts: ts_us(end_us),
        host: "loja.test".into(),
        status,
        duration_us: rt,
        upstream_us: ut,
        bytes: 1000,
        method: "GET".into(),
        route: route.into(),
    }
}

#[derive(Debug, Clone, Copy)]
struct Req {
    end_us: i64,
    rt: u64,
    ut: Option<u64>,
    status: u16,
}

/// Feeds requests (sorted by end) in 250 ms batches, each delivered `delay_ms` after it ends
/// (less than the 2 s grace: nothing is late), advancing the clock before each batch.
fn feed(agg: &mut Aggregator, site: SiteId, reqs: &[Req], delay_ms: i64) {
    const BATCH: i64 = 250_000;
    let mut rec = record(reqs[0].end_us, 0, None, 200, "/");
    let mut i = 0;
    while i < reqs.len() {
        let batch_end = (reqs[i].end_us.div_euclid(BATCH) + 1) * BATCH;
        agg.advance(ts_us(batch_end + delay_ms * 1000));
        while i < reqs.len() && reqs[i].end_us < batch_end {
            let r = reqs[i];
            rec.end_ts = ts_us(r.end_us);
            rec.duration_us = r.rt;
            rec.upstream_us = r.ut;
            rec.status = r.status;
            agg.record(site, &rec, 1);
            i += 1;
        }
    }
}

/// `n` requests evenly spread (with jitter) over `secs` seconds from `start_us`.
fn spread(
    rng: &mut Rng,
    start_us: i64,
    secs: i64,
    n: usize,
    mut latency: impl FnMut(&mut Rng) -> u64,
) -> Vec<Req> {
    let step = secs as f64 * 1e6 / n as f64;
    (0..n)
        .map(|i| {
            let end_us = start_us + (i as f64 * step + rng.f64() * step * 0.9) as i64;
            let rt = latency(rng);
            // Two thirds go to PHP; the upstream takes most of the time.
            let ut = (rng.below(3) > 0).then(|| rt.saturating_sub(rt / 20 + rng.below(300)));
            let status = match rng.below(100) {
                0..=3 => 404,
                4 => 500,
                _ => 200,
            };
            Req {
                end_us,
                rt,
                ut,
                status,
            }
        })
        .collect()
}

// ------------------------------------------------------------------------------------------
// Brute force
// ------------------------------------------------------------------------------------------

fn hdr3() -> Histogram<u64> {
    Histogram::<u64>::new_with_bounds(1, HIGHEST_US, 3).unwrap()
}

/// p50/p90/p99 must be `highest_equivalent` of the brute-force percentile, so within
/// 1/1024 of it (the bound of 3 significant digits).
fn check_percentiles(what: &str, got: Option<Percentiles>, mut vals: Vec<u64>) {
    if vals.is_empty() {
        assert_eq!(got, None, "{what}: no values");
        return;
    }
    let got = got.unwrap_or_else(|| panic!("{what}: percentiles missing"));
    vals.sort_unstable();
    let h = hdr3();
    let n = vals.len() as f64;
    for (q, v) in [(0.5, got.p50_us), (0.9, got.p90_us), (0.99, got.p99_us)] {
        let rank = ((q * n).ceil() as usize).max(1);
        let truth = vals[rank - 1];
        assert_eq!(
            v,
            h.highest_equivalent(truth),
            "{what}: q={q} truth={truth}"
        );
        assert!(
            v >= truth && (v - truth) as f64 <= truth as f64 / 1024.0,
            "{what}: q={q} {v} vs {truth}"
        );
    }
}

/// Counts and percentiles of `s` equal brute force over the span it says it covered.
fn check_summary(what: &str, s: &Summary, reqs: &[Req]) {
    let (from, to) = (us(s.from), us(s.to));
    let inside: Vec<&Req> = reqs
        .iter()
        .filter(|r| r.end_us >= from && r.end_us < to)
        .collect();
    assert_eq!(s.counters.requests, inside.len() as u64, "{what}: requests");
    let e5 = inside.iter().filter(|r| r.status >= 500).count() as u64;
    let e4 = inside
        .iter()
        .filter(|r| (400..500).contains(&r.status))
        .count() as u64;
    assert_eq!(
        (s.counters.err4xx(), s.counters.err5xx()),
        (e4, e5),
        "{what}: errors"
    );
    let rt_sum: u64 = inside.iter().map(|r| r.rt).sum();
    let ut: Vec<u64> = inside.iter().filter_map(|r| r.ut).collect();
    assert_eq!(s.counters.rt_sum_us, rt_sum, "{what}: rt sum");
    assert_eq!(
        s.counters.ut_sum_us,
        ut.iter().sum::<u64>(),
        "{what}: ut sum"
    );
    assert_eq!(s.counters.ut_count, ut.len() as u64, "{what}: ut count");
    check_percentiles(
        &format!("{what} rt"),
        s.rt,
        inside.iter().map(|r| r.rt.min(HIGHEST_US)).collect(),
    );
    check_percentiles(
        &format!("{what} ut"),
        s.ut,
        ut.into_iter().map(|v| v.min(HIGHEST_US)).collect(),
    );
    assert!(!s.sampled, "{what}: nothing was sampled");
}

fn assert_aligned(what: &str, s: &Summary, req_from: Timestamp, req_to: Timestamp, now: Timestamp) {
    let (f, t) = (s.from.as_second(), s.to);
    assert!(
        s.from <= req_from,
        "{what}: from {} after {req_from}",
        s.from
    );
    assert!(t >= req_to.min(now), "{what}: to {t} before {req_to}");
    assert_eq!(f % 2, 0, "{what}: from aligned to a bucket");
    assert!(t <= now, "{what}: past the clock");
}

fn cfg() -> AggConfig {
    AggConfig {
        minute_outbox: true,
        ..AggConfig::default()
    }
}

// ------------------------------------------------------------------------------------------
// Exactness
// ------------------------------------------------------------------------------------------

#[test]
fn windows_and_ranges_match_brute_force_lognormal_1m_samples() {
    let site = SiteId::new();
    let start = us(base());
    let mut rng = Rng(1);
    // 2 h at ~150 req/s, log-normal (median 20 ms, p99 ~120 ms), microsecond resolution.
    let reqs = spread(&mut rng, start, 7200, 1_080_000, |r| {
        r.lognormal(20_000.0, 0.77)
    });
    let mut agg = Aggregator::new(cfg(), base());
    feed(&mut agg, site, &reqs, 300);
    let now = agg.now();
    assert_eq!(agg.stats().late, 0);

    for (name, span) in [("1m", 60), ("5m", 300), ("1h", 3600), ("24h", 86_400)] {
        let s = agg.window(site, Duration::from_secs(span));
        let req_from = now - SignedDuration::from_secs(span as i64);
        assert_aligned(name, &s, req_from, now, now);
        check_summary(name, &s, &reqs);
    }
    let t = |secs: i64| base() + SignedDuration::from_secs(secs);
    let ranges = [
        // Tier 0 at both edges.
        (
            "fine",
            now - SignedDuration::from_secs(451),
            now - SignedDuration::from_secs(60),
        ),
        // Partial minute older than tier 0 (outward to the minute), whole minutes, tier 0 end.
        (
            "mixed",
            now - SignedDuration::from_secs(37 * 60 + 13),
            now - SignedDuration::from_secs(187),
        ),
        // Older than an hour: minute histograms past the budget fall back to their hour.
        ("old", t(5 * 60 + 7), t(65 * 60 + 31)),
        ("first-half", t(0), t(3600)),
        ("all", t(-10), now),
    ];
    for (name, from, to) in ranges {
        let s = agg.range(site, from, to);
        assert_aligned(name, &s, from, to, now);
        check_summary(name, &s, &reqs);
    }
    // A whole hour of the clock: the hour bucket.
    let h = (base().as_second() / 3600 + 1) * 3600;
    let (from, to) = (
        Timestamp::from_second(h).unwrap(),
        Timestamp::from_second(h + 3600).unwrap(),
    );
    let s = agg.range(site, from, to);
    assert_eq!((s.from, s.to), (from, to));
    check_summary("hour", &s, &reqs);
}

#[test]
fn windows_and_ranges_match_brute_force_bimodal() {
    let site = SiteId::new();
    let start = us(base());
    let mut rng = Rng(2);
    // 1 h at 30 req/s: 80 % fast (median 4 ms), 20 % slow (median 350 ms), 1 % beyond 10 min.
    let reqs = spread(&mut rng, start, 3600, 108_000, |r| match r.below(100) {
        0 => 700_000_000,
        1..=20 => r.lognormal(350_000.0, 0.5),
        _ => r.lognormal(4_000.0, 0.3),
    });
    let mut agg = Aggregator::new(cfg(), base());
    feed(&mut agg, site, &reqs, 1500);
    let now = agg.now();
    for span in [60, 300, 3600, 86_400] {
        let s = agg.window(site, Duration::from_secs(span));
        check_summary(&format!("window {span}"), &s, &reqs);
    }
    let s = agg.range(
        site,
        base() + SignedDuration::from_secs(1234),
        now - SignedDuration::from_secs(99),
    );
    check_summary("range", &s, &reqs);
}

#[test]
fn sixty_minutes_merge_to_the_hour() {
    let site = SiteId::new();
    let h = (base().as_second() / 3600 + 1) * 3600;
    let mut rng = Rng(3);
    let reqs = spread(&mut rng, h * 1_000_000, 3600, 200_000, |r| {
        r.lognormal(20_000.0, 0.9)
    });
    let mut agg = Aggregator::new(cfg(), Timestamp::from_second(h).unwrap());
    feed(&mut agg, site, &reqs, 500);
    agg.advance(Timestamp::from_second(h + 3600 + 10).unwrap());
    let deltas: Vec<MinuteDelta> = agg.take_minutes();
    assert_eq!(deltas.len(), 60, "one complete delta per minute");
    assert!(deltas.iter().all(|d| d.complete && d.site == site));
    let merged_rt = deltas
        .iter()
        .fold(LatencyHist::new(), |acc, d| acc.merge(&d.rt));
    let merged_ut = deltas
        .iter()
        .fold(LatencyHist::new(), |acc, d| acc.merge(&d.ut));
    let direct_rt = LatencyHist::from_values(reqs.iter().map(|r| r.rt));
    let direct_ut = LatencyHist::from_values(reqs.iter().filter_map(|r| r.ut));
    assert_eq!(merged_rt, direct_rt);
    assert_eq!(merged_ut, direct_ut);
    // And the same as the crate fed with the raw values.
    let mut crate_h = hdr3();
    for r in &reqs {
        crate_h.record(r.rt.min(HIGHEST_US)).unwrap();
    }
    assert_eq!(merged_rt.to_hdr(), crate_h);
    for q in [0.5, 0.9, 0.99, 0.999] {
        assert_eq!(merged_rt.quantile(q), Some(crate_h.value_at_quantile(q)));
    }
    // The hour bucket and the 60 minutes answer the same.
    let (from, to) = (
        Timestamp::from_second(h).unwrap(),
        Timestamp::from_second(h + 3600).unwrap(),
    );
    let (s, rt, ut) = agg.range_hists(site, from, to);
    assert_eq!((rt, ut), (direct_rt.clone(), direct_ut));
    assert_eq!(s.rt, direct_rt.percentiles());
    // The persisted form round-trips.
    let v2 = merged_rt.to_v2().unwrap();
    assert_eq!(LatencyHist::from_v2(&v2).unwrap(), merged_rt);
    // Minute counts sum to the total.
    let total: u64 = deltas.iter().map(|d| d.counters.requests).sum();
    assert_eq!(total, reqs.len() as u64);
}

// ------------------------------------------------------------------------------------------
// Clock: late, future, old, backwards
// ------------------------------------------------------------------------------------------

#[test]
fn late_records_go_to_tier2_only() {
    let site = SiteId::new();
    // Clock at 05 s into a minute.
    let m = (base().as_second() / 60 + 1) * 60;
    let t = |s: i64| Timestamp::from_second(m + s).unwrap();
    let mut agg = Aggregator::new(cfg(), t(0));
    agg.advance(t(5));
    agg.record(site, &record(us(t(4)), 1000, None, 200, "/a"), 1);
    agg.advance(t(15));
    // Its 2 s bucket [4, 6) closed at 6 + 2 s: this one is late.
    agg.record(site, &record(us(t(5)), 9000, None, 200, "/a"), 1);
    assert_eq!(agg.stats().late, 1);
    agg.advance(t(16));
    let w = agg.window(site, Duration::from_secs(60));
    assert_eq!(
        w.counters.requests, 1,
        "1m window (tier 0) has the on-time record only"
    );
    let points = agg.series(site, t(0), t(16), Step::TenSeconds);
    assert_eq!(points.iter().map(|p| p.requests).sum::<u64>(), 1);
    // Once the minute closes, tier 2 has both.
    agg.advance(t(63));
    let hour = agg.window(site, Duration::from_secs(3600));
    assert_eq!(hour.counters.requests, 2);
    assert_eq!(hour.rt.unwrap().p99_us, hdr3().highest_equivalent(9000));
    let minutes = agg.series(site, t(0), t(60), Step::Minute);
    assert_eq!(minutes.len(), 1);
    assert_eq!(minutes[0].requests, 2);
    let deltas = agg.take_minutes();
    assert_eq!(deltas.len(), 1);
    assert!(deltas[0].complete);
    assert_eq!(deltas[0].counters.requests, 2);
    // Late after the minute closed: a delta, folded into the minute and its hour.
    agg.record(site, &record(us(t(30)), 4000, Some(3000), 503, "/b"), 1);
    let hour = agg.window(site, Duration::from_secs(3600));
    assert_eq!(hour.counters.requests, 3);
    assert_eq!(hour.counters.err5xx(), 1);
    let deltas = agg.take_minutes();
    assert_eq!(deltas.len(), 1);
    assert!(!deltas[0].complete);
    assert_eq!(deltas[0].counters.requests, 1);
    assert_eq!(deltas[0].ut.total(), 1);
    assert_eq!(agg.series(site, t(0), t(60), Step::Minute)[0].requests, 3);
}

#[test]
fn future_records_are_clamped_and_old_ones_dropped() {
    let site = SiteId::new();
    let mut agg = Aggregator::new(cfg(), base());
    agg.advance(base() + SignedDuration::from_secs(1));
    agg.record(
        site,
        &record(us(base()) + 3_600_000_000, 1000, None, 200, "/"),
        1,
    );
    assert_eq!(agg.stats().clamped, 1);
    assert_eq!(agg.last_request_at(site), Some(agg.now()));
    let w = agg.window(site, Duration::from_secs(60));
    assert_eq!(w.counters.requests, 1);
    agg.record(
        site,
        &record(us(base()) - 25 * 3_600_000_000, 1000, None, 200, "/"),
        1,
    );
    assert_eq!(agg.stats().dropped, 1);
    // A clock that goes back is ignored.
    agg.advance(base());
    assert_eq!(agg.stats().clock_back, 1);
    assert_eq!(agg.now(), base() + SignedDuration::from_secs(1));
}

/// Errors and resources from the future are clamped like the buckets and
/// `last_request_at`: nothing is shown or stored ahead of the aggregator clock.
#[test]
fn future_errors_and_resources_are_clamped_to_the_clock() {
    let site = SiteId::new();
    let m = (base().as_second() / 60 + 1) * 60;
    let t = |s: i64| Timestamp::from_second(m + s).unwrap();
    let mut agg = Aggregator::new(cfg(), t(0));
    agg.advance(t(5));
    // A server clock an hour or two ahead.
    agg.record(site, &record(us(t(3600)), 1000, None, 502, "/x"), 1);
    agg.record(site, &record(us(t(7200)), 1000, None, 404, "/y"), 1);
    let errors = agg.recent_errors(site);
    assert_eq!(
        errors.server[0].at,
        agg.now(),
        "5xx at the clock, not in the future"
    );
    assert_eq!(
        errors.client[0].at,
        agg.now(),
        "4xx at the clock, not in the future"
    );
    assert_eq!(agg.last_request_at(site), Some(agg.now()));
    // Resources for a period ahead of the clock land in the clock's own period.
    let r = SiteResources {
        cpu_pct: 12.0,
        mem_bytes: 64 << 20,
        approx: false,
        workers_active: 1,
    };
    agg.set_resources(site, t(3600), &r);
    let now = agg.series(site, t(0), t(10), Step::TenSeconds);
    assert_eq!(now.len(), 1);
    assert_eq!(now[0].start, t(0));
    assert_eq!(now[0].resources.as_ref(), Some(&r));
    // An hour later there is still no point at the future period.
    agg.advance(t(3700));
    assert!(
        agg.series(site, t(3590), t(3620), Step::TenSeconds)
            .is_empty(),
        "no point was created ahead of the clock"
    );
    let then = agg.series(site, t(0), t(10), Step::TenSeconds);
    assert_eq!(then[0].resources.as_ref(), Some(&r));
    assert_eq!(then[0].requests, 2);
}

/// Stored buckets count in `u32` (an hour holds a million requests per second); totals of
/// queries add them in `u64`, so a long range at a high rate does not stop at 4.29e9.
#[test]
fn query_totals_do_not_saturate_at_u32() {
    const W: u32 = 2_000_000_000;
    let site = SiteId::new();
    let h0 = (base().as_second() / 3600 + 1) * 3600;
    let t = |s: i64| Timestamp::from_second(h0 + s).unwrap();
    let mut agg = Aggregator::new(cfg(), t(0));
    // One record standing for 2e9 lines per hour, for 3 hours: each bucket fits in u32.
    for h in 0..3 {
        agg.advance(t(h * 3600 + 1));
        agg.record(site, &record(us(t(h * 3600)), 1000, Some(500), 503, "/"), W);
    }
    agg.advance(t(3 * 3600 + 10));
    let total = 3 * u64::from(W);
    assert!(total > u64::from(u32::MAX));
    let check = |what: &str, c: &forge_metrics::Counters| {
        assert_eq!(c.requests, total, "{what}: requests");
        assert_eq!(c.err5xx(), total, "{what}: 5xx");
        assert_eq!(c.ut_count, total, "{what}: ut count");
        assert_eq!(c.rt_sum_us, 1000 * total, "{what}: rt sum");
        assert_eq!(c.bytes, 1000 * total, "{what}: bytes");
    };
    check(
        "window",
        &agg.window(site, Duration::from_secs(86_400)).counters,
    );
    check("range", &agg.range(site, t(0), t(3 * 3600)).counters);
    check("sums", &agg.sums(site, t(0), t(3 * 3600)));
    let hours = agg.series(site, t(0), t(3 * 3600), Step::Hour);
    assert_eq!(hours.len(), 3);
    assert!(hours.iter().all(|p| p.requests == u64::from(W)));
}

#[test]
fn weighted_records_scale_counts_and_mark_sampled() {
    let site = SiteId::new();
    let mut agg = Aggregator::new(cfg(), base());
    agg.advance(base() + SignedDuration::from_secs(1));
    agg.record(site, &record(us(base()), 1000, Some(800), 200, "/"), 4);
    agg.record(site, &record(us(base()), 50_000, None, 500, "/"), 1);
    let w = agg.window(site, Duration::from_secs(60));
    assert_eq!(w.counters.requests, 5);
    assert_eq!(w.counters.ut_count, 4);
    assert_eq!(w.counters.rt_sum_us, 4 * 1000 + 50_000);
    assert_eq!(w.counters.bytes, 5000);
    assert!(w.sampled);
    // 4 of 5 at 1 ms: p50 is 1 ms; ranks 5 (p90) and 5 (p99) are the 50 ms one.
    let p = w.rt.unwrap();
    let slow = hdr3().highest_equivalent(50_000);
    assert_eq!((p.p50_us, p.p90_us, p.p99_us), (1000, slow, slow));
}

// ------------------------------------------------------------------------------------------
// Cost of a query: bounded by the retention, not by the span asked
// ------------------------------------------------------------------------------------------

/// Runs `f` on its own thread and fails instead of hanging when it does not return within
/// `limit` (an unbounded planner never returns from `window(Duration::MAX)`).
fn within<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(limit) {
        Ok(v) => v,
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            std::panic::resume_unwind(handle.join().expect_err("the sender is gone"))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("queries did not return within {limit:?}: unbounded planning")
        }
    }
}

/// Best of `runs` calls (the machine may be busy compiling other things).
fn timed<T>(runs: usize, mut f: impl FnMut() -> T) -> (T, Duration) {
    let mut best = Duration::MAX;
    let mut out = None;
    for _ in 0..runs {
        let t = Instant::now();
        let v = f();
        best = best.min(t.elapsed());
        out = Some(v);
    }
    (out.expect("runs > 0"), best)
}

/// Client-supplied spans (`stats.get`, `stats.series`) reach the aggregator as is:
/// `Timestamp::MIN` and `Duration::MAX` must cost what a 24 h window costs.
#[test]
fn unbounded_spans_are_clamped_to_the_retention_horizon() {
    const DAY: Duration = Duration::from_secs(86_400);
    // Debug build, other builds running: still two orders of magnitude above the real cost.
    const BUDGET: Duration = Duration::from_millis(50);
    let site = SiteId::new();
    let h0 = (base().as_second() / 3600 + 1) * 3600;
    let start = h0 - 2 * 3600;
    let mut rng = Rng(9);
    // One request every ~20 s for 26 h, the last 5 min before the clock: the first 2 h
    // leave the retention.
    let reqs = spread(&mut rng, start * 1_000_000, 26 * 3600 - 300, 4_680, |r| {
        r.lognormal(15_000.0, 0.6)
    });
    let mut agg = Aggregator::new(AggConfig::default(), Timestamp::from_second(start).unwrap());
    feed(&mut agg, site, &reqs, 300);
    // `now - 24h` is the hour `h0` itself: the horizon.
    let now = Timestamp::from_second(start + 26 * 3600).unwrap();
    agg.advance(now);
    let horizon = Timestamp::from_second(h0).unwrap();
    let in_day = reqs.iter().filter(|r| r.end_us >= h0 * 1_000_000).count() as u64;
    let reqs_bg = reqs.clone();

    let (agg, timings) = within(Duration::from_secs(20), move || {
        let reqs = reqs_bg;
        let mut agg = agg;
        let mut timings = Vec::new();
        let (w24, d) = timed(3, || agg.window(site, DAY));
        timings.push(("window(24h)", d));
        let (wmax, d) = timed(3, || agg.window(site, Duration::MAX));
        timings.push(("window(MAX)", d));
        let (rmin, d) = timed(3, || agg.range(site, Timestamp::MIN, now));
        timings.push(("range(MIN, now)", d));
        let (rall, d) = timed(3, || agg.range(site, Timestamp::MIN, Timestamp::MAX));
        timings.push(("range(MIN, MAX)", d));
        let (sums, d) = timed(3, || agg.sums(site, Timestamp::MIN, now));
        timings.push(("sums(MIN, now)", d));
        let ((rh, rt, ut), d) = timed(3, || agg.range_hists(site, Timestamp::MIN, now));
        timings.push(("range_hists(MIN, now)", d));
        let mut series = Vec::new();
        for (name, step) in [
            ("series(MIN, MAX, 1h)", Step::Hour),
            ("series(MIN, MAX, 1m)", Step::Minute),
            ("series(MIN, MAX, 10s)", Step::TenSeconds),
        ] {
            let (points, d) = timed(3, || agg.series(site, Timestamp::MIN, Timestamp::MAX, step));
            timings.push((name, d));
            series.push((name, points));
        }
        // An empty span far in the past, and a site without data.
        let (old, d) = timed(3, || agg.range(site, Timestamp::MIN, Timestamp::MIN));
        timings.push(("range(MIN, MIN)", d));
        let (none, d) = timed(3, || agg.window(SiteId::new(), Duration::MAX));
        timings.push(("window(MAX) unknown site", d));

        assert_eq!((w24.from, w24.to), (horizon, now));
        assert_eq!(w24.counters.requests, in_day);
        check_summary("window(24h)", &w24, &reqs);
        assert_eq!(wmax, w24, "window(MAX)");
        assert_eq!(rmin, w24, "range(MIN, now)");
        assert_eq!(rall, w24, "range(MIN, MAX)");
        assert_eq!(sums, w24.counters, "sums(MIN, now)");
        assert_eq!(rh, w24, "range_hists(MIN, now)");
        assert_eq!(rt.total(), in_day);
        assert_eq!(ut.total(), w24.counters.ut_count);
        for (name, points) in &series {
            let total: u64 = points.iter().map(|p| p.requests).sum();
            assert_eq!(total, in_day, "{name}");
            assert!(points[0].start >= horizon, "{name}: before the horizon");
        }
        assert_eq!((old.from, old.to), (horizon, horizon));
        assert_eq!(old.counters.requests, 0);
        assert_eq!((none.from, none.counters.requests), (horizon, 0));
        (agg, timings)
    });
    println!("query cost (best of 3): {timings:?}");
    for (what, d) in &timings {
        assert!(*d < BUDGET, "{what} took {d:?}: {timings:?}");
    }

    // A clock off the hour: the horizon is the hour holding `now - 24h`, older than the
    // 24 h window, and the range covers it exactly (its whole hour bucket).
    let mut agg = agg;
    let later = now + SignedDuration::from_secs(1234);
    agg.advance(later);
    let (s, w, d) = within(Duration::from_secs(20), move || {
        let (s, d) = timed(3, || agg.range(site, Timestamp::MIN, Timestamp::MAX));
        (s, agg.window(site, Duration::MAX), d)
    });
    assert!(d < BUDGET, "range(MIN, MAX) off the hour took {d:?}");
    assert_eq!((s.from, s.to), (horizon, later));
    check_summary("range(MIN, MAX) off the hour", &s, &reqs);
    assert_eq!(w, s, "window(MAX) off the hour");
}

/// The horizon bound holds whatever the config: the retention is capped at 7 days.
#[test]
fn a_huge_retention_is_capped() {
    let site = SiteId::new();
    let config = AggConfig {
        retention: Duration::MAX,
        ..AggConfig::default()
    };
    let mut agg = Aggregator::new(config, base());
    agg.advance(base() + SignedDuration::from_secs(1));
    agg.record(site, &record(us(base()), 1000, None, 200, "/"), 1);
    let now = agg.now();
    let horizon = (now.as_second() - MAX_RETENTION.as_secs() as i64).div_euclid(3600) * 3600;
    let (r, w, d) = within(Duration::from_secs(20), move || {
        let (r, d) = timed(3, || agg.range(site, Timestamp::MIN, Timestamp::MAX));
        (r, agg.window(site, Duration::MAX), d)
    });
    assert!(d < Duration::from_millis(50), "range(MIN, MAX) took {d:?}");
    assert_eq!(r.from.as_second(), horizon);
    assert_eq!(r.counters.requests, 1);
    assert_eq!(w, r);
}

// ------------------------------------------------------------------------------------------
// Series, resources, persistence hooks
// ------------------------------------------------------------------------------------------

#[test]
fn series_have_one_point_per_step_with_data() {
    let site = SiteId::new();
    let h = (base().as_second() / 3600 + 1) * 3600;
    let t = |s: i64| Timestamp::from_second(h + s).unwrap();
    let mut rng = Rng(4);
    // One request per second for an hour.
    let reqs: Vec<Req> = (0..3600)
        .map(|i| Req {
            end_us: (h + i) * 1_000_000 + 500_000,
            rt: rng.lognormal(10_000.0, 0.5),
            ut: None,
            status: 200,
        })
        .collect();
    let mut agg = Aggregator::new(cfg(), t(0));
    feed(&mut agg, site, &reqs, 200);
    agg.advance(t(3600 + 1));
    assert_eq!(agg.series(site, t(0), t(3600), Step::TenSeconds).len(), 360);
    assert_eq!(agg.series(site, t(0), t(3600), Step::Minute).len(), 60);
    let fine = agg.series(site, t(3000), t(3600), Step::TwoSeconds);
    assert_eq!(fine.len(), 300);
    assert!(fine.iter().all(|p| p.requests == 2));
    let hours = agg.series(site, t(-7200), t(3600), Step::Hour);
    assert_eq!(hours.len(), 1);
    assert_eq!(hours[0].requests, 3600);
    assert_eq!(hours[0].start, t(0));
    // The minute points have the exact percentiles of their minute.
    let minutes = agg.series(site, t(0), t(3600), Step::Minute);
    for (i, p) in minutes.iter().enumerate() {
        let vals: Vec<u64> = reqs[i * 60..(i + 1) * 60].iter().map(|r| r.rt).collect();
        check_percentiles("minute", p.rt, vals);
    }
    // Trailing periods that are not closed yet come from tier 0.
    agg.record(
        site,
        &record(us(t(3600)) + 200_000, 1000, None, 200, "/"),
        1,
    );
    let last = agg.series(site, t(3590), t(3610), Step::TenSeconds);
    assert_eq!(last.last().unwrap().requests, 1);
    assert_eq!(last.last().unwrap().start, t(3600));
}

#[test]
fn resources_land_in_tier1_points_and_minutes_average_them() {
    let site = SiteId::new();
    let m = (base().as_second() / 60 + 1) * 60;
    let t = |s: i64| Timestamp::from_second(m + s).unwrap();
    let mut agg = Aggregator::new(cfg(), t(0));
    for i in 0..6 {
        agg.advance(t(i * 10 + 1));
        agg.record(site, &record(us(t(i * 10)), 1000, Some(900), 200, "/"), 1);
    }
    agg.advance(t(80));
    for i in 0..6 {
        let r = SiteResources {
            cpu_pct: 10.0 * f64::from(i + 1),
            mem_bytes: 100 << 20,
            approx: true,
            workers_active: i as u32,
        };
        agg.set_resources(site, t(i64::from(i) * 10 + 3), &r);
    }
    let points = agg.series(site, t(0), t(60), Step::TenSeconds);
    assert_eq!(points.len(), 6);
    assert_eq!(points[2].resources.as_ref().unwrap().cpu_pct, 30.0);
    assert_eq!(points[2].ut_sum_us, 900);
    let minute = &agg.series(site, t(0), t(60), Step::Minute)[0];
    let r = minute.resources.as_ref().unwrap();
    assert_eq!(r.cpu_pct, 35.0);
    assert_eq!(r.mem_bytes, 100 << 20);
    assert!(r.approx);
    assert_eq!(r.workers_active, 5);
    // A period without requests gets a point with resources only.
    agg.set_resources(site, t(75), &SiteResources::default());
    assert_eq!(agg.series(site, t(70), t(80), Step::TenSeconds).len(), 1);
    // Attribution sums.
    let sums = agg.sums(site, t(0), t(60));
    assert_eq!(
        (sums.requests, sums.ut_sum_us, sums.rt_sum_us),
        (6, 5400, 6000)
    );
}

#[test]
fn attribution_output_is_stored_packed_and_saturated() {
    let site = SiteId::new();
    let m = (base().as_second() / 60 + 1) * 60;
    let t = |s: i64| Timestamp::from_second(m + s).unwrap();
    let mut agg = Aggregator::new(cfg(), t(0));
    agg.advance(t(60));
    let cases = [
        // (stored, read back)
        (
            SiteResources {
                cpu_pct: 33.3,
                mem_bytes: 1,
                approx: false,
                workers_active: 3,
            },
            SiteResources {
                cpu_pct: f64::from(33.3_f32),
                mem_bytes: 1024,
                approx: false,
                workers_active: 3,
            },
        ),
        (
            SiteResources {
                cpu_pct: f64::INFINITY,
                mem_bytes: u64::MAX,
                approx: true,
                workers_active: 70_000,
            },
            SiteResources {
                cpu_pct: f64::from(f32::MAX),
                mem_bytes: u64::from(u32::MAX) * 1024,
                approx: true,
                workers_active: u32::from(u16::MAX),
            },
        ),
        (
            SiteResources {
                cpu_pct: f64::NAN,
                mem_bytes: 0,
                approx: false,
                workers_active: 0,
            },
            SiteResources::default(),
        ),
        (
            SiteResources {
                cpu_pct: -1.0,
                mem_bytes: 2048,
                approx: false,
                workers_active: 1,
            },
            SiteResources {
                cpu_pct: 0.0,
                mem_bytes: 2048,
                approx: false,
                workers_active: 1,
            },
        ),
    ];
    for (i, (stored, expected)) in cases.iter().enumerate() {
        let at = t(i as i64 * 10);
        agg.set_resources(site, at, stored);
        let points = agg.series(
            site,
            at,
            at + SignedDuration::from_secs(10),
            Step::TenSeconds,
        );
        assert_eq!(points.len(), 1, "case {i}");
        assert_eq!(points[0].resources.as_ref(), Some(expected), "case {i}");
    }
    // Setting it again replaces the point's resources, `approx` included.
    agg.set_resources(site, t(10), &SiteResources::default());
    let points = agg.series(site, t(10), t(20), Step::TenSeconds);
    assert_eq!(points[0].resources, Some(SiteResources::default()));
}

/// The T6 -> T5 loop the metrics actor runs every 10 s: the sums of a bucket feed
/// `attribute`, whose output goes back into the same tier 1 points, with no type in between.
#[test]
fn sums_feed_attribution_and_its_output_lands_in_tier1() {
    let (a, b) = (SiteId::new(), SiteId::new());
    let m = (base().as_second() / 60 + 1) * 60;
    let t = |s: i64| Timestamp::from_second(m + s).unwrap();
    let mut agg = Aggregator::new(cfg(), t(0));
    // Site a: 3 ms of PHP per request, b: 1 ms, same request count -> 75/25.
    for i in 0..10 {
        agg.advance(t(i + 1));
        agg.record(a, &record(us(t(i)), 4000, Some(3000), 200, "/a"), 1);
        agg.record(b, &record(us(t(i)), 2000, Some(1000), 200, "/b"), 1);
    }
    agg.advance(t(20));
    let mut bucket = Bucket {
        length_ms: 10_000,
        pools: vec![PoolSample {
            pool: "php_8_3".into(),
            mode: PoolMode::Shared,
            cpu_pct: 40.0,
            mem_bytes: 400 << 20,
            workers_active: 4,
            from_ms: 0,
            to_ms: 10_000,
        }],
        ..Bucket::default()
    };
    for (site, name) in [(a, "a"), (b, "b")] {
        let sums = agg.sums(site, t(0), t(10));
        assert_eq!(sums.requests, 10);
        bucket.loads.insert(name.into(), sums.site_load());
        bucket.routes.push(PoolRoute {
            site: name.into(),
            pool: "php_8_3".into(),
            from_ms: 0,
            to_ms: 10_000,
        });
    }
    let out = attribute(&bucket);
    for (site, name) in [(a, "a"), (b, "b")] {
        agg.set_resources(site, t(0), &out.sites[name]);
    }
    let ra = agg.series(a, t(0), t(10), Step::TenSeconds)[0]
        .resources
        .clone()
        .unwrap();
    let rb = agg.series(b, t(0), t(10), Step::TenSeconds)[0]
        .resources
        .clone()
        .unwrap();
    assert_eq!((ra.cpu_pct, rb.cpu_pct), (30.0, 10.0));
    assert_eq!((ra.mem_bytes, rb.mem_bytes), (300 << 20, 100 << 20));
    assert!(ra.approx && rb.approx);
    assert_eq!((ra.workers_active, rb.workers_active), (3, 1));
}

#[test]
fn restored_minutes_give_the_same_24h_numbers() {
    let site = SiteId::new();
    let mut rng = Rng(5);
    let reqs = spread(&mut rng, us(base()), 3 * 3600, 150_000, |r| {
        r.lognormal(30_000.0, 0.8)
    });
    let mut a = Aggregator::new(cfg(), base());
    feed(&mut a, site, &reqs, 400);
    let end = a.now() + SignedDuration::from_secs(120);
    a.advance(end);
    let deltas = a.take_minutes();
    let before = a.window(site, Duration::from_secs(86_400));
    // A restarted daemon: a new aggregator, the minutes read back from storage.
    let mut b = Aggregator::new(cfg(), end);
    for d in deltas {
        b.restore_minute(d);
    }
    let after = b.window(site, Duration::from_secs(86_400));
    assert_eq!(after.counters, before.counters);
    assert_eq!((after.rt, after.ut), (before.rt, before.ut));
    check_summary("restored", &after, &reqs);
    assert!(
        b.take_minutes().is_empty(),
        "restored minutes are not flushed again"
    );
}

/// The system clock went back between two boots: minutes persisted by the first run are
/// still open for the second. Restored, they count in queries and in tier 2, but the deltas
/// of the second run only carry its own records, so adding them to storage stays exact.
#[test]
fn a_restored_open_minute_is_not_flushed_twice() {
    let site = SiteId::new();
    let m = (base().as_second() / 60 + 1) * 60;
    let t = |s: i64| Timestamp::from_second(m + s).unwrap();
    // First run: minutes [0, 60) and [60, 120) close and are persisted.
    let mut first = Aggregator::new(cfg(), t(0));
    first.advance(t(11));
    for i in 0..5 {
        first.record(site, &record(us(t(10)) + i, 2000, Some(1500), 200, "/a"), 1);
    }
    first.advance(t(71));
    for i in 0..7 {
        first.record(site, &record(us(t(70)) + i, 8000, None, 500, "/b"), 1);
    }
    first.advance(t(125));
    let persisted = first.take_minutes();
    assert_eq!(persisted.len(), 2);
    let mut stored: HashMap<Timestamp, (u64, LatencyHist)> = persisted
        .iter()
        .map(|d| (d.start, (d.counters.requests, d.rt.clone())))
        .collect();
    // Second run, clock 2 min behind: both minutes are open again.
    let mut second = Aggregator::new(cfg(), t(15));
    for d in persisted {
        second.restore_minute(d);
    }
    assert!(
        second.take_minutes().is_empty(),
        "restored minutes are not flushed"
    );
    second.advance(t(21));
    for i in 0..3 {
        second.record(site, &record(us(t(20)) + i, 4000, None, 404, "/c"), 1);
    }
    // While open, a range over the whole minute sees the restored records too.
    second.advance(t(61));
    let open = second.range(site, t(0), t(60));
    assert_eq!(open.counters.requests, 5 + 3);
    assert_eq!(open.counters.err4xx(), 3);
    // The minutes close.
    second.advance(t(125));
    let deltas = second.take_minutes();
    assert_eq!(
        deltas.len(),
        1,
        "only the minute with new records emits; the only-restored one does not"
    );
    let d = &deltas[0];
    assert!(d.complete);
    assert_eq!(d.start, t(0));
    assert_eq!(
        d.counters.requests, 3,
        "the delta holds only this run's records"
    );
    assert_eq!(d.rt.total(), 3);
    assert_eq!(d.ut.total(), 0);
    // Persistence adds the delta to its row: storage and tier 2 agree, nothing twice.
    let row = stored.get_mut(&d.start).unwrap();
    row.0 += d.counters.requests;
    row.1 = row.1.merge(&d.rt);
    let minutes = second.series(site, t(0), t(120), Step::Minute);
    assert_eq!(minutes.len(), 2);
    for p in &minutes {
        let (requests, rt) = &stored[&p.start];
        assert_eq!(p.requests, *requests, "minute {}", p.start);
        assert_eq!(p.rt, rt.percentiles(), "minute {}", p.start);
    }
    assert_eq!(minutes[0].requests, 8);
    assert_eq!(minutes[1].requests, 7);
    let (s, rt, _) = second.range_hists(site, t(0), t(120));
    assert_eq!(s.counters.requests, 15);
    assert_eq!(rt, stored[&t(0)].1.merge(&stored[&t(60)].1));
}

#[test]
fn an_undrained_outbox_is_capped() {
    let site = SiteId::new();
    let config = AggConfig {
        minute_outbox_cap: 10,
        ..cfg()
    };
    let mut agg = Aggregator::new(config, base());
    for m in 0..30 {
        let t = base() + SignedDuration::from_secs(m * 60);
        agg.advance(t + SignedDuration::from_secs(1));
        agg.record(site, &record(us(t), 1000, None, 200, "/"), 1);
    }
    agg.advance(base() + SignedDuration::from_secs(31 * 60));
    let deltas = agg.take_minutes();
    assert_eq!(deltas.len(), 10);
    assert_eq!(agg.stats().outbox_dropped, 20);
    // The newest are kept.
    assert_eq!(
        deltas.last().unwrap().start.as_second(),
        (base().as_second() + 29 * 60) / 60 * 60
    );
}

#[test]
fn the_aggregator_can_move_into_an_actor() {
    fn send<T: Send + 'static>() {}
    send::<Aggregator>();
}

// ------------------------------------------------------------------------------------------
// Routes and recent errors
// ------------------------------------------------------------------------------------------

/// `n` Zipf(s) requests over 10,000 routes, one per millisecond. With `epoch`, the route
/// table rotates every `epoch` and the truth is counted over the generations in the report.
fn zipf_run(seed: u64, s: f64, n: usize, epoch: Option<Duration>) {
    const ROUTES: usize = 10_000;
    let site = SiteId::new();
    let zipf = Zipf::new(ROUTES, s);
    let mut rng = Rng(seed);
    let names: Vec<String> = (0..ROUTES).map(|i| format!("/r{i}")).collect();
    let config = AggConfig {
        route_epoch: epoch.unwrap_or(AggConfig::default().route_epoch),
        ..cfg()
    };
    let mut agg = Aggregator::new(config, base());
    let mut sent: Vec<(Timestamp, usize)> = Vec::with_capacity(n);
    let mut rec = record(us(base()), 1000, None, 200, "");
    for i in 0..n {
        let k = zipf.sample(&mut rng);
        if i % 1000 == 0 {
            agg.advance(base() + SignedDuration::from_millis(i as i64));
        }
        rec.end_ts = agg.now();
        sent.push((rec.end_ts, k));
        rec.route.clear();
        rec.route.push_str(&names[k]);
        rec.duration_us = 1000 + k as u64;
        agg.record(site, &rec, 1);
    }
    let report = agg.routes(site).unwrap();
    let mut truth = vec![0u64; ROUTES];
    for &(_, k) in sent.iter().filter(|(at, _)| *at >= report.since) {
        truth[k] += 1;
    }
    assert_eq!(report.total, truth.iter().sum::<u64>());
    if epoch.is_none() {
        assert_eq!(report.total, n as u64);
    } else {
        assert!(
            report.since > base() && report.total < n as u64,
            "rotated: the report starts at the previous generation"
        );
    }
    assert!(report.routes.len() <= 100);
    let mut by_count: Vec<usize> = (0..ROUTES).collect();
    by_count.sort_by_key(|&k| std::cmp::Reverse(truth[k]));
    let reported: HashMap<&str, _> = report
        .routes
        .iter()
        .map(|r| (r.route.as_str(), r))
        .collect();
    for &k in &by_count[..20] {
        let r = reported
            .get(names[k].as_str())
            .unwrap_or_else(|| panic!("seed {seed} s={s}: true top-20 route {} missing", names[k]));
        assert!(r.count >= truth[k], "count is an upper bound");
        assert!(
            r.count - r.overcount <= truth[k],
            "count - overcount is a lower bound"
        );
        assert!(r.observed <= truth[k]);
    }
    // Every reported route respects the bounds too (across generations as well: a route
    // monitored in only one of them still counts what the other's filter cell holds).
    for r in &report.routes {
        let k: usize = r.route[2..].parse().unwrap();
        assert!(
            r.count >= truth[k] && r.count - r.overcount <= truth[k],
            "seed {seed} s={s}: {} count {} overcount {} truth {}",
            r.route,
            r.count,
            r.overcount,
            truth[k]
        );
    }
}

#[test]
fn zipf_true_top20_routes_are_always_present() {
    for s in [0.8, 1.0, 1.2] {
        for seed in 1..=3 {
            zipf_run(seed, s, 200_000, None);
        }
    }
}

/// 200 s of requests with 60 s route epochs: the report merges two generations, and the
/// bounds and the top 20 hold over both.
#[test]
fn zipf_bounds_hold_across_route_generations() {
    for s in [0.8, 1.0, 1.2] {
        for seed in 1..=3 {
            zipf_run(seed, s, 200_000, Some(Duration::from_secs(60)));
        }
    }
}

#[test]
#[ignore = "release: cargo test -p forge-metrics --release --test agg -- --ignored"]
fn zipf_true_top20_routes_are_always_present_1m_requests() {
    for s in [0.7, 0.8, 0.9, 1.0, 1.1, 1.2] {
        for seed in 1..=10 {
            zipf_run(seed, s, 1_000_000, None);
            zipf_run(seed, s, 1_000_000, Some(Duration::from_secs(300)));
        }
    }
}

/// A route monitored in the previous generation only: its misses in the current one (kept
/// by the admission filter) still count, so `count` stays an upper bound of the truth.
#[test]
fn a_route_monitored_in_one_generation_keeps_the_other_ones_count() {
    let site = SiteId::new();
    let config = AggConfig {
        routes_k: 4,
        route_epoch: Duration::from_secs(60),
        ..cfg()
    };
    let t = |s: i64| base() + SignedDuration::from_secs(s);
    let mut agg = Aggregator::new(config, base());
    agg.advance(t(1));
    let hit = |agg: &mut Aggregator, route: &str, n: usize| {
        let rec = record(us(agg.now()), 1000, None, 200, route);
        for _ in 0..n {
            agg.record(site, &rec, 1);
        }
    };
    hit(&mut agg, "/a", 150);
    // New generation: four heavier routes fill the table, `/a` only reaches the filter.
    agg.advance(t(62));
    for h in 0..4 {
        hit(&mut agg, &format!("/h{h}"), 100);
    }
    hit(&mut agg, "/a", 30);
    let report = agg.routes(site).unwrap();
    assert_eq!(report.since, t(1));
    assert_eq!(report.total, 150 + 400 + 30);
    let a = &report.routes[0];
    assert_eq!(a.route, "/a");
    assert!(a.count >= 180, "upper bound: {} < 180", a.count);
    assert!(a.count - a.overcount <= 180);
    assert_eq!(
        (a.count, a.overcount),
        (180, 30),
        "the cell holds exactly its 30 misses"
    );
    assert_eq!(a.observed, 150, "only monitored requests are observed");
    // Routes only in the current generation: the previous one never filled up (no filter),
    // so nothing is added.
    for r in &report.routes[1..] {
        assert_eq!(
            (r.count, r.overcount, r.observed),
            (100, 0, 100),
            "{}",
            r.route
        );
    }
}

#[test]
fn routes_have_errors_and_percentiles_and_age_out() {
    let site = SiteId::new();
    let config = AggConfig {
        route_epoch: Duration::from_secs(3600),
        ..cfg()
    };
    let mut agg = Aggregator::new(config, base());
    agg.advance(base() + SignedDuration::from_secs(1));
    for i in 0..100u64 {
        let status = if i % 10 == 0 {
            500
        } else if i % 10 == 1 {
            404
        } else {
            200
        };
        agg.record(
            site,
            &record(us(base()), 1000 * (i + 1), None, status, "/users/{id}"),
            1,
        );
    }
    agg.record(site, &record(us(base()), 5000, None, 200, "/"), 1);
    // Same concatenation, different pairs: two routes.
    let mut odd = record(us(base()), 5000, None, 200, "T/");
    odd.method = "GE".into();
    agg.record(site, &odd, 1);
    let report = agg.routes(site).unwrap();
    assert_eq!(report.routes.len(), 3);
    assert!(
        report
            .routes
            .iter()
            .any(|r| (r.method.as_str(), r.route.as_str()) == ("GE", "T/"))
    );
    let users = &report.routes[0];
    assert_eq!(
        (users.method.as_str(), users.route.as_str()),
        ("GET", "/users/{id}")
    );
    assert_eq!(
        (users.count, users.overcount, users.observed),
        (100, 0, 100)
    );
    assert_eq!((users.err4xx, users.err5xx), (10, 10));
    assert_eq!(users.rt_sum_us, 1000 * 5050);
    let p = users.rt.unwrap();
    // 2 significant digits: within 1/128.
    for (got, truth) in [
        (p.p50_us, 50_000u64),
        (p.p90_us, 90_000),
        (p.p99_us, 99_000),
    ] {
        assert!(
            got >= truth && (got - truth) as f64 <= truth as f64 / 128.0,
            "{got} {truth}"
        );
    }
    // Still there one epoch later (previous generation), gone after two.
    agg.advance(base() + SignedDuration::from_secs(3601));
    assert_eq!(agg.routes(site).unwrap().routes.len(), 3);
    agg.advance(base() + SignedDuration::from_secs(7202));
    assert!(agg.routes(site).unwrap().routes.is_empty());
}

#[test]
fn recent_errors_keep_the_last_50_of_each_class() {
    let site = SiteId::new();
    let mut agg = Aggregator::new(cfg(), base());
    agg.advance(base() + SignedDuration::from_secs(1));
    for i in 0..120i64 {
        agg.record(
            site,
            &record(us(base()) + i, 1000, Some(900), 502, &format!("/s{i}")),
            1,
        );
    }
    for i in 0..70i64 {
        agg.record(
            site,
            &record(us(base()) + i, 1000, None, 404, &format!("/c{i}")),
            1,
        );
    }
    agg.record(site, &record(us(base()), 1000, None, 200, "/ok"), 1);
    let errors = agg.recent_errors(site);
    assert_eq!(errors.server.len(), 50);
    assert_eq!(errors.client.len(), 50);
    assert_eq!(errors.server[0].route, "/s119", "newest first");
    assert_eq!(errors.server[49].route, "/s70");
    assert_eq!(errors.client[0].route, "/c69");
    assert_eq!(errors.server[0].status, 502);
    assert_eq!(errors.server[0].upstream_us, Some(900));
    assert_eq!(errors.server[0].host, "loja.test");
    // Older than the retention: gone.
    agg.advance(base() + SignedDuration::from_secs(24 * 3600 + 10));
    let errors = agg.recent_errors(site);
    assert!(errors.server.is_empty() && errors.client.is_empty());
}

// ------------------------------------------------------------------------------------------
// Memory
// ------------------------------------------------------------------------------------------

/// A web site: Zipf(1) over 2,000 routes, each with its own log-normal latency (median
/// 2..200 ms, sigma 0.4), PHP behind 85 % of them; 94 % 2xx, 1 % 3xx, 4 % 4xx, 1 % 5xx.
/// Durations are logged in milliseconds like nginx's `$request_time` (Apache on Windows is
/// coarser still, on the 15.6 ms clock tick); `micros` keeps microseconds, the resolution of
/// Caddy (below 1 ms, the worst case for memory).
struct SiteLoad {
    site: SiteId,
    rate: f64,
    micros: bool,
    zipf: Zipf,
    routes: Vec<(AccessRecord, f64, bool)>,
}

impl SiteLoad {
    fn new(rng: &mut Rng, rate: f64, micros: bool) -> Self {
        let routes = (0..2000)
            .map(|i| {
                let median = 2000.0 * 100f64.powf(rng.f64());
                let mut rec = record(0, 0, None, 200, &format!("/app/section{}/page{i}", i % 37));
                rec.method = if i % 5 == 0 { "POST" } else { "GET" }.into();
                (rec, median, rng.below(100) < 85)
            })
            .collect();
        Self {
            site: SiteId::new(),
            rate,
            micros,
            zipf: Zipf::new(2000, 1.0),
            routes,
        }
    }

    /// Records the requests of second `sec` (ending at `sec + i / n`).
    fn second(&mut self, agg: &mut Aggregator, rng: &mut Rng, sec: i64) {
        let whole = self.rate.floor() as u64;
        let n = whole + u64::from(rng.f64() < self.rate - self.rate.floor());
        for i in 0..n {
            let k = self.zipf.sample(rng);
            let (rec, median, php) = &mut self.routes[k];
            let mut rt = rng.lognormal(*median, 0.4).max(1);
            if !self.micros {
                rt = rt.div_ceil(1000) * 1000;
            }
            rec.end_ts = ts_us(sec * 1_000_000 + (i * 1_000_000 / n) as i64);
            rec.duration_us = rt;
            rec.upstream_us = php.then(|| {
                let ut = rt.saturating_sub(rng.below(1500));
                if self.micros { ut } else { ut / 1000 * 1000 }
            });
            rec.status = match rng.below(100) {
                0..=93 => 200,
                94 => 302,
                95..=98 => 404,
                _ => 500,
            };
            rec.bytes = 500 + rng.below(50_000);
            agg.record(self.site, rec, 1);
        }
    }
}

/// Runs `loads` for `hours`, one clock step per second (records delivered 300 ms late).
fn run_day(agg: &mut Aggregator, loads: &mut [SiteLoad], rng: &mut Rng, hours: i64) {
    let start = base().as_second();
    for sec in start..start + hours * 3600 {
        agg.advance(ts_us(sec * 1_000_000 + 1_300_000));
        for load in loads.iter_mut() {
            load.second(agg, rng, sec);
        }
    }
}

#[test]
fn an_idle_site_costs_almost_nothing() {
    let mut agg = Aggregator::new(AggConfig::default(), base());
    agg.advance(base() + SignedDuration::from_secs(1));
    let mark = live();
    let site = SiteId::new();
    agg.ensure_site(site);
    for h in 1..=25 {
        agg.advance(base() + SignedDuration::from_secs(h * 3600));
    }
    let bytes = live() - mark;
    println!("idle site: {bytes} B");
    assert!(bytes <= 16 * KIB, "idle site uses {bytes} B");
    assert!(agg.site_is_idle(site));
}

#[test]
fn a_site_that_goes_idle_gives_its_memory_back() {
    let mut rng = Rng(6);
    let mut agg = Aggregator::new(AggConfig::default(), base());
    agg.advance(base() + SignedDuration::from_secs(1));
    let shared = agg.shared_heap_bytes() as isize;
    let mark = live();
    let mut load = SiteLoad::new(&mut rng, 200.0, false);
    let site = load.site;
    let routes_bytes = live() - mark;
    run_day(&mut agg, std::slice::from_mut(&mut load), &mut rng, 1);
    let busy = live() - mark - routes_bytes;
    // 25 h later everything has aged out (routes after two 12 h epochs).
    for h in 1..=25 {
        agg.advance(base() + SignedDuration::from_secs(3600 + h * 3600));
    }
    let shared_growth = agg.shared_heap_bytes() as isize - shared;
    let idle = live() - mark - routes_bytes - shared_growth;
    println!(
        "busy 1 h at 200 req/s: {busy} B; idle again: {idle} B (shared growth {shared_growth} B)"
    );
    assert!(agg.site_is_idle(site));
    assert!(idle <= 16 * KIB, "idle again but uses {idle} B");
    assert!(agg.routes(site).unwrap().routes.is_empty());
    drop(load);
}

#[test]
fn an_active_site_stays_bounded_for_ten_minutes() {
    // Short version of the 24 h run (which is ignored): 1,000 req/s for 10 min fills
    // tier 0 completely; tiers 1 and 2 grow linearly from there.
    let mut rng = Rng(7);
    let mut agg = Aggregator::new(AggConfig::default(), base());
    let mut loads = [SiteLoad::new(&mut rng, 1000.0, false)];
    let mark = live();
    let start = base().as_second();
    for sec in start..start + 600 {
        agg.advance(ts_us(sec * 1_000_000 + 1_300_000));
        loads[0].second(&mut agg, &mut rng, sec);
    }
    let total = live() - mark;
    let site = agg.site_heap_bytes(loads[0].site).unwrap() as isize;
    println!("10 min at 1,000 req/s: measured {total} B, estimated site {site} B");
    assert!(total <= MIB, "{total} B");
}

/// One closed minute of [`SiteLoad`] at 1,000 req/s, as persistence receives it.
fn minute_at_1000(micros: bool) -> MinuteDelta {
    let mut rng = Rng(11);
    let mut load = SiteLoad::new(&mut rng, 1000.0, micros);
    let m = (base().as_second() / 60 + 1) * 60;
    let mut agg = Aggregator::new(cfg(), ts_us(m * 1_000_000));
    for sec in m..m + 60 {
        agg.advance(ts_us(sec * 1_000_000 + 1_300_000));
        load.second(&mut agg, &mut rng, sec);
    }
    agg.advance(ts_us((m + 63) * 1_000_000));
    let mut deltas = agg.take_minutes();
    assert_eq!(deltas.len(), 1);
    deltas.pop().unwrap()
}

/// Rehydrates `minutes` copies of a minute at 1,000 req/s, oldest first (as T7 reads
/// `metrics_1m`), and returns the peak live bytes during the restore, the peak estimate and
/// what the minute histograms alone would take if every restored minute kept them.
fn restore_peak(micros: bool, minutes: i64) -> (isize, usize, usize) {
    let template = minute_at_1000(micros);
    let site = SiteId::new();
    let end = (base().as_second() / 60 + 1) * 60 + 2 * 86_400;
    let mut agg = Aggregator::new(AggConfig::default(), Timestamp::from_second(end).unwrap());
    let untrimmed = minutes as usize * (template.rt.encoded_len() + template.ut.encoded_len());
    let mark = live();
    let (mut peak, mut peak_estimate) = (0isize, 0usize);
    for i in (1..=minutes).rev() {
        let mut d = template.clone();
        d.site = site;
        d.start = Timestamp::from_second(end - i * 60).unwrap();
        agg.restore_minute(d);
        peak = peak.max(live() - mark);
        peak_estimate = peak_estimate.max(agg.site_heap_bytes(site).unwrap());
    }
    let w = agg.window(site, Duration::from_secs(86_400));
    assert_eq!(
        w.counters.requests,
        minutes as u64 * template.counters.requests,
        "every restored minute counts"
    );
    // The newest minute is still readable at minute resolution with its own percentiles.
    let last = Timestamp::from_second(end - 120).unwrap();
    let r = agg.range(site, last, last + SignedDuration::from_secs(60));
    assert_eq!((r.from, r.rt), (last, template.rt.percentiles()));
    println!(
        "restore {minutes} min at 1,000 req/s ({}): peak {peak} B ({:.2} MiB), estimate {peak_estimate} B; \
         every minute histogram kept would be {untrimmed} B ({:.1} MiB)",
        if micros { "µs" } else { "ms" },
        peak as f64 / MIB as f64,
        untrimmed as f64 / MIB as f64,
    );
    (peak, peak_estimate, untrimmed)
}

/// A boot rehydration of 24 h holds no more minute histograms than a running aggregator:
/// the budget and the cap apply while restoring, not only at the next `advance`.
#[test]
fn restoring_a_day_of_minutes_stays_in_the_histogram_budget() {
    let (peak, _, untrimmed) = restore_peak(false, 1440);
    assert!(peak <= MIB, "ms durations: restore peak {peak} B");
    assert!(untrimmed as isize > 2 * peak);
    // Microsecond durations: the 1 MiB cap holds, three hours are enough to pass it.
    let (peak, _, untrimmed) = restore_peak(true, 180);
    assert!(peak <= 3 * MIB, "µs durations: restore peak {peak} B");
    assert!(untrimmed as isize > 2 * peak);
}

#[test]
#[ignore = "release: cargo test -p forge-metrics --release --test agg -- --ignored"]
fn restoring_a_day_of_microsecond_minutes_stays_in_the_histogram_budget() {
    let (peak, estimate, untrimmed) = restore_peak(true, 1440);
    assert!(peak <= 3 * MIB, "µs durations: restore peak {peak} B");
    assert!(untrimmed as isize > 10 * peak);
    let error = (estimate as isize - peak).abs() as f64 / peak as f64;
    assert!(error <= 0.2, "estimate {estimate} vs measured {peak}");
}

fn day_at_1000(micros: bool) -> (isize, isize, isize) {
    let mut rng = Rng(8);
    let mark = live();
    let mut agg = Aggregator::new(AggConfig::default(), base());
    let mut loads = [SiteLoad::new(&mut rng, 1000.0, micros)];
    let routes_input = live() - mark;
    let t = Instant::now();
    run_day(&mut agg, &mut loads, &mut rng, 24);
    let total = live() - mark - routes_input;
    let shared = agg.shared_heap_bytes() as isize;
    let site = total - shared;
    let estimate = agg.site_heap_bytes(loads[0].site).unwrap() as isize;
    println!(
        "24 h at 1,000 req/s ({}): site {site} B ({:.2} MiB), shared {shared} B, estimate {estimate} B, {:?}",
        if micros {
            "µs durations"
        } else {
            "ms durations"
        },
        site as f64 / MIB as f64,
        t.elapsed()
    );
    (site, shared, estimate)
}

#[test]
#[ignore = "release: cargo test -p forge-metrics --release --test agg -- --ignored"]
fn memory_active_site_24h_at_1000_rps() {
    let (site, _, estimate) = day_at_1000(false);
    assert!(site <= 2 * MIB, "{site} B");
    let error = (estimate - site).abs() as f64 / site as f64;
    assert!(error <= 0.2, "estimate {estimate} vs measured {site}");
}

#[test]
#[ignore = "release: cargo test -p forge-metrics --release --test agg -- --ignored"]
fn memory_active_site_24h_at_1000_rps_microsecond_durations() {
    // Not the budget: microsecond durations (Caddy on Windows) make almost every request a
    // distinct value at 3 significant digits. Reported in docs/14; a regression bound.
    let (site, _, _) = day_at_1000(true);
    assert!(site <= 6 * MIB, "{site} B");
}

#[test]
#[ignore = "release: cargo test -p forge-metrics --release --test agg -- --ignored"]
fn memory_50_mixed_sites_24h() {
    let mut rng = Rng(9);
    let mark = live();
    let mut agg = Aggregator::new(AggConfig::default(), base());
    let mut loads: Vec<SiteLoad> = Vec::new();
    for (count, rate) in [(2, 1000.0), (8, 100.0), (15, 10.0), (25, 1.0 / 60.0)] {
        for _ in 0..count {
            loads.push(SiteLoad::new(&mut rng, rate, false));
        }
    }
    let input = live() - mark;
    let t = Instant::now();
    run_day(&mut agg, &mut loads, &mut rng, 24);
    let total = live() - mark - input;
    let mut per_rate: Vec<(f64, usize)> = loads
        .iter()
        .map(|l| (l.rate, agg.site_heap_bytes(l.site).unwrap()))
        .collect();
    per_rate.dedup_by(|a, b| a.0 == b.0);
    println!(
        "50 mixed sites, 24 h: {total} B ({:.1} MiB), one site per rate (estimate): {per_rate:?}, {:?}",
        total as f64 / MIB as f64,
        t.elapsed()
    );
    assert!(total <= 40 * MIB, "{total} B");
}

// ------------------------------------------------------------------------------------------
// Cost per record
// ------------------------------------------------------------------------------------------

#[test]
#[ignore = "release: cargo test -p forge-metrics --release --test agg -- --ignored"]
fn record_costs_at_most_1us() {
    let mut rng = Rng(10);
    let mut load = SiteLoad::new(&mut rng, 1000.0, false);
    // Pre-built requests: only `record` and `advance` are timed.
    let mut reqs = Vec::with_capacity(1 << 16);
    for _ in 0..1 << 16 {
        let k = load.zipf.sample(&mut rng);
        let (_, median, php) = &load.routes[k];
        let rt = rng.lognormal(*median, 0.4).div_ceil(1000) * 1000;
        reqs.push((
            k,
            rt,
            php.then(|| rt / 2),
            [200u16, 200, 200, 404, 500][rng.below(5) as usize],
        ));
    }
    let mut agg = Aggregator::new(AggConfig::default(), base());
    let n = 4_000_000u64;
    let t0 = us(base());
    let started = Instant::now();
    for i in 0..n {
        // 1,000 req/s, a clock step (tail batch) every 250 records.
        let end = t0 + i as i64 * 1000;
        if i % 250 == 0 {
            agg.advance(ts_us(end + 300_000));
        }
        let (k, rt, ut, status) = reqs[(i & 0xffff) as usize];
        let rec = &mut load.routes[k].0;
        rec.end_ts = ts_us(end);
        rec.duration_us = rt;
        rec.upstream_us = ut;
        rec.status = status;
        agg.record(load.site, rec, 1);
    }
    let per = started.elapsed().as_nanos() as f64 / n as f64;
    println!("record + advance: {per:.0} ns per record over {n} records (66 min of 1,000 req/s)");
    assert!(per <= 1000.0, "{per} ns");
}
