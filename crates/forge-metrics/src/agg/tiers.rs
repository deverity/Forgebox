//! Per-site tiers (docs/14 section 3.4) and the planner that answers windows and ranges from
//! them with whole buckets, aligned outward.
//!
//! - **Open buckets**: the 2 s buckets that still accept records (end + grace not reached),
//!   as raw `(index, weight)` samples; compacted when they close.
//! - **Tier 0**: closed 2 s buckets for `tier0_span` (10 min): counts, bytes, sums, sampled
//!   flag and compact rt/ut histograms.
//! - **Tier 1**: 10 s scalar points for the retention (24 h), 64 B each, built when the
//!   period closes; CPU/RAM of the period are written into them later.
//! - **Tier 2**: closed minutes for the retention with counts and p50/p90/p99; their rt/ut
//!   histograms are kept for at least `minute_hist_keep` (1 h) and then while the site's
//!   minute histograms fit `minute_hist_budget` (128 KiB), oldest dropped first; past
//!   `minute_hist_cap` (1 MiB, microsecond logs at high rates) even younger ones go. Every
//!   closed minute is also merged into its **hour** (rt/ut histograms always kept), so
//!   windows and ranges stay exact at hour alignment when minute histograms are gone.
//! - **Late accumulators**: records whose 2 s bucket already closed, per minute. They go to
//!   tier 2 only: merged when their minute closes, or into the closed minute and its hour.
//!   Tier 0 and tier 1 never change after closing.
//!
//! Nothing is allocated for a period without records.

use std::collections::VecDeque;

use super::{Counters, HOUR, MINUTE, SeriesPoint, T0, T1, ceil_to, floor_to, secs_to_ts};
use crate::attribution::SiteResources;
use crate::hist::{Dense, LatencyHist, Percentiles, compact_raw, push_raw};

/// One record, reduced to what the tiers need.
pub(crate) struct Sample {
    pub(crate) class: usize,
    pub(crate) w: u64,
    pub(crate) rt_us: u64,
    pub(crate) rt_index: usize,
    pub(crate) ut: Option<(u64, usize)>,
    pub(crate) bytes: u64,
}

/// Counts of a bucket. `u32` request counts hold an hour at a million requests per second.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    status: [u32; 5],
    ut_count: u32,
    bytes: u64,
    rt_sum: u64,
    ut_sum: u64,
}

fn add32(a: u32, b: u64) -> u32 {
    a.saturating_add(u32::try_from(b).unwrap_or(u32::MAX))
}

impl Counts {
    #[inline]
    fn record(&mut self, s: &Sample) {
        self.status[s.class] = add32(self.status[s.class], s.w);
        self.bytes = self.bytes.saturating_add(s.bytes.saturating_mul(s.w));
        self.rt_sum = self.rt_sum.saturating_add(s.rt_us.saturating_mul(s.w));
        if let Some((ut, _)) = s.ut {
            self.ut_count = add32(self.ut_count, s.w);
            self.ut_sum = self.ut_sum.saturating_add(ut.saturating_mul(s.w));
        }
    }

    fn add(&mut self, o: &Counts) {
        for (a, b) in self.status.iter_mut().zip(o.status) {
            *a = add32(*a, u64::from(b));
        }
        self.ut_count = add32(self.ut_count, u64::from(o.ut_count));
        self.bytes = self.bytes.saturating_add(o.bytes);
        self.rt_sum = self.rt_sum.saturating_add(o.rt_sum);
        self.ut_sum = self.ut_sum.saturating_add(o.ut_sum);
    }

    pub(crate) fn requests(&self) -> u64 {
        self.status.iter().map(|c| u64::from(*c)).sum()
    }

    pub(crate) fn to_public(self) -> Counters {
        Counters {
            requests: self.requests(),
            status: self.status.map(u64::from),
            bytes: self.bytes,
            rt_sum_us: self.rt_sum,
            ut_sum_us: self.ut_sum,
            ut_count: u64::from(self.ut_count),
        }
    }

    pub(crate) fn from_public(c: &Counters) -> Self {
        Self {
            status: c.status.map(|v| u32::try_from(v).unwrap_or(u32::MAX)),
            ut_count: u32::try_from(c.ut_count).unwrap_or(u32::MAX),
            bytes: c.bytes,
            rt_sum: c.rt_sum_us,
            ut_sum: c.ut_sum_us,
        }
    }
}

/// Reusable raw sample buffers (one aggregator, all sites).
#[derive(Default)]
pub(crate) struct BufPool(Vec<Vec<u32>>);

/// Buffers kept for reuse, and the largest one kept (64 KiB).
const POOL_MAX: usize = 8;
const POOL_MAX_CAP: usize = 16 * 1024;

impl BufPool {
    fn take(&mut self) -> Vec<u32> {
        self.0.pop().unwrap_or_default()
    }

    fn give(&mut self, mut v: Vec<u32>) {
        if v.capacity() > 0 && v.capacity() <= POOL_MAX_CAP && self.0.len() < POOL_MAX {
            v.clear();
            self.0.push(v);
        }
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.0.iter().map(|v| v.capacity() * 4).sum()
    }
}

/// A bucket still accepting records (or the late records of one minute).
pub(crate) struct OpenAcc {
    start: i64,
    counts: Counts,
    sampled: bool,
    rt: Vec<u32>,
    ut: Vec<u32>,
}

impl OpenAcc {
    fn new(start: i64, pool: &mut BufPool) -> Self {
        Self {
            start,
            counts: Counts::default(),
            sampled: false,
            rt: pool.take(),
            ut: pool.take(),
        }
    }

    #[inline]
    fn push(&mut self, s: &Sample) {
        self.counts.record(s);
        push_raw(&mut self.rt, s.rt_index, s.w);
        if let Some((_, i)) = s.ut {
            push_raw(&mut self.ut, i, s.w);
        }
        self.sampled |= s.w > 1;
    }

    fn release(self, pool: &mut BufPool) {
        pool.give(self.rt);
        pool.give(self.ut);
    }

    fn heap_bytes(&self) -> usize {
        (self.rt.capacity() + self.ut.capacity()) * 4
    }
}

/// A closed 2 s bucket, or an hour.
pub(crate) struct Bucket {
    start: i64,
    counts: Counts,
    sampled: bool,
    rt: LatencyHist,
    ut: LatencyHist,
}

impl Bucket {
    fn heap_bytes(&self) -> usize {
        self.rt.encoded_len() + self.ut.encoded_len()
    }
}

type P3 = [u32; 3];

fn p3(p: Option<Percentiles>) -> P3 {
    p.map_or([0; 3], |p| {
        [p.p50_us, p.p90_us, p.p99_us].map(|v| u32::try_from(v).unwrap_or(u32::MAX))
    })
}

fn from_p3(p: P3, present: bool) -> Option<Percentiles> {
    present.then(|| Percentiles {
        p50_us: u64::from(p[0]),
        p90_us: u64::from(p[1]),
        p99_us: u64::from(p[2]),
    })
}

/// A closed minute.
pub(crate) struct Minute {
    start: i64,
    counts: Counts,
    sampled: bool,
    rt_p: P3,
    ut_p: P3,
    /// rt and ut histograms, while kept (see the module docs).
    hists: Option<Box<[LatencyHist; 2]>>,
}

impl Minute {
    fn hist_bytes(&self) -> usize {
        self.hists.as_ref().map_or(0, |h| {
            size_of::<[LatencyHist; 2]>() + h[0].encoded_len() + h[1].encoded_len()
        })
    }
}

const SAMPLED: u8 = 1;
const HAS_RESOURCES: u8 = 2;
const APPROX: u8 = 4;

/// CPU % of [`SiteResources`] as stored in a [`Point`]: `f32`, saturated (an `as` cast
/// would turn a huge value into infinity), NaN and negatives as 0.
fn pack_cpu(v: f64) -> f32 {
    if v.is_nan() || v <= 0.0 {
        0.0
    } else {
        v.min(f64::from(f32::MAX)) as f32
    }
}

/// A tier 1 point: 10 s of scalars, 64 bytes.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Point {
    /// Start / 10 s.
    period: u32,
    requests: u32,
    err4xx: u32,
    err5xx: u32,
    p: P3,
    mem_kib: u32,
    cpu_pct: f32,
    bytes: u64,
    rt_sum: u64,
    ut_sum: u64,
    workers: u16,
    flags: u8,
}

const _: () = assert!(size_of::<Point>() == 64);

impl Point {
    fn start(&self) -> i64 {
        i64::from(self.period) * T1
    }

    fn resources(&self) -> Option<SiteResources> {
        (self.flags & HAS_RESOURCES != 0).then(|| SiteResources {
            cpu_pct: f64::from(self.cpu_pct),
            mem_bytes: u64::from(self.mem_kib) * 1024,
            approx: self.flags & APPROX != 0,
            workers_active: u32::from(self.workers),
        })
    }

    fn fill(&mut self, counts: &Counts, sampled: bool, rt: Option<Percentiles>) {
        self.requests = u32::try_from(counts.requests()).unwrap_or(u32::MAX);
        self.err4xx = counts.status[3];
        self.err5xx = counts.status[4];
        self.bytes = counts.bytes;
        self.rt_sum = counts.rt_sum;
        self.ut_sum = counts.ut_sum;
        self.p = p3(rt);
        if sampled {
            self.flags |= SAMPLED;
        }
    }

    fn to_series(self) -> SeriesPoint {
        SeriesPoint {
            start: secs_to_ts(self.start()),
            requests: u64::from(self.requests),
            err4xx: u64::from(self.err4xx),
            err5xx: u64::from(self.err5xx),
            bytes: self.bytes,
            rt_sum_us: self.rt_sum,
            ut_sum_us: self.ut_sum,
            rt: from_p3(self.p, self.requests > 0),
            ut: None,
            resources: self.resources(),
            sampled: self.flags & SAMPLED != 0,
        }
    }
}

/// A closed minute or late records, as handed to persistence (site added by the caller).
pub(crate) struct Delta {
    pub(crate) start: i64,
    pub(crate) counts: Counts,
    pub(crate) sampled: bool,
    pub(crate) rt: LatencyHist,
    pub(crate) ut: LatencyHist,
    pub(crate) complete: bool,
}

/// Shared scratch of the aggregator.
pub(crate) struct Ctx<'a> {
    pub(crate) dense: &'a mut Dense,
    pub(crate) pool: &'a mut BufPool,
    pub(crate) out: Option<&'a mut Vec<Delta>>,
}

impl Ctx<'_> {
    fn emit(&mut self, d: impl FnOnce() -> Delta) {
        if let Some(out) = self.out.as_deref_mut() {
            out.push(d());
        }
    }
}

/// Where the aggregator clock is (all sites share it).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Clock {
    /// Tier 0 holds every non-late record from here on.
    pub(crate) t0_floor: i64,
    /// Periods starting before this are closed.
    pub(crate) next_period: i64,
    /// Minutes starting before this are closed.
    pub(crate) next_minute: i64,
}

/// Retention limits applied by [`SiteTiers::evict`], in seconds.
pub(crate) struct Limits {
    pub(crate) t0_floor: i64,
    pub(crate) retain_from: i64,
    pub(crate) hist_keep_from: i64,
    pub(crate) hist_budget: usize,
    pub(crate) hist_cap: usize,
}

/// Grows a deque by an eighth (it lives for hours: doubling would waste up to half).
fn grow<T>(d: &mut VecDeque<T>) {
    if d.len() == d.capacity() {
        d.reserve_exact((d.len() / 8).max(8));
    }
}

fn shrink<T>(d: &mut VecDeque<T>) {
    if d.is_empty() {
        if d.capacity() > 0 {
            *d = VecDeque::new();
        }
    } else if d.capacity() > 2 * d.len() + 16 {
        d.shrink_to(d.len() + d.len() / 8 + 8);
    }
}

/// A piece of a query plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Piece {
    /// Tier 0 and open buckets starting in `[from, to)`.
    Fine(i64, i64),
    /// The late records of an open minute.
    Late(i64),
    /// A closed minute (with histograms).
    Minute(i64),
    /// The closed minutes of an hour.
    Hour(i64),
}

/// Pieces and the span they cover (aligned outward).
#[derive(Debug, Default)]
pub(crate) struct Plan {
    pub(crate) pieces: Vec<Piece>,
    pub(crate) from: i64,
    pub(crate) to: i64,
}

impl Plan {
    fn push(&mut self, piece: Piece, from: i64, to: i64) {
        if self.pieces.is_empty() && self.from == self.to {
            self.from = from;
            self.to = to;
        } else {
            self.from = self.from.min(from);
            self.to = self.to.max(to);
        }
        self.pieces.push(piece);
    }

    fn cover(&mut self, from: i64, to: i64) {
        if self.from == self.to && self.pieces.is_empty() {
            self.from = from;
            self.to = to;
        } else {
            self.from = self.from.min(from);
            self.to = self.to.max(to);
        }
    }
}

/// What a plan piece reads.
enum Src<'a> {
    Bucket(&'a Bucket),
    Open(&'a OpenAcc),
    Minute(&'a Minute),
}

impl Src<'_> {
    fn counts(&self) -> &Counts {
        match self {
            Src::Bucket(b) => &b.counts,
            Src::Open(o) => &o.counts,
            Src::Minute(m) => &m.counts,
        }
    }

    fn sampled(&self) -> bool {
        match self {
            Src::Bucket(b) => b.sampled,
            Src::Open(o) => o.sampled,
            Src::Minute(m) => m.sampled,
        }
    }

    /// Adds the rt (`ut == false`) or ut histogram to `dense`.
    fn add_to(&self, dense: &mut Dense, ut: bool) {
        match self {
            Src::Bucket(b) => dense.add(if ut { &b.ut } else { &b.rt }),
            Src::Open(o) => dense.add_raw(if ut { &o.ut } else { &o.rt }),
            Src::Minute(m) => {
                if let Some(h) = &m.hists {
                    dense.add(&h[usize::from(ut)]);
                }
            }
        }
    }
}

/// Totals of a plan.
pub(crate) struct Totals {
    pub(crate) counts: Counts,
    pub(crate) sampled: bool,
    pub(crate) rt: Option<Percentiles>,
    pub(crate) ut: Option<Percentiles>,
    pub(crate) hists: Option<(LatencyHist, LatencyHist)>,
}

#[derive(Default)]
pub(crate) struct SiteTiers {
    open: Vec<OpenAcc>,
    t0: VecDeque<Bucket>,
    late: Vec<OpenAcc>,
    points: VecDeque<Point>,
    minutes: VecDeque<Minute>,
    /// Minutes starting at or after this keep their histograms.
    hist_from: i64,
    hist_bytes: usize,
    hours: VecDeque<Bucket>,
}

impl SiteTiers {
    pub(crate) fn new() -> Self {
        Self {
            hist_from: i64::MIN,
            ..Self::default()
        }
    }

    // ---------------------------------------------------------------------------------------
    // Recording and closing
    // ---------------------------------------------------------------------------------------

    #[inline]
    pub(crate) fn record_open(&mut self, start: i64, s: &Sample, pool: &mut BufPool) {
        // The bucket being filled is almost always the last one.
        if let Some(last) = self.open.last_mut() {
            if last.start == start {
                last.push(s);
                return;
            }
        }
        let pos = self.open.partition_point(|o| o.start < start);
        if pos == self.open.len() || self.open[pos].start != start {
            self.open.insert(pos, OpenAcc::new(start, pool));
        }
        self.open[pos].push(s);
    }

    pub(crate) fn record_late(&mut self, minute: i64, s: &Sample, pool: &mut BufPool) {
        let pos = self.late.partition_point(|o| o.start < minute);
        if pos == self.late.len() || self.late[pos].start != minute {
            self.late.insert(pos, OpenAcc::new(minute, pool));
        }
        self.late[pos].push(s);
    }

    /// Closes what the clock passed: open buckets starting before `cut2`, periods in
    /// `[from_period, cut10)`, minutes in `[from_minute, cut60)`; then folds late records of
    /// closed minutes.
    pub(crate) fn close(
        &mut self,
        cut2: i64,
        periods: (i64, i64),
        minutes: (i64, i64),
        ctx: &mut Ctx<'_>,
    ) {
        while self.open.first().is_some_and(|o| o.start < cut2) {
            let mut acc = self.open.remove(0);
            let bucket = Bucket {
                start: acc.start,
                counts: acc.counts,
                sampled: acc.sampled,
                rt: compact_raw(&mut acc.rt),
                ut: compact_raw(&mut acc.ut),
            };
            acc.release(ctx.pool);
            let pos = self.t0.partition_point(|b| b.start < bucket.start);
            grow(&mut self.t0);
            self.t0.insert(pos, bucket);
        }
        self.close_periods(periods.0, periods.1, ctx.dense);
        self.close_minutes(minutes.0, minutes.1, ctx);
        self.fold_late(minutes.1, ctx);
    }

    fn close_periods(&mut self, from: i64, cut: i64, dense: &mut Dense) {
        if cut <= from {
            return;
        }
        let mut i = self.t0.partition_point(|b| b.start < from);
        while i < self.t0.len() && self.t0[i].start < cut {
            let period = floor_to(self.t0[i].start, T1);
            let mut j = i;
            let mut counts = Counts::default();
            let mut sampled = false;
            dense.clear();
            while j < self.t0.len() && self.t0[j].start < period + T1 {
                counts.add(&self.t0[j].counts);
                sampled |= self.t0[j].sampled;
                dense.add(&self.t0[j].rt);
                j += 1;
            }
            let rt = dense.percentiles();
            dense.clear();
            let point = self.point_mut(period);
            point.fill(&counts, sampled, rt);
            i = j;
        }
    }

    /// The point of a period, inserted (empty) if missing.
    fn point_mut(&mut self, period_start: i64) -> &mut Point {
        let key = u32::try_from(period_start / T1).unwrap_or(0);
        let pos = self.points.partition_point(|p| p.period < key);
        if pos == self.points.len() || self.points[pos].period != key {
            grow(&mut self.points);
            self.points.insert(
                pos,
                Point {
                    period: key,
                    ..Point::default()
                },
            );
        }
        &mut self.points[pos]
    }

    /// Packs `r` into the 64-byte point, saturating: CPU as `f32` (NaN and negatives as 0,
    /// at most `f32::MAX`), memory in KiB rounded up (at most 4 TiB), workers at most
    /// `u16::MAX`.
    pub(crate) fn set_resources(&mut self, at: i64, r: &SiteResources) {
        let point = self.point_mut(floor_to(at, T1));
        point.cpu_pct = pack_cpu(r.cpu_pct);
        point.mem_kib = u32::try_from(r.mem_bytes.div_ceil(1024)).unwrap_or(u32::MAX);
        point.workers = u16::try_from(r.workers_active).unwrap_or(u16::MAX);
        point.flags |= HAS_RESOURCES;
        if r.approx {
            point.flags |= APPROX;
        } else {
            point.flags &= !APPROX;
        }
    }

    fn close_minutes(&mut self, from: i64, cut: i64, ctx: &mut Ctx<'_>) {
        if cut <= from {
            return;
        }
        let mut i = self.t0.partition_point(|b| b.start < from);
        loop {
            let next_t0 = (i < self.t0.len() && self.t0[i].start < cut)
                .then(|| floor_to(self.t0[i].start, MINUTE));
            let next_late = self
                .late
                .iter()
                .find(|l| l.start >= from && l.start < cut)
                .map(|l| l.start);
            let minute = match (next_t0, next_late) {
                (Some(a), Some(b)) => a.min(b),
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => break,
            };
            let mut j = i;
            while j < self.t0.len() && self.t0[j].start < minute + MINUTE {
                j += 1;
            }
            let late = self
                .late
                .iter()
                .position(|l| l.start == minute)
                .map(|p| self.late.remove(p));
            self.close_minute(minute, i..j, late, ctx);
            i = j;
        }
    }

    fn close_minute(
        &mut self,
        minute: i64,
        range: std::ops::Range<usize>,
        late: Option<OpenAcc>,
        ctx: &mut Ctx<'_>,
    ) {
        let mut counts = Counts::default();
        let mut sampled = false;
        for b in self.t0.range(range.clone()) {
            counts.add(&b.counts);
            sampled |= b.sampled;
        }
        if let Some(l) = &late {
            counts.add(&l.counts);
            sampled |= l.sampled;
        }
        let mut hist = |ut: bool| {
            ctx.dense.clear();
            for b in self.t0.range(range.clone()) {
                ctx.dense.add(if ut { &b.ut } else { &b.rt });
            }
            if let Some(l) = &late {
                ctx.dense.add_raw(if ut { &l.ut } else { &l.rt });
            }
            let h = ctx.dense.to_hist();
            ctx.dense.clear();
            h
        };
        let rt = hist(false);
        let ut = hist(true);
        if let Some(l) = late {
            l.release(ctx.pool);
        }
        ctx.emit(|| Delta {
            start: minute,
            counts,
            sampled,
            rt: rt.clone(),
            ut: ut.clone(),
            complete: true,
        });
        self.upsert_minute(minute, counts, sampled, rt, ut);
    }

    /// Late records of minutes that already closed (`start < cut`) go into those minutes.
    pub(crate) fn fold_late(&mut self, cut: i64, ctx: &mut Ctx<'_>) {
        while self.late.first().is_some_and(|l| l.start < cut) {
            let mut acc = self.late.remove(0);
            let rt = compact_raw(&mut acc.rt);
            let ut = compact_raw(&mut acc.ut);
            let (start, counts, sampled) = (acc.start, acc.counts, acc.sampled);
            acc.release(ctx.pool);
            ctx.emit(|| Delta {
                start,
                counts,
                sampled,
                rt: rt.clone(),
                ut: ut.clone(),
                complete: false,
            });
            self.upsert_minute(start, counts, sampled, rt, ut);
        }
    }

    /// Restores a persisted minute: tier 2 when it is closed, else as late records.
    pub(crate) fn restore(&mut self, d: Delta, closed: bool, pool: &mut BufPool) {
        if closed {
            self.upsert_minute(d.start, d.counts, d.sampled, d.rt, d.ut);
            return;
        }
        let pos = self.late.partition_point(|o| o.start < d.start);
        if pos == self.late.len() || self.late[pos].start != d.start {
            self.late.insert(pos, OpenAcc::new(d.start, pool));
        }
        let acc = &mut self.late[pos];
        acc.counts.add(&d.counts);
        acc.sampled |= d.sampled;
        for (i, c) in d.rt.entries() {
            push_raw(&mut acc.rt, i, c);
        }
        for (i, c) in d.ut.entries() {
            push_raw(&mut acc.ut, i, c);
        }
    }

    fn upsert_minute(
        &mut self,
        start: i64,
        counts: Counts,
        sampled: bool,
        rt: LatencyHist,
        ut: LatencyHist,
    ) {
        self.upsert_hour(floor_to(start, HOUR), &counts, sampled, &rt, &ut);
        let pos = self.minutes.partition_point(|m| m.start < start);
        if pos < self.minutes.len() && self.minutes[pos].start == start {
            let m = &mut self.minutes[pos];
            let had = m.counts.requests() > 0;
            m.counts.add(&counts);
            m.sampled |= sampled;
            let before = m.hist_bytes();
            if let Some(h) = &mut m.hists {
                h[0] = h[0].merge(&rt);
                h[1] = h[1].merge(&ut);
                m.rt_p = p3(h[0].percentiles());
                m.ut_p = p3(h[1].percentiles());
            } else if !had {
                m.rt_p = p3(rt.percentiles());
                m.ut_p = p3(ut.percentiles());
            }
            // Without histograms (dropped for memory) the percentiles stay those of the
            // records the minute had when it closed; counts are always exact.
            let after = m.hist_bytes();
            self.hist_bytes = self.hist_bytes - before + after;
            return;
        }
        let minute = Minute {
            start,
            counts,
            sampled,
            rt_p: p3(rt.percentiles()),
            ut_p: p3(ut.percentiles()),
            hists: (start >= self.hist_from).then(|| Box::new([rt, ut])),
        };
        self.hist_bytes += minute.hist_bytes();
        grow(&mut self.minutes);
        self.minutes.insert(pos, minute);
    }

    fn upsert_hour(
        &mut self,
        start: i64,
        counts: &Counts,
        sampled: bool,
        rt: &LatencyHist,
        ut: &LatencyHist,
    ) {
        let pos = self.hours.partition_point(|h| h.start < start);
        if pos < self.hours.len() && self.hours[pos].start == start {
            let h = &mut self.hours[pos];
            h.counts.add(counts);
            h.sampled |= sampled;
            h.rt = h.rt.merge(rt);
            h.ut = h.ut.merge(ut);
            return;
        }
        grow(&mut self.hours);
        self.hours.insert(
            pos,
            Bucket {
                start,
                counts: *counts,
                sampled,
                rt: rt.clone(),
                ut: ut.clone(),
            },
        );
    }

    /// Drops what left the retention and minute histograms over the budget.
    pub(crate) fn evict(&mut self, l: &Limits, pool: &mut BufPool) {
        while self.t0.front().is_some_and(|b| b.start < l.t0_floor) {
            self.t0.pop_front();
        }
        while self
            .points
            .front()
            .is_some_and(|p| p.start() + T1 <= l.retain_from)
        {
            self.points.pop_front();
        }
        while self
            .minutes
            .front()
            .is_some_and(|m| m.start + MINUTE <= l.retain_from)
        {
            let m = self.minutes.pop_front().expect("front exists");
            self.hist_bytes -= m.hist_bytes();
        }
        while self
            .hours
            .front()
            .is_some_and(|h| h.start + HOUR <= l.retain_from)
        {
            self.hours.pop_front();
        }
        while self
            .late
            .first()
            .is_some_and(|a| a.start + MINUTE <= l.retain_from)
        {
            self.late.remove(0).release(pool);
        }
        if self.hist_bytes > l.hist_budget {
            // Oldest first: past the budget only the last `minute_hist_keep` is protected,
            // past the cap nothing is.
            let mut i = self.minutes.partition_point(|m| m.start < self.hist_from);
            while i < self.minutes.len()
                && (self.hist_bytes > l.hist_cap
                    || (self.hist_bytes > l.hist_budget
                        && self.minutes[i].start < l.hist_keep_from))
            {
                let m = &mut self.minutes[i];
                self.hist_bytes -= m.hist_bytes();
                m.hists = None;
                self.hist_from = m.start + MINUTE;
                i += 1;
            }
        }
        shrink(&mut self.t0);
        shrink(&mut self.points);
        shrink(&mut self.minutes);
        shrink(&mut self.hours);
        if self.open.is_empty() && self.open.capacity() > 0 {
            self.open = Vec::new();
        }
        if self.late.is_empty() && self.late.capacity() > 0 {
            self.late = Vec::new();
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.open.is_empty()
            && self.t0.is_empty()
            && self.late.is_empty()
            && self.points.is_empty()
            && self.minutes.is_empty()
            && self.hours.is_empty()
    }

    // ---------------------------------------------------------------------------------------
    // Queries
    // ---------------------------------------------------------------------------------------

    fn minute_index(&self, start: i64) -> Option<usize> {
        let pos = self.minutes.partition_point(|m| m.start < start);
        (pos < self.minutes.len() && self.minutes[pos].start == start).then_some(pos)
    }

    /// A closed minute can be read at minute resolution: it has histograms, or no data.
    fn minute_readable(&self, start: i64) -> bool {
        start >= self.hist_from || self.minute_index(start).is_none()
    }

    /// Plans `[from, to)`: closed whole hours from the hour buckets, closed minutes from
    /// tier 2 (falling back to their hour when their histograms are gone), partial minutes
    /// from tier 0 while it still has them, open minutes from tier 0 plus their late records.
    pub(crate) fn plan(&self, from: i64, to: i64, clock: &Clock) -> Plan {
        let mut plan = Plan {
            pieces: Vec::new(),
            from: floor_to(from, T0),
            to: floor_to(from, T0),
        };
        if from >= to {
            return plan;
        }
        let cut = clock.next_minute;
        let mut h = floor_to(from, HOUR);
        while h < to {
            let h_end = h + HOUR;
            let (a, b) = (from.max(h), to.min(h_end));
            let closed_end = h_end.min(cut).max(h);
            let cb = b.min(closed_end);
            if a < cb {
                let whole = a == h && cb == closed_end;
                if whole || !self.minutes_readable(a, cb, clock) {
                    if self.hours.iter().any(|x| x.start == h) {
                        plan.push(Piece::Hour(h), h, closed_end);
                    } else {
                        plan.cover(h, closed_end);
                    }
                } else {
                    self.plan_closed(a, cb, clock, &mut plan);
                }
            }
            let oa = a.max(closed_end);
            if oa < b {
                self.plan_open(oa, b, &mut plan);
            }
            h = h_end;
        }
        plan
    }

    fn minutes_readable(&self, a: i64, b: i64, clock: &Clock) -> bool {
        let mut m = floor_to(a, MINUTE);
        while m < b {
            let (ma, mb) = (a.max(m), b.min(m + MINUTE));
            let whole = ma == m && mb == m + MINUTE;
            let fine = !whole && floor_to(ma, T0) >= clock.t0_floor;
            if !fine && !self.minute_readable(m) {
                return false;
            }
            m += MINUTE;
        }
        true
    }

    fn plan_closed(&self, a: i64, b: i64, clock: &Clock, plan: &mut Plan) {
        let mut m = floor_to(a, MINUTE);
        while m < b {
            let (ma, mb) = (a.max(m), b.min(m + MINUTE));
            let whole = ma == m && mb == m + MINUTE;
            if !whole && floor_to(ma, T0) >= clock.t0_floor {
                let (fa, fb) = (floor_to(ma, T0), ceil_to(mb, T0));
                plan.push(Piece::Fine(fa, fb), fa, fb);
            } else if self.minute_index(m).is_some() {
                plan.push(Piece::Minute(m), m, m + MINUTE);
            } else {
                plan.cover(m, m + MINUTE);
            }
            m += MINUTE;
        }
    }

    fn plan_open(&self, a: i64, b: i64, plan: &mut Plan) {
        let mut m = floor_to(a, MINUTE);
        while m < b {
            let (ma, mb) = (a.max(m), b.min(m + MINUTE));
            let (fa, fb) = (floor_to(ma, T0), ceil_to(mb, T0));
            plan.push(Piece::Fine(fa, fb), fa, fb);
            if ma == m && mb == m + MINUTE && self.late.iter().any(|l| l.start == m) {
                plan.push(Piece::Late(m), m, m + MINUTE);
            }
            m += MINUTE;
        }
    }

    fn visit(&self, plan: &Plan, mut f: impl FnMut(Src<'_>)) {
        for piece in &plan.pieces {
            match *piece {
                Piece::Fine(a, b) => {
                    let start = self.t0.partition_point(|x| x.start < a);
                    for x in self.t0.range(start..).take_while(|x| x.start < b) {
                        f(Src::Bucket(x));
                    }
                    for o in self.open.iter().filter(|o| o.start >= a && o.start < b) {
                        f(Src::Open(o));
                    }
                }
                Piece::Late(m) => {
                    if let Some(l) = self.late.iter().find(|l| l.start == m) {
                        f(Src::Open(l));
                    }
                }
                Piece::Minute(m) => {
                    if let Some(i) = self.minute_index(m) {
                        f(Src::Minute(&self.minutes[i]));
                    }
                }
                Piece::Hour(h) => {
                    if let Some(x) = self.hours.iter().find(|x| x.start == h) {
                        f(Src::Bucket(x));
                    }
                }
            }
        }
    }

    /// Counts only (no histogram work).
    pub(crate) fn sums(&self, plan: &Plan) -> (Counts, bool) {
        let mut counts = Counts::default();
        let mut sampled = false;
        self.visit(plan, |s| {
            counts.add(s.counts());
            sampled |= s.sampled();
        });
        (counts, sampled)
    }

    /// Counts and percentiles of a plan (and the merged histograms when asked).
    pub(crate) fn totals(&self, plan: &Plan, dense: &mut Dense, with_hists: bool) -> Totals {
        let (counts, sampled) = self.sums(plan);
        let mut pass = |ut: bool| {
            dense.clear();
            self.visit(plan, |s| s.add_to(dense, ut));
            let p = dense.percentiles();
            let h = with_hists.then(|| dense.to_hist());
            dense.clear();
            (p, h)
        };
        let (rt, rt_h) = pass(false);
        let (ut, ut_h) = pass(true);
        Totals {
            counts,
            sampled,
            rt,
            ut,
            hists: rt_h.zip(ut_h),
        }
    }

    /// 2 s points: closed tier 0 buckets, then open ones.
    pub(crate) fn series_fine(&self, from: i64, to: i64, dense: &mut Dense) -> Vec<SeriesPoint> {
        let a = floor_to(from, T0);
        let start = self.t0.partition_point(|x| x.start < a);
        let mut out: Vec<SeriesPoint> = self
            .t0
            .range(start..)
            .take_while(|x| x.start < to)
            .map(|x| {
                series_point(
                    x.start,
                    &x.counts,
                    x.sampled,
                    x.rt.percentiles(),
                    x.ut.percentiles(),
                )
            })
            .collect();
        for o in self.open.iter().filter(|o| o.start >= a && o.start < to) {
            dense.clear();
            dense.add_raw(&o.rt);
            let rt = dense.percentiles();
            dense.clear();
            dense.add_raw(&o.ut);
            let ut = dense.percentiles();
            dense.clear();
            out.push(series_point(o.start, &o.counts, o.sampled, rt, ut));
        }
        out
    }

    /// Closed 10 s points in `[from, min(to, next_period))`.
    pub(crate) fn series_points(&self, from: i64, to: i64) -> Vec<SeriesPoint> {
        let a = floor_to(from, T1);
        let start = self.points.partition_point(|p| p.start() < a);
        self.points
            .range(start..)
            .take_while(|p| p.start() < to)
            .map(|p| p.to_series())
            .collect()
    }

    /// Resources of the open 10 s period starting at `start`, if set.
    pub(crate) fn point_resources(&self, start: i64) -> Option<SiteResources> {
        let key = u32::try_from(start / T1).ok()?;
        let pos = self.points.partition_point(|p| p.period < key);
        self.points
            .get(pos)
            .filter(|p| p.period == key)
            .and_then(Point::resources)
    }

    /// Closed minutes in `[from, to)` with resources averaged from their 10 s points.
    pub(crate) fn series_minutes(&self, from: i64, to: i64) -> Vec<SeriesPoint> {
        let a = floor_to(from, MINUTE);
        let start = self.minutes.partition_point(|m| m.start < a);
        self.minutes
            .range(start..)
            .take_while(|m| m.start < to)
            .map(|m| {
                let mut p = series_point(
                    m.start,
                    &m.counts,
                    m.sampled,
                    from_p3(m.rt_p, m.counts.requests() > 0),
                    from_p3(m.ut_p, m.counts.ut_count > 0),
                );
                p.resources = self.average_resources(m.start, m.start + MINUTE);
                p
            })
            .collect()
    }

    /// Mean CPU and memory, max workers, of the 10 s points in `[from, to)` that have them.
    pub(crate) fn average_resources(&self, from: i64, to: i64) -> Option<SiteResources> {
        let start = self.points.partition_point(|p| p.start() < from);
        let mut n = 0u32;
        let (mut cpu, mut mem, mut workers, mut approx) = (0f64, 0u128, 0u32, false);
        for p in self.points.range(start..).take_while(|p| p.start() < to) {
            if let Some(r) = p.resources() {
                n += 1;
                cpu += r.cpu_pct;
                mem += u128::from(r.mem_bytes);
                workers = workers.max(r.workers_active);
                approx |= r.approx;
            }
        }
        (n > 0).then(|| SiteResources {
            cpu_pct: cpu / f64::from(n),
            mem_bytes: u64::try_from(mem / u128::from(n)).unwrap_or(u64::MAX),
            approx,
            workers_active: workers,
        })
    }

    /// `true` when tier 0, the open buckets or a late accumulator have data in `[from, to)`.
    pub(crate) fn has_fine_data(&self, from: i64, to: i64) -> bool {
        let start = self.t0.partition_point(|x| x.start < from);
        self.t0.get(start).is_some_and(|x| x.start < to)
            || self.open.iter().any(|o| o.start >= from && o.start < to)
            || self
                .late
                .iter()
                .any(|l| l.start >= floor_to(from, MINUTE) && l.start < to)
    }

    /// Starts of the hour buckets in `[from, to)`.
    pub(crate) fn hour_starts(&self, from: i64, to: i64) -> Vec<i64> {
        self.hours
            .iter()
            .map(|h| h.start)
            .filter(|s| *s >= from && *s < to)
            .collect()
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        let accs = |v: &Vec<OpenAcc>| {
            v.capacity() * size_of::<OpenAcc>() + v.iter().map(OpenAcc::heap_bytes).sum::<usize>()
        };
        let buckets = |d: &VecDeque<Bucket>| {
            d.capacity() * size_of::<Bucket>() + d.iter().map(Bucket::heap_bytes).sum::<usize>()
        };
        accs(&self.open)
            + accs(&self.late)
            + buckets(&self.t0)
            + buckets(&self.hours)
            + self.points.capacity() * size_of::<Point>()
            + self.minutes.capacity() * size_of::<Minute>()
            + self.hist_bytes
    }
}

fn series_point(
    start: i64,
    c: &Counts,
    sampled: bool,
    rt: Option<Percentiles>,
    ut: Option<Percentiles>,
) -> SeriesPoint {
    SeriesPoint {
        start: secs_to_ts(start),
        requests: c.requests(),
        err4xx: u64::from(c.status[3]),
        err5xx: u64::from(c.status[4]),
        bytes: c.bytes,
        rt_sum_us: c.rt_sum,
        ut_sum_us: c.ut_sum,
        rt,
        ut,
        resources: None,
        sampled,
    }
}

/// A point built from a plan's totals.
pub(crate) fn totals_point(
    start: i64,
    t: &Totals,
    resources: Option<SiteResources>,
) -> SeriesPoint {
    let mut p = series_point(start, &t.counts, t.sampled, t.rt, t.ut);
    p.resources = resources;
    p
}
