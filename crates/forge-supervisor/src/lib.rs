//! Forgebox process supervisor (docs/03 §1, §7): Job Objects, spawn with a
//! deterministic environment, stdout/stderr capture with rotation and live
//! subscriptions, the graceful stop cascade and one actor per service instance.
//!
//! * [`job::JobTree`]: the daemon's root Job Object (`KILL_ON_JOB_CLOSE`) and the nested
//!   job of each instance: nothing survives the daemon, nothing escapes its instance.
//! * [`process::SpawnSpec`]: what to run; `PATH` is built from the tool's folders and
//!   the system folder only.
//! * [`logs`]: pipe pumps → `logs/<tool>/<instance>.out.log` (10 MiB × 5, backpressure
//!   to the process) and a drop-oldest broadcast for clients.
//! * [`stop`]: `stop.command` → `CTRL_BREAK` (via the `forgebox-ctrl` helper, ADR-0015)
//!   → `WM_CLOSE` → `TerminateJobObject`, each step announced by `service.stopping`.
//! * [`service`]: [`InstanceActor`] (Start/Stop/Restart/Status/SubscribeLogs) behind
//!   [`InstanceHandle`], created by [`Supervisor::spawn_instance`]. Health probes and
//!   restart policies come with M1-E3-T2.
//!
//! No Win32 here: everything OS-specific goes through
//! [`forge_core::platform::ProcessControl`] / [`forge_core::platform::ProcessGroup`].
//! Callers in the daemon stop instances with `Stop` before shutting the actors down.

pub mod events;
pub mod job;
pub mod logs;
pub mod process;
pub mod service;
pub mod stop;

pub use job::JobTree;
pub use logs::{LogItem, LogLine, LogOptions, LogStream, LogSubscription, Rotation};
pub use process::{EnvMap, SpawnSpec, Stdio};
pub use service::{
    InstanceActor, InstanceHandle, InstanceMsg, InstanceSpec, InstanceState, InstanceStatus,
    QUICK_ASK_TIMEOUT, Supervisor, instance_log_path,
};
pub use stop::{StopPlan, StopReport, StopStrategy, TERMINATED_EXIT_CODE};

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-supervisor");
    }
}
