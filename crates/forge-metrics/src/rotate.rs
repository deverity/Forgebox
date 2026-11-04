//! Access log rotation decided by the daemon (M2-E5-T12, docs/14 section 3.8).
//!
//! The manifest's `logs[].access.rotate` says how an access log is kept from growing for
//! ever. This module is the tool-independent half of it (std only, no async, no Win32):
//!
//! * [`Strategy::RenameReopen`]: when the file reaches `max_size` the daemon renames it to
//!   `<name>.1` (older ones shift to `.2`..`.<keep>`, the one past `keep` is deleted), then
//!   asks the server to reopen its log (the manifest's `reopen`, run by the daemon through
//!   the instance's reload path). Writers that still hold the old file keep appending to
//!   `.1` until they reopen; the tailer (docs/14 section 3.2) drains it, so no line is lost
//!   or read twice;
//! * [`Strategy::SelfRotating`]: the server rolls by itself (Caddy `roll_size`); nothing is
//!   done here, the tailer follows the rolls;
//! * [`Strategy::None`]: never rotated; [`Rotator::check`] only reports (once per crossing)
//!   when the file passed [`NONE_WARN_FACTOR`] times `max_size`.
//!
//! [`Rotator`] holds the per-file state of the checks (pending reopen, failure back-off,
//! warning already given) and is driven by the caller: [`Rotator::check`] says what to do,
//! [`rotate_files`] does the renames (blocking), [`Rotator::rotated`] and
//! [`Rotator::reopened`] record the steps. The caller runs the reopen in between.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Size that triggers a rotation when the manifest gives none (64 MiB).
pub const DEFAULT_MAX_SIZE: u64 = 64 << 20;
/// Rotated files kept when the caller gives none.
pub const DEFAULT_KEEP: u32 = 1;
/// `none`: warn once the file is this many times `max_size`.
pub const NONE_WARN_FACTOR: u64 = 4;
/// Two rotations of the same file are at least this far apart (the tailer still drains the
/// previous one; see `MAX_RETIRED` in docs/14 section 3.2).
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);
/// First wait after a failed rotation or reopen; doubles up to [`MAX_BACKOFF`].
pub const FIRST_BACKOFF: Duration = Duration::from_secs(5);
/// Longest wait after repeated failures.
pub const MAX_BACKOFF: Duration = Duration::from_secs(600);

/// `logs[].access.rotate.strategy`, without the manifest types (this crate depends only on
/// `forge-core`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strategy {
    /// The daemon renames the file and asks the server to reopen it.
    RenameReopen,
    /// The server rotates by itself.
    SelfRotating,
    /// Not rotated.
    #[default]
    None,
}

/// What to do with one access log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotatePolicy {
    pub strategy: Strategy,
    /// Size that triggers the rotation (or, for `none`, the base of the warning).
    pub max_size: u64,
    /// Rotated files kept (`.1`..`.<keep>`), at least 1.
    pub keep: u32,
}

impl RotatePolicy {
    /// A policy with the defaults ([`DEFAULT_MAX_SIZE`], [`DEFAULT_KEEP`]) for what is
    /// missing.
    pub fn new(strategy: Strategy, max_size: Option<u64>, keep: Option<u32>) -> Self {
        Self {
            strategy,
            max_size: max_size.filter(|s| *s > 0).unwrap_or(DEFAULT_MAX_SIZE),
            keep: keep.unwrap_or(DEFAULT_KEEP).max(1),
        }
    }
}

/// The verdict of [`Rotator::check`] for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Nothing to do.
    Idle,
    /// Rename the file ([`rotate_files`]), then reopen it in the server.
    Rotate { size: u64 },
    /// A rotation renamed the file but the server was not told (or failed to reopen):
    /// run the reopen again.
    Reopen,
    /// `none`: the file is `size` bytes, over `limit`. Given once per crossing.
    Warn { size: u64, limit: u64 },
}

#[derive(Debug, Default)]
struct FileState {
    /// Renamed but not reopened yet.
    pending_reopen: bool,
    last_rotation: Option<Instant>,
    /// No action before this instant (failure back-off).
    blocked_until: Option<Instant>,
    failures: u32,
    warned: bool,
}

/// Per-file state of the rotation checks.
#[derive(Debug)]
pub struct Rotator {
    min_interval: Duration,
    files: HashMap<PathBuf, FileState>,
}

impl Default for Rotator {
    fn default() -> Self {
        Self::new(MIN_INTERVAL)
    }
}

impl Rotator {
    /// A rotator that waits at least `min_interval` between two rotations of a file.
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            files: HashMap::new(),
        }
    }

    /// What to do with `path` at `now`. Reads only the file's metadata (no read access, so
    /// an antivirus does not rescan the log at every check).
    pub fn check(&mut self, path: &Path, policy: &RotatePolicy, now: Instant) -> Check {
        let state = self.files.entry(path.to_path_buf()).or_default();
        if state.blocked_until.is_some_and(|until| now < until) {
            return Check::Idle;
        }
        match policy.strategy {
            Strategy::SelfRotating => Check::Idle,
            Strategy::None => {
                let size = std::fs::metadata(path).map_or(0, |m| m.len());
                let limit = policy.max_size.saturating_mul(NONE_WARN_FACTOR);
                if size > limit {
                    if !state.warned {
                        state.warned = true;
                        return Check::Warn { size, limit };
                    }
                } else {
                    state.warned = false;
                }
                Check::Idle
            }
            Strategy::RenameReopen => {
                if state.pending_reopen {
                    return Check::Reopen;
                }
                if state
                    .last_rotation
                    .is_some_and(|at| now.saturating_duration_since(at) < self.min_interval)
                {
                    return Check::Idle;
                }
                match std::fs::metadata(path) {
                    Ok(meta) if meta.len() >= policy.max_size => Check::Rotate { size: meta.len() },
                    _ => Check::Idle,
                }
            }
        }
    }

    /// The rename of `path` succeeded: the reopen is due.
    pub fn rotated(&mut self, path: &Path, now: Instant) {
        let state = self.files.entry(path.to_path_buf()).or_default();
        state.pending_reopen = true;
        state.last_rotation = Some(now);
        state.blocked_until = None;
    }

    /// The server reopened its log.
    pub fn reopened(&mut self, path: &Path) {
        let state = self.files.entry(path.to_path_buf()).or_default();
        state.pending_reopen = false;
        state.failures = 0;
        state.blocked_until = None;
    }

    /// The rename or the reopen failed: wait before trying again (5 s, doubling to 10 min).
    /// Returns the wait.
    pub fn failed(&mut self, path: &Path, now: Instant) -> Duration {
        let state = self.files.entry(path.to_path_buf()).or_default();
        state.failures = state.failures.saturating_add(1);
        let shift = (state.failures - 1).min(10);
        let wait = FIRST_BACKOFF.saturating_mul(1u32 << shift).min(MAX_BACKOFF);
        state.blocked_until = Some(now + wait);
        wait
    }

    /// Forgets the files that are not in `keep` (sources that left).
    pub fn retain(&mut self, keep: &[&Path]) {
        self.files.retain(|path, _| keep.contains(&path.as_path()));
    }
}

/// `<path>.<n>` (`access.json.1`).
pub fn rotated_name(path: &Path, n: u32) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

/// Shifts the rotated files and renames `path` to `<path>.1`: `.<keep>` is deleted, `.n`
/// becomes `.n+1`. Blocking. A missing file is fine (nothing to rotate). On `Err` the
/// shifts already done stay (the names only move up, no file is lost, except the one past
/// `keep` which is meant to go).
///
/// Deleting and renaming work on Windows even while the tailer or the server holds a file
/// open: std opens with the `READ | WRITE | DELETE` share mode and so do nginx and Apache
/// (docs/14 section 3.8 has the measurement); Caddy does not, which is why it rotates itself.
pub fn rotate_files(path: &Path, keep: u32) -> io::Result<()> {
    let keep = keep.max(1);
    let oldest = rotated_name(path, keep);
    match std::fs::remove_file(&oldest) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    for n in (1..keep).rev() {
        let from = rotated_name(path, n);
        match std::fs::rename(&from, rotated_name(path, n + 1)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    match std::fs::rename(path, rotated_name(path, 1)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: usize) {
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    fn policy(strategy: Strategy, max: u64, keep: u32) -> RotatePolicy {
        RotatePolicy::new(strategy, Some(max), Some(keep))
    }

    #[test]
    fn defaults_are_64_mib_and_one_file() {
        let p = RotatePolicy::new(Strategy::RenameReopen, None, None);
        assert_eq!((p.max_size, p.keep), (64 << 20, 1));
        let p = RotatePolicy::new(Strategy::RenameReopen, Some(0), Some(0));
        assert_eq!((p.max_size, p.keep), (64 << 20, 1));
    }

    #[test]
    fn files_shift_and_the_oldest_goes() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("access.json");
        std::fs::write(&log, "gen1").unwrap();
        rotate_files(&log, 2).unwrap();
        std::fs::write(&log, "gen2").unwrap();
        rotate_files(&log, 2).unwrap();
        std::fs::write(&log, "gen3").unwrap();
        rotate_files(&log, 2).unwrap();
        assert!(!log.exists());
        assert_eq!(
            std::fs::read_to_string(rotated_name(&log, 1)).unwrap(),
            "gen3"
        );
        assert_eq!(
            std::fs::read_to_string(rotated_name(&log, 2)).unwrap(),
            "gen2"
        );
        assert!(!rotated_name(&log, 3).exists(), "gen1 is past keep");
    }

    #[test]
    fn rotating_a_missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        rotate_files(&dir.path().join("none.json"), 3).unwrap();
    }

    #[test]
    fn rename_reopen_rotates_at_max_size_then_asks_for_the_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("access.json");
        let policy = policy(Strategy::RenameReopen, 100, 1);
        let mut r = Rotator::new(Duration::from_secs(1));
        let t0 = Instant::now();
        assert_eq!(r.check(&log, &policy, t0), Check::Idle, "no file");
        write(&log, 99);
        assert_eq!(r.check(&log, &policy, t0), Check::Idle);
        write(&log, 100);
        assert_eq!(r.check(&log, &policy, t0), Check::Rotate { size: 100 });
        rotate_files(&log, policy.keep).unwrap();
        r.rotated(&log, t0);
        // Until the reopen is recorded the answer is `Reopen`, even without a file.
        assert_eq!(r.check(&log, &policy, t0), Check::Reopen);
        r.reopened(&log);
        write(&log, 500);
        assert_eq!(
            r.check(&log, &policy, t0 + Duration::from_millis(500)),
            Check::Idle,
            "too soon after the last rotation"
        );
        assert_eq!(
            r.check(&log, &policy, t0 + Duration::from_secs(2)),
            Check::Rotate { size: 500 }
        );
    }

    #[test]
    fn failures_back_off_and_double() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("access.json");
        write(&log, 1000);
        let policy = policy(Strategy::RenameReopen, 100, 1);
        let mut r = Rotator::new(Duration::ZERO);
        let t0 = Instant::now();
        assert_eq!(r.failed(&log, t0), Duration::from_secs(5));
        assert_eq!(r.check(&log, &policy, t0), Check::Idle);
        assert_eq!(r.failed(&log, t0), Duration::from_secs(10));
        assert_eq!(
            r.check(&log, &policy, t0 + Duration::from_secs(11)),
            Check::Rotate { size: 1000 }
        );
        for _ in 0..30 {
            r.failed(&log, t0);
        }
        assert_eq!(r.failed(&log, t0), MAX_BACKOFF);
        r.reopened(&log);
        assert_eq!(r.check(&log, &policy, t0), Check::Rotate { size: 1000 });
    }

    #[test]
    fn self_rotating_is_never_touched() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("access.json");
        write(&log, 10_000);
        let policy = policy(Strategy::SelfRotating, 100, 1);
        let mut r = Rotator::default();
        assert_eq!(r.check(&log, &policy, Instant::now()), Check::Idle);
    }

    #[test]
    fn none_warns_once_above_four_times_max_size() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("access.json");
        let policy = policy(Strategy::None, 100, 1);
        let mut r = Rotator::default();
        let now = Instant::now();
        write(&log, 400);
        assert_eq!(r.check(&log, &policy, now), Check::Idle, "4x is the limit");
        write(&log, 401);
        assert_eq!(
            r.check(&log, &policy, now),
            Check::Warn {
                size: 401,
                limit: 400
            }
        );
        write(&log, 900);
        assert_eq!(r.check(&log, &policy, now), Check::Idle, "once");
        write(&log, 10);
        assert_eq!(r.check(&log, &policy, now), Check::Idle);
        write(&log, 500);
        assert!(matches!(r.check(&log, &policy, now), Check::Warn { .. }));
    }
}
