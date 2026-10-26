//! Job Objects of the daemon (docs/03 §1.1): one root job created at boot with
//! `KILL_ON_JOB_CLOSE | BREAKAWAY_OK`, and one job per instance (and per PHP pool, M1-E3-T4)
//! nested in it.
//!
//! The root job's handle lives as long as the daemon: when the daemon dies, for any
//! reason, the kernel closes it and kills every supervised process and their
//! descendants. A per-instance job lets the supervisor terminate a whole service
//! (`httpd.exe` and its children) with one call and list its processes without walking
//! the process tree. Instance jobs also set `DIE_ON_UNHANDLED_EXCEPTION`, so a crashing
//! service exits with its exception code instead of hanging on a WER dialog.

use std::fmt;
use std::sync::Arc;

use forge_core::Result;
use forge_core::platform::{GroupOptions, ProcessControl, ProcessGroup};

/// Root job plus the factory of nested instance jobs. Cheap to clone.
#[derive(Clone)]
pub struct JobTree {
    processes: Arc<dyn ProcessControl>,
    root: Arc<dyn ProcessGroup>,
}

impl fmt::Debug for JobTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobTree")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl JobTree {
    /// Creates the root job ([`GroupOptions::ROOT`]). Fast syscalls (no I/O); the daemon
    /// calls it once at boot.
    pub fn new(processes: Arc<dyn ProcessControl>) -> Result<Self> {
        let root = processes.create_group(GroupOptions::ROOT)?;
        Ok(Self { processes, root })
    }

    /// Process operations of the platform.
    pub fn processes(&self) -> &Arc<dyn ProcessControl> {
        &self.processes
    }

    /// The root job.
    pub fn root(&self) -> &Arc<dyn ProcessGroup> {
        &self.root
    }

    /// New job nested in the root ([`GroupOptions::INSTANCE`]).
    pub fn instance_job(&self) -> Result<Arc<dyn ProcessGroup>> {
        self.root.create_child(GroupOptions::INSTANCE)
    }

    /// Every supervised process (all instance jobs included). Blocking.
    pub fn pids(&self) -> Result<Vec<u32>> {
        self.root.pids()
    }

    /// Kills every supervised process (last resort of a daemon shutdown). Blocking.
    pub fn terminate_all(&self) -> Result<()> {
        self.root.terminate_all(crate::stop::TERMINATED_EXIT_CODE)
    }
}
