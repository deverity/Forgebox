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
//!   group (`daemon.restart` starts the new daemon this way before the old one exits).
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
