//! Aggregator acceptance (M2-E5-T5, docs/14 section 3.3, ADR-0048).
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
use forge_metrics::hist::HIGHEST_US;
use forge_metrics::{
    AccessRecord, AggConfig, Aggregator, LatencyHist, MinuteDelta, Percentiles, Resources, Step,
    Summary,
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
        let r = Resources {
            cpu_pct: 10.0 * (i + 1) as f32,
            mem_bytes: 100 << 20,
            approx: true,
            workers_active: i as u16,
        };
        agg.set_resources(site, t(i * 10 + 3), r);
    }
    let points = agg.series(site, t(0), t(60), Step::TenSeconds);
    assert_eq!(points.len(), 6);
    assert_eq!(points[2].resources.unwrap().cpu_pct, 30.0);
    assert_eq!(points[2].ut_sum_us, 900);
    let minute = &agg.series(site, t(0), t(60), Step::Minute)[0];
    let r = minute.resources.unwrap();
    assert_eq!(r.cpu_pct, 35.0);
    assert_eq!(r.mem_bytes, 100 << 20);
    assert!(r.approx);
    assert_eq!(r.workers_active, 5);
    // A period without requests gets a point with resources only.
    agg.set_resources(site, t(75), Resources::default());
    assert_eq!(agg.series(site, t(70), t(80), Step::TenSeconds).len(), 1);
    // Attribution sums.
    let sums = agg.sums(site, t(0), t(60));
    assert_eq!(
        (sums.requests, sums.ut_sum_us, sums.rt_sum_us),
        (6, 5400, 6000)
    );
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

fn zipf_run(seed: u64, s: f64, n: usize) {
    const ROUTES: usize = 10_000;
    let site = SiteId::new();
    let zipf = Zipf::new(ROUTES, s);
    let mut rng = Rng(seed);
    let names: Vec<String> = (0..ROUTES).map(|i| format!("/r{i}")).collect();
    let mut agg = Aggregator::new(cfg(), base());
    let mut truth = vec![0u64; ROUTES];
    let mut rec = record(us(base()), 1000, None, 200, "");
    for i in 0..n {
        let k = zipf.sample(&mut rng);
        truth[k] += 1;
        if i % 1000 == 0 {
            agg.advance(base() + SignedDuration::from_millis(i as i64));
        }
        rec.end_ts = agg.now();
        rec.route.clear();
        rec.route.push_str(&names[k]);
        rec.duration_us = 1000 + k as u64;
        agg.record(site, &rec, 1);
    }
    let report = agg.routes(site).unwrap();
    assert_eq!(report.total, n as u64);
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
    // Every reported route respects the bounds too.
    for r in &report.routes {
        let k: usize = r.route[2..].parse().unwrap();
        assert!(
            r.count >= truth[k] && r.count - r.overcount <= truth[k],
            "{}",
            r.route
        );
    }
}

#[test]
fn zipf_true_top20_routes_are_always_present() {
    for s in [0.8, 1.0, 1.2] {
        for seed in 1..=3 {
            zipf_run(seed, s, 200_000);
        }
    }
}

#[test]
#[ignore = "release: cargo test -p forge-metrics --release --test agg -- --ignored"]
fn zipf_true_top20_routes_are_always_present_1m_requests() {
    for s in [0.7, 0.8, 0.9, 1.0, 1.1, 1.2] {
        for seed in 1..=10 {
            zipf_run(seed, s, 1_000_000);
        }
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
/// Durations are logged in milliseconds like nginx's `$request_time` (and, on Windows, the
/// clock tick of Apache and Caddy too); `micros` keeps microseconds (the worst case).
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
