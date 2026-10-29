//! Machine-wide resource counters (docs/03 §6, docs/14 §4.2): the "monitor da máquina" of
//! the dashboard and the header of `forge top` — CPU, physical memory and the space of a
//! volume, next to the share the Forgebox processes take of them.
//!
//! [`SystemStats::sample`] returns cumulative CPU times (busy and idle, summed over every
//! logical processor since boot) and the current physical memory; like
//! [`super::ProcessStats`], rates come from two samples: the machine CPU % between them is
//! `Δbusy / (Δbusy + Δidle)` ([`MachineSample::cpu_pct_since`]), the number of Task
//! Manager's Performance tab, and the share of a set of processes is their CPU time over
//! the same capacity `Δbusy + Δidle` ([`MachineSample::cpu_share_since`]), so both
//! figures are on the same 0–100 scale (100 = the whole machine).
//!
//! Every [`SystemStats`] method is **blocking** (kernel queries; [`SystemStats::volume`]
//! may stall on a sleeping disk or a network share): async code goes through
//! [`sample_machine`] and [`volume_space`], which run them in `spawn_blocking` with a
//! timeout (ADR-0011).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::{ForgeError, Result};
use crate::runtime::spawn_blocking_timeout;

/// Bound of one [`SystemStats`] call made through [`sample_machine`] or [`volume_space`].
pub const SYSTEM_STATS_TIMEOUT: Duration = Duration::from_secs(5);

/// Machine counters at one instant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MachineSample {
    /// CPU time spent running code (kernel and user, idle excluded) since boot, summed
    /// over every logical processor.
    pub cpu_busy_total: Duration,
    /// CPU time spent idle since boot, summed over every logical processor.
    pub cpu_idle_total: Duration,
    /// Logical processors of the machine, every processor group included.
    pub logical_cpus: u32,
    /// Installed physical memory usable by the OS, in bytes (`Win32_ComputerSystem`
    /// `TotalPhysicalMemory`).
    pub mem_total: u64,
    /// Physical memory available without paging anything out (free + standby), in bytes:
    /// Task Manager "Available".
    pub mem_avail: u64,
}

impl MachineSample {
    /// Busy share of the whole machine between `earlier` and `self`, 0–100 (Task Manager's
    /// CPU graph). `None` when no CPU time elapsed between the samples or a counter went
    /// backwards (samples of different boots or machines).
    pub fn cpu_pct_since(&self, earlier: &MachineSample) -> Option<f64> {
        let (busy, capacity) = self.cpu_delta(earlier)?;
        Some(ratio_pct(busy, capacity))
    }

    /// Share of the machine's CPU capacity between `earlier` and `self` that `cpu_used`
    /// represents, 0–100: `cpu_used` is the CPU time some processes used over the same
    /// interval (the sum of their `ProcStats::cpu_total` deltas). On the same scale as
    /// [`MachineSample::cpu_pct_since`], so "Forgebox share ≤ machine busy" holds up to
    /// the counters' granularity; clamped to 100. `None` like `cpu_pct_since`.
    pub fn cpu_share_since(&self, earlier: &MachineSample, cpu_used: Duration) -> Option<f64> {
        let (_, capacity) = self.cpu_delta(earlier)?;
        Some(ratio_pct(cpu_used, capacity).min(100.0))
    }

    /// `(Δbusy, Δbusy + Δidle)`, when both counters moved forward and some time elapsed.
    fn cpu_delta(&self, earlier: &MachineSample) -> Option<(Duration, Duration)> {
        let busy = self.cpu_busy_total.checked_sub(earlier.cpu_busy_total)?;
        let idle = self.cpu_idle_total.checked_sub(earlier.cpu_idle_total)?;
        let capacity = busy.checked_add(idle)?;
        (!capacity.is_zero()).then_some((busy, capacity))
    }

    /// Physical memory in use, in bytes (`mem_total - mem_avail`).
    pub fn mem_used(&self) -> u64 {
        self.mem_total.saturating_sub(self.mem_avail)
    }

    /// Physical memory in use, 0–100 (Task Manager "In use" over the total). `None` when
    /// the total is unknown (zero).
    pub fn mem_used_pct(&self) -> Option<f64> {
        (self.mem_total > 0).then(|| self.mem_used() as f64 * 100.0 / self.mem_total as f64)
    }
}

/// `part / whole` in percent (`whole` is never zero here).
fn ratio_pct(part: Duration, whole: Duration) -> f64 {
    part.as_secs_f64() * 100.0 / whole.as_secs_f64()
}

/// Size and free space of a volume, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VolumeSpace {
    /// Size of the volume as seen by the current user (a disk quota lowers it).
    pub total: u64,
    /// Free bytes on the volume (`Get-PSDrive` "Free").
    pub free: u64,
    /// Free bytes the current user may still write (quotas); at most `free`.
    pub available: u64,
}

impl VolumeSpace {
    /// Bytes in use (`total - free`; `Get-PSDrive` "Used").
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    /// Bytes in use, 0–100. `None` when the size is unknown (zero).
    pub fn used_pct(&self) -> Option<f64> {
        (self.total > 0).then(|| self.used() as f64 * 100.0 / self.total as f64)
    }
}

/// Machine-wide counters of the OS.
pub trait SystemStats: Send + Sync + fmt::Debug {
    /// Cumulative CPU times, logical processors and physical memory. Blocking.
    fn sample(&self) -> Result<MachineSample>;

    /// Size and free space of the volume holding `path`, an existing folder or file
    /// anywhere on it (the root folder of a Forgebox installation, a site's project
    /// folder). `NotFound` when `path` does not exist. Blocking, and may stall on a
    /// sleeping disk or an unreachable network share.
    fn volume(&self, path: &Path) -> Result<VolumeSpace>;
}

/// [`SystemStats::sample`] on the blocking pool, bounded by [`SYSTEM_STATS_TIMEOUT`].
pub async fn sample_machine(stats: Arc<dyn SystemStats>) -> Result<MachineSample> {
    spawn_blocking_timeout(
        "sampling the machine counters",
        SYSTEM_STATS_TIMEOUT,
        move || stats.sample(),
    )
    .await
}

/// [`SystemStats::volume`] on the blocking pool, bounded by [`SYSTEM_STATS_TIMEOUT`].
pub async fn volume_space(stats: Arc<dyn SystemStats>, path: PathBuf) -> Result<VolumeSpace> {
    spawn_blocking_timeout(
        "reading the space of a volume",
        SYSTEM_STATS_TIMEOUT,
        move || stats.volume(&path),
    )
    .await
}

/// Machine counters of the running OS.
#[cfg(windows)]
pub fn system_stats() -> Arc<dyn SystemStats> {
    Arc::new(super::windows::WindowsSystemStats)
}

/// Machine counters of the running OS (stubs answering `UnsupportedPlatform` until the
/// Linux/macOS ports, docs/13 §4).
#[cfg(not(windows))]
pub fn system_stats() -> Arc<dyn SystemStats> {
    Arc::new(UnsupportedSystemStats)
}

/// [`SystemStats`] of OSes without an implementation: every call fails with
/// `UnsupportedPlatform`, never panics.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnsupportedSystemStats;

impl SystemStats for UnsupportedSystemStats {
    fn sample(&self) -> Result<MachineSample> {
        Err(ForgeError::UnsupportedPlatform(format!(
            "machine statistics are not implemented on {}",
            std::env::consts::OS
        )))
    }

    fn volume(&self, _: &Path) -> Result<VolumeSpace> {
        Err(ForgeError::UnsupportedPlatform(format!(
            "volume statistics are not implemented on {}",
            std::env::consts::OS
        )))
    }
}

/// Scriptable [`SystemStats`] for tests: answers the sample it holds (moved forward with
/// [`FakeSystemStats::advance`]) and the volumes it was given.
#[derive(Debug, Default)]
pub struct FakeSystemStats {
    state: Mutex<FakeState>,
}

#[derive(Debug, Default)]
struct FakeState {
    sample: MachineSample,
    /// `(root, space)`: `space` answers for `root` and every path under it.
    volumes: Vec<(PathBuf, VolumeSpace)>,
    failure: Option<String>,
}

impl FakeSystemStats {
    /// A fake answering `sample` and no volume.
    pub fn new(sample: MachineSample) -> Self {
        Self {
            state: Mutex::new(FakeState {
                sample,
                ..FakeState::default()
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Replaces the answered sample.
    pub fn set_sample(&self, sample: MachineSample) {
        self.lock().sample = sample;
    }

    /// Moves the CPU counters forward, as if the machine had been busy for `busy` and
    /// idle for `idle` (summed over its processors) since the last sample.
    pub fn advance(&self, busy: Duration, idle: Duration) {
        let mut state = self.lock();
        state.sample.cpu_busy_total += busy;
        state.sample.cpu_idle_total += idle;
    }

    /// Sets the physical memory figures.
    pub fn set_memory(&self, total: u64, avail: u64) {
        let mut state = self.lock();
        state.sample.mem_total = total;
        state.sample.mem_avail = avail;
    }

    /// Answers `space` for `root` and every path under it (the longest matching root
    /// wins). Paths under no root are `NotFound`.
    pub fn set_volume(&self, root: impl Into<PathBuf>, space: VolumeSpace) {
        let root = root.into();
        let mut state = self.lock();
        state.volumes.retain(|(r, _)| *r != root);
        state.volumes.push((root, space));
    }

    /// Makes every call fail with an I/O error carrying `message` (`None` heals).
    pub fn set_failure(&self, message: Option<&str>) {
        self.lock().failure = message.map(str::to_owned);
    }

    fn check(state: &FakeState) -> Result<()> {
        match &state.failure {
            Some(message) => Err(ForgeError::io(
                "fake system stats",
                std::io::Error::other(message.clone()),
            )),
            None => Ok(()),
        }
    }
}

impl SystemStats for FakeSystemStats {
    fn sample(&self) -> Result<MachineSample> {
        let state = self.lock();
        Self::check(&state)?;
        Ok(state.sample)
    }

    fn volume(&self, path: &Path) -> Result<VolumeSpace> {
        let state = self.lock();
        Self::check(&state)?;
        state
            .volumes
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .map(|(_, space)| *space)
            .ok_or_else(|| ForgeError::NotFound(format!("{} (volume)", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(busy_ms: u64, idle_ms: u64) -> MachineSample {
        MachineSample {
            cpu_busy_total: Duration::from_millis(busy_ms),
            cpu_idle_total: Duration::from_millis(idle_ms),
            logical_cpus: 8,
            mem_total: 16 << 30,
            mem_avail: 4 << 30,
        }
    }

    #[test]
    fn machine_cpu_is_busy_over_capacity() {
        let a = sample(10_000, 90_000);
        // 2 s of wall time on 8 CPUs = 16 s of capacity, 4 s of it busy.
        let b = sample(14_000, 102_000);
        assert_eq!(b.cpu_pct_since(&a), Some(25.0));
        assert_eq!(a.cpu_pct_since(&a), None, "no time elapsed");
        assert_eq!(a.cpu_pct_since(&b), None, "counters went backwards");
        // Fully busy and fully idle intervals.
        assert_eq!(sample(26_000, 90_000).cpu_pct_since(&a), Some(100.0));
        assert_eq!(sample(10_000, 106_000).cpu_pct_since(&a), Some(0.0));
        // Busy moved forward but idle went back: a different boot, not a rate.
        assert_eq!(sample(11_000, 1_000).cpu_pct_since(&a), None);
    }

    #[test]
    fn process_share_uses_the_same_capacity() {
        let a = sample(10_000, 90_000);
        let b = sample(14_000, 102_000);
        // One core fully used for 2 s out of 8 cores: 12.5 % of the machine.
        let share = b.cpu_share_since(&a, Duration::from_secs(2)).unwrap();
        assert!((share - 12.5).abs() < 1e-9, "{share}");
        assert!(share <= b.cpu_pct_since(&a).unwrap());
        // Granularity overshoot never reads above the whole machine.
        assert_eq!(b.cpu_share_since(&a, Duration::from_secs(60)), Some(100.0));
        assert_eq!(b.cpu_share_since(&a, Duration::ZERO), Some(0.0));
        assert_eq!(a.cpu_share_since(&a, Duration::from_secs(1)), None);
    }

    #[test]
    fn memory_and_volume_derivations() {
        let s = sample(0, 0);
        assert_eq!(s.mem_used(), 12 << 30);
        assert_eq!(s.mem_used_pct(), Some(75.0));
        let odd = MachineSample {
            mem_total: 0,
            mem_avail: 5,
            ..s
        };
        assert_eq!(odd.mem_used(), 0);
        assert_eq!(odd.mem_used_pct(), None);

        let v = VolumeSpace {
            total: 1000,
            free: 250,
            available: 200,
        };
        assert_eq!(v.used(), 750);
        assert_eq!(v.used_pct(), Some(75.0));
        assert_eq!(VolumeSpace::default().used_pct(), None);
        // A quota smaller than the free space never underflows.
        let quota = VolumeSpace {
            total: 100,
            free: 500,
            available: 100,
        };
        assert_eq!(quota.used(), 0);
    }

    #[test]
    fn fake_answers_what_it_was_given() {
        let fake = FakeSystemStats::new(sample(1_000, 9_000));
        let first = fake.sample().unwrap();
        fake.advance(Duration::from_secs(1), Duration::from_secs(3));
        let second = fake.sample().unwrap();
        assert_eq!(second.cpu_pct_since(&first), Some(25.0));
        fake.set_memory(8 << 30, 2 << 30);
        assert_eq!(fake.sample().unwrap().mem_used(), 6 << 30);
        fake.set_sample(MachineSample::default());
        assert_eq!(fake.sample().unwrap(), MachineSample::default());

        let root = Path::new("/forgebox");
        let data = Path::new("/forgebox/data");
        let small = VolumeSpace {
            total: 10,
            free: 5,
            available: 5,
        };
        let big = VolumeSpace {
            total: 100,
            free: 50,
            available: 40,
        };
        fake.set_volume(root, big);
        fake.set_volume(data, small);
        assert_eq!(fake.volume(Path::new("/forgebox/www/site")).unwrap(), big);
        assert_eq!(
            fake.volume(Path::new("/forgebox/data/mysql")).unwrap(),
            small
        );
        assert_eq!(
            fake.volume(Path::new("/elsewhere")).unwrap_err().code(),
            "not_found"
        );
        fake.set_volume(root, small);
        assert_eq!(fake.volume(root).unwrap(), small);

        fake.set_failure(Some("disk asleep"));
        assert_eq!(fake.sample().unwrap_err().code(), "io");
        assert_eq!(fake.volume(root).unwrap_err().code(), "io");
        fake.set_failure(None);
        assert!(fake.sample().is_ok());
    }

    #[test]
    fn unsupported_fails_without_panicking() {
        let stats = UnsupportedSystemStats;
        assert_eq!(stats.sample().unwrap_err().code(), "unsupported_platform");
        assert_eq!(
            stats.volume(Path::new(".")).unwrap_err().code(),
            "unsupported_platform"
        );
    }

    #[tokio::test]
    async fn async_helpers_run_on_the_blocking_pool() {
        let fake = Arc::new(FakeSystemStats::new(sample(1, 2)));
        fake.set_volume("/r", VolumeSpace::default());
        let stats: Arc<dyn SystemStats> = fake.clone();
        assert_eq!(
            sample_machine(Arc::clone(&stats)).await.unwrap(),
            sample(1, 2)
        );
        assert_eq!(
            volume_space(Arc::clone(&stats), PathBuf::from("/r/x"))
                .await
                .unwrap(),
            VolumeSpace::default()
        );
        fake.set_failure(Some("boom"));
        assert_eq!(sample_machine(stats).await.unwrap_err().code(), "io");
    }
}
