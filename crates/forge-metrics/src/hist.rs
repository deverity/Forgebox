//! Latency histograms (docs/14 section 3.4): the HDR log-linear layout of the `hdrhistogram`
//! crate, stored sparsely so the thousands of histograms of the aggregator fit in a site's
//! memory budget, and merged exactly (counts add up index by index; percentiles are never
//! averaged).
//!
//! # Layout
//!
//! Values are microseconds, `0..=`[`HIGHEST_US`] (10 min); larger values are clamped to it.
//! [`LatencyHist`] has 3 significant digits: 2048 sub-buckets per power of two, so one index
//! spans at most 1/1024 of its value (exact below 2048 µs). It is the layout of
//! `Histogram::new_with_bounds(1, HIGHEST_US, 3)`, so converting to and from the crate, and
//! through it to the HDR V2 encoding stored in SQLite ([`FORMAT_VERSION`]), loses nothing.
//! Route histograms use 2 significant digits (256 sub-buckets, at most 1/128 of the value).
//!
//! # Encoding
//!
//! A [`LatencyHist`] is the list of its non-empty indexes in increasing order, each as one
//! LEB128 varint `gap << 1 | (count == 1)` (`gap` = indexes skipped since the previous one),
//! followed by a second varint `count - 2` when the count is not 1. Latencies logged with
//! millisecond resolution (nginx `$request_time`; Apache on Windows is coarser, on the
//! 15.6 ms clock tick) cost about 2 to 3 bytes per distinct millisecond; a dense array would
//! cost 168 KiB. Caddy logs below 1 ms, so almost every request is a distinct index at 3
//! significant digits and its histograms are several times larger (ADR-0048).
//!
//! # Percentiles
//!
//! Same definition as `Histogram::value_at_quantile`: the rank is `max(1, ceil(q * total))`
//! and the result is the highest value equivalent to the index holding that rank (the lowest
//! one for `q == 0`). With exact counts this is exactly
//! `highest_equivalent(sorted_values[rank - 1])`.

use std::fmt;

use hdrhistogram::Histogram;
use hdrhistogram::serialization::{Deserializer, Serializer, V2Serializer};

/// Largest value tracked, in microseconds (10 min). Larger values are recorded as this one.
pub const HIGHEST_US: u64 = 600_000_000;

/// Significant digits of [`LatencyHist`].
pub const SIGNIFICANT_DIGITS: u8 = 3;

/// Significant digits of the per-route histograms.
pub const ROUTE_SIGNIFICANT_DIGITS: u8 = 2;

/// Version of the persisted histogram format: HDR V2 (`hdrhistogram` `V2Serializer`, no
/// DEFLATE) of a histogram with lowest discernible value 1, highest trackable value
/// [`HIGHEST_US`] and [`SIGNIFICANT_DIGITS`], values in microseconds. Stored next to every
/// blob (`format_version` column) so a future layout can be told apart.
pub const FORMAT_VERSION: u32 = 1;

/// Index arithmetic of an HDR histogram with lowest discernible value 1 (unit magnitude 0)
/// and highest trackable value [`HIGHEST_US`], as in `hdrhistogram` 7.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Layout {
    /// log2 of half the number of sub-buckets.
    half_mag: u32,
    /// 64 - log2(sub-buckets): `bucket = base - leading_zeros(value | mask)`.
    lzc_base: u32,
    /// Sub-buckets - 1.
    mask: u64,
    /// Number of indexes.
    len: usize,
}

impl Layout {
    /// `sub_bucket_count_mag` = `ceil(log2(2 * 10^digits))`: 11 for 3 digits, 8 for 2.
    const fn new(sub_bucket_count_mag: u32) -> Self {
        let mut smallest_untrackable = 1u64 << sub_bucket_count_mag;
        let mut buckets = 1usize;
        while smallest_untrackable <= HIGHEST_US {
            smallest_untrackable <<= 1;
            buckets += 1;
        }
        Self {
            half_mag: sub_bucket_count_mag - 1,
            lzc_base: 64 - sub_bucket_count_mag,
            mask: (1u64 << sub_bucket_count_mag) - 1,
            len: (buckets + 1) << (sub_bucket_count_mag - 1),
        }
    }

    /// Number of indexes.
    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    /// Index of a value (clamped to [`HIGHEST_US`]).
    #[inline]
    pub(crate) fn index(&self, value: u64) -> usize {
        let v = if value > HIGHEST_US {
            HIGHEST_US
        } else {
            value
        };
        let bucket = self.lzc_base - (v | self.mask).leading_zeros();
        let sub = (v >> bucket) as usize;
        ((bucket as usize + 1) << self.half_mag) + sub - (1usize << self.half_mag)
    }

    #[inline]
    fn locate(&self, index: usize) -> (u32, u64) {
        let half = 1usize << self.half_mag;
        let sub = ((index & (half - 1)) + half) as u64;
        match (index >> self.half_mag).checked_sub(1) {
            Some(bucket) => (bucket as u32, sub),
            None => (0, sub - half as u64),
        }
    }

    /// Lowest value of an index.
    #[inline]
    pub(crate) fn lowest(&self, index: usize) -> u64 {
        let (bucket, sub) = self.locate(index);
        sub << bucket
    }

    /// Highest value of an index (`highest_equivalent`).
    #[inline]
    pub(crate) fn highest(&self, index: usize) -> u64 {
        let (bucket, sub) = self.locate(index);
        (sub << bucket) + (1u64 << bucket) - 1
    }

    /// The reported value of the index holding a quantile.
    #[inline]
    fn quantile_value(&self, index: usize, q: f64) -> u64 {
        if q == 0.0 {
            self.lowest(index)
        } else {
            self.highest(index)
        }
    }
}

/// 3 significant digits: the latency tiers.
pub(crate) const LATENCY: Layout = Layout::new(11);
/// 2 significant digits: the per-route histograms.
pub(crate) const ROUTE: Layout = Layout::new(8);

/// The quantiles reported everywhere (docs/14 section 4.1).
const QUANTILES: [f64; 3] = [0.5, 0.9, 0.99];

/// Rank of a quantile, as `Histogram::value_at_quantile` computes it.
#[inline]
fn rank(q: f64, total: u64) -> u64 {
    let q = if q > 1.0 { 1.0 } else { q };
    // Negative and NaN quantiles saturate to 0, then to the first recorded value.
    ((q * total as f64).ceil() as u64).max(1)
}

/// p50/p90/p99 of a histogram, in microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Percentiles {
    /// Median.
    pub p50_us: u64,
    /// 90th percentile.
    pub p90_us: u64,
    /// 99th percentile.
    pub p99_us: u64,
}

impl Percentiles {
    /// Walks `(index, count)` pairs in increasing index order.
    fn from_entries(
        layout: &Layout,
        total: u64,
        entries: impl Iterator<Item = (usize, u64)>,
    ) -> Option<Self> {
        if total == 0 {
            return None;
        }
        let ranks = QUANTILES.map(|q| rank(q, total));
        let mut out = [0u64; 3];
        let mut next = 0;
        let mut seen = 0u64;
        for (index, count) in entries {
            seen = seen.saturating_add(count);
            while next < 3 && seen >= ranks[next] {
                out[next] = layout.quantile_value(index, QUANTILES[next]);
                next += 1;
            }
            if next == 3 {
                break;
            }
        }
        Some(Self {
            p50_us: out[0],
            p90_us: out[1],
            p99_us: out[2],
        })
    }
}

#[inline]
fn put_varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

/// Only ever reads what [`Encoder`] wrote.
#[inline]
fn get_varint(data: &[u8], pos: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let b = data[*pos];
        *pos += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b < 0x80 {
            return v;
        }
        shift += 7;
    }
}

/// Builds a [`LatencyHist`] from `(index, count)` pairs in strictly increasing index order.
pub(crate) struct Encoder {
    buf: Vec<u8>,
    next: usize,
    total: u64,
}

impl Encoder {
    pub(crate) fn new() -> Self {
        Self {
            buf: Vec::new(),
            next: 0,
            total: 0,
        }
    }

    #[inline]
    pub(crate) fn push(&mut self, index: usize, count: u64) {
        debug_assert!(index >= self.next && count > 0);
        let gap = (index - self.next) as u64;
        if count == 1 {
            put_varint(&mut self.buf, (gap << 1) | 1);
        } else {
            put_varint(&mut self.buf, gap << 1);
            put_varint(&mut self.buf, count - 2);
        }
        self.next = index + 1;
        self.total = self.total.saturating_add(count);
    }

    pub(crate) fn finish(self) -> LatencyHist {
        LatencyHist {
            data: self.buf.into_boxed_slice(),
            total: self.total,
        }
    }
}

/// `(index, count)` pairs of a [`LatencyHist`], in increasing index order.
pub(crate) struct Entries<'a> {
    data: &'a [u8],
    pos: usize,
    next: usize,
}

impl Iterator for Entries<'_> {
    type Item = (usize, u64);

    #[inline]
    fn next(&mut self) -> Option<(usize, u64)> {
        if self.pos >= self.data.len() {
            return None;
        }
        let head = get_varint(self.data, &mut self.pos);
        let index = self.next + (head >> 1) as usize;
        let count = if head & 1 == 1 {
            1
        } else {
            get_varint(self.data, &mut self.pos) + 2
        };
        self.next = index + 1;
        Some((index, count))
    }
}

/// An immutable latency histogram with 3 significant digits, stored sparsely (see the
/// module docs). Cheap to clone when empty; merging is exact.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct LatencyHist {
    data: Box<[u8]>,
    total: u64,
}

impl fmt::Debug for LatencyHist {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LatencyHist")
            .field("total", &self.total)
            .field("bytes", &self.data.len())
            .finish()
    }
}

/// A histogram that cannot be read as a [`LatencyHist`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HistError {
    /// Another lowest discernible value or precision.
    #[error(
        "histogram layout differs: lowest discernible value {lowest}, {digits} significant digits (expected 1 and {SIGNIFICANT_DIGITS})"
    )]
    Layout {
        /// Lowest discernible value of the source.
        lowest: u64,
        /// Significant digits of the source.
        digits: u8,
    },
    /// The V2 encoder refused the histogram.
    #[error("cannot encode histogram: {0}")]
    Encode(String),
    /// Not a V2 histogram (or a truncated one).
    #[error("cannot decode histogram: {0}")]
    Decode(String),
}

impl LatencyHist {
    /// An empty histogram (no allocation).
    pub fn new() -> Self {
        Self::default()
    }

    /// A histogram of these values (microseconds), each counted once.
    pub fn from_values(values: impl IntoIterator<Item = u64>) -> Self {
        let mut indexes: Vec<u32> = values
            .into_iter()
            .map(|v| LATENCY.index(v) as u32)
            .collect();
        indexes.sort_unstable();
        let mut enc = Encoder::new();
        let mut iter = indexes.into_iter().peekable();
        while let Some(index) = iter.next() {
            let mut count = 1u64;
            while iter.peek() == Some(&index) {
                iter.next();
                count += 1;
            }
            enc.push(index as usize, count);
        }
        enc.finish()
    }

    /// Number of recorded values.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// `true` when nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Bytes of the encoded form (heap).
    pub fn encoded_len(&self) -> usize {
        self.data.len()
    }

    pub(crate) fn entries(&self) -> Entries<'_> {
        Entries {
            data: &self.data,
            pos: 0,
            next: 0,
        }
    }

    /// `(highest equivalent value, count)` of every non-empty index, in increasing order
    /// (what `Histogram::iter_recorded` yields as `value_iterated_to` and `count_at_value`).
    pub fn recorded(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.entries().map(|(i, c)| (LATENCY.highest(i), c))
    }

    /// The value at a quantile (`0.0..=1.0`), `None` when empty.
    pub fn quantile(&self, q: f64) -> Option<u64> {
        if self.total == 0 {
            return None;
        }
        let rank = rank(q, self.total);
        let mut seen = 0u64;
        for (index, count) in self.entries() {
            seen = seen.saturating_add(count);
            if seen >= rank {
                return Some(LATENCY.quantile_value(index, q));
            }
        }
        None
    }

    /// p50/p90/p99, `None` when empty.
    pub fn percentiles(&self) -> Option<Percentiles> {
        Percentiles::from_entries(&LATENCY, self.total, self.entries())
    }

    /// Highest equivalent value of the largest recorded value.
    pub fn max(&self) -> Option<u64> {
        self.entries().last().map(|(i, _)| LATENCY.highest(i))
    }

    /// Exact merge: the histogram of both sets of values.
    pub fn merge(&self, other: &Self) -> Self {
        if other.is_empty() {
            return self.clone();
        }
        if self.is_empty() {
            return other.clone();
        }
        let mut enc = Encoder::new();
        enc.buf.reserve(self.data.len().max(other.data.len()) + 8);
        let mut a = self.entries().peekable();
        let mut b = other.entries().peekable();
        loop {
            match (a.peek().copied(), b.peek().copied()) {
                (Some((ia, ca)), Some((ib, cb))) => {
                    if ia < ib {
                        enc.push(ia, ca);
                        a.next();
                    } else if ib < ia {
                        enc.push(ib, cb);
                        b.next();
                    } else {
                        enc.push(ia, ca.saturating_add(cb));
                        a.next();
                        b.next();
                    }
                }
                (Some((ia, ca)), None) => {
                    enc.push(ia, ca);
                    a.next();
                }
                (None, Some((ib, cb))) => {
                    enc.push(ib, cb);
                    b.next();
                }
                (None, None) => break,
            }
        }
        enc.finish()
    }

    /// The same histogram as an `hdrhistogram` one (`new_with_bounds(1, HIGHEST_US, 3)`).
    pub fn to_hdr(&self) -> Histogram<u64> {
        let mut h = Histogram::<u64>::new_with_bounds(1, HIGHEST_US, SIGNIFICANT_DIGITS)
            .expect("constant bounds are valid");
        for (index, count) in self.entries() {
            // Every index is at most `index(HIGHEST_US)`, inside the counts array.
            h.record_n(LATENCY.lowest(index), count)
                .expect("value within the histogram range");
        }
        h
    }

    /// Reads an `hdrhistogram` histogram with the same layout (lowest discernible value 1,
    /// 3 significant digits). Values above [`HIGHEST_US`] are clamped to it.
    pub fn from_hdr(h: &Histogram<u64>) -> Result<Self, HistError> {
        if h.low() != 1 || h.sigfig() != SIGNIFICANT_DIGITS {
            return Err(HistError::Layout {
                lowest: h.low(),
                digits: h.sigfig(),
            });
        }
        let mut enc = Encoder::new();
        let mut pending: Option<(usize, u64)> = None;
        for v in h.iter_recorded() {
            let index = LATENCY.index(v.value_iterated_to());
            let count = v.count_at_value();
            pending = match pending {
                Some((i, c)) if i == index => Some((i, c.saturating_add(count))),
                Some((i, c)) => {
                    enc.push(i, c);
                    Some((index, count))
                }
                None => Some((index, count)),
            };
        }
        if let Some((i, c)) = pending {
            enc.push(i, c);
        }
        Ok(enc.finish())
    }

    /// HDR V2 encoding (no DEFLATE) of [`Self::to_hdr`]: the persisted form, see
    /// [`FORMAT_VERSION`].
    pub fn to_v2(&self) -> Result<Vec<u8>, HistError> {
        let mut out = Vec::new();
        V2Serializer::new()
            .serialize(&self.to_hdr(), &mut out)
            .map_err(|e| HistError::Encode(format!("{e:?}")))?;
        Ok(out)
    }

    /// Reads [`Self::to_v2`] (any V2 histogram with the same layout).
    pub fn from_v2(mut bytes: &[u8]) -> Result<Self, HistError> {
        let h: Histogram<u64> = Deserializer::new()
            .deserialize(&mut bytes)
            .map_err(|e| HistError::Decode(format!("{e:?}")))?;
        Self::from_hdr(&h)
    }
}

/// Packs a raw sample: index in the high 16 bits, weight (1..=65535) in the low 16 bits, so
/// sorting groups equal indexes.
#[inline]
pub(crate) fn push_raw(buf: &mut Vec<u32>, index: usize, mut weight: u64) {
    debug_assert!(index < 1 << 16);
    while weight > 0 {
        let w = weight.min(0xffff);
        buf.push(((index as u32) << 16) | w as u32);
        weight -= w;
    }
}

/// Sorts raw samples ([`push_raw`]) and encodes them.
pub(crate) fn compact_raw(raw: &mut [u32]) -> LatencyHist {
    raw.sort_unstable();
    let mut enc = Encoder::new();
    let mut current: Option<(usize, u64)> = None;
    for &r in raw.iter() {
        let index = (r >> 16) as usize;
        let w = u64::from(r & 0xffff);
        current = match current {
            Some((i, c)) if i == index => Some((i, c + w)),
            Some((i, c)) => {
                enc.push(i, c);
                Some((index, w))
            }
            None => Some((index, w)),
        };
    }
    if let Some((i, c)) = current {
        enc.push(i, c);
    }
    enc.finish()
}

/// Dense scratch histogram for merges and queries: one per aggregator, reused (168 KiB).
/// Tracks the touched range so clearing and scanning stay proportional to the data.
pub(crate) struct Dense {
    counts: Vec<u64>,
    lo: usize,
    hi: usize,
    total: u64,
}

impl Dense {
    pub(crate) fn new() -> Self {
        Self {
            counts: vec![0; LATENCY.len()],
            lo: usize::MAX,
            hi: 0,
            total: 0,
        }
    }

    #[inline]
    pub(crate) fn add_at(&mut self, index: usize, count: u64) {
        let c = &mut self.counts[index];
        *c = c.saturating_add(count);
        self.lo = self.lo.min(index);
        self.hi = self.hi.max(index + 1);
        self.total = self.total.saturating_add(count);
    }

    pub(crate) fn add(&mut self, h: &LatencyHist) {
        for (index, count) in h.entries() {
            self.add_at(index, count);
        }
    }

    pub(crate) fn add_raw(&mut self, raw: &[u32]) {
        for &r in raw {
            self.add_at((r >> 16) as usize, u64::from(r & 0xffff));
        }
    }

    fn touched(&self) -> impl Iterator<Item = (usize, u64)> + '_ {
        let (lo, hi) = if self.lo < self.hi {
            (self.lo, self.hi)
        } else {
            (0, 0)
        };
        self.counts[lo..hi]
            .iter()
            .enumerate()
            .filter(|(_, c)| **c > 0)
            .map(move |(i, c)| (lo + i, *c))
    }

    pub(crate) fn percentiles(&self) -> Option<Percentiles> {
        Percentiles::from_entries(&LATENCY, self.total, self.touched())
    }

    pub(crate) fn to_hist(&self) -> LatencyHist {
        let mut enc = Encoder::new();
        for (index, count) in self.touched() {
            enc.push(index, count);
        }
        enc.finish()
    }

    pub(crate) fn clear(&mut self) {
        if self.lo < self.hi {
            self.counts[self.lo..self.hi].fill(0);
        }
        self.lo = usize::MAX;
        self.hi = 0;
        self.total = 0;
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.counts.capacity() * 8
    }
}

const COUNT_MASK: u64 = (1 << 48) - 1;

/// Per-route histogram, 2 significant digits, updated in place: sorted packed entries
/// (`index << 48 | count`), so a route with a few hundred distinct latencies costs a few KiB.
#[derive(Debug, Clone, Default)]
pub(crate) struct RouteHist {
    entries: Vec<u64>,
}

impl RouteHist {
    #[inline]
    pub(crate) fn record(&mut self, value: u64, count: u64) {
        self.add_index(ROUTE.index(value) as u64, count);
    }

    #[inline]
    fn add_index(&mut self, index: u64, count: u64) {
        match self.entries.binary_search_by(|e| (e >> 48).cmp(&index)) {
            Ok(pos) => {
                let e = &mut self.entries[pos];
                let c = (*e & COUNT_MASK).saturating_add(count).min(COUNT_MASK);
                *e = (index << 48) | c;
            }
            Err(pos) => {
                if self.entries.len() == self.entries.capacity() {
                    // Grow by a quarter: these vectors live for hours, doubling wastes memory.
                    let extra = (self.entries.len() / 4).max(4);
                    self.entries.reserve_exact(extra);
                }
                self.entries
                    .insert(pos, (index << 48) | count.min(COUNT_MASK));
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(crate) fn total(&self) -> u64 {
        self.entries.iter().map(|e| e & COUNT_MASK).sum()
    }

    pub(crate) fn merge_from(&mut self, other: &RouteHist) {
        for &e in &other.entries {
            self.add_index(e >> 48, e & COUNT_MASK);
        }
    }

    pub(crate) fn percentiles(&self) -> Option<Percentiles> {
        Percentiles::from_entries(
            &ROUTE,
            self.total(),
            self.entries
                .iter()
                .map(|e| ((e >> 48) as usize, e & COUNT_MASK)),
        )
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.entries.capacity() * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic values over the whole range: dense at the bottom, spread above.
    fn values() -> Vec<u64> {
        let mut out: Vec<u64> = (0..5000).collect();
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..200_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            // Log-uniform over 1 µs .. 2^31 µs (beyond HIGHEST_US on purpose).
            let bits = (x >> 59) as u32 + 1;
            out.push(x >> (64 - bits));
        }
        out.extend([HIGHEST_US - 1, HIGHEST_US, HIGHEST_US + 1, u64::MAX]);
        out
    }

    #[test]
    fn layouts_have_the_crate_sizes() {
        let h3 = Histogram::<u64>::new_with_bounds(1, HIGHEST_US, 3).unwrap();
        let h2 = Histogram::<u64>::new_with_bounds(1, HIGHEST_US, 2).unwrap();
        assert_eq!(LATENCY.len(), h3.distinct_values());
        assert_eq!(ROUTE.len(), h2.distinct_values());
        assert_eq!(LATENCY.len(), 21_504);
        assert_eq!(ROUTE.len(), 3_072);
        assert!(LATENCY.index(u64::MAX) < LATENCY.len());
        assert!(LATENCY.index(u64::MAX) < 1 << 16);
    }

    #[test]
    fn index_and_equivalents_match_hdrhistogram() {
        for (layout, digits) in [(LATENCY, 3), (ROUTE, 2)] {
            let h = Histogram::<u64>::new_with_bounds(1, HIGHEST_US, digits).unwrap();
            for v in values().into_iter().filter(|v| *v <= HIGHEST_US) {
                let i = layout.index(v);
                assert_eq!(layout.lowest(i), h.lowest_equivalent(v), "lowest of {v}");
                assert_eq!(layout.highest(i), h.highest_equivalent(v), "highest of {v}");
                assert!(layout.lowest(i) <= v && v <= layout.highest(i));
                assert_eq!(layout.index(layout.highest(i)), i);
                assert_eq!(layout.index(layout.lowest(i)), i);
            }
        }
    }

    #[test]
    fn encoding_round_trips_and_matches_the_crate() {
        let vals = values();
        let hist = LatencyHist::from_values(vals.iter().copied());
        assert_eq!(hist.total(), vals.len() as u64);
        let mut crate_h = Histogram::<u64>::new_with_bounds(1, HIGHEST_US, 3).unwrap();
        for &v in &vals {
            crate_h.record(v.min(HIGHEST_US)).unwrap();
        }
        let ours: Vec<_> = hist.recorded().collect();
        let theirs: Vec<_> = crate_h
            .iter_recorded()
            .map(|v| (v.value_iterated_to(), v.count_at_value()))
            .collect();
        assert_eq!(ours, theirs);
        for q in [0.0, 0.001, 0.25, 0.5, 0.9, 0.99, 0.999, 1.0] {
            assert_eq!(
                hist.quantile(q),
                Some(crate_h.value_at_quantile(q)),
                "q={q}"
            );
        }
        assert_eq!(hist.max(), Some(crate_h.max()));
        assert_eq!(hist.to_hdr(), crate_h);
        assert_eq!(LatencyHist::from_hdr(&crate_h).unwrap(), hist);
        let v2 = hist.to_v2().unwrap();
        assert_eq!(LatencyHist::from_v2(&v2).unwrap(), hist);
    }

    #[test]
    fn merge_is_exact_and_associative() {
        let vals = values();
        let (a, b) = vals.split_at(vals.len() / 3);
        let (b, c) = b.split_at(b.len() / 2);
        let ha = LatencyHist::from_values(a.iter().copied());
        let hb = LatencyHist::from_values(b.iter().copied());
        let hc = LatencyHist::from_values(c.iter().copied());
        let all = LatencyHist::from_values(vals.iter().copied());
        assert_eq!(ha.merge(&hb).merge(&hc), all);
        assert_eq!(hc.merge(&ha.merge(&hb)), all);
        assert_eq!(ha.merge(&LatencyHist::new()), ha);
        assert_eq!(LatencyHist::new().merge(&ha), ha);
    }

    #[test]
    fn raw_samples_and_dense_agree_with_from_values() {
        let vals = values();
        let mut raw = Vec::new();
        let mut dense = Dense::new();
        for &v in &vals {
            push_raw(&mut raw, LATENCY.index(v), 1);
            dense.add_at(LATENCY.index(v), 1);
        }
        let expected = LatencyHist::from_values(vals.iter().copied());
        assert_eq!(compact_raw(&mut raw), expected);
        assert_eq!(dense.to_hist(), expected);
        assert_eq!(dense.percentiles(), expected.percentiles());
        dense.clear();
        assert_eq!(dense.total, 0);
        assert!(dense.counts.iter().all(|c| *c == 0));
        // Weights above 16 bits are split.
        let mut raw = Vec::new();
        push_raw(&mut raw, 7, 200_000);
        assert_eq!(compact_raw(&mut raw).total(), 200_000);
    }

    #[test]
    fn other_layouts_are_rejected() {
        let h = Histogram::<u64>::new_with_bounds(1, HIGHEST_US, 2).unwrap();
        assert!(matches!(
            LatencyHist::from_hdr(&h),
            Err(HistError::Layout { digits: 2, .. })
        ));
        assert!(matches!(
            LatencyHist::from_v2(b"not a histogram"),
            Err(HistError::Decode(_))
        ));
    }

    #[test]
    fn route_hist_matches_the_crate_at_two_digits() {
        let vals = values();
        let mut ours = RouteHist::default();
        let mut theirs = Histogram::<u64>::new_with_bounds(1, HIGHEST_US, 2).unwrap();
        for &v in &vals {
            ours.record(v, 1);
            theirs.record(v.min(HIGHEST_US)).unwrap();
        }
        let p = ours.percentiles().unwrap();
        assert_eq!(p.p50_us, theirs.value_at_quantile(0.5));
        assert_eq!(p.p90_us, theirs.value_at_quantile(0.9));
        assert_eq!(p.p99_us, theirs.value_at_quantile(0.99));
        assert_eq!(ours.total(), vals.len() as u64);
        let mut merged = RouteHist::default();
        merged.merge_from(&ours);
        merged.merge_from(&ours);
        assert_eq!(merged.total(), 2 * vals.len() as u64);
        assert_eq!(merged.percentiles(), ours.percentiles());
    }
}
