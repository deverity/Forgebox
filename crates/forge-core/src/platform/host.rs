//! Facilities of the process that hosts the daemon (docs/01 §1, §5.1; docs/05 §2):
//!
//! * [`InstanceMutex`]: the named mutex that makes `forgeboxd` a single instance per
//!   installation root (`Global\Forgebox.Daemon` for the default root,
//!   `Global\Forgebox.Daemon-<key>` for the others, [`daemon_mutex_name`]). The name
//!   exists while the handle is open; the OS closes it when the process dies, so a crash
//!   never leaves a stale lock behind.
//! * [`NetworkCost`]: whether the active connection is metered (Windows
//!   `INetworkCostManager`), the probe the update scheduler checks before background
//!   downloads (`[updates] metered`).
//! * [`spawn_detached`]: launches a process outside the caller's console and process
//!   group (`daemon.restart` starts the new daemon this way before the old one exits);
//!   [`spawn_detached_watched`] also reports how it ended (client auto-start of the
//!   daemon, `forge_ipc::client::local`).
//!
//! All calls are blocking (fast syscalls; the network cost goes through COM): async code
//! uses `spawn_blocking`.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use crate::error::Result;
use crate::paths::{ForgeRoot, root_instance_key};

/// Prefix of the daemon mutex name (docs/01 §1).
pub const DAEMON_MUTEX_PREFIX: &str = r"Global\Forgebox.Daemon";

/// Name of the single-instance mutex of the daemon of `root`.
pub fn daemon_mutex_name(root: &ForgeRoot) -> String {
    match root_instance_key(root) {
        None => DAEMON_MUTEX_PREFIX.to_owned(),
        Some(key) => format!("{DAEMON_MUTEX_PREFIX}-{key}"),
    }
}

/// An owned named mutex. Dropping it closes the handle (and frees the name when no other
/// handle is open).
pub struct InstanceMutex {
    name: String,
    #[cfg(windows)]
    _inner: super::windows::NamedMutex,
}

impl std::fmt::Debug for InstanceMutex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstanceMutex")
            .field("name", &self.name)
            .finish()
    }
}

impl InstanceMutex {
    /// The mutex name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Creates the named mutex `name`. Fails with `Conflict` when it already exists (another
/// daemon of the same root is running, possibly as another user).
pub fn acquire_instance_mutex(name: &str) -> Result<InstanceMutex> {
    #[cfg(windows)]
    {
        Ok(InstanceMutex {
            name: name.to_owned(),
            _inner: super::windows::NamedMutex::create_new(name)?,
        })
    }
    #[cfg(not(windows))]
    {
        Err(crate::error::ForgeError::UnsupportedPlatform(format!(
            "named mutex {name}"
        )))
    }
}

/// Cost of the network connection.
pub trait NetworkCost: Send + Sync {
    /// The connection is metered (fixed or variable cost, roaming or over its data limit).
    /// Blocking.
    fn is_metered(&self) -> Result<bool>;
}

/// The connection cost of the running OS.
pub fn network_cost() -> Arc<dyn NetworkCost> {
    #[cfg(windows)]
    {
        Arc::new(super::windows::WindowsNetworkCost)
    }
    #[cfg(not(windows))]
    {
        Arc::new(NeverMetered)
    }
}

/// [`NetworkCost`] that always answers "not metered" (other OSes, tests).
#[derive(Debug, Clone, Copy, Default)]
pub struct NeverMetered;

impl NetworkCost for NeverMetered {
    fn is_metered(&self) -> Result<bool> {
        Ok(false)
    }
}

/// `path` with every 8.3 short component expanded to its long name (`GUSTAV~1` →
/// `gustavo_assis`) when it exists; junctions and symlinks are **not** resolved and the
/// path is otherwise unchanged. Paths written into server configs must be long (the
/// Windows nginx refuses short names, docs/03 §3.1). `path` itself when it cannot be
/// expanded (missing, other OSes). Blocking (one syscall).
pub fn long_path(path: &Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        super::windows::long_path(path).unwrap_or_else(|| path.to_path_buf())
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// Starts `exe` with `args` in `cwd`, detached from the caller's console and process
/// group and, when the caller's Job Object allows it, outside that job; standard handles
/// go to `NUL`. Returns the pid. The caller never waits for it.
pub fn spawn_detached(exe: &Path, args: &[OsString], cwd: &Path) -> Result<u32> {
    #[cfg(windows)]
    {
        super::windows::spawn_detached(exe, args, cwd)
    }
    #[cfg(not(windows))]
    {
        let child = std::process::Command::new(exe)
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| crate::error::ForgeError::io_at("starting", exe, e))?;
        Ok(child.id())
    }
}

/// [`spawn_detached`] that also watches how the new process ends, without racing a pid
/// that may already be gone: an auto-started daemon that exits at once (another one owns
/// the root, a broken root) is reported instead of waited for. Dropping the
/// [`ProcessExit`] only stops watching; the process keeps running.
pub fn spawn_detached_watched(
    exe: &Path,
    args: &[OsString],
    cwd: &Path,
) -> Result<super::process::ProcessExit> {
    #[cfg(windows)]
    {
        super::windows::spawn_detached_watched(exe, args, cwd)
    }
    #[cfg(not(windows))]
    {
        let child = std::process::Command::new(exe)
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| crate::error::ForgeError::io_at("starting", exe, e))?;
        let pid = child.id();
        Ok(super::process::ProcessExit::new(
            pid,
            Box::new(unix_exit::ChildExit(std::sync::Mutex::new(child))),
        ))
    }
}

#[cfg(not(windows))]
mod unix_exit {
    use std::task::{Context, Poll};
    use std::time::Duration;

    use crate::error::{ForgeError, Result};
    use crate::platform::process::ExitSource;

    /// Exit of a std child, polled (the Linux/macOS ports may replace it with pidfd).
    pub(super) struct ChildExit(pub(super) std::sync::Mutex<std::process::Child>);

    impl ExitSource for ChildExit {
        fn poll_exit(&mut self, cx: &mut Context<'_>) -> Poll<Result<u32>> {
            match self.try_exit_code() {
                Ok(Some(code)) => Poll::Ready(Ok(code)),
                Ok(None) => {
                    let waker = cx.waker().clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(50));
                        waker.wake();
                    });
                    Poll::Pending
                }
                Err(err) => Poll::Ready(Err(err)),
            }
        }

        fn try_exit_code(&self) -> Result<Option<u32>> {
            let mut child = self
                .0
                .lock()
                .map_err(|_| ForgeError::Internal("child exit lock poisoned".into()))?;
            child
                .try_wait()
                .map(|status| status.map(|s| s.code().unwrap_or(-1) as u32))
                .map_err(|e| ForgeError::io("polling a process exit", e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn short_names_are_expanded_and_missing_paths_kept() {
        let dir = tempfile::tempdir().unwrap();
        let long = dir.path().join("a long folder name");
        std::fs::create_dir_all(&long).unwrap();
        let expanded = long_path(&long);
        let text = expanded.to_string_lossy().into_owned();
        assert!(text.ends_with("a long folder name"), "{text}");
        assert!(!text.contains('~'), "{text}");
        let missing = long.join("missing");
        assert_eq!(long_path(&missing), missing);
    }

    #[test]
    fn mutex_name_follows_the_root_key() {
        let default = ForgeRoot::new(crate::paths::default_root()).unwrap();
        assert_eq!(daemon_mutex_name(&default), DAEMON_MUTEX_PREFIX);
        let other = ForgeRoot::new(r"D:\Portable\Forgebox").unwrap();
        let name = daemon_mutex_name(&other);
        assert!(name.starts_with(r"Global\Forgebox.Daemon-"), "{name}");
        assert_eq!(name.len(), DAEMON_MUTEX_PREFIX.len() + 17);
    }

    #[cfg(windows)]
    #[test]
    fn second_mutex_with_the_same_name_conflicts() {
        let name = format!(r"Local\Forgebox.Test-{}", crate::ids::JobId::new());
        let first = acquire_instance_mutex(&name).unwrap();
        let err = acquire_instance_mutex(&name).unwrap_err();
        assert_eq!(err.code(), "conflict", "{err}");
        drop(first);
        let again = acquire_instance_mutex(&name).unwrap();
        assert_eq!(again.name(), name);
    }

    #[cfg(windows)]
    #[test]
    fn network_cost_answers() {
        // Any answer is fine (depends on the machine); it must not fail or panic.
        let _ = network_cost().is_metered();
    }

    #[cfg(windows)]
    #[test]
    fn detached_start_runs_the_program_and_reports_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        let shell = std::env::var_os("ComSpec")
            .map(std::path::PathBuf::from)
            .unwrap();
        let args: Vec<OsString> = vec!["/c".into(), "exit".into(), "0".into()];
        let pid = spawn_detached(&shell, &args, dir.path()).unwrap();
        assert!(pid > 0);
        let missing = dir.path().join("nope.exe");
        let err = spawn_detached(&missing, &[], dir.path()).unwrap_err();
        assert_eq!(err.code(), "not_found", "{err}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_watched_detached_start_reports_an_early_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let shell = std::env::var_os("ComSpec")
            .map(std::path::PathBuf::from)
            .unwrap();
        let args: Vec<OsString> = vec!["/c".into(), "exit".into(), "4".into()];
        let mut exit = spawn_detached_watched(&shell, &args, dir.path()).unwrap();
        assert!(exit.pid() > 0);
        let code = exit
            .wait_timeout(std::time::Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(code, Some(4));
        assert_eq!(exit.try_exit_code().unwrap(), Some(4));
        let missing = dir.path().join("nope.exe");
        let err = spawn_detached_watched(&missing, &[], dir.path()).unwrap_err();
        assert_eq!(err.code(), "not_found", "{err}");
    }
}
