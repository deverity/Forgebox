//! Resource usage of single processes (docs/03 §6, docs/14 §2): the raw counters the
//! supervisor's metrics sampler turns into CPU %, memory and handle figures per instance.
//!
//! [`ProcessStats::sample`] is **blocking** (it opens the process and asks the kernel)
//! and belongs in `spawn_blocking` (ADR-0011). It returns cumulative counters only; rates
//! such as CPU % come from two samples (`Δcpu_total / Δwall / logical_cpus`), which is
//! how Task Manager computes its "CPU" column.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

#[cfg(not(windows))]
use crate::error::ForgeError;
use crate::error::Result;

/// Counters of one process at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStats {
    pub pid: u32,
    /// User + kernel CPU time used since the process started (all its threads).
    pub cpu_total: Duration,
    /// Working set in bytes: physical memory in use by the process (Task Manager
    /// "Working set (memory)", PowerShell `Get-Process` `WorkingSet64`).
    pub rss_bytes: u64,
    /// Private committed memory in bytes (Task Manager "Commit size").
    pub private_bytes: u64,
    /// Open kernel handles.
    pub handles: u32,
    /// When the process was created. Together with `pid` it identifies the process:
    /// a pid reused by a newer process has a later `started_at`.
    pub started_at: SystemTime,
    /// Time since `started_at` when the sample was taken.
    pub uptime: Duration,
}

/// Per-process resource counters of the OS.
pub trait ProcessStats: Send + Sync + fmt::Debug {
    /// Counters of `pid`. `NotFound` when no such process exists (it exited and its id
    /// was released). Blocking.
    fn sample(&self, pid: u32) -> Result<ProcStats>;

    /// Logical processors of the machine, every processor group included: the divisor
    /// that turns CPU time per wall time into a share of the whole machine (0–100 %).
    fn logical_cpus(&self) -> u32;
}

/// Process counters for the running OS.
#[cfg(windows)]
pub fn process_stats() -> Arc<dyn ProcessStats> {
    Arc::new(super::windows::WindowsProcessStats)
}

/// Process counters for the running OS (stubs until the Linux/macOS ports).
#[cfg(not(windows))]
pub fn process_stats() -> Arc<dyn ProcessStats> {
    Arc::new(unsupported::Unsupported)
}

#[cfg(not(windows))]
mod unsupported {
    use super::*;

    #[derive(Debug)]
    pub struct Unsupported;

    impl ProcessStats for Unsupported {
        fn sample(&self, _: u32) -> Result<ProcStats> {
            Err(ForgeError::UnsupportedPlatform(format!(
                "process statistics are not implemented on {}",
                std::env::consts::OS
            )))
        }

        fn logical_cpus(&self) -> u32 {
            std::thread::available_parallelism()
                .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
                .unwrap_or(1)
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn samples_the_current_process() {
        let stats = process_stats();
        let pid = std::process::id();
        // Burn a little CPU so the counter is visibly non-zero.
        let started = std::time::Instant::now();
        let mut x = 0_u64;
        while started.elapsed() < Duration::from_millis(50) {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(7));
        }
        let first = stats.sample(pid).unwrap();
        assert_eq!(first.pid, pid);
        assert!(first.cpu_total >= Duration::from_millis(10), "{first:?}");
        assert!(first.rss_bytes > 1024 * 1024, "{first:?}");
        assert!(first.private_bytes > 0, "{first:?}");
        assert!(first.handles > 0, "{first:?}");
        assert!(first.started_at <= SystemTime::now());
        let second = stats.sample(pid).unwrap();
        assert!(second.cpu_total >= first.cpu_total);
        assert!(second.uptime >= first.uptime);
        assert_eq!(second.started_at, first.started_at);
        assert!(stats.logical_cpus() >= 1);
        assert_eq!(
            stats.sample(u32::MAX - 3).unwrap_err().code(),
            "not_found",
            "no such process"
        );
    }
}
