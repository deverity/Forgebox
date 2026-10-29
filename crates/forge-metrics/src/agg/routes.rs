//! Top routes of a site ("Rotas mais lentas", docs/14 section 4.1): space-saving top-K with an
//! admission filter (Filtered Space-Saving, Homem & Carvalho 2010), K = 100 monitored
//! `(method, route)` pairs, each with request/error counts and a 2-significant-digit latency
//! histogram. In memory only.
//!
//! Plain space-saving replaces the least counted entry on every miss, so with a long tail of
//! rare routes the minimum counter climbs at the rate of the tail and evicts moderately
//! popular routes (with a Zipf(1) over 10,000 routes and K = 100, ranks from about 13 on keep
//! getting evicted). The filter is an array of 1,024 counters indexed by the route hash: a
//! miss only enters the table when its cell plus its weight reaches the minimum counter;
//! otherwise it only bumps the cell. When an entry is evicted its count goes to its cell, so
//! a cell is always an upper bound of the count of any unmonitored route that hashes to it.
//! Counts stay upper bounds and `count - overcount` lower bounds, as in space-saving.
//!
//! The table covers two generations of `epoch` (12 h by default): a report merges the current
//! and the previous one, so it spans between one and two epochs, and a site that goes idle
//! frees it all after two epochs.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, BuildHasherDefault, Hasher};

use jiff::Timestamp;

use super::secs_to_ts;
use crate::hist::{Percentiles, RouteHist};

const FILTER_CELLS: usize = 1024;
const FILTER_SHIFT: u32 = 64 - FILTER_CELLS.trailing_zeros();

/// One monitored route, merged over the generations in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteStats {
    /// Request method (`GET`, `POST`, `OTHER`; empty when the format has none).
    pub method: String,
    /// Normalized route (`/users/{id}`), never with a query string.
    pub route: String,
    /// Estimated requests: an upper bound of the true count.
    pub count: u64,
    /// How much `count` may overestimate (`count - overcount` is a lower bound).
    pub overcount: u64,
    /// Requests actually observed since the route entered the table (what the histogram,
    /// the error counts and `rt_sum_us` cover).
    pub observed: u64,
    /// Observed 4xx responses.
    pub err4xx: u64,
    /// Observed 5xx responses.
    pub err5xx: u64,
    /// Sum of the observed durations, in microseconds (mean = `rt_sum_us / observed`).
    pub rt_sum_us: u64,
    /// p50/p90/p99 of the observed durations, 2 significant digits.
    pub rt: Option<Percentiles>,
}

/// The top routes of a site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutesReport {
    /// Start of the oldest generation in the report.
    pub since: Timestamp,
    /// Requests seen (weighted) since `since`, monitored or not.
    pub total: u64,
    /// At most K routes, by decreasing `count`.
    pub routes: Vec<RouteStats>,
}

/// Identity hasher for keys that already are 64-bit hashes.
#[derive(Default)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(b);
        }
    }

    fn write_u64(&mut self, v: u64) {
        self.0 = v;
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

#[derive(Debug)]
struct Slot {
    hash: u64,
    /// `method` then `route`.
    key: Box<str>,
    method_len: usize,
    overcount: u64,
    err4xx: u64,
    err5xx: u64,
    rt_sum: u64,
    hist: RouteHist,
}

impl Slot {
    fn matches(&self, method: &str, route: &str) -> bool {
        self.method_len == method.len()
            && self.key.len() == method.len() + route.len()
            && self.key.starts_with(method)
            && self.key.ends_with(route)
    }

    fn method(&self) -> &str {
        &self.key[..self.method_len]
    }

    fn route(&self) -> &str {
        &self.key[self.method_len..]
    }

    fn reset(&mut self, hash: u64, method: &str, route: &str, overcount: u64) {
        let mut key = String::with_capacity(method.len() + route.len());
        key.push_str(method);
        key.push_str(route);
        self.hash = hash;
        self.key = key.into_boxed_str();
        self.method_len = method.len();
        self.overcount = overcount;
        self.err4xx = 0;
        self.err5xx = 0;
        self.rt_sum = 0;
        self.hist.clear();
    }

    #[inline]
    fn observe(&mut self, w: u64, duration_us: u64, status: u16) {
        self.hist.record(duration_us, w);
        self.rt_sum = self.rt_sum.saturating_add(duration_us.saturating_mul(w));
        match status {
            400..=499 => self.err4xx += w,
            500..=599 => self.err5xx += w,
            _ => {}
        }
    }

    fn heap_bytes(&self) -> usize {
        self.key.len() + self.hist.heap_bytes()
    }
}

#[derive(Debug, Default)]
struct Generation {
    since: i64,
    total: u64,
    /// Estimated count per slot, apart from the slots for a fast minimum scan.
    counts: Vec<u64>,
    slots: Vec<Slot>,
    index: HashMap<u64, u16, BuildHasherDefault<IdHasher>>,
    filter: Option<Box<[u32]>>,
}

impl Generation {
    fn new(since: i64) -> Self {
        Self {
            since,
            ..Self::default()
        }
    }

    fn heap_bytes(&self) -> usize {
        self.counts.capacity() * 8
            + self.slots.capacity() * size_of::<Slot>()
            + self.slots.iter().map(Slot::heap_bytes).sum::<usize>()
            // hashbrown: one control byte and one (u64, u16) bucket per slot.
            + self.index.capacity() * (1 + 16)
            + self.filter.as_ref().map_or(0, |f| f.len() * 4)
    }
}

#[derive(Debug)]
pub(crate) struct RouteTable {
    hasher: RandomState,
    current: Generation,
    previous: Option<Generation>,
}

impl RouteTable {
    pub(crate) fn new(now: i64) -> Self {
        Self {
            hasher: RandomState::new(),
            current: Generation::new(now),
            previous: None,
        }
    }

    /// Starts a new generation every `epoch` seconds; the one before the previous is dropped.
    pub(crate) fn rotate(&mut self, now: i64, epoch: i64) {
        if now - self.current.since >= epoch {
            // A generation that stayed empty is not worth keeping as `previous`.
            let old = std::mem::replace(&mut self.current, Generation::new(now));
            self.previous = (old.total > 0 && now - old.since < 2 * epoch).then_some(old);
        }
    }

    #[inline]
    pub(crate) fn record(
        &mut self,
        k: usize,
        method: &str,
        route: &str,
        w: u64,
        duration_us: u64,
        status: u16,
    ) {
        let hash = {
            let mut h = self.hasher.build_hasher();
            h.write(method.as_bytes());
            // 0xff never occurs in UTF-8: `GE` + `T/x` and `GET` + `/x` hash apart.
            h.write_u8(0xff);
            h.write(route.as_bytes());
            h.finish()
        };
        let g = &mut self.current;
        g.total = g.total.saturating_add(w);
        if let Some(&slot) = g.index.get(&hash) {
            let slot = usize::from(slot);
            if g.slots[slot].matches(method, route) {
                g.counts[slot] = g.counts[slot].saturating_add(w);
                g.slots[slot].observe(w, duration_us, status);
            }
            // Else a 64-bit hash collision (~1e-17 per pair): the request is not tracked.
            return;
        }
        if g.slots.len() < k {
            if g.slots.len() == g.slots.capacity() {
                let extra = (g.slots.len().max(8)).min(k - g.slots.len());
                g.slots.reserve_exact(extra);
                g.counts.reserve_exact(extra);
            }
            let mut slot = Slot {
                hash,
                key: Box::default(),
                method_len: 0,
                overcount: 0,
                err4xx: 0,
                err5xx: 0,
                rt_sum: 0,
                hist: RouteHist::default(),
            };
            slot.reset(hash, method, route, 0);
            slot.observe(w, duration_us, status);
            g.index.insert(hash, g.slots.len() as u16);
            g.slots.push(slot);
            g.counts.push(w);
            return;
        }
        if k == 0 {
            return;
        }
        let filter = g
            .filter
            .get_or_insert_with(|| vec![0u32; FILTER_CELLS].into_boxed_slice());
        let cell = (hash >> FILTER_SHIFT) as usize;
        let (min_slot, min) = g
            .counts
            .iter()
            .copied()
            .enumerate()
            .min_by_key(|&(_, c)| c)
            .expect("a full table is not empty");
        let alpha = u64::from(filter[cell]);
        let candidate = alpha.saturating_add(w);
        if candidate >= min {
            let evicted = &mut g.slots[min_slot];
            let evicted_cell = (evicted.hash >> FILTER_SHIFT) as usize;
            let kept = u32::try_from(min).unwrap_or(u32::MAX);
            filter[evicted_cell] = filter[evicted_cell].max(kept);
            g.index.remove(&evicted.hash);
            evicted.reset(hash, method, route, alpha);
            evicted.observe(w, duration_us, status);
            g.counts[min_slot] = candidate;
            g.index.insert(hash, min_slot as u16);
        } else {
            let w32 = u32::try_from(w).unwrap_or(u32::MAX);
            filter[cell] = filter[cell].saturating_add(w32);
        }
    }

    pub(crate) fn report(&self, k: usize) -> RoutesReport {
        let mut merged: HashMap<(&str, &str), (RouteStats, RouteHist)> = HashMap::new();
        let gens = self.previous.iter().chain(std::iter::once(&self.current));
        let mut total = 0u64;
        let mut since = self.current.since;
        for g in gens {
            total = total.saturating_add(g.total);
            since = since.min(g.since);
            for (slot, &count) in g.slots.iter().zip(&g.counts) {
                let e = merged
                    .entry((slot.method(), slot.route()))
                    .or_insert_with(|| {
                        (
                            RouteStats {
                                method: slot.method().to_string(),
                                route: slot.route().to_string(),
                                count: 0,
                                overcount: 0,
                                observed: 0,
                                err4xx: 0,
                                err5xx: 0,
                                rt_sum_us: 0,
                                rt: None,
                            },
                            RouteHist::default(),
                        )
                    });
                e.0.count = e.0.count.saturating_add(count);
                e.0.overcount = e.0.overcount.saturating_add(slot.overcount);
                e.0.err4xx += slot.err4xx;
                e.0.err5xx += slot.err5xx;
                e.0.rt_sum_us = e.0.rt_sum_us.saturating_add(slot.rt_sum);
                e.1.merge_from(&slot.hist);
            }
        }
        let mut routes: Vec<RouteStats> = merged
            .into_values()
            .map(|(mut stats, hist)| {
                stats.observed = hist.total();
                stats.rt = hist.percentiles();
                stats
            })
            .collect();
        routes.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.method.cmp(&b.method))
                .then_with(|| a.route.cmp(&b.route))
        });
        routes.truncate(k);
        RoutesReport {
            since: secs_to_ts(since),
            total,
            routes,
        }
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.current.heap_bytes() + self.previous.as_ref().map_or(0, Generation::heap_bytes)
    }
}
