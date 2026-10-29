//! CPU/RAM attribution per vhost (docs/14 section 2, M2-E5-T6). Pure: no I/O, no clocks.
//!
//! Input is one bucket (10 s in production, any length here) with what the process samplers
//! saw ([`PoolSample`], [`AppSample`]), which site pointed at which pool and when
//! ([`PoolRoute`]) and how much upstream time each site used ([`SiteLoad`]); output is a
//! [`SiteResources`] per site plus the cost that belongs to nobody ([`Overhead`]).
//!
//! Rules:
//! - **Dedicated pool** and **app processes** are exact (`approx = false`): the whole cost
//!   goes to the owner.
//! - **Shared pool**: CPU and memory are split by each site's share of the upstream time
//!   (`ut_sum`, falling back to `rt_sum` when the site has no upstream time), `approx = true`.
//! - **Idle pool time**: CPU is only spent while serving, so what is attributed is what the
//!   pool burned; a shared pool that served nothing in the bucket (zero total weight)
//!   attributes nothing, and its CPU and memory are reported as [`Overhead`].
//! - **Moves inside the bucket** (blue/green swap, `pool_mode` change) are attributed by
//!   time: a pool sample is scaled by the fraction of the bucket the pool was alive, and a
//!   site's load is spread over the pools it pointed at in proportion to the time on each.
//!
//! `cpu_pct` is a share of the whole machine (0-100), like the supervisor's; `mem_bytes` is
//! the time-weighted working set over the bucket.

use std::collections::BTreeMap;

/// Whose pool it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolMode {
    /// Workers shared by every site routed to the pool.
    Shared,
    /// Exclusive pool of one site.
    Dedicated(String),
}

/// What a pool cost while it was alive inside the bucket.
#[derive(Debug, Clone, PartialEq)]
pub struct PoolSample {
    /// Pool identity (`php_8_3` or the version id); matches [`PoolRoute::pool`].
    pub pool: String,
    pub mode: PoolMode,
    /// CPU share of the machine averaged over the time the pool was alive in the bucket.
    pub cpu_pct: f64,
    /// Working set of the pool's workers.
    pub mem_bytes: u64,
    pub workers_active: u32,
    /// Milliseconds from the start of the bucket where the pool existed.
    pub from_ms: u64,
    pub to_ms: u64,
}

/// Processes of a site (`web`, `queue`...): own pids, so always exact.
#[derive(Debug, Clone, PartialEq)]
pub struct AppSample {
    pub site: String,
    pub cpu_pct: f64,
    pub mem_bytes: u64,
}

/// A site pointed at a pool from `from_ms` to `to_ms` of the bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolRoute {
    pub site: String,
    pub pool: String,
    pub from_ms: u64,
    pub to_ms: u64,
}

/// Time the site's requests spent in the upstream during the bucket, in microseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SiteLoad {
    pub ut_sum_us: u64,
    /// Fallback when the log has no upstream time.
    pub rt_sum_us: u64,
}

impl SiteLoad {
    fn weight(&self) -> u64 {
        if self.ut_sum_us > 0 {
            self.ut_sum_us
        } else {
            self.rt_sum_us
        }
    }
}

/// Everything known about one bucket.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bucket {
    pub length_ms: u64,
    pub pools: Vec<PoolSample>,
    pub apps: Vec<AppSample>,
    pub routes: Vec<PoolRoute>,
    pub loads: BTreeMap<String, SiteLoad>,
}

/// Resources attributed to one site in a bucket.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SiteResources {
    pub cpu_pct: f64,
    pub mem_bytes: u64,
    /// Some of it comes from a shared pool (shown as an estimate).
    pub approx: bool,
    pub workers_active: u32,
}

/// Cost of a shared pool that no site can be charged for.
#[derive(Debug, Clone, PartialEq)]
pub struct Overhead {
    pub pool: String,
    pub cpu_pct: f64,
    pub mem_bytes: u64,
}

/// Result of [`attribute`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Attribution {
    pub sites: BTreeMap<String, SiteResources>,
    pub overhead: Vec<Overhead>,
}

fn overlap(from: u64, to: u64, lo: u64, hi: u64) -> u64 {
    to.min(hi).saturating_sub(from.max(lo))
}

/// Running totals of one site (memory kept as float until the end to avoid rounding drift).
#[derive(Default)]
struct Acc {
    cpu: f64,
    mem: f64,
    approx: bool,
    workers: u32,
}

/// Attributes one bucket.
pub fn attribute(bucket: &Bucket) -> Attribution {
    let mut out = Attribution::default();
    let length = bucket.length_ms;
    if length == 0 {
        return out;
    }
    let mut acc: BTreeMap<String, Acc> = BTreeMap::new();
    let mut add = |site: &str, cpu: f64, mem: f64, approx: bool, workers: u32| {
        let entry = acc.entry(site.to_owned()).or_default();
        entry.cpu += cpu;
        entry.mem += mem;
        entry.approx |= approx;
        entry.workers += workers;
    };

    for app in &bucket.apps {
        add(&app.site, app.cpu_pct, app.mem_bytes as f64, false, 0);
    }

    // Total routed time per site, to spread its load over the pools it used.
    let mut routed_total: BTreeMap<&str, u64> = BTreeMap::new();
    for r in &bucket.routes {
        *routed_total.entry(r.site.as_str()).or_default() += overlap(r.from_ms, r.to_ms, 0, length);
    }

    for pool in &bucket.pools {
        let alive = overlap(pool.from_ms, pool.to_ms, 0, length);
        if alive == 0 {
            continue;
        }
        let scale = alive as f64 / length as f64;
        let cpu = pool.cpu_pct * scale;
        let mem = pool.mem_bytes as f64 * scale;
        match &pool.mode {
            PoolMode::Dedicated(site) => add(site, cpu, mem, false, pool.workers_active),
            PoolMode::Shared => {
                let mut weights: BTreeMap<&str, f64> = BTreeMap::new();
                for r in bucket.routes.iter().filter(|r| r.pool == pool.pool) {
                    let on_pool = overlap(r.from_ms, r.to_ms, 0, length);
                    let total = routed_total.get(r.site.as_str()).copied().unwrap_or(0);
                    if on_pool == 0 || total == 0 {
                        continue;
                    }
                    let load = bucket.loads.get(&r.site).map_or(0, SiteLoad::weight);
                    *weights.entry(r.site.as_str()).or_default() +=
                        load as f64 * on_pool as f64 / total as f64;
                }
                let sum: f64 = weights.values().sum();
                if sum <= 0.0 {
                    out.overhead.push(Overhead {
                        pool: pool.pool.clone(),
                        cpu_pct: cpu,
                        mem_bytes: mem.round() as u64,
                    });
                    continue;
                }
                for (site, weight) in weights {
                    if weight <= 0.0 {
                        continue;
                    }
                    let share = weight / sum;
                    let workers = ((f64::from(pool.workers_active) * share).round() as u32).max(1);
                    add(site, cpu * share, mem * share, true, workers);
                }
            }
        }
    }
    for (site, a) in acc {
        out.sites.insert(
            site,
            SiteResources {
                cpu_pct: a.cpu,
                mem_bytes: a.mem.round() as u64,
                approx: a.approx,
                workers_active: a.workers,
            },
        );
    }
    out
}
