//! Byte counters shared by the transfer tasks and the reporter that turns them into
//! `job.progress` events (bytes, EMA speed, ETA) every 250 ms.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use forge_core::jobs::{JobContext, JobProgress};
use tokio::task::JoinHandle;

/// Smoothing factor of the speed EMA (weight of the newest sample).
const EMA_ALPHA: f64 = 0.3;

/// Counters of one download. `total == 0` means "unknown".
#[derive(Debug, Default)]
pub(crate) struct Progress {
    done: AtomicU64,
    total: AtomicU64,
}

impl Progress {
    pub(crate) fn add(&self, bytes: u64) {
        self.done.fetch_add(bytes, Ordering::Relaxed);
    }

    pub(crate) fn set_done(&self, bytes: u64) {
        self.done.store(bytes, Ordering::Relaxed);
    }

    pub(crate) fn set_total(&self, total: Option<u64>) {
        self.total.store(total.unwrap_or(0), Ordering::Relaxed);
    }

    pub(crate) fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }

    pub(crate) fn total(&self) -> Option<u64> {
        Some(self.total.load(Ordering::Relaxed)).filter(|t| *t > 0)
    }
}

/// Exponential moving average of the transfer speed.
#[derive(Debug, Default)]
pub(crate) struct SpeedMeter {
    last_done: u64,
    ema: Option<f64>,
}

impl SpeedMeter {
    pub(crate) fn new(start_done: u64) -> Self {
        Self {
            last_done: start_done,
            ema: None,
        }
    }

    /// Feeds the bytes done at the end of a `dt` window and returns the smoothed speed
    /// in bytes per second. A counter that went backwards (restart of a segment) counts
    /// as no progress.
    pub(crate) fn sample(&mut self, done: u64, dt: Duration) -> f64 {
        let delta = done.saturating_sub(self.last_done) as f64;
        self.last_done = done;
        let secs = dt.as_secs_f64().max(1e-6);
        let inst = delta / secs;
        let next = match self.ema {
            None => inst,
            Some(prev) => EMA_ALPHA * inst + (1.0 - EMA_ALPHA) * prev,
        };
        self.ema = Some(next);
        next
    }
}

/// Seconds to go at `speed` bytes/s, `None` when it cannot be estimated.
pub(crate) fn eta_secs(done: u64, total: Option<u64>, speed: f64) -> Option<u64> {
    let total = total?;
    if speed < 1.0 {
        return None;
    }
    Some((total.saturating_sub(done) as f64 / speed).ceil() as u64)
}

/// Aborts the reporter task when dropped.
#[derive(Debug)]
pub(crate) struct ReporterGuard(JoinHandle<()>);

impl Drop for ReporterGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Starts the task that reports `progress` through `ctx` every `interval`.
pub(crate) fn spawn_reporter(
    ctx: JobContext,
    progress: Arc<Progress>,
    interval: Duration,
) -> ReporterGuard {
    let task = tokio::spawn(async move {
        // First report one interval from now: an immediate tick would race with the
        // job's first message update and be swallowed by the registry's throttle.
        let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut meter = SpeedMeter::new(progress.done());
        let mut last = Instant::now();
        loop {
            ticker.tick().await;
            let now = Instant::now();
            let done = progress.done();
            let total = progress.total();
            let speed = meter.sample(done, now.duration_since(last));
            last = now;
            let mut report = JobProgress::new()
                .bytes(done, total)
                .rate(speed as u64, eta_secs(done, total, speed));
            if let Some(total) = total {
                report = report.pct((done as f64 / total as f64 * 100.0) as f32);
            }
            ctx.progress(report);
        }
    });
    ReporterGuard(task)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ema_converges_towards_the_new_speed() {
        let mut meter = SpeedMeter::new(0);
        let one_s = Duration::from_secs(1);
        assert_eq!(meter.sample(1000, one_s), 1000.0);
        let second = meter.sample(1000 + 2000, one_s);
        assert!((second - (0.3 * 2000.0 + 0.7 * 1000.0)).abs() < 1e-9);
        // Stalled transfer decays but never goes negative.
        let third = meter.sample(3000, one_s);
        assert!(third < second && third > 0.0);
    }

    #[test]
    fn counter_going_backwards_is_no_progress() {
        let mut meter = SpeedMeter::new(5000);
        assert_eq!(meter.sample(100, Duration::from_secs(1)), 0.0);
    }

    #[test]
    fn eta_needs_total_and_speed() {
        assert_eq!(eta_secs(0, None, 100.0), None);
        assert_eq!(eta_secs(0, Some(1000), 0.0), None);
        assert_eq!(eta_secs(500, Some(1000), 100.0), Some(5));
        assert_eq!(eta_secs(2000, Some(1000), 100.0), Some(0));
    }

    #[test]
    fn unknown_total_is_none() {
        let p = Progress::default();
        assert_eq!(p.total(), None);
        p.set_total(Some(10));
        p.add(3);
        assert_eq!((p.done(), p.total()), (3, Some(10)));
    }
}
