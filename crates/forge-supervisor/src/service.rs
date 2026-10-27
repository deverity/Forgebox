//! The instance actor (docs/03 §1.4 and §7.2): one actor per service instance, owning its
//! Job Object, its process and its output.
//!
//! The actor never waits for a process. Spawning (`CreateProcess` in `spawn_blocking`),
//! watching the exit and the stop cascade run in tasks of their own and report back
//! with internal messages, so `Status` is answered in microseconds even while a stop is
//! stuck for 15 s (docs/03 §7.6). Requests that take time (`Start`, `Stop`, `Restart`)
//! keep their reply channel and are answered when the operation completes; callers pick
//! their `ask` timeout ([`InstanceHandle`] has helpers).
//!
//! States (basic machine of M1-E3-T1; health probes and restart policies are M1-E3-T2):
//!
//! ```text
//! Stopped ─start─▶ Starting ─spawned─▶ Running ─stop─▶ Stopping ─exited─▶ Stopped
//!    ▲                │ spawn failed        │ unexpected exit
//!    │                ▼                     ▼
//!    └──start── Failed{code, reason}    Crashed{exit_code} ──start──▶ Starting
//! ```
//!
//! Tasks spawned by the actor hold only a weak address and a weak reference to the job:
//! dropping every [`InstanceHandle`] stops the actor, its `on_stop` terminates the job,
//! and the job's `KILL_ON_JOB_CLOSE` covers everything else. After a panic the actor is
//! restarted by the [`ActorSupervisor`]; the new incarnation terminates whatever ran in
//! the job (its state was lost) and reports `Failed`.
//!
//! A run is reported finished (`service.crashed`, `service.stopped`) only once the job is
//! empty: job termination is asynchronous in the OS and a job still terminating refuses
//! new processes (docs/03 §1.1). Every termination of the job waits for it
//! ([`ProcessGroup::terminate_all`]); the ones started outside a run (a process of a run
//! this incarnation does not track) are awaited by the next run before it spawns.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use forge_core::actor::{
    Actor, ActorHandle, ActorRef, ActorSpec, ActorSupervisor, Context, Reply, RestartInfo,
    StopOutcome, TellError, WeakActorRef,
};
use forge_core::event::EventBus;
use forge_core::ids::{InstanceId, ToolId};
use forge_core::paths::ForgeRoot;
use forge_core::platform::{ProcessControl, ProcessGroup, SpawnedProcess, TERMINATE_TIMEOUT};
use forge_core::{ForgeError, Result, Timestamp};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::events::{ServiceEventPayload, ServiceRef};
use crate::job::JobTree;
use crate::logs::{LogHub, LogOptions, LogSubscription};
use crate::process::SpawnSpec;
use crate::stop::{
    Cascade, ExitInfo, StepObserver, StopPlan, StopReport, StopStrategy, TERMINATED_EXIT_CODE,
};

/// Timeout used by [`InstanceHandle`] for quick requests (`Status`, `SubscribeLogs`).
pub const QUICK_ASK_TIMEOUT: Duration = Duration::from_secs(1);

/// Everything needed to run one instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceSpec {
    pub id: InstanceId,
    pub tool: ToolId,
    /// Instance name (`default`, `legacy`...).
    pub name: String,
    pub spawn: SpawnSpec,
    pub stop: StopPlan,
    pub logs: LogOptions,
}

impl InstanceSpec {
    /// Spec with the default stop plan and live-only logs (no file).
    pub fn new(id: InstanceId, tool: ToolId, name: impl Into<String>, spawn: SpawnSpec) -> Self {
        Self {
            id,
            tool,
            name: name.into(),
            spawn,
            stop: StopPlan::default(),
            logs: LogOptions::default(),
        }
    }

    /// Writes the output to `logs/<tool>/<name>.out.log` under `root`.
    pub fn log_to(mut self, root: &ForgeRoot) -> Result<Self> {
        self.logs.file = Some(instance_log_path(root, &self.tool, &self.name)?);
        Ok(self)
    }
}

/// `logs/<tool>/<instance>.out.log` (docs/03 §1.2). `instance` must be a plain name
/// (`[A-Za-z0-9._-]`, not starting with `.`).
pub fn instance_log_path(root: &ForgeRoot, tool: &ToolId, instance: &str) -> Result<PathBuf> {
    let valid = !instance.is_empty()
        && !instance.starts_with('.')
        && instance
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !valid {
        return Err(ForgeError::InvalidInput(format!(
            "invalid instance name {instance:?}"
        )));
    }
    Ok(root
        .logs()
        .join(tool.as_str())
        .join(format!("{instance}.out.log")))
}

/// Lifecycle state of an instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum InstanceState {
    Stopped,
    /// Creating the process.
    Starting,
    /// Process created and resumed (health checks arrive in M1-E3-T2).
    Running,
    /// Stop cascade in progress; `strategy` is the step being tried.
    Stopping {
        strategy: Option<StopStrategy>,
    },
    /// Exited without being asked to.
    Crashed {
        exit_code: u32,
    },
    /// Could not start (or stop); `code` is a [`ForgeError::code`].
    Failed {
        code: String,
        reason: String,
    },
}

impl InstanceState {
    /// Whether a process may be alive (`Starting`, `Running`, `Stopping`).
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping { .. })
    }
}

/// Snapshot answered by `Status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceStatus {
    pub instance_id: InstanceId,
    pub tool: ToolId,
    pub name: String,
    #[serde(flatten)]
    pub state: InstanceState,
    pub pid: Option<u32>,
    /// When the current state was entered.
    pub since: Timestamp,
    /// Starts so far (the current run number).
    pub runs: u64,
    pub log_file: Option<PathBuf>,
}

/// Mailbox of an [`InstanceActor`].
pub enum InstanceMsg {
    /// Start; answered with the pid once the process runs (immediately if it already
    /// does).
    Start(Reply<Result<u32>>),
    /// Stop with the cascade; answered when the instance is down.
    Stop(Reply<Result<StopReport>>),
    /// Stop (if running) then start; answered with the new pid.
    Restart(Reply<Result<u32>>),
    /// Snapshot; always answered at once.
    Status(Reply<InstanceStatus>),
    /// Live output (survives restarts of the process).
    SubscribeLogs(Reply<LogSubscription>),
    /// Reports of the actor's own tasks (cannot be built outside this crate).
    #[doc(hidden)]
    Internal(Internal),
}

/// Opaque internal message.
#[doc(hidden)]
pub struct Internal(Notice);

enum Notice {
    Spawned {
        run: u64,
        result: Result<(u32, watch::Receiver<Option<ExitInfo>>)>,
    },
    Exited {
        run: u64,
        info: ExitInfo,
    },
    StopFinished {
        run: u64,
        report: StopReport,
    },
}

/// State shared by every incarnation of an instance actor.
struct Shared {
    spec: InstanceSpec,
    service: ServiceRef,
    job: Arc<dyn ProcessGroup>,
    root: Arc<dyn ProcessGroup>,
    processes: Arc<dyn ProcessControl>,
    logs: LogHub,
    runs: AtomicU64,
}

/// Actor of one instance. Built by [`Supervisor::spawn_instance`].
pub struct InstanceActor {
    shared: Arc<Shared>,
    state: InstanceState,
    since: Timestamp,
    run: u64,
    pid: Option<u32>,
    exited: Option<watch::Receiver<Option<ExitInfo>>>,
    stop_progress: Option<watch::Receiver<Option<StopStrategy>>>,
    /// Waiting for the run being started.
    pending_start: Vec<Reply<Result<u32>>>,
    /// Waiting for the run that follows the current stop.
    pending_restart: Vec<Reply<Result<u32>>>,
    pending_stop: Vec<Reply<Result<StopReport>>>,
    stop_after_start: bool,
    start_after_stop: bool,
    /// Terminations of the job started outside a run; the next run waits for them
    /// before spawning into the job.
    job_cleanups: Vec<JoinHandle<()>>,
}

/// Publishes `payload` under its kind (`service.<state>`).
fn publish(events: &EventBus, payload: &ServiceEventPayload) {
    let kind = payload.kind();
    match serde_json::to_value(payload) {
        Ok(value) => {
            events.publish(kind, value);
        }
        Err(err) => tracing::error!(%err, %kind, "could not serialize a service event"),
    }
}

/// Same error for every waiter (`ForgeError` is not `Clone`).
fn duplicate(err: &ForgeError) -> ForgeError {
    match err {
        ForgeError::NotFound(m) => ForgeError::NotFound(m.clone()),
        ForgeError::Conflict(m) => ForgeError::Conflict(m.clone()),
        ForgeError::PortInUse { port, owner } => ForgeError::PortInUse {
            port: *port,
            owner: owner.clone(),
        },
        ForgeError::NeedsElevation(m) => ForgeError::NeedsElevation(m.clone()),
        ForgeError::VerificationFailed(m) => ForgeError::VerificationFailed(m.clone()),
        ForgeError::Io { context, source } => ForgeError::io(
            context.clone(),
            std::io::Error::new(source.kind(), source.to_string()),
        ),
        ForgeError::ConfigInvalid(m) => ForgeError::ConfigInvalid(m.clone()),
        ForgeError::InvalidInput(m) => ForgeError::InvalidInput(m.clone()),
        ForgeError::UnsupportedPlatform(m) => ForgeError::UnsupportedPlatform(m.clone()),
        ForgeError::Timeout { operation, after } => ForgeError::Timeout {
            operation: operation.clone(),
            after: *after,
        },
        ForgeError::NotImplemented(m) => ForgeError::NotImplemented(m.clone()),
        ForgeError::Cancelled(m) => ForgeError::Cancelled(m.clone()),
        ForgeError::Internal(m) => ForgeError::Internal(m.clone()),
        other => ForgeError::Internal(other.to_string()),
    }
}

/// Answers every waiter with the same error.
fn fail_all<T>(replies: Vec<Reply<Result<T>>>, err: &ForgeError) {
    for reply in replies {
        let _ = reply.send(Err(duplicate(err)));
    }
}

/// Hands a report to the actor; retries while its mailbox is full, gives up once it is
/// gone.
async fn deliver(me: &WeakActorRef<InstanceMsg>, notice: Notice) {
    let mut msg = InstanceMsg::Internal(Internal(notice));
    loop {
        let Some(actor) = me.upgrade() else {
            return;
        };
        match actor.tell(msg) {
            Ok(()) => return,
            Err(TellError::MailboxFull { msg: back, .. }) => {
                msg = back;
                drop(actor);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(TellError::Stopped { .. }) => return,
        }
    }
}

/// How long the job may take to empty by itself after the main process exited.
///
/// The console host of the service's hidden console (`conhost.exe`) is a member of the
/// job and exits on its own a few milliseconds after its last client; it only counts as
/// a leftover when other processes keep the console alive.
const LEFTOVER_GRACE: Duration = Duration::from_millis(300);

/// Longest wait of a new run for a termination of the job started outside a run.
const CLEANUP_WAIT: Duration = TERMINATE_TIMEOUT.saturating_add(Duration::from_secs(1));

/// Terminates what is left in `job` after the main process exited and waits until the
/// job is empty; returns how many processes were still there (descendants, and their
/// console host).
async fn kill_leftovers(job: Arc<dyn ProcessGroup>) -> u32 {
    let deadline = tokio::time::Instant::now() + LEFTOVER_GRACE;
    loop {
        let expired = tokio::time::Instant::now() >= deadline;
        let job = Arc::clone(&job);
        let result = tokio::task::spawn_blocking(move || -> Result<(usize, Result<()>)> {
            let pids = job.pids()?;
            let killed = if expired && !pids.is_empty() {
                job.terminate_all(TERMINATED_EXIT_CODE)
            } else {
                Ok(())
            };
            Ok((pids.len(), killed))
        })
        .await;
        match result {
            Ok(Ok((0, _))) => return 0,
            Ok(Ok((count, killed))) if expired => {
                if let Err(err) = killed {
                    tracing::warn!(%err, "leftovers of an instance job did not go away");
                }
                return u32::try_from(count).unwrap_or(u32::MAX);
            }
            Ok(Ok(_)) => tokio::time::sleep(Duration::from_millis(25)).await,
            Ok(Err(err)) => {
                tracing::warn!(%err, "could not clean up an instance job");
                return 0;
            }
            Err(err) => {
                tracing::warn!(%err, "instance job cleanup task failed");
                return 0;
            }
        }
    }
}

/// Terminates everything in `job` and waits until it is empty.
async fn terminate_job(job: Arc<dyn ProcessGroup>) {
    match tokio::task::spawn_blocking(move || job.terminate_all(TERMINATED_EXIT_CODE)).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::warn!(%err, "could not empty an instance job"),
        Err(err) => tracing::warn!(%err, "instance job termination task failed"),
    }
}

/// One run: wait for pending terminations of the job, create the process, start the log
/// pumps, report, wait for the exit, empty the job, report again.
async fn run_process(
    shared: Arc<Shared>,
    me: WeakActorRef<InstanceMsg>,
    run: u64,
    cleanups: Vec<JoinHandle<()>>,
) {
    for cleanup in cleanups {
        if tokio::time::timeout(CLEANUP_WAIT, cleanup).await.is_err() {
            tracing::warn!(run, "a termination of the instance job is still running");
        }
    }
    let job = Arc::clone(&shared.job);
    let spawned = match shared.spec.spawn.to_command(shared.processes.as_ref()) {
        Err(err) => Err(err),
        Ok(command) => {
            let job = Arc::clone(&job);
            match tokio::task::spawn_blocking(move || job.spawn(&command)).await {
                Ok(result) => result,
                Err(err) => Err(ForgeError::Internal(format!("spawn task failed: {err}"))),
            }
        }
    };
    let SpawnedProcess {
        pid,
        stdout,
        stderr,
        exit,
    } = match spawned {
        Ok(process) => process,
        Err(err) => {
            deliver(
                &me,
                Notice::Spawned {
                    run,
                    result: Err(err),
                },
            )
            .await;
            return;
        }
    };
    // Never keep the job alive while waiting: if the actor goes away, closing the job
    // must kill the process (KILL_ON_JOB_CLOSE).
    let weak_job: Weak<dyn ProcessGroup> = Arc::downgrade(&job);
    drop(job);
    let _pipeline = shared.logs.attach(stdout, stderr);
    drop(shared);

    let (exit_tx, exit_rx) = watch::channel(None);
    deliver(
        &me,
        Notice::Spawned {
            run,
            result: Ok((pid, exit_rx)),
        },
    )
    .await;
    let code = match exit.await {
        Ok(code) => code,
        Err(err) => {
            tracing::warn!(pid, %err, "exit code of a supervised process is unknown");
            u32::MAX
        }
    };
    let leftovers_killed = match weak_job.upgrade() {
        Some(job) => kill_leftovers(job).await,
        None => 0,
    };
    let info = ExitInfo {
        code,
        leftovers_killed,
    };
    exit_tx.send_replace(Some(info));
    deliver(&me, Notice::Exited { run, info }).await;
}

impl InstanceActor {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            state: InstanceState::Stopped,
            since: Timestamp::now(),
            run: 0,
            pid: None,
            exited: None,
            stop_progress: None,
            pending_start: Vec::new(),
            pending_restart: Vec::new(),
            pending_stop: Vec::new(),
            stop_after_start: false,
            start_after_stop: false,
            job_cleanups: Vec::new(),
        }
    }

    fn set_state(&mut self, state: InstanceState) {
        self.state = state;
        self.since = Timestamp::now();
    }

    fn status(&self) -> InstanceStatus {
        let state = match &self.state {
            InstanceState::Stopping { .. } => InstanceState::Stopping {
                strategy: self.stop_progress.as_ref().and_then(|p| *p.borrow()),
            },
            other => other.clone(),
        };
        InstanceStatus {
            instance_id: self.shared.spec.id,
            tool: self.shared.spec.tool.clone(),
            name: self.shared.spec.name.clone(),
            state,
            pid: self.pid,
            since: self.since,
            runs: self.run,
            log_file: self.shared.spec.logs.file.clone(),
        }
    }

    fn begin_start(&mut self, ctx: &mut Context<Self>) {
        let run = self.shared.runs.fetch_add(1, Ordering::SeqCst) + 1;
        self.run = run;
        self.pid = None;
        self.exited = None;
        self.stop_progress = None;
        self.set_state(InstanceState::Starting);
        publish(
            ctx.events(),
            &ServiceEventPayload::starting(&self.shared.service, run),
        );
        tokio::spawn(run_process(
            Arc::clone(&self.shared),
            ctx.myself_weak(),
            run,
            std::mem::take(&mut self.job_cleanups),
        ));
    }

    fn begin_stop(&mut self, ctx: &mut Context<Self>) {
        let (Some(pid), Some(exited)) = (self.pid, self.exited.clone()) else {
            // Not running: nothing to stop.
            self.finish_stop(StopReport::not_running(), ctx);
            return;
        };
        self.set_state(InstanceState::Stopping { strategy: None });
        let (progress_tx, progress_rx) = watch::channel(None);
        self.stop_progress = Some(progress_rx);
        let events = ctx.events().clone();
        let service = self.shared.service.clone();
        let on_step: StepObserver = Box::new(move |strategy: StopStrategy, wait: Duration| {
            progress_tx.send_replace(Some(strategy));
            publish(
                &events,
                &ServiceEventPayload::stopping(&service, pid, strategy, wait),
            );
        });
        let cascade = Cascade {
            plan: self.shared.spec.stop.clone(),
            pid,
            new_process_group: self.shared.spec.spawn.new_process_group,
            job: Arc::clone(&self.shared.job),
            root: Arc::clone(&self.shared.root),
            processes: Arc::clone(&self.shared.processes),
            exited,
            on_step,
        };
        let me = ctx.myself_weak();
        let run = self.run;
        tokio::spawn(async move {
            let report = cascade.run().await;
            deliver(&me, Notice::StopFinished { run, report }).await;
        });
    }

    fn finish_stop(&mut self, report: StopReport, ctx: &mut Context<Self>) {
        let pid = self.pid.take();
        self.stop_progress = None;
        let stopped = report.exited() || (report.strategy.is_none() && report.attempted.is_empty());
        if stopped {
            self.set_state(InstanceState::Stopped);
            publish(
                ctx.events(),
                &ServiceEventPayload::stopped(&self.shared.service, pid, &report),
            );
        } else {
            let err = ForgeError::Timeout {
                operation: "the process did not exit after TerminateJobObject".to_owned(),
                after: self.shared.spec.stop.worst_case(),
            };
            // Nothing new goes into a job whose processes may still be dying.
            if std::mem::take(&mut self.start_after_stop) {
                fail_all(std::mem::take(&mut self.pending_restart), &err);
            }
            self.fail(err, pid, ctx);
        }
        for reply in std::mem::take(&mut self.pending_stop) {
            let _ = reply.send(Ok(report.clone()));
        }
        if std::mem::take(&mut self.start_after_stop) {
            self.pending_start.append(&mut self.pending_restart);
            self.begin_start(ctx);
        }
    }

    /// Enters `Failed` and publishes `service.failed` (`pid`: a process left behind).
    fn fail(&mut self, err: ForgeError, pid: Option<u32>, ctx: &mut Context<Self>) {
        publish(
            ctx.events(),
            &ServiceEventPayload::failed(&self.shared.service, pid, &err),
        );
        self.set_state(InstanceState::Failed {
            code: err.code().to_owned(),
            reason: err.to_string(),
        });
    }

    /// A pending start/restart is superseded by a stop request.
    fn cancel_restart(&mut self) {
        if std::mem::take(&mut self.start_after_stop) {
            let err = ForgeError::Cancelled("a stop was requested before the restart".into());
            fail_all(std::mem::take(&mut self.pending_restart), &err);
        }
    }

    fn on_start(&mut self, reply: Reply<Result<u32>>, ctx: &mut Context<Self>) {
        match self.state {
            InstanceState::Running => {
                let _ =
                    reply.send(self.pid.ok_or_else(|| {
                        ForgeError::Internal("running instance without a pid".into())
                    }));
            }
            InstanceState::Starting if self.stop_after_start => {
                self.start_after_stop = true;
                self.pending_restart.push(reply);
            }
            InstanceState::Starting => self.pending_start.push(reply),
            InstanceState::Stopping { .. } => {
                self.start_after_stop = true;
                self.pending_restart.push(reply);
            }
            InstanceState::Stopped
            | InstanceState::Crashed { .. }
            | InstanceState::Failed { .. } => {
                self.pending_start.push(reply);
                self.begin_start(ctx);
            }
        }
    }

    fn on_stop(&mut self, reply: Reply<Result<StopReport>>, ctx: &mut Context<Self>) {
        match self.state {
            InstanceState::Running => {
                self.pending_stop.push(reply);
                self.begin_stop(ctx);
            }
            InstanceState::Starting => {
                self.cancel_restart();
                self.pending_stop.push(reply);
                self.stop_after_start = true;
            }
            InstanceState::Stopping { .. } => {
                self.cancel_restart();
                self.pending_stop.push(reply);
            }
            InstanceState::Stopped
            | InstanceState::Crashed { .. }
            | InstanceState::Failed { .. } => {
                let _ = reply.send(Ok(StopReport::not_running()));
            }
        }
    }

    fn on_restart(&mut self, reply: Reply<Result<u32>>, ctx: &mut Context<Self>) {
        match self.state {
            InstanceState::Running => {
                self.pending_restart.push(reply);
                self.start_after_stop = true;
                self.begin_stop(ctx);
            }
            InstanceState::Starting => {
                self.pending_restart.push(reply);
                self.stop_after_start = true;
                self.start_after_stop = true;
            }
            InstanceState::Stopping { .. } => {
                self.pending_restart.push(reply);
                self.start_after_stop = true;
            }
            InstanceState::Stopped
            | InstanceState::Crashed { .. }
            | InstanceState::Failed { .. } => {
                self.pending_start.push(reply);
                self.begin_start(ctx);
            }
        }
    }

    fn on_notice(&mut self, notice: Notice, ctx: &mut Context<Self>) {
        match notice {
            Notice::Spawned { run, result } if run == self.run => match result {
                Ok((pid, exited)) => {
                    self.pid = Some(pid);
                    self.exited = Some(exited);
                    self.set_state(InstanceState::Running);
                    publish(
                        ctx.events(),
                        &ServiceEventPayload::running(&self.shared.service, run, pid),
                    );
                    for reply in std::mem::take(&mut self.pending_start) {
                        let _ = reply.send(Ok(pid));
                    }
                    if std::mem::take(&mut self.stop_after_start) {
                        self.begin_stop(ctx);
                    }
                }
                Err(err) => {
                    tracing::warn!(instance = %self.shared.service.instance, tool = %self.shared.service.tool, %err, "instance failed to start");
                    fail_all(std::mem::take(&mut self.pending_start), &err);
                    fail_all(std::mem::take(&mut self.pending_restart), &err);
                    for reply in std::mem::take(&mut self.pending_stop) {
                        let _ = reply.send(Ok(StopReport::not_running()));
                    }
                    self.stop_after_start = false;
                    self.start_after_stop = false;
                    self.fail(err, None, ctx);
                }
            },
            Notice::Exited { run, info } if run == self.run => {
                if self.state == InstanceState::Running {
                    let pid = self.pid.take();
                    tracing::warn!(instance = %self.shared.service.instance, tool = %self.shared.service.tool, code = info.code, "instance exited unexpectedly");
                    self.set_state(InstanceState::Crashed {
                        exit_code: info.code,
                    });
                    publish(
                        ctx.events(),
                        &ServiceEventPayload::crashed(
                            &self.shared.service,
                            pid,
                            info.code,
                            info.leftovers_killed,
                        ),
                    );
                }
                // Stopping: the cascade observes the same exit and reports.
            }
            Notice::StopFinished { run, report } if run == self.run => {
                self.finish_stop(report, ctx);
            }
            Notice::Spawned {
                result: Ok((pid, _)),
                ..
            } if !self.state.is_active() => {
                // A process started for a run this incarnation does not track (the actor
                // was restarted meanwhile): nobody could ever stop it, so it goes now. The
                // next run waits for this termination before spawning.
                tracing::warn!(pid, "terminating a process of a previous run");
                self.job_cleanups.retain(|cleanup| !cleanup.is_finished());
                self.job_cleanups
                    .push(tokio::spawn(terminate_job(Arc::clone(&self.shared.job))));
            }
            _ => tracing::debug!("ignoring a report of a previous run"),
        }
    }

    /// Terminates the job and waits until it is empty.
    async fn terminate_job(&self) {
        terminate_job(Arc::clone(&self.shared.job)).await;
    }
}

impl Actor for InstanceActor {
    type Msg = InstanceMsg;

    async fn handle(&mut self, msg: InstanceMsg, ctx: &mut Context<Self>) {
        match msg {
            InstanceMsg::Start(reply) => self.on_start(reply, ctx),
            InstanceMsg::Stop(reply) => self.on_stop(reply, ctx),
            InstanceMsg::Restart(reply) => self.on_restart(reply, ctx),
            InstanceMsg::Status(reply) => {
                let _ = reply.send(self.status());
            }
            InstanceMsg::SubscribeLogs(reply) => {
                let _ = reply.send(self.shared.logs.subscribe());
            }
            InstanceMsg::Internal(Internal(notice)) => self.on_notice(notice, ctx),
        }
    }

    /// Actor shutdown: the processes go with it (graceful stops are requested before, by
    /// the daemon, with `Stop`).
    async fn on_stop(&mut self, _ctx: &mut Context<Self>) {
        self.terminate_job().await;
    }

    /// The previous incarnation panicked and its view of the process is lost: kill what
    /// runs in the job and report `Failed`.
    async fn on_restart(&mut self, ctx: &mut Context<Self>, info: &RestartInfo) {
        self.terminate_job().await;
        self.fail(
            ForgeError::Internal(format!(
                "instance supervisor restarted after an internal error: {}",
                info.reason
            )),
            None,
            ctx,
        );
    }
}

/// Address of a running instance actor plus typed helpers. Cheap to clone; the actor
/// (and its processes) stop when the last clone is dropped.
#[derive(Clone)]
pub struct InstanceHandle {
    handle: Arc<ActorHandle<InstanceMsg>>,
    id: InstanceId,
    tool: ToolId,
    name: String,
}

impl std::fmt::Debug for InstanceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstanceHandle")
            .field("tool", &self.tool)
            .field("name", &self.name)
            .field("id", &self.id)
            .finish()
    }
}

impl InstanceHandle {
    pub fn id(&self) -> InstanceId {
        self.id
    }

    pub fn tool(&self) -> &ToolId {
        &self.tool
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Mailbox address (for `ask` with custom timeouts).
    pub fn actor(&self) -> &ActorRef<InstanceMsg> {
        self.handle.actor_ref()
    }

    /// Starts and waits at most `timeout` for the pid.
    pub async fn start(&self, timeout: Duration) -> Result<u32> {
        self.actor().ask(InstanceMsg::Start, timeout).await?
    }

    /// Stops with the cascade and waits at most `timeout` for the report.
    pub async fn stop(&self, timeout: Duration) -> Result<StopReport> {
        self.actor().ask(InstanceMsg::Stop, timeout).await?
    }

    /// Restarts and waits at most `timeout` for the new pid.
    pub async fn restart(&self, timeout: Duration) -> Result<u32> {
        self.actor().ask(InstanceMsg::Restart, timeout).await?
    }

    /// Snapshot within `timeout`.
    pub async fn status(&self, timeout: Duration) -> Result<InstanceStatus> {
        Ok(self.actor().ask(InstanceMsg::Status, timeout).await?)
    }

    /// Live output within `timeout`.
    pub async fn subscribe_logs(&self, timeout: Duration) -> Result<LogSubscription> {
        Ok(self
            .actor()
            .ask(InstanceMsg::SubscribeLogs, timeout)
            .await?)
    }

    /// Polls the status until `pred` holds, at most `timeout`.
    pub async fn wait_for_state(
        &self,
        mut pred: impl FnMut(&InstanceState) -> bool,
        timeout: Duration,
    ) -> Result<InstanceStatus> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let status = self.status(QUICK_ASK_TIMEOUT).await?;
            if pred(&status.state) {
                return Ok(status);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(ForgeError::Timeout {
                    operation: format!(
                        "waiting for instance {}/{} (last state {:?})",
                        self.tool, self.name, status.state
                    ),
                    after: timeout,
                });
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Stops the actor itself (its `on_stop` terminates the job).
    pub async fn shutdown(&self) -> StopOutcome {
        self.handle.stop().await
    }
}

/// Entry point: the root job and the actors of the instances. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Supervisor {
    jobs: JobTree,
    actors: ActorSupervisor,
}

impl Supervisor {
    pub fn new(jobs: JobTree, actors: ActorSupervisor) -> Self {
        Self { jobs, actors }
    }

    pub fn jobs(&self) -> &JobTree {
        &self.jobs
    }

    pub fn actors(&self) -> &ActorSupervisor {
        &self.actors
    }

    /// Creates the instance's job (nested in the root) and starts its actor (state
    /// `Stopped`). Actor name: `instance/<tool>/<name>`.
    pub fn spawn_instance(&self, spec: InstanceSpec) -> Result<InstanceHandle> {
        if let Some(file) = &spec.logs.file {
            if file.file_name().is_none() {
                return Err(ForgeError::InvalidInput(format!(
                    "log file {} has no file name",
                    file.display()
                )));
            }
        }
        let job = self.jobs.instance_job()?;
        let service = ServiceRef {
            instance_id: spec.id,
            tool: spec.tool.clone(),
            instance: spec.name.clone(),
        };
        let id = spec.id;
        let tool = spec.tool.clone();
        let name = spec.name.clone();
        let shared = Arc::new(Shared {
            logs: LogHub::new(spec.logs.clone()),
            spec,
            service,
            job,
            root: Arc::clone(self.jobs.root()),
            processes: Arc::clone(self.jobs.processes()),
            runs: AtomicU64::new(0),
        });
        let actor_spec = ActorSpec::new(format!("instance/{tool}/{name}"));
        let handle = self
            .actors
            .spawn(actor_spec, move || InstanceActor::new(Arc::clone(&shared)));
        Ok(InstanceHandle {
            handle: Arc::new(handle),
            id,
            tool,
            name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_path_follows_engine_doc() {
        let root = ForgeRoot::new("C:/Forgebox").unwrap();
        let tool = ToolId::new("mysql").unwrap();
        let path = instance_log_path(&root, &tool, "default").unwrap();
        assert_eq!(
            forge_core::paths::posix_string(&path),
            "C:/Forgebox/logs/mysql/default.out.log"
        );
        for bad in ["", "..", ".hidden", "a/b", "a\\b", "x y"] {
            assert!(instance_log_path(&root, &tool, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn state_serializes_with_tag() {
        let json = serde_json::to_value(InstanceState::Stopping {
            strategy: Some(StopStrategy::CtrlBreak),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"state": "stopping", "strategy": "ctrl_break"})
        );
        assert!(InstanceState::Starting.is_active());
        assert!(!InstanceState::Crashed { exit_code: 3 }.is_active());
    }

    #[test]
    fn duplicated_errors_keep_code_and_message() {
        let errors = [
            ForgeError::NotFound("exe".into()),
            ForgeError::io("starting x", std::io::Error::other("boom")),
            ForgeError::Timeout {
                operation: "stop".into(),
                after: Duration::from_secs(1),
            },
            ForgeError::PortInUse {
                port: 80,
                owner: None,
            },
        ];
        for err in errors {
            let copy = duplicate(&err);
            assert_eq!(copy.code(), err.code());
            assert_eq!(copy.to_string(), err.to_string());
        }
    }
}
