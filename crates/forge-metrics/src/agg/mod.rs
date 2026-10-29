//! Per-site aggregation of access records (docs/14 sections 3 and 3.3, ADR-0048).
//!
//! [`Aggregator`] is single-owner, synchronous code: the metrics actor (M2-E5-T8) owns one,
//! feeds it the records of the tailers ([`Aggregator::record`]), moves its clock
//! ([`Aggregator::advance`]) and answers queries from it. Nothing in here blocks, allocates
//! per idle site or depends on the wall clock: time comes from the caller.
//!
//! # Time
//!
//! Records are keyed by their `end_ts`, never by arrival. The aggregator clock is the
//! largest `now` given to [`Aggregator::advance`] (it never goes back: after a backward jump
//! of the system clock, records land in tier 2 as late ones until the clock passes its old
//! value). A record from the future (`end_ts > now`) is clamped to `now`. A 2 s bucket
//! accepts records until `grace` (2 s) after its end; a record for a bucket that already
//! closed is *late* and only goes to tier 2 (its minute and hour), so the 2 s and 10 s tiers
//! never change after they are shown. Records older than the retention (24 h) are dropped.
//!
//! # Tiers
//!
//! See [`tiers`](self::tiers): open 2 s buckets, tier 0 (2 s, 10 min, rt/ut histograms),
//! tier 1 (10 s scalar points, 24 h), tier 2 (minutes for 24 h with histograms for at least
//! 1 h within a per-site budget, and hours with histograms for 24 h). Windows and ranges are
//! answered with whole buckets, aligned outward, from the finest tier that has the span
//! ([`Summary::from`]/[`Summary::to`] say what was covered). Merges are exact.

mod errors;
mod routes;
mod tiers;

use std::collections::HashMap;
use std::time::Duration;

use forge_core::ids::SiteId;
use jiff::Timestamp;

use crate::hist::{Dense, LATENCY, LatencyHist, Percentiles};
use crate::record::AccessRecord;
use errors::ErrorRings;
use routes::RouteTable;
use tiers::{BufPool, Clock, Counts, Ctx, Delta, Limits, Sample, SiteTiers};

pub use errors::{RecentError, RecentErrors};
pub use routes::{RouteStats, RoutesReport};

/// Width of a tier 0 bucket, in seconds.
pub(crate) const T0: i64 = 2;
/// Width of a tier 1 point.
pub(crate) const T1: i64 = 10;
/// Width of a tier 2 minute.
pub(crate) const MINUTE: i64 = 60;
/// Width of an hour bucket.
pub(crate) const HOUR: i64 = 3600;

#[inline]
pub(crate) fn floor_to(s: i64, w: i64) -> i64 {
    s.div_euclid(w) * w
}

#[inline]
pub(crate) fn ceil_to(s: i64, w: i64) -> i64 {
    floor_to(s.saturating_add(w - 1), w)
}

/// Whole seconds of a timestamp, rounded down.
#[inline]
fn ts_floor(ts: Timestamp) -> i64 {
    let s = ts.as_second();
    if ts.subsec_nanosecond() < 0 { s - 1 } else { s }
}

/// Whole seconds of a timestamp, rounded up.
#[inline]
fn ts_ceil(ts: Timestamp) -> i64 {
    let s = ts.as_second();
    if ts.subsec_nanosecond() > 0 { s + 1 } else { s }
}

pub(crate) fn secs_to_ts(s: i64) -> Timestamp {
    Timestamp::from_second(s).unwrap_or(if s < 0 {
        Timestamp::MIN
    } else {
        Timestamp::MAX
    })
}

fn secs(d: Duration) -> i64 {
    i64::try_from(d.as_secs()).unwrap_or(i64::MAX / 4)
}

/// Knobs of the aggregator. The defaults are the v1 design (docs/14 section 3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggConfig {
    /// How long after its end a 2 s bucket still accepts records (whole seconds).
    pub grace: Duration,
    /// How long tier 0 (2 s buckets) is kept; at least 2 min.
    pub tier0_span: Duration,
    /// How long tiers 1 and 2, the recent errors and the routes are kept; at least 1 h.
    pub retention: Duration,
    /// Minute histograms younger than this are always kept.
    pub minute_hist_keep: Duration,
    /// Bytes of minute histograms a site may keep beyond `minute_hist_keep`.
    pub minute_hist_budget: usize,
    /// Hard cap of a site's minute histograms, `minute_hist_keep` included: past it the
    /// oldest go even if younger than `minute_hist_keep` (ranges then align to the hour).
    /// Only reached by microsecond-resolution logs at hundreds of requests per second.
    pub minute_hist_cap: usize,
    /// Routes monitored per site (space-saving K).
    pub routes_k: usize,
    /// Length of a route-table generation (a report spans one to two of them).
    pub route_epoch: Duration,
    /// Entries in each recent-errors ring (5xx and 4xx).
    pub errors_kept: usize,
    /// Keep closed minutes and late deltas for [`Aggregator::take_minutes`] (persistence).
    pub minute_outbox: bool,
    /// Deltas kept when nobody takes them (a stalled writer); the oldest are dropped and
    /// counted in [`AggStats::outbox_dropped`].
    pub minute_outbox_cap: usize,
}

impl Default for AggConfig {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(2),
            tier0_span: Duration::from_secs(10 * 60),
            retention: Duration::from_secs(24 * 3600),
            minute_hist_keep: Duration::from_secs(3600),
            minute_hist_budget: 128 * 1024,
            minute_hist_cap: 1024 * 1024,
            routes_k: 100,
            route_epoch: Duration::from_secs(12 * 3600),
            errors_kept: 50,
            minute_outbox: false,
            minute_outbox_cap: 8192,
        }
    }
}

/// [`AggConfig`] in seconds, with the minimums applied.
#[derive(Debug, Clone)]
struct Cfg {
    grace: i64,
    tier0_span: i64,
    retention: i64,
    minute_hist_keep: i64,
    minute_hist_budget: usize,
    minute_hist_cap: usize,
    routes_k: usize,
    route_epoch: i64,
    errors_kept: usize,
    outbox: bool,
    outbox_cap: usize,
}

impl Cfg {
    fn new(c: &AggConfig) -> Self {
        let retention = secs(c.retention).max(HOUR);
        Self {
            grace: secs(c.grace).min(MINUTE),
            tier0_span: secs(c.tier0_span).max(2 * MINUTE),
            retention,
            minute_hist_keep: secs(c.minute_hist_keep).min(retention),
            minute_hist_budget: c.minute_hist_budget,
            minute_hist_cap: c.minute_hist_cap.max(c.minute_hist_budget),
            routes_k: c.routes_k.min(usize::from(u16::MAX)),
            route_epoch: secs(c.route_epoch).max(MINUTE),
            errors_kept: c.errors_kept,
            outbox: c.minute_outbox,
            outbox_cap: c.minute_outbox_cap,
        }
    }
}

/// Request counts, bytes and time sums.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// Requests (sum of `status`), scaled by the sampling weight.
    pub requests: u64,
    /// Requests by status class: 1xx, 2xx, 3xx, 4xx, 5xx.
    pub status: [u64; 5],
    /// Response body bytes.
    pub bytes: u64,
    /// Sum of the total durations, in microseconds.
    pub rt_sum_us: u64,
    /// Sum of the upstream durations, in microseconds (attribution of shared pools, T6).
    pub ut_sum_us: u64,
    /// Requests that went to an upstream.
    pub ut_count: u64,
}

impl Counters {
    /// 4xx responses.
    pub fn err4xx(&self) -> u64 {
        self.status[3]
    }

    /// 5xx responses.
    pub fn err5xx(&self) -> u64 {
        self.status[4]
    }
}

/// Totals of a window or range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// Start of the span actually covered (aligned outward to whole buckets).
    pub from: Timestamp,
    /// End of the span covered, at most the aggregator clock.
    pub to: Timestamp,
    /// Counts in the span.
    pub counters: Counters,
    /// Percentiles of the total duration (`None` without requests).
    pub rt: Option<Percentiles>,
    /// Percentiles of the upstream (PHP) duration (`None` without upstream requests).
    pub ut: Option<Percentiles>,
    /// Some of the records stand for several lines (adaptive sampling of the tailer).
    pub sampled: bool,
}

/// CPU/RAM attributed to a site for a 10 s period (M2-E5-T6), stored in its tier 1 point.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Resources {
    /// CPU %, normalized by the logical CPUs (Task Manager style).
    pub cpu_pct: f32,
    /// Memory, in bytes (stored in KiB).
    pub mem_bytes: u64,
    /// Apportioned from a shared pool (shown with "≈").
    pub approx: bool,
    /// Workers serving the site.
    pub workers_active: u16,
}

/// Step of a series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    /// Tier 0 buckets (the last 10 min).
    TwoSeconds,
    /// Tier 1 points (24 h); trailing periods not closed yet come from tier 0.
    TenSeconds,
    /// Tier 2 minutes (24 h); open minutes come from tier 0 and their late records.
    Minute,
    /// Hours (24 h), exact.
    Hour,
}

impl Step {
    /// Width in seconds.
    pub fn seconds(self) -> i64 {
        match self {
            Step::TwoSeconds => T0,
            Step::TenSeconds => T1,
            Step::Minute => MINUTE,
            Step::Hour => HOUR,
        }
    }
}

/// One point of a series. Only steps with data are returned (empty steps are gaps).
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesPoint {
    /// Start of the step.
    pub start: Timestamp,
    /// Requests.
    pub requests: u64,
    /// 4xx responses.
    pub err4xx: u64,
    /// 5xx responses.
    pub err5xx: u64,
    /// Response body bytes.
    pub bytes: u64,
    /// Sum of total durations, in microseconds.
    pub rt_sum_us: u64,
    /// Sum of upstream durations, in microseconds.
    pub ut_sum_us: u64,
    /// Total-duration percentiles.
    pub rt: Option<Percentiles>,
    /// Upstream-duration percentiles (not kept by tier 1 points: `None` at 10 s).
    pub ut: Option<Percentiles>,
    /// CPU/RAM (10 s points; minutes average theirs).
    pub resources: Option<Resources>,
    /// Some records stand for several lines.
    pub sampled: bool,
}

/// A closed minute, or late records of a minute that already closed, for persistence
/// (enable with [`AggConfig::minute_outbox`], drain with [`Aggregator::take_minutes`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinuteDelta {
    /// The site.
    pub site: SiteId,
    /// Start of the minute.
    pub start: Timestamp,
    /// Counts to add to the minute.
    pub counters: Counters,
    /// Some records stand for several lines.
    pub sampled: bool,
    /// Total-duration histogram to merge into the minute.
    pub rt: LatencyHist,
    /// Upstream-duration histogram to merge into the minute.
    pub ut: LatencyHist,
    /// `true`: the minute as it closed. `false`: late records to merge into it. Either way
    /// adding the delta to what is stored is exact.
    pub complete: bool,
}

/// Counters of the aggregator itself (weighted record counts).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AggStats {
    /// Records accepted.
    pub records: u64,
    /// Records whose 2 s bucket had closed (tier 2 only).
    pub late: u64,
    /// Records from the future, clamped to the clock.
    pub clamped: u64,
    /// Records older than the retention, dropped.
    pub dropped: u64,
    /// `advance` calls with a clock older than the aggregator's (ignored).
    pub clock_back: u64,
    /// Minute deltas dropped because the outbox was full.
    pub outbox_dropped: u64,
}

struct SiteAgg {
    tiers: SiteTiers,
    routes: RouteTable,
    errors: ErrorRings,
    last_request: Option<Timestamp>,
}

impl SiteAgg {
    fn new(now: i64) -> Self {
        Self {
            tiers: SiteTiers::new(),
            routes: RouteTable::new(now),
            errors: ErrorRings::default(),
            last_request: None,
        }
    }

    fn heap_bytes(&self) -> usize {
        size_of::<Self>()
            + self.tiers.heap_bytes()
            + self.routes.heap_bytes()
            + self.errors.heap_bytes()
    }
}

/// The per-site metrics aggregator (see the module docs).
pub struct Aggregator {
    cfg: Cfg,
    sites: HashMap<SiteId, Box<SiteAgg>>,
    now: Timestamp,
    now_s: i64,
    clock: Clock,
    dense: Option<Box<Dense>>,
    pool: BufPool,
    outbox: Vec<Delta>,
    outbox_sites: Vec<SiteId>,
    stats: AggStats,
}

impl std::fmt::Debug for Aggregator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Aggregator")
            .field("now", &self.now)
            .field("sites", &self.sites.len())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Aggregator {
    /// An aggregator whose clock starts at `now`.
    pub fn new(config: AggConfig, now: Timestamp) -> Self {
        let cfg = Cfg::new(&config);
        let now_s = ts_floor(now);
        let edge = now_s - cfg.grace;
        Self {
            clock: Clock {
                t0_floor: floor_to(edge, T0),
                next_period: floor_to(edge, T1),
                next_minute: floor_to(edge, MINUTE),
            },
            cfg,
            sites: HashMap::new(),
            now,
            now_s,
            dense: None,
            pool: BufPool::default(),
            outbox: Vec::new(),
            outbox_sites: Vec::new(),
            stats: AggStats::default(),
        }
    }

    /// The aggregator clock.
    pub fn now(&self) -> Timestamp {
        self.now
    }

    /// Counters of the aggregator.
    pub fn stats(&self) -> AggStats {
        self.stats
    }

    /// Registers a site with no data (it costs a few hundred bytes).
    pub fn ensure_site(&mut self, site: SiteId) {
        let now = self.now_s;
        self.sites
            .entry(site)
            .or_insert_with(|| Box::new(SiteAgg::new(now)));
    }

    /// Forgets a site and everything about it.
    pub fn remove_site(&mut self, site: SiteId) {
        self.sites.remove(&site);
    }

    /// Sites known (registered or with data).
    pub fn sites(&self) -> impl Iterator<Item = SiteId> + '_ {
        self.sites.keys().copied()
    }

    /// Adds one record of `site` that stands for `weight` lines (the tailer batch weight).
    ///
    /// Call [`Self::advance`] with the current time before each batch.
    pub fn record(&mut self, site: SiteId, rec: &AccessRecord, weight: u32) {
        if weight == 0 {
            return;
        }
        let w = u64::from(weight);
        self.stats.records = self.stats.records.saturating_add(w);
        let mut secs = ts_floor(rec.end_ts);
        let mut end = rec.end_ts;
        if end > self.now {
            end = self.now;
            secs = self.now_s;
            self.stats.clamped = self.stats.clamped.saturating_add(w);
        }
        if secs < self.now_s - self.cfg.retention {
            self.stats.dropped = self.stats.dropped.saturating_add(w);
            return;
        }
        let sample = Sample {
            class: usize::from((rec.status / 100).clamp(1, 5) - 1),
            w,
            rt_us: rec.duration_us,
            rt_index: LATENCY.index(rec.duration_us),
            ut: rec.upstream_us.map(|u| (u, LATENCY.index(u))),
            bytes: rec.bytes,
        };
        let cut2 = floor_to(self.now_s - self.cfg.grace, T0);
        let now_s = self.now_s;
        let s = self
            .sites
            .entry(site)
            .or_insert_with(|| Box::new(SiteAgg::new(now_s)));
        if s.last_request.is_none_or(|t| t < end) {
            s.last_request = Some(end);
        }
        let bucket = floor_to(secs, T0);
        if bucket < cut2 {
            self.stats.late = self.stats.late.saturating_add(w);
            s.tiers
                .record_late(floor_to(secs, MINUTE), &sample, &mut self.pool);
        } else {
            s.tiers.record_open(bucket, &sample, &mut self.pool);
        }
        if !rec.route.is_empty() && self.cfg.routes_k > 0 {
            s.routes.record(
                self.cfg.routes_k,
                &rec.method,
                &rec.route,
                w,
                rec.duration_us,
                rec.status,
            );
        }
        if rec.status >= 400 {
            s.errors.push(rec, self.cfg.errors_kept);
        }
    }

    /// Moves the clock to `now`: closes buckets, periods and minutes the clock passed,
    /// rotates route generations and drops what left the retention. A `now` older than the
    /// clock is ignored.
    pub fn advance(&mut self, now: Timestamp) {
        if now < self.now {
            self.stats.clock_back += 1;
            return;
        }
        self.now = now;
        let now_s = ts_floor(now);
        if now_s == self.now_s {
            return;
        }
        self.now_s = now_s;
        let edge = now_s - self.cfg.grace;
        let (cut2, cut10, cut60) = (
            floor_to(edge, T0),
            floor_to(edge, T1),
            floor_to(edge, MINUTE),
        );
        let periods = (self.clock.next_period, cut10.max(self.clock.next_period));
        let minutes = (self.clock.next_minute, cut60.max(self.clock.next_minute));
        let limits = Limits {
            t0_floor: floor_to(now_s - self.cfg.tier0_span, T0),
            retain_from: now_s - self.cfg.retention,
            hist_keep_from: floor_to(now_s - self.cfg.minute_hist_keep, MINUTE),
            hist_budget: self.cfg.minute_hist_budget,
            hist_cap: self.cfg.minute_hist_cap,
        };
        let dense = self.dense.get_or_insert_with(|| Box::new(Dense::new()));
        let retain_ts = secs_to_ts(limits.retain_from);
        for (id, s) in &mut self.sites {
            let before = self.outbox.len();
            let mut ctx = Ctx {
                dense: dense.as_mut(),
                pool: &mut self.pool,
                out: self.cfg.outbox.then_some(&mut self.outbox),
            };
            s.tiers.close(cut2, periods, minutes, &mut ctx);
            s.tiers.evict(&limits, &mut self.pool);
            s.routes.rotate(now_s, self.cfg.route_epoch);
            s.errors.prune(retain_ts);
            self.outbox_sites
                .extend(std::iter::repeat_n(*id, self.outbox.len() - before));
        }
        self.clock.next_period = periods.1;
        self.clock.next_minute = minutes.1;
        self.clock.t0_floor = self.clock.t0_floor.max(limits.t0_floor);
        if self.outbox.len() > self.cfg.outbox_cap {
            let excess = self.outbox.len() - self.cfg.outbox_cap;
            self.outbox.drain(..excess);
            self.outbox_sites.drain(..excess);
            self.stats.outbox_dropped += excess as u64;
        }
    }

    /// Folds late records of closed minutes of `site` before a query.
    fn settle(&mut self, site: SiteId) -> Option<&SiteAgg> {
        let s = self.sites.get_mut(&site)?;
        let dense = self.dense.get_or_insert_with(|| Box::new(Dense::new()));
        let before = self.outbox.len();
        let mut ctx = Ctx {
            dense: dense.as_mut(),
            pool: &mut self.pool,
            out: self.cfg.outbox.then_some(&mut self.outbox),
        };
        s.tiers.fold_late(self.clock.next_minute, &mut ctx);
        self.outbox_sites
            .extend(std::iter::repeat_n(site, self.outbox.len() - before));
        Some(&**s)
    }

    fn span(&self, from: Timestamp, to: Timestamp) -> (i64, i64) {
        (ts_floor(from), ts_ceil(to.min(self.now)))
    }

    fn summary_bounds(&self, from: i64, to: i64) -> (Timestamp, Timestamp) {
        (secs_to_ts(from), secs_to_ts(to).min(self.now))
    }

    fn empty_summary(&self, from: i64, to: i64) -> Summary {
        let from = floor_to(from, T0);
        let (from, to) = self.summary_bounds(from, ceil_to(to, T0).max(from));
        Summary {
            from,
            to,
            counters: Counters::default(),
            rt: None,
            ut: None,
            sampled: false,
        }
    }

    fn range_inner(
        &mut self,
        site: SiteId,
        from: i64,
        to: i64,
        with_hists: bool,
    ) -> (Summary, Option<(LatencyHist, LatencyHist)>) {
        let clock = self.clock;
        if self.settle(site).is_none() {
            return (self.empty_summary(from, to), None);
        }
        let s = &self.sites[&site];
        let plan = s.tiers.plan(from, to, &clock);
        let dense = self.dense.get_or_insert_with(|| Box::new(Dense::new()));
        let t = s.tiers.totals(&plan, dense.as_mut(), with_hists);
        let (f, e) = self.summary_bounds(plan.from, plan.to);
        (
            Summary {
                from: f,
                to: e,
                counters: t.counts.to_public(),
                rt: t.rt,
                ut: t.ut,
                sampled: t.sampled,
            },
            t.hists,
        )
    }

    /// The last `span` up to the clock (`1m`, `5m`, `1h`, `24h`), aligned outward.
    pub fn window(&mut self, site: SiteId, span: Duration) -> Summary {
        let to = self.now_s + 1;
        let from = self.now_s.saturating_sub(secs(span));
        self.range_inner(site, from, to, false).0
    }

    /// `[from, to)`, aligned outward to whole buckets (2 s where tier 0 still has the span,
    /// else minutes, else hours).
    pub fn range(&mut self, site: SiteId, from: Timestamp, to: Timestamp) -> Summary {
        let (from, to) = self.span(from, to);
        self.range_inner(site, from, to, false).0
    }

    /// [`Self::range`] plus the merged rt and ut histograms.
    pub fn range_hists(
        &mut self,
        site: SiteId,
        from: Timestamp,
        to: Timestamp,
    ) -> (Summary, LatencyHist, LatencyHist) {
        let (from, to) = self.span(from, to);
        let (summary, hists) = self.range_inner(site, from, to, true);
        let (rt, ut) = hists.unwrap_or_default();
        (summary, rt, ut)
    }

    /// Counts and time sums only (no histogram work): the input of CPU attribution (T6).
    pub fn sums(&mut self, site: SiteId, from: Timestamp, to: Timestamp) -> Counters {
        let (from, to) = self.span(from, to);
        let clock = self.clock;
        let Some(s) = self.settle(site) else {
            return Counters::default();
        };
        let plan = s.tiers.plan(from, to, &clock);
        s.tiers.sums(&plan).0.to_public()
    }

    /// Points of `[from, to)` at `step`; steps without data are left out.
    pub fn series(
        &mut self,
        site: SiteId,
        from: Timestamp,
        to: Timestamp,
        step: Step,
    ) -> Vec<SeriesPoint> {
        let (from, to) = self.span(from, to);
        let clock = self.clock;
        if self.settle(site).is_none() || from >= to {
            return Vec::new();
        }
        let s = &self.sites[&site];
        let dense = self.dense.get_or_insert_with(|| Box::new(Dense::new()));
        let t = &s.tiers;
        let trailing = |next: i64, width: i64, dense: &mut Dense, out: &mut Vec<SeriesPoint>| {
            let mut p = floor_to(from, width).max(next);
            while p < to {
                let res = if width == T1 {
                    t.point_resources(p)
                } else {
                    t.average_resources(p, p + width)
                };
                if res.is_some() || t.has_fine_data(p, p + width) {
                    let plan = t.plan(p, p + width, &clock);
                    let totals = t.totals(&plan, dense, false);
                    if totals.counts.requests() > 0 || res.is_some() {
                        out.push(tiers::totals_point(p, &totals, res));
                    }
                }
                p += width;
            }
        };
        match step {
            Step::TwoSeconds => t.series_fine(from, to, dense),
            Step::TenSeconds => {
                let mut out = t.series_points(from, to.min(clock.next_period));
                trailing(clock.next_period, T1, dense, &mut out);
                out
            }
            Step::Minute => {
                let mut out = t.series_minutes(from, to.min(clock.next_minute));
                trailing(clock.next_minute, MINUTE, dense, &mut out);
                out
            }
            Step::Hour => {
                let first = floor_to(from, HOUR);
                let mut hours = t.hour_starts(first, to);
                let mut h = first.max(floor_to(clock.next_minute, HOUR));
                while h < to {
                    if t.has_fine_data(h, h + HOUR) && !hours.contains(&h) {
                        hours.push(h);
                    }
                    h += HOUR;
                }
                hours.sort_unstable();
                hours
                    .into_iter()
                    .map(|h| {
                        let plan = t.plan(h, h + HOUR, &clock);
                        let totals = t.totals(&plan, dense, false);
                        tiers::totals_point(h, &totals, t.average_resources(h, h + HOUR))
                    })
                    .collect()
            }
        }
    }

    /// Stores the CPU/RAM attributed to `site` for the 10 s period containing `at`.
    pub fn set_resources(&mut self, site: SiteId, at: Timestamp, resources: Resources) {
        let now = self.now_s;
        let secs = ts_floor(at);
        if secs < now - self.cfg.retention {
            return;
        }
        self.sites
            .entry(site)
            .or_insert_with(|| Box::new(SiteAgg::new(now)))
            .tiers
            .set_resources(secs, &resources);
    }

    /// Top routes of the site (one to two route epochs).
    pub fn routes(&self, site: SiteId) -> Option<RoutesReport> {
        self.sites
            .get(&site)
            .map(|s| s.routes.report(self.cfg.routes_k))
    }

    /// The last 5xx and 4xx requests of the site, newest first.
    pub fn recent_errors(&self, site: SiteId) -> RecentErrors {
        self.sites
            .get(&site)
            .map(|s| s.errors.snapshot())
            .unwrap_or_default()
    }

    /// End of the most recent request of the site.
    pub fn last_request_at(&self, site: SiteId) -> Option<Timestamp> {
        self.sites.get(&site).and_then(|s| s.last_request)
    }

    /// Closed minutes and late deltas since the last call (empty unless
    /// [`AggConfig::minute_outbox`]). Late deltas of a minute may follow its complete one.
    pub fn take_minutes(&mut self) -> Vec<MinuteDelta> {
        let deltas = std::mem::take(&mut self.outbox);
        let sites = std::mem::take(&mut self.outbox_sites);
        deltas
            .into_iter()
            .zip(sites)
            .map(|(d, site)| MinuteDelta {
                site,
                start: secs_to_ts(d.start),
                counters: d.counts.to_public(),
                sampled: d.sampled,
                rt: d.rt,
                ut: d.ut,
                complete: d.complete,
            })
            .collect()
    }

    /// Puts a persisted minute back (boot rehydration, M2-E5-T7): into tier 2 when the
    /// minute is closed for this clock, else as late records of the open minute. Not sent
    /// to the outbox again.
    pub fn restore_minute(&mut self, delta: MinuteDelta) {
        let start = floor_to(ts_floor(delta.start), MINUTE);
        let now = self.now_s;
        if start + MINUTE <= now - self.cfg.retention {
            return;
        }
        let s = self
            .sites
            .entry(delta.site)
            .or_insert_with(|| Box::new(SiteAgg::new(now)));
        let closed = start < self.clock.next_minute;
        s.tiers.restore(
            Delta {
                start,
                counts: Counts::from_public(&delta.counters),
                sampled: delta.sampled,
                rt: delta.rt,
                ut: delta.ut,
                complete: delta.complete,
            },
            closed,
            &mut self.pool,
        );
    }

    /// Estimated heap bytes of one site (capacity accounting).
    pub fn site_heap_bytes(&self, site: SiteId) -> Option<usize> {
        self.sites.get(&site).map(|s| s.heap_bytes())
    }

    /// Estimated heap bytes of the whole aggregator, shared scratch included.
    pub fn heap_bytes(&self) -> usize {
        let sites: usize = self.sites.values().map(|s| s.heap_bytes()).sum();
        sites + self.shared_heap_bytes()
    }

    /// Estimated heap bytes not owned by any site: the site table, the dense scratch
    /// histogram (168 KiB once a query or close ran) and the pooled sample buffers.
    pub fn shared_heap_bytes(&self) -> usize {
        let map = self.sites.capacity() * (size_of::<(SiteId, Box<SiteAgg>)>() + 1);
        let dense = self.dense.as_ref().map_or(0, |d| d.heap_bytes());
        let outbox = self.outbox.capacity() * size_of::<Delta>()
            + self
                .outbox
                .iter()
                .map(|d| d.rt.encoded_len() + d.ut.encoded_len())
                .sum::<usize>()
            + self.outbox_sites.capacity() * size_of::<SiteId>();
        map + dense + self.pool.heap_bytes() + outbox
    }

    /// `true` when the site holds no tier data (it may still have routes or errors).
    pub fn site_is_idle(&self, site: SiteId) -> bool {
        self.sites.get(&site).is_none_or(|s| s.tiers.is_empty())
    }
}
