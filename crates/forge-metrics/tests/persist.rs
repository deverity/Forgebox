//! Persistence acceptance (M2-E5-T7, docs/14 section 3.5, ADR-0044 section 3).
//!
//! Real `Store` (in-memory SQLite with its writer thread and reader pool) driven by a simulated
//! clock: flush, rollup with exactly merged percentiles, prune, resume from a tail checkpoint
//! after a kill, rehydration across a restart, and the cost of a 50-site flush.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use forge_core::ids::SiteId;
use forge_core::store::{MetricsRepo, MetricsRow, MetricsTier, Store, StoreOptions};
use forge_metrics::hist::{FORMAT_VERSION, HIGHEST_US};
use forge_metrics::persist::{self, Persister};
use forge_metrics::{AccessRecord, AggConfig, Aggregator, Checkpoint, LatencyHist};
use hdrhistogram::Histogram;
use jiff::Timestamp;

/// 2026-01-01T00:00:00Z, aligned to the hour.
const BASE: i64 = 1_767_225_600;

fn ts(secs: i64) -> Timestamp {
    Timestamp::from_second(secs).unwrap()
}

fn cfg() -> AggConfig {
    AggConfig {
        minute_outbox: true,
        ..AggConfig::default()
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

    /// 1..=300 ms in microseconds, now and then 2 s.
    fn duration(&mut self) -> u64 {
        let n = self.next();
        if n.is_multiple_of(97) {
            2_000_000 + n % 1000
        } else {
            1000 + (n >> 8) % 299_000
        }
    }
}

fn record(end: i64, rt: u64, status: u16) -> AccessRecord {
    AccessRecord {
        end_ts: ts(end),
        host: "x.test".into(),
        status,
        duration_us: rt,
        upstream_us: Some(rt / 2),
        bytes: 100,
        method: "GET".into(),
        route: "/".into(),
    }
}

fn feed(agg: &mut Aggregator, site: SiteId, rec: &AccessRecord) {
    agg.advance(rec.end_ts);
    agg.record(site, rec, 1);
}

fn brute(values: &[u64]) -> (u64, u64, u64) {
    let mut h = Histogram::<u64>::new_with_bounds(1, HIGHEST_US, 3).unwrap();
    for &v in values {
        h.record(v).unwrap();
    }
    (
        h.value_at_quantile(0.5),
        h.value_at_quantile(0.9),
        h.value_at_quantile(0.99),
    )
}

fn rows(store: &Store, tier: MetricsTier) -> Vec<MetricsRow> {
    MetricsRepo::new(store.conn())
        .range(tier, None, 0, i64::MAX)
        .unwrap()
}

// -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_sites_two_hours_roll_up_exactly_and_prune_with_a_simulated_clock() {
    let store = Store::open_in_memory().unwrap();
    let sites = [SiteId::new(), SiteId::new(), SiteId::new()];
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut agg = Aggregator::new(cfg(), ts(BASE));
    let mut persister = Persister::new();
    // (site, hour) -> durations / statuses, the oracle.
    let mut oracle: BTreeMap<(SiteId, i64), (Vec<u64>, u64)> = BTreeMap::new();

    for minute in 0..120_i64 {
        let mut recs = Vec::new();
        for &site in &sites {
            for off in [3, 14, 25, 37, 58] {
                let rt = rng.duration();
                let status = if rng.next().is_multiple_of(10) {
                    503
                } else {
                    200
                };
                recs.push((site, record(BASE + minute * 60 + off, rt, status)));
            }
        }
        recs.sort_by_key(|(_, r)| r.end_ts);
        for (site, rec) in &recs {
            feed(&mut agg, *site, rec);
            let e = oracle
                .entry((*site, (minute * 60) / 3600 * 3600))
                .or_default();
            e.0.push(rec.duration_us);
            e.1 += u64::from(rec.status == 503);
        }
        let report = persister
            .flush(&mut agg, store.writer(), ts(BASE + (minute + 1) * 60 + 3))
            .await
            .unwrap();
        assert_eq!(report.rows, 3, "minute {minute}");
    }

    // 3 sites x 120 minutes = 360 rows, every one with both histograms.
    let minutes = rows(&store, MetricsTier::Minute);
    assert_eq!(minutes.len(), 360);
    assert!(
        minutes
            .iter()
            .all(|r| r.requests == 5 && r.format_version == FORMAT_VERSION)
    );
    assert!(
        minutes
            .iter()
            .all(|r| !r.rt_hist.is_empty() && !r.ut_hist.is_empty())
    );

    // Rollup of the two hours that ended: 6 rows, histograms merged (p99 of the hour, not an
    // average of minute p99s) and equal to the brute-force oracle.
    let now = ts(BASE + 2 * 3600 + 30);
    let rolled = persist::rollup(store.reader(), store.writer(), now)
        .await
        .unwrap();
    assert_eq!((rolled.hours, rolled.minutes), (6, 360));
    let hours = rows(&store, MetricsTier::Hour);
    assert_eq!(hours.len(), 6);
    for row in &hours {
        let (values, errors) = &oracle[&(row.site, row.start - BASE)];
        let _ = errors;
        assert_eq!(row.requests, 300);
        let hist = LatencyHist::from_v2(&row.rt_hist).unwrap();
        assert_eq!(hist.total(), 300);
        let p = hist.percentiles().unwrap();
        assert_eq!(
            (p.p50_us, p.p90_us, p.p99_us),
            brute(values),
            "{}",
            row.start
        );
        assert_eq!(row.status[4], oracle[&(row.site, row.start - BASE)].1);
        assert_eq!(row.rt_sum_us, values.iter().sum::<u64>());
    }
    // Running it again changes nothing.
    persist::rollup(store.reader(), store.writer(), now)
        .await
        .unwrap();
    assert_eq!(rows(&store, MetricsTier::Hour), hours);

    // Prune with a simulated clock: nothing is old yet; after 7 days + 1 h the first hour of
    // minutes goes (the hour rows stay); after 90 days + 2 h the hours go too.
    let none = persist::prune(store.writer(), now).await.unwrap();
    assert_eq!((none.minutes, none.hours), (0, 0));
    let week = ts(BASE + 7 * 86_400 + 3600 + 5 * 60);
    let p = persist::prune(store.writer(), week).await.unwrap();
    assert_eq!((p.minutes, p.hours), (3 * 65, 0));
    assert_eq!(rows(&store, MetricsTier::Minute).len(), 360 - 3 * 65);
    assert_eq!(rows(&store, MetricsTier::Hour).len(), 6);
    // Rolling up later does not rebuild an hour whose minutes are partly gone.
    persist::rollup(store.reader(), store.writer(), week)
        .await
        .unwrap();
    assert_eq!(rows(&store, MetricsTier::Hour), hours);
    let far = ts(BASE + 90 * 86_400 + 2 * 3600 + 1);
    let p = persist::prune(store.writer(), far).await.unwrap();
    assert_eq!(p.hours, 6);
    assert!(rows(&store, MetricsTier::Hour).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_delta_is_merged_into_the_stored_minute_exactly() {
    let store = Store::open_in_memory().unwrap();
    let site = SiteId::new();
    let mut agg = Aggregator::new(cfg(), ts(BASE));
    let mut persister = Persister::new();
    let mut all = Vec::new();
    for (off, rt) in [(5, 1000), (20, 2000), (40, 50_000)] {
        feed(&mut agg, site, &record(BASE + off, rt, 200));
        all.push(rt);
    }
    persister
        .flush(&mut agg, store.writer(), ts(BASE + 120))
        .await
        .unwrap();
    // A record for the stored minute arrives a minute late.
    agg.record(site, &record(BASE + 50, 900_000, 500), 1);
    all.push(900_000);
    let report = persister
        .flush(&mut agg, store.writer(), ts(BASE + 130))
        .await
        .unwrap();
    assert_eq!((report.rows, report.merged), (1, 1));
    let row = MetricsRepo::new(store.conn())
        .get(MetricsTier::Minute, site, BASE)
        .unwrap()
        .unwrap();
    assert_eq!((row.requests, row.status[4]), (4, 1));
    let p = LatencyHist::from_v2(&row.rt_hist)
        .unwrap()
        .percentiles()
        .unwrap();
    assert_eq!((p.p50_us, p.p90_us, p.p99_us), brute(&all));
}

/// A log of records written 2 s apart for 3 sites, read in batches of 7 lines.
struct Log {
    sites: [SiteId; 3],
    recs: Vec<(usize, AccessRecord)>,
}

impl Log {
    fn new(lines: usize) -> Self {
        let mut rng = Rng(42);
        let recs = (0..lines)
            .map(|j| (j % 3, record(BASE + 2 * j as i64, rng.duration(), 200)))
            .collect();
        Self {
            sites: [SiteId::new(), SiteId::new(), SiteId::new()],
            recs,
        }
    }

    fn checkpoint(offset: usize) -> Checkpoint {
        Checkpoint {
            creation_time: 77,
            offset: offset as u64,
            head_len: 0,
            head_hash: 0,
        }
    }

    /// Expected requests per (site, minute).
    fn expected(&self) -> BTreeMap<(SiteId, i64), u64> {
        let mut m = BTreeMap::new();
        for (i, r) in &self.recs {
            *m.entry((self.sites[*i], r.end_ts.as_second() / 60 * 60))
                .or_default() += 1;
        }
        m
    }
}

/// Reads the log from the stored checkpoint, flushing after every batch, until `stop_after`
/// batches have been processed (`None`: to the end). Returns the offset reached and whether
/// it ended because of the kill point. Dropping the aggregator and persister is the kill.
async fn run(
    log: &Log,
    store: &Store,
    stop_after: Option<usize>,
    key: &str,
) -> (usize, usize, bool) {
    let stored = persist::load_checkpoints(store.reader()).await.unwrap();
    let resume = stored.iter().find(|(k, _)| k == key).map(|(_, r)| *r);
    let start = resume.map_or(0, |r| r.checkpoint.offset as usize);
    let now = log.recs.get(start).map_or(ts(BASE), |(_, r)| r.end_ts);
    let mut agg = Aggregator::new(cfg(), now);
    persist::rehydrate(&mut agg, store.reader(), now, persist::REHYDRATE_WINDOW)
        .await
        .unwrap();
    let mut persister = Persister::new();
    let mut offset = start;
    let mut batches = 0;
    while offset < log.recs.len() {
        if stop_after.is_some_and(|n| batches == n) {
            return (start, offset, true);
        }
        let end = (offset + 7).min(log.recs.len());
        for (i, rec) in &log.recs[offset..end] {
            // Re-read records that are in the stored minutes already are not counted twice.
            if resume.is_none_or(|r| r.keeps(rec)) {
                feed(&mut agg, log.sites[*i], rec);
            }
        }
        offset = end;
        let newest = log.recs[end - 1].1.end_ts;
        persister.observe(key, Some(Log::checkpoint(offset)), Some(newest));
        persister
            .flush(&mut agg, store.writer(), newest)
            .await
            .unwrap();
        batches += 1;
    }
    // Close the last minutes.
    let last = log.recs[offset - 1].1.end_ts.as_second();
    persister
        .flush(&mut agg, store.writer(), ts(last + 300))
        .await
        .unwrap();
    (start, offset, false)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_between_flushes_then_resume_gives_exact_per_minute_counts() {
    let log = Log::new(360);
    let want = log.expected();
    for kill_after in [3, 11, 24, 40] {
        let store = Store::open_in_memory().unwrap();
        let key = "access.log";
        let (_, reached, killed) = run(&log, &store, Some(kill_after), key).await;
        assert!(killed);
        // The stored position is behind what was consumed: the unflushed part is re-read.
        let (resumed_from, _, _) = {
            let stored = persist::load_checkpoints(store.reader()).await.unwrap();
            let from = stored
                .first()
                .map_or(0, |(_, r)| r.checkpoint.offset as usize);
            (from, 0, 0)
        };
        assert!(
            resumed_from < reached,
            "kill {kill_after}: {resumed_from} < {reached}"
        );
        // Resume, kill again once in the middle of the resumed run, resume to the end.
        let (start, _, killed_again) = run(&log, &store, Some(5), key).await;
        assert_eq!(start, resumed_from);
        assert!(killed_again);
        let (start, end, killed) = run(&log, &store, None, key).await;
        assert!(
            !killed && end == 360 && start > 0,
            "kill {kill_after}: from {start}"
        );

        let mut got = BTreeMap::new();
        for r in rows(&store, MetricsTier::Minute) {
            got.insert((r.site, r.start), r.requests);
        }
        let want_rows = want.len();
        assert_eq!(got.len(), want_rows, "kill {kill_after}");
        assert_eq!(got, want, "kill {kill_after}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rehydrated_24h_window_equals_the_one_before_the_restart() {
    let store = Store::open_in_memory().unwrap();
    let sites = [SiteId::new(), SiteId::new()];
    let mut rng = Rng(7);
    let mut agg = Aggregator::new(cfg(), ts(BASE));
    let mut persister = Persister::new();
    // 26 hours, 3 records per site per minute: the first hours are older than the minute
    // histograms kept, so the 24 h answer also exercises the hour tier.
    let minutes = 26 * 60;
    for minute in 0..minutes {
        for &site in &sites {
            for off in [4, 23, 51] {
                let status = if rng.next().is_multiple_of(13) {
                    404
                } else {
                    200
                };
                feed(
                    &mut agg,
                    site,
                    &record(BASE + minute * 60 + off, rng.duration(), status),
                );
            }
        }
        if minute % 15 == 14 {
            persister
                .flush(&mut agg, store.writer(), ts(BASE + (minute + 1) * 60 + 3))
                .await
                .unwrap();
        }
    }
    // Everything closed and stored; the clock moves 5 quiet minutes on.
    let now = ts(BASE + minutes * 60 + 5 * 60);
    persister
        .flush(&mut agg, store.writer(), now)
        .await
        .unwrap();

    let before: Vec<_> = sites
        .iter()
        .map(|&s| {
            (
                agg.window(s, Duration::from_secs(24 * 3600)),
                agg.window(s, Duration::from_secs(3600)),
                agg.range(s, ts(BASE + 4 * 3600), ts(BASE + 9 * 3600 + 17 * 60)),
            )
        })
        .collect();

    // "Restart": a new aggregator at the same clock, filled from SQLite only.
    let mut again = Aggregator::new(cfg(), now);
    let report = persist::rehydrate(&mut again, store.reader(), now, persist::REHYDRATE_WINDOW)
        .await
        .unwrap();
    assert_eq!(report.skipped, 0);
    assert_eq!(report.restored, 2 * (26 * 60 - 125));
    for (i, &s) in sites.iter().enumerate() {
        let w24 = again.window(s, Duration::from_secs(24 * 3600));
        assert!(w24.counters.requests > 0);
        assert_eq!(w24, before[i].0, "24 h window of site {i}");
        assert_eq!(
            again.window(s, Duration::from_secs(3600)),
            before[i].1,
            "1 h window"
        );
        assert_eq!(
            again.range(s, ts(BASE + 4 * 3600), ts(BASE + 9 * 3600 + 17 * 60)),
            before[i].2,
            "range"
        );
    }
    // The rehydrated minutes are not sent to the outbox again.
    assert!(again.take_minutes().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rehydrate_skips_rows_it_cannot_read_and_keeps_going() {
    let store = Store::open_in_memory().unwrap();
    let site = SiteId::new();
    let good = MetricsRow {
        site,
        start: BASE,
        requests: 3,
        status: [0, 3, 0, 0, 0],
        bytes: 30,
        rt_sum_us: 6000,
        ut_sum_us: 0,
        ut_count: 0,
        sampled: false,
        rt_hist: LatencyHist::from_values([1000, 2000, 3000])
            .to_v2()
            .unwrap(),
        ut_hist: Vec::new(),
        format_version: FORMAT_VERSION,
    };
    let future_codec = MetricsRow {
        start: BASE + 60,
        format_version: FORMAT_VERSION + 1,
        ..good.clone()
    };
    let garbage = MetricsRow {
        start: BASE + 120,
        rt_hist: vec![0xde, 0xad],
        ..good.clone()
    };
    let rows_in = vec![good.clone(), future_codec, garbage];
    store
        .writer()
        .execute(move |conn| {
            let repo = MetricsRepo::new(conn);
            for r in &rows_in {
                repo.put(MetricsTier::Minute, r)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let now = ts(BASE + 600);
    let mut agg = Aggregator::new(cfg(), now);
    let report = persist::rehydrate(&mut agg, store.reader(), now, persist::REHYDRATE_WINDOW)
        .await
        .unwrap();
    assert_eq!((report.restored, report.skipped), (1, 2));
    assert_eq!(
        agg.window(site, Duration::from_secs(3600))
            .counters
            .requests,
        3
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_write_keeps_minutes_and_positions_for_the_next_flush() {
    let store = Store::open_in_memory().unwrap();
    let writer = store.writer().clone();
    let site = SiteId::new();
    let mut agg = Aggregator::new(cfg(), ts(BASE));
    let mut persister = Persister::new();
    feed(&mut agg, site, &record(BASE + 10, 5000, 200));
    persister.observe("a.log", Some(Log::checkpoint(10)), Some(ts(BASE + 10)));
    store.close().unwrap();
    let err = persister
        .flush(&mut agg, &writer, ts(BASE + 130))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "internal");
    assert_eq!(persister.carried(), 1);
    // The store comes back (a new one stands for the reopened file); the same persister
    // writes the carried minute and the held position.
    let store = Store::open_in_memory().unwrap();
    let report = persister
        .flush(&mut agg, store.writer(), ts(BASE + 140))
        .await
        .unwrap();
    // The failed batch is re-sent first, then the newer one (the position is rewritten with
    // the newer edge).
    assert_eq!((report.rows, report.checkpoints), (1, 2));
    assert_eq!(persister.carried(), 0);
    let tails = persist::load_checkpoints(store.reader()).await.unwrap();
    assert_eq!(tails[0].1.checkpoint.offset, 10);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ranges_over_24_hours_come_from_sqlite_hours_then_minutes() {
    let store = Store::open_in_memory().unwrap();
    let site = SiteId::new();
    let now_s = BASE + 20 * 86_400 + 1800;
    let row = |start: i64, rt: &[u64]| MetricsRow {
        site,
        start,
        requests: rt.len() as u64,
        status: [0, rt.len() as u64, 0, 0, 0],
        bytes: 1,
        rt_sum_us: rt.iter().sum(),
        ut_sum_us: 0,
        ut_count: 0,
        sampled: false,
        rt_hist: LatencyHist::from_values(rt.iter().copied())
            .to_v2()
            .unwrap(),
        ut_hist: Vec::new(),
        format_version: FORMAT_VERSION,
    };
    // Hours 10 days back and 8 days back; minutes in the last 2 days (inside the 7 days).
    let h1 = (now_s - 10 * 86_400) / 3600 * 3600;
    let h2 = (now_s - 8 * 86_400) / 3600 * 3600;
    let m1 = (now_s - 86_400) / 60 * 60;
    let m2 = m1 + 60;
    let (r1, r2, r3, r4) = (
        row(h1, &[1000; 10]),
        row(h2, &[2000; 20]),
        row(m1, &[3000; 3]),
        row(m2, &[400_000; 1]),
    );
    store
        .writer()
        .execute(move |conn| {
            let repo = MetricsRepo::new(conn);
            repo.put(MetricsTier::Hour, &r1)?;
            repo.put(MetricsTier::Hour, &r2)?;
            repo.put(MetricsTier::Minute, &r3)?;
            repo.put(MetricsTier::Minute, &r4)
        })
        .await
        .unwrap();
    let now = ts(now_s);
    let all = persist::query_range(store.reader(), site, ts(h1 + 5), now, now)
        .await
        .unwrap();
    assert_eq!(all.counters.requests, 10 + 20 + 3 + 1);
    assert_eq!(all.from, ts(h1));
    assert!(all.rt.unwrap().p99_us >= 400_000);
    // Only the old hours.
    let old = persist::query_range(store.reader(), site, ts(h1), ts(h2 + 3600), now)
        .await
        .unwrap();
    assert_eq!(old.counters.requests, 30);
    assert_eq!((old.from, old.to), (ts(h1), ts(h2 + 3600)));
    // Only the recent minutes, aligned outward to the minute.
    let recent = persist::query_range(store.reader(), site, ts(m1 + 7), ts(m2 + 7), now)
        .await
        .unwrap();
    assert_eq!(recent.counters.requests, 4);
    assert_eq!((recent.from, recent.to), (ts(m1), ts(m2 + 60)));
    // Nothing there.
    let empty = persist::query_range(store.reader(), site, ts(BASE), ts(BASE + 600), now)
        .await
        .unwrap();
    assert_eq!(empty.counters.requests, 0);
    assert!(empty.rt.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fifty_site_flush_costs_the_writer_at_most_5_ms() {
    let store = Store::open_in_memory().unwrap();
    let sites: Vec<SiteId> = (0..50).map(|_| SiteId::new()).collect();
    let mut rng = Rng(99);
    let mut agg = Aggregator::new(cfg(), ts(BASE));
    let mut persister = Persister::new();
    let mut times = Vec::new();
    for minute in 0..6_i64 {
        let mut recs = Vec::new();
        for &site in &sites {
            for k in 0..200 {
                let rt = (1 + rng.next() % 300) * 1000;
                recs.push((site, record(BASE + minute * 60 + k % 58, rt, 200)));
            }
        }
        recs.sort_by_key(|(_, r)| r.end_ts);
        for (site, rec) in &recs {
            feed(&mut agg, *site, rec);
        }
        persister.observe(
            "a.log",
            Some(Log::checkpoint(minute as usize)),
            Some(ts(BASE + minute * 60 + 58)),
        );
        let report = persister
            .flush(&mut agg, store.writer(), ts(BASE + (minute + 1) * 60 + 3))
            .await
            .unwrap();
        assert_eq!((report.rows, report.checkpoints), (50, 1));
        times.push(report.writer_time);
    }
    times.sort();
    let median = times[times.len() / 2];
    eprintln!("50-site flush, writer time: {times:?} (median {median:?})");
    assert!(median <= Duration::from_millis(5), "median {median:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn flush_and_rehydrate_never_stall_the_runtime() {
    // One-thread runtime: any SQLite call or long CPU burst made on it shows as a gap in the
    // heartbeat. Writes run on the store writer thread, reads and encoding in spawn_blocking,
    // and the restore loop yields.
    let store = Store::open_in_memory().unwrap();
    let sites: Vec<SiteId> = (0..50).map(|_| SiteId::new()).collect();
    let mut rng = Rng(5);
    let mut agg = Aggregator::new(cfg(), ts(BASE));
    let mut persister = Persister::new();
    let worst = Arc::new(AtomicU64::new(0));
    let beat = {
        let worst = worst.clone();
        tokio::spawn(async move {
            let mut last = Instant::now();
            loop {
                tokio::time::sleep(Duration::from_millis(1)).await;
                let gap = u64::try_from(last.elapsed().as_micros()).unwrap();
                worst.fetch_max(gap, Ordering::Relaxed);
                last = Instant::now();
            }
        })
    };
    let mut worst_flush = 0;
    for minute in 0..30_i64 {
        for &site in &sites {
            for k in 0..4 {
                let rt = (1 + rng.next() % 300) * 1000;
                feed(
                    &mut agg,
                    site,
                    &record(BASE + minute * 60 + 10 + k, rt, 200),
                );
            }
        }
        // Closing the minutes is the aggregator's own CPU work, not persistence: do it first,
        // let the heartbeat settle, then measure only the flush.
        let now = ts(BASE + (minute + 1) * 60 + 3);
        agg.advance(now);
        tokio::time::sleep(Duration::from_millis(5)).await;
        worst.store(0, Ordering::Relaxed);
        persister
            .flush(&mut agg, store.writer(), now)
            .await
            .unwrap();
        worst_flush = worst_flush.max(worst.load(Ordering::Relaxed));
    }
    let now = ts(BASE + 31 * 60);
    let mut again = Aggregator::new(cfg(), now);
    tokio::time::sleep(Duration::from_millis(5)).await;
    worst.store(0, Ordering::Relaxed);
    let report = persist::rehydrate(&mut again, store.reader(), now, persist::REHYDRATE_WINDOW)
        .await
        .unwrap();
    let worst_rehydrate = worst.load(Ordering::Relaxed);
    beat.abort();
    assert_eq!(report.restored, 50 * 30);
    eprintln!("worst runtime gap: flush {worst_flush} us, rehydrate {worst_rehydrate} us");
    assert!(
        worst_flush < 50_000,
        "flush stalled the runtime {worst_flush} us"
    );
    assert!(
        worst_rehydrate < 50_000,
        "rehydrate stalled the runtime {worst_rehydrate} us"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_minutes_never_block_rollup_prune_or_queries() {
    let store = Store::open_in_memory().unwrap();
    let site = SiteId::new();
    let good = |start: i64| MetricsRow {
        site,
        start,
        requests: 3,
        status: [0, 3, 0, 0, 0],
        bytes: 30,
        rt_sum_us: 6000,
        ut_sum_us: 0,
        ut_count: 0,
        sampled: false,
        rt_hist: LatencyHist::from_values([1000, 2000, 3000])
            .to_v2()
            .unwrap(),
        ut_hist: Vec::new(),
        format_version: FORMAT_VERSION,
    };
    // The first row of the site is the garbage one: it must not be copied unchecked.
    let garbage = MetricsRow {
        rt_hist: vec![0xde, 0xad],
        ..good(BASE)
    };
    let future_codec = MetricsRow {
        format_version: FORMAT_VERSION + 1,
        ..good(BASE + 120)
    };
    let rows_in = vec![garbage, good(BASE + 60), future_codec];
    store
        .writer()
        .execute(move |conn| {
            let repo = MetricsRepo::new(conn);
            for r in &rows_in {
                repo.put(MetricsTier::Minute, r)?;
            }
            Ok(())
        })
        .await
        .unwrap();

    let mut agg = Aggregator::new(cfg(), ts(BASE + 3630));
    let mut persister = Persister::new();
    let report = persister
        .tick(&mut agg, store.reader(), store.writer(), ts(BASE + 3630))
        .await
        .unwrap();
    let rollup = report.rollup.unwrap();
    assert_eq!((rollup.hours, rollup.skipped), (1, 2));
    let hours = rows(&store, MetricsTier::Hour);
    assert_eq!(hours.len(), 1);
    assert_eq!(hours[0].requests, 3);

    // A range holding the bad rows still answers, without them.
    let now = ts(BASE + 3630);
    let s = persist::query_range(store.reader(), site, ts(BASE), ts(BASE + 600), now)
        .await
        .unwrap();
    assert_eq!(s.counters.requests, 3);

    // 8 days later the tick prunes the bad minutes too.
    let later = ts(BASE + 8 * 86_400 + 30);
    let report = persister
        .tick(&mut agg, store.reader(), store.writer(), later)
        .await
        .unwrap();
    assert_eq!(report.prune.unwrap().minutes, 3);
    assert!(rows(&store, MetricsTier::Minute).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timed_out_flush_that_still_commits_is_not_applied_twice() {
    // The writer reports Timeout ("outcome unknown") while the operation keeps running and
    // commits. The retry must not add the same minutes again (docs/14 section 3.5).
    let store = Store::open_in_memory().unwrap();
    let writer = store.writer().clone();
    let sites: Vec<SiteId> = (0..100).map(|_| SiteId::new()).collect();
    let mut agg = Aggregator::new(
        AggConfig {
            minute_outbox_cap: 10_000,
            ..cfg()
        },
        ts(BASE),
    );
    // The timeout is generous for the writer to pick the operation up and short against the
    // 4,000 rows it then writes.
    let mut persister = Persister::new().with_write_timeout(Duration::from_millis(30));
    for minute in 0..40_i64 {
        for &site in &sites {
            feed(
                &mut agg,
                site,
                &record(BASE + minute * 60 + 5, 1000 + minute as u64, 200),
            );
            feed(&mut agg, site, &record(BASE + minute * 60 + 9, 90_000, 500));
        }
    }
    persister.observe(
        "a.log",
        Some(Log::checkpoint(1)),
        Some(ts(BASE + 39 * 60 + 9)),
    );
    let err = persister
        .flush(&mut agg, &writer, ts(BASE + 41 * 60))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "timeout", "{err}");
    assert!(persister.carried() > 0);
    // Wait for the writer to finish what the caller stopped waiting for.
    writer.flush(Duration::from_secs(60)).await.unwrap();
    let committed = rows(&store, MetricsTier::Minute).len();
    assert_eq!(
        committed, 4000,
        "the timed out operation should have committed"
    );

    let report = persister
        .flush(&mut agg, &writer, ts(BASE + 41 * 60 + 5))
        .await
        .unwrap();
    assert_eq!(report.replayed, 1);
    assert_eq!((report.rows, report.merged), (0, 0));
    assert_eq!(persister.carried(), 0);
    let stored = rows(&store, MetricsTier::Minute);
    assert_eq!(stored.len(), 4000);
    for row in &stored {
        assert_eq!((row.requests, row.status[1], row.status[4]), (2, 1, 1));
        assert_eq!(
            row.rt_sum_us,
            1000 + (row.start - BASE) as u64 / 60 + 90_000
        );
        assert_eq!(LatencyHist::from_v2(&row.rt_hist).unwrap().total(), 2);
        assert_eq!(LatencyHist::from_v2(&row.ut_hist).unwrap().total(), 2);
    }
    // A third flush has nothing left to do.
    let again = persister
        .flush(&mut agg, &writer, ts(BASE + 42 * 60))
        .await
        .unwrap();
    assert_eq!((again.rows, again.replayed), (0, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rolling_up_50_sites_over_3_hours_keeps_each_writer_operation_short() {
    // The store-read timeout bounds the SQL of a chunk (tens of ms for 9,000 rows), never the
    // histogram merge that follows it (seconds in a debug build): a tight read timeout keeps that
    // split honest. Before gate B3 the merge ran inside the read and hit the timeout under load.
    let store = Store::open_in_memory_with(StoreOptions {
        read_timeout: Duration::from_secs(1),
        ..StoreOptions::default()
    })
    .unwrap();
    let sites: Vec<SiteId> = (0..50).map(|_| SiteId::new()).collect();
    let mut rng = Rng(7);
    let mut agg = Aggregator::new(cfg(), ts(BASE));
    let mut persister = Persister::new();
    for minute in 0..180_i64 {
        for &site in &sites {
            for k in 0..5 {
                feed(
                    &mut agg,
                    site,
                    &record(BASE + minute * 60 + 10 * k, rng.duration(), 200),
                );
            }
        }
        persister
            .flush(&mut agg, store.writer(), ts(BASE + (minute + 1) * 60 + 3))
            .await
            .unwrap();
    }
    assert_eq!(rows(&store, MetricsTier::Minute).len(), 50 * 180);

    let now = ts(BASE + 3 * 3600 + 30);
    let mut worst = Vec::new();
    for run in 0..6 {
        let report = persist::rollup(store.reader(), store.writer(), now)
            .await
            .unwrap();
        // The first run builds the 3 hours; later ones rebuild the newest rolled hour and the
        // one before it.
        let (hours, ops) = if run == 0 { (150, 3) } else { (100, 2) };
        assert_eq!((report.hours, report.writer_ops), (hours, ops));
        assert_eq!(report.minutes, hours * 60);
        worst.push(report.writer_time_max);
    }
    worst.sort();
    let median = worst[worst.len() / 2];
    eprintln!("rollup 50 sites x 3 h, longest writer operation: {worst:?} (median {median:?})");
    assert!(median <= Duration::from_millis(5), "median {median:?}");

    let hours = rows(&store, MetricsTier::Hour);
    assert_eq!(hours.len(), 150);
    assert!(hours.iter().all(|h| h.requests == 300));
}
