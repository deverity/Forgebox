//! Process metrics of an instance (docs/03 §6, docs/14 §2): CPU %, memory, handles,
//! uptime, sampled every 2 s (±10 % jitter) while a run is active.
//!
//! Each sample lists the processes of the instance's Job Object
//! (`QueryInformationJobObject(JobObjectBasicProcessIdList)`, nested jobs included: the
//! service, its children and its console host) and reads each one through
//! [`forge_core::platform::ProcessStats`] (`GetProcessTimes`, `GetProcessMemoryInfo`,
//! `GetProcessHandleCount`) in `spawn_blocking`. Memory and handles are summed; CPU % is
//! the CPU time used between two samples divided by the wall time between them and by
//! the number of logical processors, the way Task Manager shows it (100 % = the whole
//! machine). A process is followed by `(pid, creation time)`, so a reused pid is a new
//! process. A process seen for the first time counts the CPU it used inside the window
//! (all of it when it was born after the previous sample; otherwise its average rate);
//! the CPU of a process that started and exited between two samples is not seen.
//!
//! The latest sample is kept in a `watch` channel owned by the instance: `Metrics`
//! answers it and `SubscribeMetrics` hands out receivers (the future IPC stream). The
//! optional `service.metrics` event ([`MetricsOptions::publish_events`]) is off by
//! default: every event is persisted by the daemon.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime};

use forge_core::Timestamp;
use forge_core::event::{EventBus, EventKind};
use forge_core::ids::{InstanceId, ToolId};
use forge_core::platform::{ProcStats, ProcessGroup, ProcessStats};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::events::{SERVICE_EVENT_VERSION, ServiceRef};
use crate::health::jittered;

/// Sampling of an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricsOptions {
    /// Sample every `interval` (±10 %; default 2 s). Zero disables sampling.
    pub interval: Duration,
    /// Publish every sample as a `service.metrics` event (default false).
    pub publish_events: bool,
}

impl Default for MetricsOptions {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(2),
            publish_events: false,
        }
    }
}

/// One process of an instance in a sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessMetrics {
    pub pid: u32,
    /// Share of the whole machine since the previous sample (0–100).
    pub cpu_pct: f64,
    /// CPU time used since the process started.
    pub cpu_total_ms: u64,
    pub rss_bytes: u64,
    pub private_bytes: u64,
    pub handles: u32,
    pub uptime_ms: u64,
}

/// Latest sample of an instance (sum over the processes of its job).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceMetrics {
    pub sampled_at: Timestamp,
    /// Wall time covered by `cpu_pct` (since the previous sample, or since the run
    /// started for the first one).
    pub window_ms: u64,
    /// Share of the whole machine (0–100, like Task Manager's CPU column).
    pub cpu_pct: f64,
    /// Working sets, summed.
    pub rss_bytes: u64,
    /// Private commit, summed.
    pub private_bytes: u64,
    pub handles: u64,
    /// Main process of the run, if it was among the sampled processes.
    pub pid: Option<u32>,
    /// Uptime of the main process (0 if it was not sampled).
    pub uptime_ms: u64,
    pub logical_cpus: u32,
    pub processes: Vec<ProcessMetrics>,
}

/// Payload of `service.metrics` (same shape as `forge_ipc::api::events::ServiceMetricsPayload`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceMetricsPayload {
    pub v: u32,
    pub instance_id: InstanceId,
    /// `<tool>/<instance>`.
    pub name: String,
    pub tool: ToolId,
    pub pid: Option<u32>,
    pub cpu_pct: f64,
    pub rss_bytes: u64,
    pub private_bytes: u64,
    pub handles: u64,
    /// Processes in the instance's job.
    pub processes: u32,
    pub uptime_ms: u64,
    pub window_ms: u64,
}

impl ServiceMetricsPayload {
    pub fn new(service: &ServiceRef, metrics: &InstanceMetrics) -> Self {
        Self {
            v: SERVICE_EVENT_VERSION,
            instance_id: service.instance_id,
            name: service.name(),
            tool: service.tool.clone(),
            pid: metrics.pid,
            cpu_pct: metrics.cpu_pct,
            rss_bytes: metrics.rss_bytes,
            private_bytes: metrics.private_bytes,
            handles: metrics.handles,
            processes: u32::try_from(metrics.processes.len()).unwrap_or(u32::MAX),
            uptime_ms: metrics.uptime_ms,
            window_ms: metrics.window_ms,
        }
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// CPU accounting between samples (pure: samples and instants are passed in).
#[derive(Debug, Clone)]
pub(crate) struct CpuTracker {
    previous_at: Instant,
    previous: HashMap<(u32, SystemTime), Duration>,
}

impl CpuTracker {
    /// Accounting that starts at `started` (the spawn of the run).
    pub(crate) fn new(started: Instant) -> Self {
        Self {
            previous_at: started,
            previous: HashMap::new(),
        }
    }

    /// Turns the counters read at `now` into a sample.
    pub(crate) fn update(
        &mut self,
        stats: &[ProcStats],
        now: Instant,
        logical_cpus: u32,
        main_pid: u32,
    ) -> InstanceMetrics {
        let window = now.saturating_duration_since(self.previous_at);
        let divisor = window.as_secs_f64() * f64::from(logical_cpus.max(1));
        let mut processes = Vec::with_capacity(stats.len());
        let mut used_total = Duration::ZERO;
        let mut current = HashMap::with_capacity(stats.len());
        for stat in stats {
            let key = (stat.pid, stat.started_at);
            let used = match self.previous.get(&key) {
                Some(before) => stat.cpu_total.saturating_sub(*before),
                None if stat.uptime <= window || stat.uptime.is_zero() => stat.cpu_total,
                // Alive before the window but never sampled: its average rate.
                None => stat
                    .cpu_total
                    .mul_f64(window.as_secs_f64() / stat.uptime.as_secs_f64()),
            };
            current.insert(key, stat.cpu_total);
            used_total += used;
            processes.push(ProcessMetrics {
                pid: stat.pid,
                cpu_pct: if divisor > 0.0 {
                    used.as_secs_f64() * 100.0 / divisor
                } else {
                    0.0
                },
                cpu_total_ms: millis(stat.cpu_total),
                rss_bytes: stat.rss_bytes,
                private_bytes: stat.private_bytes,
                handles: stat.handles,
                uptime_ms: millis(stat.uptime),
            });
        }
        self.previous = current;
        self.previous_at = now;
        let main = stats.iter().find(|s| s.pid == main_pid);
        InstanceMetrics {
            sampled_at: Timestamp::now(),
            window_ms: millis(window),
            cpu_pct: if divisor > 0.0 {
                used_total.as_secs_f64() * 100.0 / divisor
            } else {
                0.0
            },
            rss_bytes: stats.iter().map(|s| s.rss_bytes).sum(),
            private_bytes: stats.iter().map(|s| s.private_bytes).sum(),
            handles: stats.iter().map(|s| u64::from(s.handles)).sum(),
            pid: main.map(|s| s.pid),
            uptime_ms: main.map_or(0, |s| millis(s.uptime)),
            logical_cpus,
            processes,
        }
    }
}

/// Reads every process of `job` (blocking). Processes that exit between the listing and
/// the read are skipped.
fn read_job(
    job: &dyn ProcessGroup,
    stats: &dyn ProcessStats,
) -> forge_core::Result<Vec<ProcStats>> {
    let pids = job.pids()?;
    Ok(pids
        .into_iter()
        .filter_map(|pid| match stats.sample(pid) {
            Ok(stat) => Some(stat),
            Err(err) => {
                tracing::trace!(pid, %err, "process left before it was sampled");
                None
            }
        })
        .collect())
}

/// Where a run's sampler reports.
pub(crate) struct SamplerOutput {
    pub latest: Arc<watch::Sender<Option<InstanceMetrics>>>,
    /// `Some` when `service.metrics` events are on.
    pub events: Option<(EventBus, ServiceRef)>,
}

/// Samples the processes of `job` every `interval` until the job is gone or the task is
/// aborted (the run ended). The job is held weakly: sampling never keeps it alive.
pub(crate) async fn sample_run(
    job: Weak<dyn ProcessGroup>,
    stats: Arc<dyn ProcessStats>,
    main_pid: u32,
    interval: Duration,
    output: SamplerOutput,
) {
    if interval.is_zero() {
        return;
    }
    let logical_cpus = stats.logical_cpus();
    let mut tracker = CpuTracker::new(Instant::now());
    loop {
        tokio::time::sleep(jittered(interval, 0.1)).await;
        let Some(group) = job.upgrade() else {
            return;
        };
        let reader = Arc::clone(&stats);
        let read = tokio::task::spawn_blocking(move || read_job(group.as_ref(), reader.as_ref()));
        let samples = match read.await {
            Ok(Ok(samples)) => samples,
            Ok(Err(err)) => {
                tracing::debug!(%err, "could not list the processes of an instance job");
                continue;
            }
            Err(err) => {
                tracing::warn!(%err, "metrics sampling task failed");
                continue;
            }
        };
        let metrics = tracker.update(&samples, Instant::now(), logical_cpus, main_pid);
        if let Some((bus, service)) = &output.events {
            match serde_json::to_value(ServiceMetricsPayload::new(service, &metrics)) {
                Ok(payload) => {
                    bus.publish(EventKind::ServiceMetrics, payload);
                }
                Err(err) => tracing::error!(%err, "could not serialize service.metrics"),
            }
        }
        output.latest.send_replace(Some(metrics));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(pid: u32, started: SystemTime, cpu_ms: u64, uptime_ms: u64, rss: u64) -> ProcStats {
        ProcStats {
            pid,
            cpu_total: Duration::from_millis(cpu_ms),
            rss_bytes: rss,
            private_bytes: rss / 2,
            handles: 10,
            started_at: started,
            uptime: Duration::from_millis(uptime_ms),
        }
    }

    #[test]
    fn cpu_percent_is_delta_over_wall_over_cpus() {
        let t0 = Instant::now();
        let born = SystemTime::now();
        let mut tracker = CpuTracker::new(t0);
        // First sample 2 s after the spawn: everything used counts.
        let first = tracker.update(
            &[
                stat(10, born, 1000, 2000, 100),
                stat(11, born, 200, 1500, 50),
            ],
            t0 + Duration::from_secs(2),
            4,
            10,
        );
        // (1000 + 200) ms / 2 s / 4 cpus = 15 %.
        assert!((first.cpu_pct - 15.0).abs() < 1e-9, "{first:?}");
        assert!((first.processes[0].cpu_pct - 12.5).abs() < 1e-9);
        assert_eq!(first.rss_bytes, 150);
        assert_eq!(first.private_bytes, 75);
        assert_eq!(first.handles, 20);
        assert_eq!(first.pid, Some(10));
        assert_eq!(first.uptime_ms, 2000);
        assert_eq!(first.window_ms, 2000);

        // Second sample: pid 10 used 2 s of CPU in 2 s (one full core); pid 11 exited;
        // pid 12 is new and was born inside the window.
        let second = tracker.update(
            &[
                stat(10, born, 3000, 4000, 100),
                stat(12, born, 400, 500, 25),
            ],
            t0 + Duration::from_secs(4),
            4,
            10,
        );
        // (2000 + 400) ms / 2 s / 4 = 30 %.
        assert!((second.cpu_pct - 30.0).abs() < 1e-9, "{second:?}");
        assert_eq!(second.processes.len(), 2);
    }

    #[test]
    fn reused_pids_and_unseen_processes() {
        let t0 = Instant::now();
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let mut tracker = CpuTracker::new(t0);
        tracker.update(
            &[stat(7, old, 5000, 60_000, 1)],
            t0 + Duration::from_secs(1),
            1,
            7,
        );
        // Same pid, newer creation time: a different process, born in the window.
        let reused = old + Duration::from_secs(70);
        let sample = tracker.update(
            &[stat(7, reused, 300, 500, 1)],
            t0 + Duration::from_secs(2),
            1,
            7,
        );
        assert!((sample.cpu_pct - 30.0).abs() < 1e-9, "{sample:?}");
        // A process alive long before the window and never sampled: average rate.
        let mut tracker = CpuTracker::new(t0);
        let sample = tracker.update(
            &[stat(8, old, 10_000, 100_000, 1)],
            t0 + Duration::from_secs(10),
            1,
            0,
        );
        // 10 s of CPU over 100 s of life → 10 % of one cpu.
        assert!((sample.cpu_pct - 10.0).abs() < 1e-9, "{sample:?}");
        assert_eq!(sample.pid, None);
        assert_eq!(sample.uptime_ms, 0);
    }

    #[test]
    fn empty_window_is_zero_not_nan() {
        let t0 = Instant::now();
        let mut tracker = CpuTracker::new(t0);
        let sample = tracker.update(&[stat(1, SystemTime::now(), 10, 0, 1)], t0, 8, 1);
        assert_eq!(sample.cpu_pct, 0.0);
        assert!(sample.processes.iter().all(|p| p.cpu_pct == 0.0));
    }

    #[test]
    fn metrics_payload_shape() {
        let service = ServiceRef {
            instance_id: InstanceId::new(),
            tool: ToolId::new("mysql").unwrap(),
            instance: "default".into(),
        };
        let t0 = Instant::now();
        let mut tracker = CpuTracker::new(t0);
        let metrics = tracker.update(
            &[stat(5, SystemTime::now(), 100, 1000, 4096)],
            t0 + Duration::from_secs(1),
            2,
            5,
        );
        let json = serde_json::to_value(ServiceMetricsPayload::new(&service, &metrics)).unwrap();
        assert_eq!(json["v"], 1);
        assert_eq!(json["name"], "mysql/default");
        assert_eq!(json["processes"], 1);
        assert_eq!(json["rss_bytes"], 4096);
        assert_eq!(json["pid"], 5);
        assert!((json["cpu_pct"].as_f64().unwrap() - 5.0).abs() < 1e-9);
    }
}
