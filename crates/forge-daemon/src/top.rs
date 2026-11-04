//! `stats.top` (docs/14 section 4.2): every process the Forgebox runs, with its owner, and
//! the machine against the Forgebox share of it.
//!
//! The daemon keeps no extra sampler: instance, pool and app processes come from the CPU/RSS
//! samples their actors already take every 2 s (`InstanceMetrics`, `PoolStatus.workers`),
//! image names from [`ImageNameCache`] (by `(pid, started_at)`, filled inside
//! `spawn_blocking`), and the daemon itself from [`ProcessStats`] between two calls. The
//! machine CPU is the busy share between the previous call and this one
//! ([`MachineSample::cpu_pct_since`]); the first call has no earlier sample and answers
//! `None` (the second is accurate).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use forge_core::platform::{
    ImageNameCache, MachineSample, ProcessStats, SystemStats, sample_machine, volume_space,
};
use forge_core::sync::{LockLevel, OrderedMutex};
use forge_ipc::api::{
    DiskRole, DiskSpaceDto, MachineStatsDto, PortOwnerKind, ProcessUsageDto, TopDto,
};

use crate::daemon::Daemon;

/// One process to report, before its image name is read.
struct Row {
    pid: u32,
    cpu_pct: f64,
    rss_bytes: u64,
    kind: PortOwnerKind,
    owner_id: String,
    owner_name: String,
}

#[derive(Default)]
struct State {
    machine: Option<MachineSample>,
    /// CPU time of the daemon process and when it was read.
    own: Option<(Instant, Duration)>,
}

/// State between two `stats.top` calls.
pub struct TopSampler {
    names: ImageNameCache,
    stats: Arc<dyn ProcessStats>,
    state: OrderedMutex<State>,
}

impl std::fmt::Debug for TopSampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TopSampler").finish_non_exhaustive()
    }
}

impl TopSampler {
    pub fn new(stats: Arc<dyn ProcessStats>) -> Self {
        Self {
            names: ImageNameCache::new(Arc::clone(&stats)),
            stats,
            state: OrderedMutex::new(LockLevel::DAEMON_TABLES, State::default()),
        }
    }

    /// CPU of the daemon since the previous call (the first averages over its lifetime),
    /// as a share of the whole machine. Blocking.
    fn own_usage(&self) -> Option<(u32, f64, u64)> {
        let pid = std::process::id();
        let stat = self.stats.sample(pid).ok()?;
        let now = Instant::now();
        let logical = f64::from(self.stats.logical_cpus().max(1));
        let mut state = self.state.lock();
        let (window, used) = match state.own {
            Some((at, before)) => (
                now.saturating_duration_since(at),
                stat.cpu_total.saturating_sub(before),
            ),
            None => (stat.uptime, stat.cpu_total),
        };
        state.own = Some((now, stat.cpu_total));
        drop(state);
        let cpu = if window.is_zero() {
            0.0
        } else {
            used.as_secs_f64() * 100.0 / (window.as_secs_f64() * logical)
        };
        Some((pid, cpu, stat.rss_bytes))
    }

    /// Image base names of `pids`, read where they are not cached yet. Blocking.
    fn image_names(&self, pids: &[u32]) -> HashMap<u32, String> {
        let mut out = HashMap::new();
        let mut alive: Vec<(u32, SystemTime)> = Vec::new();
        for &pid in pids {
            let Ok(started) = self.stats.creation_time(pid) else {
                continue;
            };
            alive.push((pid, started));
            if let Some(name) = self.names.name(pid, started) {
                out.insert(pid, name.to_string());
            }
        }
        self.names
            .retain(|pid, started| alive.contains(&(pid, started)));
        out
    }
}

/// `stats.top`: the processes of the daemon, the instances, the pools and the apps, and
/// the machine.
pub async fn top(daemon: &Daemon) -> forge_core::error::Result<TopDto> {
    let mut rows: Vec<Row> = Vec::new();

    // Instances (web servers, databases, mail...), each process of its job.
    for (entry, _) in daemon.services.statuses().await {
        let Some(handle) = &entry.handle else {
            continue;
        };
        let Ok(Some(metrics)) = handle.metrics(forge_supervisor::QUICK_ASK_TIMEOUT).await else {
            continue;
        };
        for p in &metrics.processes {
            rows.push(Row {
                pid: p.pid,
                cpu_pct: p.cpu_pct,
                rss_bytes: p.rss_bytes,
                kind: PortOwnerKind::Instance,
                owner_id: entry.row.id.to_string(),
                owner_name: entry.label(),
            });
        }
    }

    // Pools: one row per worker, owner `php@<series>`.
    for status in daemon.pools.statuses().await {
        for worker in &status.workers {
            let Some(pid) = worker.pid else { continue };
            rows.push(Row {
                pid,
                cpu_pct: worker.cpu_pct.unwrap_or(0.0),
                rss_bytes: worker.rss_bytes.unwrap_or(0),
                kind: PortOwnerKind::Pool,
                owner_id: status.version_id.to_string(),
                owner_name: format!("{}@{}", status.tool, status.series),
            });
        }
    }

    // App processes of the sites, owner `<host>/<process>`.
    if let Ok(samples) = daemon.apps.process_samples().await {
        for sample in samples {
            let Some(metrics) = &sample.metrics else {
                continue;
            };
            let name = format!("{}/{}", sample.host, sample.name);
            for p in &metrics.processes {
                rows.push(Row {
                    pid: p.pid,
                    cpu_pct: p.cpu_pct,
                    rss_bytes: p.rss_bytes,
                    kind: PortOwnerKind::App,
                    owner_id: sample.site_id.to_string(),
                    owner_name: name.clone(),
                });
            }
        }
    }

    // The daemon and the image names, off the runtime.
    let sampler = Arc::clone(&daemon.top);
    let pids: Vec<u32> = rows.iter().map(|r| r.pid).collect();
    let (own, names) = forge_core::runtime::spawn_blocking_timeout(
        "reading the daemon and the process names",
        Duration::from_secs(5),
        move || {
            let own = sampler.own_usage();
            let mut all = pids;
            all.extend(own.map(|(pid, _, _)| pid));
            let names = sampler.image_names(&all);
            Ok((own, names))
        },
    )
    .await?;
    if let Some((pid, cpu, rss)) = own {
        rows.push(Row {
            pid,
            cpu_pct: cpu,
            rss_bytes: rss,
            kind: PortOwnerKind::Daemon,
            owner_id: "daemon".into(),
            owner_name: "forgeboxd".into(),
        });
    }

    // A pid seen twice (an instance job and a pool) counts once.
    let mut seen = std::collections::HashSet::new();
    rows.retain(|r| seen.insert(r.pid));
    let forgebox_cpu: f64 = rows.iter().map(|r| r.cpu_pct).sum();
    let forgebox_rss: u64 = rows.iter().map(|r| r.rss_bytes).sum();
    let processes: Vec<ProcessUsageDto> = rows
        .into_iter()
        .map(|r| ProcessUsageDto {
            pid: r.pid,
            name: names.get(&r.pid).cloned().unwrap_or_default(),
            owner_kind: r.kind,
            owner_id: r.owner_id,
            owner_name: r.owner_name,
            cpu_pct: r.cpu_pct as f32,
            rss_bytes: r.rss_bytes,
        })
        .collect();

    let machine = machine(daemon, forgebox_cpu, forgebox_rss).await?;
    Ok(TopDto {
        processes,
        machine: Some(machine),
    })
}

async fn machine(
    daemon: &Daemon,
    forgebox_cpu: f64,
    forgebox_rss: u64,
) -> forge_core::error::Result<MachineStatsDto> {
    let stats: Arc<dyn SystemStats> = Arc::clone(daemon.platform.system_stats());
    let now = sample_machine(Arc::clone(&stats)).await?;
    let before = daemon.top.state.lock().machine.replace(now);
    let cpu_pct = before.and_then(|b| now.cpu_pct_since(&b));
    let mut disks = Vec::new();
    let root = daemon.root.path().to_path_buf();
    if let Ok(space) = volume_space(Arc::clone(&stats), root.clone()).await {
        disks.push(DiskSpaceDto {
            role: DiskRole::Root,
            total: space.total,
            free: space.free,
            available: space.available,
        });
        let data = daemon.root.data();
        if let Ok(data_space) = volume_space(Arc::clone(&stats), data.clone()).await {
            let other_drive = data.components().next() != root.components().next();
            if other_drive || data_space.total != space.total {
                disks.push(DiskSpaceDto {
                    role: DiskRole::Data,
                    total: data_space.total,
                    free: data_space.free,
                    available: data_space.available,
                });
            }
        }
    }
    Ok(MachineStatsDto {
        at: forge_core::Timestamp::now(),
        cpu_pct,
        logical_cpus: now.logical_cpus,
        mem_total: now.mem_total,
        mem_available: now.mem_avail,
        // Never above the machine: the two come from different intervals.
        forgebox_cpu_pct: cpu_pct.map(|machine| forgebox_cpu.min(machine)),
        forgebox_rss,
        disks,
    })
}
