//! CPU and memory of the daemon process itself, for `daemon.status` (`cpu_pct`,
//! `rss_bytes`; API 1.1, ADR-0039).
//!
//! The OS only gives cumulative CPU time, so the share is computed between two looks:
//! each `daemon.status` reports the average since the previous one (the first one averages
//! over the process lifetime), like Task Manager's "CPU" column. Calls closer than
//! [`MIN_WINDOW`] repeat the previous figure instead of dividing by a tiny window.
//! [`ProcessMeter::usage`] is **blocking** (it opens the process): the handler runs it in
//! `spawn_blocking`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::platform::ProcessStats;
use forge_core::sync::{LockLevel, OrderedMutex};

/// Shortest window a CPU share is computed over.
pub(crate) const MIN_WINDOW: Duration = Duration::from_millis(250);

/// What `daemon.status` reports about the process.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Usage {
    /// Share of the whole machine, 0-100.
    pub(crate) cpu_pct: f64,
    pub(crate) rss_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct Last {
    at: Instant,
    cpu_total: Duration,
    pct: f64,
}

/// Samples this process; cheap to share.
pub(crate) struct ProcessMeter {
    stats: Arc<dyn ProcessStats>,
    last: OrderedMutex<Option<Last>>,
}

impl ProcessMeter {
    pub(crate) fn new(stats: Arc<dyn ProcessStats>) -> Self {
        Self {
            stats,
            last: OrderedMutex::new(LockLevel::IPC_PROCESS_METER, None),
        }
    }

    /// Current usage of this process. Blocking.
    pub(crate) fn usage(&self) -> forge_core::Result<Usage> {
        let now = Instant::now();
        let sample = self.stats.sample(std::process::id())?;
        let cpus = self.stats.logical_cpus();
        let mut last = self.last.lock();
        let pct = match *last {
            None => share(sample.cpu_total, sample.uptime, cpus),
            Some(prev) if now.duration_since(prev.at) < MIN_WINDOW => prev.pct,
            Some(prev) => share(
                sample.cpu_total.saturating_sub(prev.cpu_total),
                now.duration_since(prev.at),
                cpus,
            ),
        };
        if last.is_none_or(|prev| now.duration_since(prev.at) >= MIN_WINDOW) {
            *last = Some(Last {
                at: now,
                cpu_total: sample.cpu_total,
                pct,
            });
        }
        Ok(Usage {
            cpu_pct: pct,
            rss_bytes: sample.rss_bytes,
        })
    }
}

/// `cpu` seconds used during `wall` on a machine with `cpus` logical processors, as a
/// percentage of the whole machine (clamped to 0-100).
pub(crate) fn share(cpu: Duration, wall: Duration, cpus: u32) -> f64 {
    if wall.is_zero() || cpus == 0 {
        return 0.0;
    }
    let pct = cpu.as_secs_f64() / wall.as_secs_f64() / f64::from(cpus) * 100.0;
    // Rounded to 0.1 so a JSON client does not see 0.30000000000000004.
    (pct.clamp(0.0, 100.0) * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;

    use forge_core::platform::ProcStats;

    use super::*;

    #[derive(Debug)]
    struct Fake {
        cpu_ms: AtomicU64,
        uptime_ms: u64,
    }

    impl ProcessStats for Fake {
        fn sample(&self, pid: u32) -> forge_core::Result<ProcStats> {
            Ok(ProcStats {
                pid,
                cpu_total: Duration::from_millis(self.cpu_ms.load(Ordering::SeqCst)),
                rss_bytes: 42 * 1024 * 1024,
                private_bytes: 0,
                handles: 0,
                started_at: SystemTime::UNIX_EPOCH,
                uptime: Duration::from_millis(self.uptime_ms),
            })
        }
        fn logical_cpus(&self) -> u32 {
            4
        }
    }

    #[test]
    fn share_is_a_percentage_of_the_whole_machine() {
        // 1 s of CPU in 1 s on 4 processors: one core busy = 25 %.
        assert_eq!(
            share(Duration::from_secs(1), Duration::from_secs(1), 4),
            25.0
        );
        assert_eq!(
            share(Duration::from_secs(8), Duration::from_secs(1), 4),
            100.0
        );
        assert_eq!(share(Duration::from_secs(1), Duration::ZERO, 4), 0.0);
        assert_eq!(
            share(Duration::from_secs(1), Duration::from_secs(1), 0),
            0.0
        );
    }

    #[test]
    fn first_call_averages_over_the_lifetime_then_windows_follow() {
        let fake = Arc::new(Fake {
            cpu_ms: AtomicU64::new(2_000),
            uptime_ms: 100_000,
        });
        let meter = ProcessMeter::new(fake.clone());
        // 2 s of CPU in 100 s on 4 cpus = 0.5 %.
        let first = meter.usage().unwrap();
        assert_eq!(first.cpu_pct, 0.5);
        assert_eq!(first.rss_bytes, 42 * 1024 * 1024);
        // Too soon: the previous figure, no division by a tiny window.
        assert_eq!(meter.usage().unwrap().cpu_pct, 0.5);
        std::thread::sleep(MIN_WINDOW + Duration::from_millis(50));
        // No CPU used since the last look: 0 %.
        assert_eq!(meter.usage().unwrap().cpu_pct, 0.0);
        std::thread::sleep(MIN_WINDOW + Duration::from_millis(50));
        fake.cpu_ms.fetch_add(1_000_000, Ordering::SeqCst);
        // Absurd burst (more CPU than wall time allows): clamped.
        assert_eq!(meter.usage().unwrap().cpu_pct, 100.0);
    }
}
