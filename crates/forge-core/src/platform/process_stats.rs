//! Resource usage of single processes (docs/03 §6, docs/14 §2): the raw counters the
//! supervisor's metrics sampler turns into CPU %, memory and handle figures per instance.
//!
//! [`ProcessStats::sample`] is **blocking** (it opens the process and asks the kernel)
//! and belongs in `spawn_blocking` (ADR-0011). It returns cumulative counters only; rates
//! such as CPU % come from two samples (`Δcpu_total / Δwall / logical_cpus`), which is
//! how Task Manager computes its "CPU" column.
//!
//! [`ProcessStats::info`] names a process: its identity `(pid, started_at)` and executable
//! ([`ProcessInfo`]), what `forge top` and the machine monitor show next to each PID
//! (docs/14 §4.2-§4.3). [`ImageNameCache`] keeps the base names by identity, so a sampler
//! that runs every 2 s asks the OS once per process, and a PID reused by a newer process
//! never inherits the old name.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::error::{ForgeError, Result};
use crate::sync::{LockLevel, OrderedMutex};

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

    /// When `pid` was created, without the other counters (cheaper than
    /// [`ProcessStats::sample`]). `NotFound` when no such process exists. Blocking.
    fn creation_time(&self, pid: u32) -> Result<SystemTime> {
        self.sample(pid).map(|stats| stats.started_at)
    }

    /// Logical processors of the machine, every processor group included: the divisor
    /// that turns CPU time per wall time into a share of the whole machine (0–100 %).
    fn logical_cpus(&self) -> u32;

    /// Identity and executable of `pid` (Windows: one `PROCESS_QUERY_LIMITED_INFORMATION`
    /// handle for the creation time and `QueryFullProcessImageNameW`). A process that opens
    /// but whose image cannot be read has `image_path: None`; one that cannot be opened at
    /// all (access denied: the System process for a non-elevated caller, protected
    /// processes of other users) is an `io` error. `NotFound` when no such process exists.
    /// Blocking; [`ImageNameCache`] avoids asking again for the same process.
    ///
    /// The default implementation knows no image: `image_path`/`image_name` are `None`.
    fn info(&self, pid: u32) -> Result<ProcessInfo> {
        self.creation_time(pid)
            .map(|started_at| ProcessInfo::new(pid, started_at, None))
    }
}

/// Identity and executable of one process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    /// When the process was created: with `pid`, its identity (see
    /// [`ProcStats::started_at`]).
    pub started_at: SystemTime,
    /// Full path of the executable (`C:\forgebox\bin\php\8.4.27\php-cgi.exe`); `None` when
    /// the OS does not tell.
    pub image_path: Option<PathBuf>,
    /// Base name of the executable (`php-cgi.exe`): the last component of `image_path`.
    pub image_name: Option<String>,
}

impl ProcessInfo {
    /// Info of `pid` created at `started_at` running `image_path`; `image_name` is derived.
    pub fn new(pid: u32, started_at: SystemTime, image_path: Option<PathBuf>) -> Self {
        let image_name = image_path
            .as_deref()
            .and_then(std::path::Path::file_name)
            .map(|name| name.to_string_lossy().into_owned());
        Self {
            pid,
            started_at,
            image_path,
            image_name,
        }
    }
}

/// Identity of a process: its PID and creation time.
type ProcessKey = (u32, SystemTime);

/// Lock of the [`ImageNameCache`] table: rank 10, leaf. Held only to look up, insert or
/// prune names; never while the OS is asked.
const IMAGE_NAMES_LOCK: LockLevel = LockLevel::new(10, "platform.image_names");

/// Image base names of processes, cached by identity `(pid, started_at)`.
///
/// [`ImageNameCache::name`] answers from the table, or asks [`ProcessStats::info`] once
/// (blocking: call it where the sampling already runs, inside `spawn_blocking`). A PID
/// whose process was replaced by a newer one (another `started_at`) answers `None` for the
/// old identity, never the new process' name. Processes the OS refuses to name are
/// remembered as unnamed; processes that are gone are not remembered at all.
///
/// The table is bounded: the owner calls [`ImageNameCache::retain`] with the processes
/// of its last sampling round, and past `capacity` entries the table starts over.
#[derive(Debug)]
pub struct ImageNameCache {
    stats: Arc<dyn ProcessStats>,
    capacity: usize,
    names: OrderedMutex<HashMap<ProcessKey, Option<Arc<str>>>>,
}

impl ImageNameCache {
    /// Entries kept before the table starts over (far above the processes of a Forgebox
    /// installation: services, pool workers, apps and their children).
    pub const DEFAULT_CAPACITY: usize = 4096;

    /// Cache in front of `stats`, bounded by [`ImageNameCache::DEFAULT_CAPACITY`].
    pub fn new(stats: Arc<dyn ProcessStats>) -> Self {
        Self::with_capacity(stats, Self::DEFAULT_CAPACITY)
    }

    /// Cache in front of `stats` holding at most `capacity` (at least 1) entries.
    pub fn with_capacity(stats: Arc<dyn ProcessStats>, capacity: usize) -> Self {
        Self {
            stats,
            capacity: capacity.max(1),
            names: OrderedMutex::new(IMAGE_NAMES_LOCK, HashMap::new()),
        }
    }

    /// Base name of the executable of the process `pid` created at `started_at`
    /// (`php-cgi.exe`). `None` when the process is gone or was replaced, or the OS does
    /// not tell its image. Blocking on a miss.
    pub fn name(&self, pid: u32, started_at: SystemTime) -> Option<Arc<str>> {
        if let Some(hit) = self.names.lock().get(&(pid, started_at)) {
            return hit.clone();
        }
        match self.stats.info(pid) {
            Ok(info) => {
                let name: Option<Arc<str>> = info.image_name.map(Arc::from);
                // Remembered under the identity the OS reported: when the PID was reused,
                // that is the newer process, and the asked one gets nothing.
                self.insert((pid, info.started_at), name.clone());
                if info.started_at == started_at {
                    name
                } else {
                    None
                }
            }
            // Gone: its identity never comes back, nothing to remember.
            Err(ForgeError::NotFound(_)) => None,
            // Access denied and the like do not change for the same process.
            Err(_) => {
                self.insert((pid, started_at), None);
                None
            }
        }
    }

    /// [`ImageNameCache::name`] of the process `stats` was sampled from.
    pub fn name_of(&self, stats: &ProcStats) -> Option<Arc<str>> {
        self.name(stats.pid, stats.started_at)
    }

    /// Keeps only the identities for which `keep(pid, started_at)` is true (the
    /// processes of the last sampling round).
    pub fn retain(&self, mut keep: impl FnMut(u32, SystemTime) -> bool) {
        self.names
            .lock()
            .retain(|&(pid, started_at), _| keep(pid, started_at));
    }

    /// Identities currently remembered.
    pub fn len(&self) -> usize {
        self.names.lock().len()
    }

    /// Whether nothing is remembered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn insert(&self, key: ProcessKey, name: Option<Arc<str>>) {
        let mut names = self.names.lock();
        if names.len() >= self.capacity && !names.contains_key(&key) {
            names.clear();
        }
        names.insert(key, name);
    }
}

/// Scriptable [`ProcessStats`] for tests: answers the processes it was given and counts
/// the [`ProcessStats::info`] calls (to check caching).
#[derive(Debug)]
pub struct FakeProcessStats {
    logical_cpus: u32,
    state: Mutex<FakeProcesses>,
}

#[derive(Debug, Default)]
struct FakeProcesses {
    processes: HashMap<u32, (ProcStats, Option<PathBuf>)>,
    /// PIDs whose `info` fails with an I/O error (access denied).
    denied: Vec<u32>,
    info_calls: usize,
}

impl FakeProcessStats {
    /// No processes, `logical_cpus` processors.
    pub fn new(logical_cpus: u32) -> Self {
        Self {
            logical_cpus,
            state: Mutex::new(FakeProcesses::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeProcesses> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Adds (or replaces: a reused PID) the process `stats.pid` running `image_path`.
    pub fn insert(&self, stats: ProcStats, image_path: Option<PathBuf>) {
        self.lock().processes.insert(stats.pid, (stats, image_path));
    }

    /// The process `pid` exits: every later call answers `NotFound`.
    pub fn remove(&self, pid: u32) {
        self.lock().processes.remove(&pid);
    }

    /// Makes [`ProcessStats::info`] of `pid` fail with an access denied I/O error.
    pub fn deny(&self, pid: u32) {
        self.lock().denied.push(pid);
    }

    /// How many times [`ProcessStats::info`] was called.
    pub fn info_calls(&self) -> usize {
        self.lock().info_calls
    }
}

impl ProcessStats for FakeProcessStats {
    fn sample(&self, pid: u32) -> Result<ProcStats> {
        self.lock()
            .processes
            .get(&pid)
            .map(|(stats, _)| *stats)
            .ok_or_else(|| ForgeError::NotFound(format!("process {pid}")))
    }

    fn logical_cpus(&self) -> u32 {
        self.logical_cpus
    }

    fn info(&self, pid: u32) -> Result<ProcessInfo> {
        let mut state = self.lock();
        state.info_calls += 1;
        if state.denied.contains(&pid) {
            return Err(ForgeError::io(
                format!("opening process {pid}"),
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ));
        }
        state
            .processes
            .get(&pid)
            .map(|(stats, path)| ProcessInfo::new(pid, stats.started_at, path.clone()))
            .ok_or_else(|| ForgeError::NotFound(format!("process {pid}")))
    }
}

/// Process counters for the running OS.
#[cfg(windows)]
pub fn process_stats() -> Arc<dyn ProcessStats> {
    Arc::new(super::windows::WindowsProcessStats)
}

/// When the process `pid` of the running OS was created: the start of the desktop shell's
/// cold-start measurement (`Forgebox.exe` reads its own pid, M2-E1-T7). `NotFound` when no
/// such process exists; `UnsupportedPlatform` until the Linux/macOS ports. Blocking (one
/// process open and one kernel query).
pub fn process_creation_time(pid: u32) -> Result<SystemTime> {
    process_stats().creation_time(pid)
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

        fn info(&self, _: u32) -> Result<ProcessInfo> {
            Err(ForgeError::UnsupportedPlatform(format!(
                "process information is not implemented on {}",
                std::env::consts::OS
            )))
        }
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    fn proc_stats(pid: u32, started_secs: u64) -> ProcStats {
        ProcStats {
            pid,
            cpu_total: Duration::ZERO,
            rss_bytes: 0,
            private_bytes: 0,
            handles: 0,
            started_at: SystemTime::UNIX_EPOCH + Duration::from_secs(started_secs),
            uptime: Duration::ZERO,
        }
    }

    fn exe(name: &str) -> Option<PathBuf> {
        Some(PathBuf::from("/forgebox/bin/tool/1.0").join(name))
    }

    #[test]
    fn info_derives_the_base_name() {
        let at = SystemTime::UNIX_EPOCH;
        let info = ProcessInfo::new(7, at, exe("php-cgi.exe"));
        assert_eq!(info.image_name.as_deref(), Some("php-cgi.exe"));
        assert_eq!(ProcessInfo::new(7, at, None).image_name, None);
    }

    #[test]
    fn default_info_knows_no_image() {
        #[derive(Debug)]
        struct CountersOnly;
        impl ProcessStats for CountersOnly {
            fn sample(&self, pid: u32) -> Result<ProcStats> {
                Ok(proc_stats(pid, 5))
            }
            fn logical_cpus(&self) -> u32 {
                1
            }
        }
        let info = CountersOnly.info(3).unwrap();
        assert_eq!(info, ProcessInfo::new(3, proc_stats(3, 5).started_at, None));
        let cache = ImageNameCache::new(Arc::new(CountersOnly));
        assert_eq!(cache.name(3, info.started_at), None);
    }

    #[test]
    fn names_are_cached_by_identity() {
        let fake = Arc::new(FakeProcessStats::new(4));
        let nginx = proc_stats(100, 10);
        fake.insert(nginx, exe("nginx.exe"));
        let cache = ImageNameCache::new(fake.clone());

        assert_eq!(cache.name_of(&nginx).as_deref(), Some("nginx.exe"));
        assert_eq!(cache.name_of(&nginx).as_deref(), Some("nginx.exe"));
        assert_eq!(fake.info_calls(), 1, "the second lookup is a hit");

        // PID 100 reused by a newer process: the cached identity still answers its own
        // name, an identity never seen gets nothing.
        let php = proc_stats(100, 20);
        fake.insert(php, exe("php-cgi.exe"));
        assert_eq!(
            cache.name_of(&nginx).as_deref(),
            Some("nginx.exe"),
            "cached"
        );
        let calls = fake.info_calls();
        let older = proc_stats(100, 5);
        assert_eq!(cache.name_of(&older), None, "replaced process");
        assert_eq!(fake.info_calls(), calls + 1);
        // ...and that lookup remembered the newer process under its own identity.
        assert_eq!(cache.name_of(&php).as_deref(), Some("php-cgi.exe"));
        assert_eq!(fake.info_calls(), calls + 1);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn gone_processes_are_not_remembered_but_denied_ones_are() {
        let fake = Arc::new(FakeProcessStats::new(4));
        let cache = ImageNameCache::new(fake.clone());
        let gone = proc_stats(1, 1);
        assert_eq!(cache.name_of(&gone), None);
        assert_eq!(cache.name_of(&gone), None);
        assert_eq!(fake.info_calls(), 2, "a gone process is asked again");
        assert!(cache.is_empty());

        let protected = proc_stats(2, 1);
        fake.insert(protected, exe("csrss.exe"));
        fake.deny(2);
        assert_eq!(cache.name_of(&protected), None);
        assert_eq!(cache.name_of(&protected), None);
        assert_eq!(fake.info_calls(), 3, "access denied is remembered");

        let nameless = proc_stats(3, 1);
        fake.insert(nameless, None);
        assert_eq!(cache.name_of(&nameless), None);
        assert_eq!(cache.name_of(&nameless), None);
        assert_eq!(
            fake.info_calls(),
            4,
            "an image the OS does not tell is remembered"
        );
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn retain_and_capacity_bound_the_table() {
        let fake = Arc::new(FakeProcessStats::new(4));
        let cache = ImageNameCache::with_capacity(fake.clone(), 3);
        let procs: Vec<ProcStats> = (1..=3).map(|pid| proc_stats(pid, 1)).collect();
        for p in &procs {
            fake.insert(*p, exe(&format!("p{}.exe", p.pid)));
            assert!(cache.name_of(p).is_some());
        }
        assert_eq!(cache.len(), 3);
        cache.retain(|pid, _| pid != 2);
        assert_eq!(cache.len(), 2);

        // Two more processes: the table starts over instead of growing past 3.
        for pid in [4, 5] {
            let p = proc_stats(pid, 1);
            fake.insert(p, exe("x.exe"));
            assert_eq!(cache.name_of(&p).as_deref(), Some("x.exe"));
        }
        assert!(cache.len() <= 3, "{}", cache.len());
        // A hit or a new lookup, the answer is the same.
        assert_eq!(cache.name_of(&procs[0]).as_deref(), Some("p1.exe"));
    }

    #[test]
    fn fake_answers_not_found_after_exit() {
        let fake = FakeProcessStats::new(2);
        let p = proc_stats(9, 3);
        fake.insert(p, exe("a.exe"));
        assert_eq!(fake.sample(9).unwrap(), p);
        assert_eq!(fake.creation_time(9).unwrap(), p.started_at);
        assert_eq!(fake.logical_cpus(), 2);
        fake.remove(9);
        assert_eq!(fake.sample(9).unwrap_err().code(), "not_found");
        assert_eq!(fake.info(9).unwrap_err().code(), "not_found");
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
        let created = process_creation_time(pid).unwrap();
        assert_eq!(created, first.started_at);
        assert!(created <= SystemTime::now());
        assert_eq!(
            process_creation_time(u32::MAX - 3).unwrap_err().code(),
            "not_found"
        );
        assert_eq!(
            stats.sample(u32::MAX - 3).unwrap_err().code(),
            "not_found",
            "no such process"
        );
    }
}
