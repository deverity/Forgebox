//! Single instance per root (docs/01 §1, §5.1 step 1): the named mutex
//! `Global\Forgebox.Daemon[-<key>]` decides; `state/locks/forgeboxd.lock` only describes
//! the owner (pid, version, pipe) for humans and clients, and is removed on a clean exit.
//! A crashed daemon leaves the file but not the mutex (the OS closes it), so the next
//! daemon simply rewrites the file.

use std::path::PathBuf;

use forge_core::Timestamp;
use forge_core::error::{IoResultExt, Result};
use forge_core::paths::{ForgeRoot, write_atomic};
use forge_core::platform::{InstanceMutex, acquire_instance_mutex, daemon_mutex_name};
use serde::{Deserialize, Serialize};

/// File name under `state/locks/`.
pub const LOCK_FILE: &str = "forgeboxd.lock";

/// Content of `state/locks/forgeboxd.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockInfo {
    pub pid: u32,
    pub version: String,
    pub started_at: Timestamp,
    pub mutex: String,
    pub endpoint: String,
}

/// The mutex plus the description file. Releasing (or dropping) removes the file first.
#[derive(Debug)]
pub struct InstanceLock {
    mutex: Option<InstanceMutex>,
    file: PathBuf,
}

/// Name of the mutex of `root`.
pub fn mutex_name(root: &ForgeRoot) -> String {
    daemon_mutex_name(root)
}

impl InstanceLock {
    /// Takes the mutex (`conflict` when another daemon owns it). Blocking, fast.
    pub fn acquire(root: &ForgeRoot) -> Result<Self> {
        let mutex = acquire_instance_mutex(&mutex_name(root))?;
        Ok(Self {
            mutex: Some(mutex),
            file: root.state_locks().join(LOCK_FILE),
        })
    }

    /// Writes the description file (after `state/locks/` exists). Blocking.
    pub fn describe(&self, endpoint: &str) -> Result<()> {
        (self.describe_task(endpoint.to_owned()))()
    }

    /// [`InstanceLock::describe`] as a closure for `spawn_blocking`.
    pub fn describe_task(&self, endpoint: String) -> impl FnOnce() -> Result<()> + Send + 'static {
        let file = self.file.clone();
        let mutex = self
            .mutex
            .as_ref()
            .map(|m| m.name().to_owned())
            .unwrap_or_default();
        move || {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir).at("creating", dir)?;
            }
            let info = LockInfo {
                pid: std::process::id(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                started_at: Timestamp::now(),
                mutex,
                endpoint,
            };
            let text = serde_json::to_string_pretty(&info)
                .map_err(|e| forge_core::ForgeError::Internal(format!("lock file: {e}")))?;
            write_atomic(&file, format!("{text}\n").as_bytes())
        }
    }

    /// Path of the description file.
    pub fn file(&self) -> &std::path::Path {
        &self.file
    }

    /// Removes the file, then frees the mutex.
    pub fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if self.mutex.is_some() {
            match std::fs::remove_file(&self.file) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(file = %self.file.display(), %e, "could not remove the lock file")
                }
            }
            self.mutex = None;
        }
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        self.release_inner();
    }
}

/// Reads the description file of `root` (clients, tests). Blocking.
pub fn read_lock_info(root: &ForgeRoot) -> Result<Option<LockInfo>> {
    let file = root.state_locks().join(LOCK_FILE);
    match std::fs::read_to_string(&file) {
        Ok(text) => Ok(serde_json::from_str(&text).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(forge_core::ForgeError::io_at("reading", &file, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_on_the_same_root_conflicts_and_file_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let lock = InstanceLock::acquire(&root).unwrap();
        lock.describe(r"\\.\pipe\forgebox-test").unwrap();
        let info = read_lock_info(&root).unwrap().unwrap();
        assert_eq!(info.pid, std::process::id());
        assert!(info.mutex.starts_with(r"Global\Forgebox.Daemon-"));
        assert_eq!(InstanceLock::acquire(&root).unwrap_err().code(), "conflict");
        let file = lock.file().to_path_buf();
        lock.release();
        assert!(!file.exists());
        assert!(read_lock_info(&root).unwrap().is_none());
        let again = InstanceLock::acquire(&root).unwrap();
        drop(again);
    }
}
