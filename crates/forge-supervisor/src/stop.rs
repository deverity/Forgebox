//! Graceful stop cascade (docs/03 §1.3).
//!
//! 1. [`StopStrategy::Command`]: the manifest's `stop.command` (`nginx -s quit`,
//!    `mysqladmin shutdown`…) runs in its own Job Object; a non-zero exit escalates at
//!    once, a zero exit keeps waiting for the service to go away.
//! 2. [`StopStrategy::CtrlBreak`]: `CTRL_BREAK_EVENT` to the service's process group,
//!    through the `forgebox-ctrl` helper (ADR-0016). Skipped when the service was not
//!    started in its own process group.
//! 3. [`StopStrategy::WmClose`]: `WM_CLOSE` to the top-level windows of the instance's
//!    processes; only when they have windows.
//! 4. [`StopStrategy::Terminate`]: `TerminateJobObject`, then wait until the job is
//!    empty ([`ProcessGroup::terminate_all`], at most [`TERMINATE_TIMEOUT`]): the stop is
//!    never reported while the job is still terminating, so a restart can spawn into it
//!    at once (docs/03 §1.1). Always works, never leaves an orphan.
//!
//! Waits: the **first** graceful step that runs waits [`StopPlan::timeout`] (manifest
//! `stop.timeout`, 15 s by default); each later graceful step waits
//! [`StopPlan::followup_grace`] (5 s). A service without `stop.command` that ignores
//! `CTRL_BREAK` is therefore terminated `stop.timeout` after the request (plus the
//! window step if it has windows). Every step that runs publishes
//! `service.stopping{strategy, step, timeout_ms}` before acting.
//!
//! Once the main process exited, the process watcher terminates whatever is still in
//! the instance's job (grandchildren that ignored the signal) and waits for the job to
//! empty; the count is reported in [`StopReport::leftovers_killed`].
//!
//! The cascade runs in its own task: the instance actor keeps answering `Status` while a
//! stuck stop waits (docs/03 §7.6). Every Win32 call goes through `spawn_blocking`.

use std::sync::Arc;
use std::time::Duration;

use forge_core::Result;
use forge_core::platform::{GroupOptions, ProcessControl, ProcessGroup, TERMINATE_TIMEOUT};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::process::{SpawnSpec, Stdio};

/// Exit code given to processes killed by [`StopStrategy::Terminate`].
pub const TERMINATED_EXIT_CODE: u32 = 1;

/// Time allowed to the `forgebox-ctrl` helper to deliver `CTRL_BREAK`.
const CTRL_BREAK_DELIVERY: Duration = Duration::from_secs(5);

/// One step of the cascade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopStrategy {
    Command,
    CtrlBreak,
    WmClose,
    Terminate,
}

impl StopStrategy {
    /// Position in the cascade (1–4, docs/03 §1.3).
    pub fn step(self) -> u8 {
        match self {
            Self::Command => 1,
            Self::CtrlBreak => 2,
            Self::WmClose => 3,
            Self::Terminate => 4,
        }
    }

    /// Wire name (`ctrl_break`...).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::CtrlBreak => "ctrl_break",
            Self::WmClose => "wm_close",
            Self::Terminate => "terminate",
        }
    }
}

/// How to stop an instance (from the manifest's `service.stop`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopPlan {
    /// Step 1, `stop.command` (stdio is discarded; `PATH`/env built like the service's).
    pub command: Option<SpawnSpec>,
    /// Wait of the first graceful step that runs (`stop.timeout`, default 15 s).
    pub timeout: Duration,
    /// Step 2 enabled (requires the service to have its own process group).
    pub ctrl_break: bool,
    /// Step 3 enabled.
    pub wm_close: bool,
    /// Wait of each later graceful step (default 5 s).
    pub followup_grace: Duration,
    /// Wait for the exit to be observed once `TerminateJobObject` emptied the job
    /// (default 5 s; emptying the job itself is bounded by [`TERMINATE_TIMEOUT`]).
    pub terminate_wait: Duration,
}

impl Default for StopPlan {
    fn default() -> Self {
        Self {
            command: None,
            timeout: Duration::from_secs(15),
            ctrl_break: true,
            wm_close: true,
            followup_grace: Duration::from_secs(5),
            terminate_wait: Duration::from_secs(5),
        }
    }
}

impl StopPlan {
    /// Upper bound of a whole cascade (every graceful step waiting its full time, the
    /// job taking [`TERMINATE_TIMEOUT`] to empty).
    pub fn worst_case(&self) -> Duration {
        let graceful = usize::from(self.command.is_some())
            + usize::from(self.ctrl_break)
            + usize::from(self.wm_close);
        let mut total = TERMINATE_TIMEOUT + self.terminate_wait + CTRL_BREAK_DELIVERY;
        if graceful > 0 {
            total += self.timeout
                + self.followup_grace * u32::try_from(graceful - 1).unwrap_or(u32::MAX);
        }
        total
    }
}

/// Outcome of a stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopReport {
    /// The step after which the main process exited; `None` when it was not running.
    pub strategy: Option<StopStrategy>,
    /// Steps that ran, in order.
    pub attempted: Vec<StopStrategy>,
    /// Exit code of the main process; `None` if it was not running or never exited.
    pub exit_code: Option<u32>,
    pub elapsed_ms: u64,
    /// Processes still in the job after the main process exited, then terminated.
    pub leftovers_killed: u32,
}

impl StopReport {
    /// Report for a stop request on an instance that was not running.
    pub fn not_running() -> Self {
        Self {
            strategy: None,
            attempted: Vec::new(),
            exit_code: None,
            elapsed_ms: 0,
            leftovers_killed: 0,
        }
    }

    /// Whether the main process is known to have exited.
    pub fn exited(&self) -> bool {
        self.exit_code.is_some()
    }
}

/// What the process watcher reports when the main process exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExitInfo {
    pub code: u32,
    /// Processes left in the instance's job after the main process exited, terminated
    /// by the watcher.
    pub leftovers_killed: u32,
}

/// Called before each step acts: `(strategy, how long it will wait)`.
pub(crate) type StepObserver = Box<dyn Fn(StopStrategy, Duration) + Send + Sync>;

/// What the cascade needs; owned by its task.
pub(crate) struct Cascade {
    pub plan: StopPlan,
    /// Main process (and process group leader).
    pub pid: u32,
    pub new_process_group: bool,
    /// Job of the instance.
    pub job: Arc<dyn ProcessGroup>,
    /// Parent for the stop command's job.
    pub root: Arc<dyn ProcessGroup>,
    pub processes: Arc<dyn ProcessControl>,
    /// Set by the process watcher when the main process exited (after it cleaned up
    /// the leftovers of the job).
    pub exited: watch::Receiver<Option<ExitInfo>>,
    pub on_step: StepObserver,
}

/// Runs a blocking platform call off the async workers.
async fn blocking<T, F>(what: &'static str, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(err) => Err(forge_core::ForgeError::Internal(format!("{what}: {err}"))),
    }
}

/// Waits at most `limit` for the main process; `Some` once it exited.
async fn wait_exit(
    exited: &mut watch::Receiver<Option<ExitInfo>>,
    limit: Duration,
) -> Option<ExitInfo> {
    if let Some(info) = *exited.borrow() {
        return Some(info);
    }
    // Copy the value out: a `watch::Ref` must not live across an await.
    let waited = tokio::time::timeout(limit, async {
        exited.wait_for(Option::is_some).await.map(|info| *info)
    })
    .await;
    match waited {
        Ok(Ok(info)) => info,
        // Sender gone without a value: the watcher died; let the cascade escalate.
        Ok(Err(_)) => {
            tokio::time::sleep(limit).await;
            None
        }
        Err(_) => None,
    }
}

impl Cascade {
    /// Runs the cascade to the end.
    pub(crate) async fn run(mut self) -> StopReport {
        let started = Instant::now();
        let mut attempted = Vec::new();
        let mut first_graceful = true;
        let mut strategy = None;
        // Already gone when the cascade starts (exit raced with the stop request).
        let mut exit: Option<ExitInfo> = *self.exited.borrow();

        // Step 1: stop.command
        if exit.is_none() {
            if let Some(command) = self.plan.command.clone() {
                let wait = self.next_wait(&mut first_graceful);
                (self.on_step)(StopStrategy::Command, wait);
                attempted.push(StopStrategy::Command);
                exit = self.run_command(command, wait).await;
                strategy = Some(StopStrategy::Command);
            }
        }

        // Step 2: CTRL_BREAK
        if exit.is_none() && self.plan.ctrl_break && self.new_process_group {
            let wait = self.next_wait(&mut first_graceful);
            (self.on_step)(StopStrategy::CtrlBreak, wait);
            attempted.push(StopStrategy::CtrlBreak);
            strategy = Some(StopStrategy::CtrlBreak);
            let processes = Arc::clone(&self.processes);
            let pid = self.pid;
            let sent = blocking("sending CTRL_BREAK", move || {
                processes.send_ctrl_break(pid, CTRL_BREAK_DELIVERY)
            })
            .await;
            exit = match sent {
                Ok(()) => wait_exit(&mut self.exited, wait).await,
                Err(err) => {
                    tracing::warn!(pid, %err, "CTRL_BREAK could not be delivered; escalating");
                    *self.exited.borrow()
                }
            };
        }

        // Step 3: WM_CLOSE (only when there are windows)
        if exit.is_none() && self.plan.wm_close {
            let job = Arc::clone(&self.job);
            let processes = Arc::clone(&self.processes);
            let pid = self.pid;
            let posted = blocking("closing windows", move || {
                let mut pids = job.pids()?;
                if !pids.contains(&pid) {
                    pids.push(pid);
                }
                processes.close_windows(&pids)
            })
            .await;
            match posted {
                Ok(0) => {}
                Ok(windows) => {
                    let wait = self.next_wait(&mut first_graceful);
                    (self.on_step)(StopStrategy::WmClose, wait);
                    attempted.push(StopStrategy::WmClose);
                    strategy = Some(StopStrategy::WmClose);
                    tracing::debug!(pid, windows, "WM_CLOSE posted");
                    exit = wait_exit(&mut self.exited, wait).await;
                }
                Err(err) => tracing::warn!(pid, %err, "WM_CLOSE step failed; escalating"),
            }
        }

        // Step 4: TerminateJobObject, returning once the job is empty
        if exit.is_none() {
            (self.on_step)(StopStrategy::Terminate, self.plan.terminate_wait);
            attempted.push(StopStrategy::Terminate);
            strategy = Some(StopStrategy::Terminate);
            let job = Arc::clone(&self.job);
            let terminated = blocking("terminating the job", move || {
                job.terminate_all(TERMINATED_EXIT_CODE)
            })
            .await;
            if let Err(err) = terminated {
                tracing::error!(pid = self.pid, %err, "TerminateJobObject failed");
            }
            exit = wait_exit(&mut self.exited, self.plan.terminate_wait).await;
        }

        StopReport {
            strategy: if exit.is_some() { strategy } else { None },
            attempted,
            exit_code: exit.map(|info| info.code),
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            leftovers_killed: exit.map_or(0, |info| info.leftovers_killed),
        }
    }

    fn next_wait(&self, first: &mut bool) -> Duration {
        if std::mem::take(first) {
            self.plan.timeout
        } else {
            self.plan.followup_grace
        }
    }

    /// Step 1. Returns the service's exit code if it exited within `wait`.
    async fn run_command(&mut self, command: SpawnSpec, wait: Duration) -> Option<ExitInfo> {
        let pid = self.pid;
        let deadline = Instant::now() + wait;
        let command = command.stdio(Stdio::Discard);
        let root = Arc::clone(&self.root);
        let processes = Arc::clone(&self.processes);
        let spawned = blocking("starting the stop command", move || {
            let cmd = command.to_command(processes.as_ref())?;
            let job = root.create_child(GroupOptions::INSTANCE)?;
            let process = job.spawn(&cmd)?;
            Ok((job, process))
        })
        .await;
        let (job, process) = match spawned {
            Ok(spawned) => spawned,
            Err(err) => {
                tracing::warn!(pid, %err, "stop command could not start; escalating");
                return None;
            }
        };
        let mut command_exit = process.exit;

        // Phase 1: until the service exits, the command fails, or the deadline.
        let command_status = {
            let exited = async { self.exited.wait_for(Option::is_some).await.is_ok() };
            tokio::select! {
                biased;
                _ = exited => None,
                () = tokio::time::sleep_until(deadline) => None,
                status = &mut command_exit => Some(status),
            }
        };
        match command_status {
            Some(Ok(0)) | Some(Err(_)) | None => {
                // Phase 2: the command did its part (or we cannot tell): wait for the
                // service until the deadline.
                if self.exited.borrow().is_none() && Instant::now() < deadline {
                    let exited = async { self.exited.wait_for(Option::is_some).await.is_ok() };
                    let _ = tokio::time::timeout_at(deadline, exited).await;
                }
            }
            Some(Ok(code)) => {
                tracing::info!(pid, code, "stop command failed; escalating");
            }
        }
        // The command is no longer needed (finished, hung or irrelevant).
        let _ = blocking("cleaning up the stop command", move || {
            job.terminate_all(TERMINATED_EXIT_CODE)
        })
        .await;
        *self.exited.borrow()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategies_have_steps_and_wire_names() {
        let all = [
            StopStrategy::Command,
            StopStrategy::CtrlBreak,
            StopStrategy::WmClose,
            StopStrategy::Terminate,
        ];
        let steps: Vec<u8> = all.iter().map(|s| s.step()).collect();
        assert_eq!(steps, vec![1, 2, 3, 4]);
        for strategy in all {
            let json = serde_json::to_value(strategy).unwrap();
            assert_eq!(json, serde_json::json!(strategy.as_str()));
        }
    }

    #[test]
    fn worst_case_counts_first_timeout_and_followups() {
        let plan = StopPlan::default();
        // 15 (ctrl_break) + 5 (wm_close) + 5 (job emptying) + 5 (terminate wait)
        // + 5 (helper delivery)
        assert_eq!(TERMINATE_TIMEOUT, Duration::from_secs(5));
        assert_eq!(plan.worst_case(), Duration::from_secs(35));
        let plan = StopPlan {
            command: Some(SpawnSpec::new("C:/x.exe", "C:/")),
            timeout: Duration::from_secs(20),
            ..StopPlan::default()
        };
        assert_eq!(
            plan.worst_case(),
            Duration::from_secs(20 + 5 + 5 + 5 + 5 + 5)
        );
        let plan = StopPlan {
            ctrl_break: false,
            wm_close: false,
            ..StopPlan::default()
        };
        assert_eq!(plan.worst_case(), Duration::from_secs(15));
    }

    #[test]
    fn not_running_report_is_empty() {
        let report = StopReport::not_running();
        assert!(!report.exited());
        assert_eq!(report.strategy, None);
        assert!(report.attempted.is_empty());
    }
}
