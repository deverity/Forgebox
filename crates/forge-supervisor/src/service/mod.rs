//! The instance actor (docs/03 §1.4 and §7.2): one actor per service instance, owning its
//! Job Object, its process, its output, its health checks, its restart policy and its
//! metrics.
//!
//! The actor never waits for a process. Spawning (`CreateProcess` in `spawn_blocking`),
//! watching the exit, the stop cascade, the health probes, the start deadline, the
//! restart backoff and the metrics sampler run in tasks of their own and report back
//! with internal messages (the sampler writes to a `watch` channel instead), so
//! `Status` is answered in microseconds even while a stop is stuck for 15 s
//! (docs/03 §7.6). Requests that take time (`Start`, `Stop`, `Restart`, maintenance)
//! keep their reply channel and are answered when the operation completes; callers pick
//! their `ask` timeout ([`InstanceHandle`] has helpers).
//!
//! Every state change goes through the pure machine of [`state`] ([`next_state`]): a
//! trigger that is not allowed is an error, never a panic, and every accepted transition
//! publishes its `service.*` event ([`crate::events`]). Summary:
//!
//! * `Start` → `Starting`; the pre-checks run first ([`crate::precheck`]: runtime deps,
//!   dependencies, ports, configuration; a failure ends in `Failed(reason)` before any
//!   process exists); then the process is created; without a [`HealthSpec`] it is
//!   `Running` at once, with one it is `Running` after `success_threshold` passing
//!   probes. `Start` is answered with the pid when the instance is `Running`.
//! * `Restart` of a running instance validates first (runtime deps, dependencies,
//!   configuration): when that fails the restart is refused and the instance keeps
//!   running (a broken configuration never takes down a working service).
//! * Past `start_timeout` the instance is stopped with the cascade and ends
//!   `Failed(HealthTimeout)`; a process that exits while `Starting` ends
//!   `Failed(ExitedDuringStart | MissingDll | Permission)`.
//! * An unexpected exit while `Running` is `Crashed(code)`; `failure_threshold` failing
//!   probes stop it (cascade) and also end in `Crashed`. Then the [`RestartPolicy`]
//!   decides: a restart after a backoff (`service.crashed` carries `restart_in_ms`),
//!   staying `Crashed`, or `Failed(CrashLoop)`. Exit codes that no restart fixes
//!   (missing DLL, access denied) go straight to `Failed`.
//! * `EnterMaintenance` stops a running instance first, then holds it in `Maintenance`
//!   (starts refused) until `ExitMaintenance`.
//!
//! Tasks spawned by the actor hold only a weak address and a weak reference to the job:
//! dropping every [`InstanceHandle`] stops the actor, its `on_stop` terminates the job,
//! and the job's `KILL_ON_JOB_CLOSE` covers everything else. Probe, deadline and restart
//! tasks are aborted when the actor drops them. After a panic the actor is restarted by
//! the [`ActorSupervisor`]; the new incarnation terminates whatever ran in the job (its
//! state was lost) and reports `Failed`.
//!
//! A run is reported finished (`service.crashed`, `service.stopped`) only once the job is
//! empty: job termination is asynchronous in the OS and a job still terminating refuses
//! new processes (docs/03 §1.1). Every termination of the job waits for it
//! ([`ProcessGroup::terminate_all`]); the ones started outside a run (a process of a run
//! this incarnation does not track) are awaited by the next run before it spawns.

pub mod state;

use std::collections::BTreeSet;
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
use forge_core::platform::{
    PortTable, ProcessControl, ProcessGroup, ProcessStats, RuntimeDeps, SpawnedProcess,
    TERMINATE_TIMEOUT, port_table, process_stats, runtime_deps,
};
use forge_core::runtime::spawn_blocking_timeout;
use forge_core::{ForgeError, Result, Timestamp};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub use self::state::{
    FailureReason, InstanceState, InvalidTransition, STATUS_ACCESS_DENIED, STATUS_DLL_NOT_FOUND,
    STATUS_ENTRYPOINT_NOT_FOUND, StopCause, Trigger, next_state,
};
use crate::events::{ServiceEventPayload, ServiceRef};
use crate::health::{HealthSpec, HealthStatus, Prober};
use crate::job::JobTree;
use crate::logs::{LogHub, LogOptions, LogSubscription};
use crate::metrics::{InstanceMetrics, MetricsOptions, SamplerOutput, sample_run};
use crate::policy::{Failure, RestartDecision, RestartPolicy, RestartTracker};
use crate::precheck::{
    self, DependencyLookup, NoDependencies, PrecheckEnv, PrecheckScope, PrecheckSpec,
};
use crate::process::SpawnSpec;
use crate::stop::{
    Cascade, ExitInfo, StepObserver, StopPlan, StopReport, StopStrategy, TERMINATED_EXIT_CODE,
};

/// Timeout used by [`InstanceHandle`] for quick requests (`Status`, `SubscribeLogs`,
/// `Metrics`).
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
    /// `None`: `Running` as soon as the process exists.
    pub health: Option<HealthSpec>,
    pub restart: RestartPolicy,
    pub metrics: MetricsOptions,
    /// Checked before each run creates its process (docs/03 §1.5); empty by default.
    pub precheck: PrecheckSpec,
}

impl InstanceSpec {
    /// Spec with the default stop plan, live-only logs (no file), no health check, the
    /// default restart policy (`on-failure`, 3 in 60 s), metrics every 2 s and no
    /// pre-checks.
    pub fn new(id: InstanceId, tool: ToolId, name: impl Into<String>, spawn: SpawnSpec) -> Self {
        Self {
            id,
            tool,
            name: name.into(),
            spawn,
            stop: StopPlan::default(),
            logs: LogOptions::default(),
            health: None,
            restart: RestartPolicy::default(),
            metrics: MetricsOptions::default(),
            precheck: PrecheckSpec::default(),
        }
    }

    /// Writes the output to `logs/<tool>/<name>.out.log` under `root`.
    pub fn log_to(mut self, root: &ForgeRoot) -> Result<Self> {
        self.logs.file = Some(instance_log_path(root, &self.tool, &self.name)?);
        Ok(self)
    }

    /// Sets the health check.
    pub fn with_health(mut self, health: HealthSpec) -> Self {
        self.health = Some(health);
        self
    }

    /// Sets the restart policy.
    pub fn with_restart(mut self, restart: RestartPolicy) -> Self {
        self.restart = restart;
        self
    }

    /// Sets the pre-checks.
    pub fn with_precheck(mut self, precheck: PrecheckSpec) -> Self {
        self.precheck = precheck;
        self
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
    /// Automatic restarts by the restart policy since the instance was created.
    pub restarts: u32,
    /// When the restart policy starts a crashed instance again.
    pub next_restart_at: Option<Timestamp>,
    /// Health of the current run; `None` without a health check.
    pub health: Option<HealthStatus>,
    pub log_file: Option<PathBuf>,
}

/// Mailbox of an [`InstanceActor`].
pub enum InstanceMsg {
    /// Start; answered with the pid once the instance is `Running` (immediately if it
    /// already is).
    Start(Reply<Result<u32>>),
    /// Stop with the cascade; answered when the instance is down.
    Stop(Reply<Result<StopReport>>),
    /// Stop (if running) then start; answered with the new pid once `Running`.
    Restart(Reply<Result<u32>>),
    /// Snapshot; always answered at once.
    Status(Reply<InstanceStatus>),
    /// Live output (survives restarts of the process).
    SubscribeLogs(Reply<LogSubscription>),
    /// Latest metrics sample (`None` when no run is active or none was taken yet).
    Metrics(Reply<Option<InstanceMetrics>>),
    /// Receiver of every new metrics sample (the latest is always readable).
    SubscribeMetrics(Reply<watch::Receiver<Option<InstanceMetrics>>>),
    /// Stop if needed, then hold the instance in `Maintenance`; answered once there.
    EnterMaintenance(Reply<Result<()>>),
    /// Leave `Maintenance` (back to `Stopped`).
    ExitMaintenance(Reply<Result<()>>),
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
    Probe {
        run: u64,
        outcome: std::result::Result<(), String>,
    },
    StartDeadline {
        run: u64,
    },
    RestartDue {
        generation: u64,
    },
    /// The pre-checks of `run` ended (before its process is created).
    Prechecked {
        run: u64,
        result: std::result::Result<(), FailureReason>,
    },
    /// The validation before restarting `run` (still running) ended.
    RestartValidated {
        run: u64,
        result: std::result::Result<(), FailureReason>,
    },
}

/// State shared by every incarnation of an instance actor.
struct Shared {
    spec: InstanceSpec,
    service: ServiceRef,
    job: Arc<dyn ProcessGroup>,
    root: Arc<dyn ProcessGroup>,
    processes: Arc<dyn ProcessControl>,
    stats: Arc<dyn ProcessStats>,
    logs: LogHub,
    metrics: Arc<watch::Sender<Option<InstanceMetrics>>>,
    runs: AtomicU64,
    precheck: PrecheckEnv,
}

/// What follows the stop in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterStop {
    Nothing,
    Start,
    Maintenance,
}

/// Aborts its task when dropped.
struct TaskGuard(JoinHandle<()>);

impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
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
    /// Waiting for the run being started to be `Running`.
    pending_start: Vec<Reply<Result<u32>>>,
    /// Waiting for the run that follows the current stop.
    pending_restart: Vec<Reply<Result<u32>>>,
    pending_stop: Vec<Reply<Result<StopReport>>>,
    pending_maintenance: Vec<Reply<Result<()>>>,
    /// A stop requested while the process was still being created.
    stop_after_spawn: Option<StopCause>,
    after_stop: AfterStop,
    /// Failure that ends the stop in progress (start deadline).
    stop_failure: Option<FailureReason>,
    /// Terminations of the job started outside a run; the next run waits for them
    /// before spawning into the job.
    job_cleanups: Vec<JoinHandle<()>>,
    health: Option<HealthStatus>,
    probes: Option<TaskGuard>,
    start_deadline: Option<TaskGuard>,
    restarts: RestartTracker,
    restart_timer: Option<TaskGuard>,
    /// Identifies the scheduled automatic restart (older timers are ignored).
    restart_generation: u64,
    next_restart_at: Option<Timestamp>,
    /// The pre-checks of the current run (`Starting`, no process yet).
    prechecks: Option<TaskGuard>,
    /// The validation of a restart requested while `Running`.
    validation: Option<TaskGuard>,
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

/// Delivers `notice` after `delay` (start deadline, restart backoff).
fn deliver_after(me: WeakActorRef<InstanceMsg>, delay: Duration, notice: Notice) -> TaskGuard {
    TaskGuard(tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        deliver(&me, notice).await;
    }))
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
/// pumps and the metrics sampler, report, wait for the exit, empty the job, report again.
async fn run_process(
    shared: Arc<Shared>,
    me: WeakActorRef<InstanceMsg>,
    run: u64,
    cleanups: Vec<JoinHandle<()>>,
    events: EventBus,
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
    let metrics = Arc::clone(&shared.metrics);
    let sampler = TaskGuard(tokio::spawn(sample_run(
        weak_job.clone(),
        Arc::clone(&shared.stats),
        pid,
        shared.spec.metrics.interval,
        SamplerOutput {
            latest: Arc::clone(&metrics),
            events: shared
                .spec
                .metrics
                .publish_events
                .then(|| (events, shared.service.clone())),
        },
    )));
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
    drop(sampler);
    metrics.send_replace(None);
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

/// Budget of listing the processes of the instance's job for the port pre-check.
const OWN_PIDS_TIMEOUT: Duration = Duration::from_secs(5);

/// Pre-checks of `run` (scope `Start`) or validation of its restart (scope `Restart`);
/// reports the outcome to the actor. Aborted when the actor drops its guard.
async fn precheck_task(
    shared: Arc<Shared>,
    me: WeakActorRef<InstanceMsg>,
    run: u64,
    scope: PrecheckScope,
) {
    let own_pids: BTreeSet<u32> = match scope {
        PrecheckScope::Restart => BTreeSet::new(),
        PrecheckScope::Start => {
            let job = Arc::clone(&shared.job);
            spawn_blocking_timeout(
                "listing the processes of the instance",
                OWN_PIDS_TIMEOUT,
                move || job.pids(),
            )
            .await
            .map(|pids| pids.into_iter().collect())
            .unwrap_or_default()
        }
    };
    let result = precheck::run(&shared.spec.precheck, &shared.precheck, scope, &own_pids).await;
    drop(shared);
    let notice = match scope {
        PrecheckScope::Start => Notice::Prechecked { run, result },
        PrecheckScope::Restart => Notice::RestartValidated { run, result },
    };
    deliver(&me, notice).await;
}

/// Probes of one run until the task is aborted (the run left `Starting`/`Running`) or
/// the actor is gone.
async fn probe_loop(spec: HealthSpec, prober: Prober, me: WeakActorRef<InstanceMsg>, run: u64) {
    let mut successes = 0_u32;
    let mut healthy_once = false;
    loop {
        let outcome = prober.check().await;
        if outcome.is_ok() {
            successes = successes.saturating_add(1);
            healthy_once |= successes >= spec.success_threshold;
        } else {
            successes = 0;
        }
        if me.upgrade().is_none() {
            return;
        }
        deliver(&me, Notice::Probe { run, outcome }).await;
        tokio::time::sleep(spec.next_wait(healthy_once)).await;
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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
            pending_maintenance: Vec::new(),
            stop_after_spawn: None,
            after_stop: AfterStop::Nothing,
            stop_failure: None,
            job_cleanups: Vec::new(),
            health: None,
            probes: None,
            start_deadline: None,
            restarts: RestartTracker::new(),
            restart_timer: None,
            restart_generation: 0,
            next_restart_at: None,
            prechecks: None,
            validation: None,
        }
    }

    /// Applies `trigger` through the state machine and publishes `payload` (`None` only
    /// for the entry into `Stopping`, announced by the cascade steps).
    fn enter(
        &mut self,
        trigger: Trigger,
        payload: Option<ServiceEventPayload>,
        ctx: &Context<Self>,
    ) -> Result<()> {
        let next = next_state(&self.state, trigger).map_err(|err| {
            tracing::error!(
                instance = %self.shared.service.instance,
                tool = %self.shared.service.tool,
                %err,
                "rejected instance state transition"
            );
            ForgeError::from(err)
        })?;
        self.state = next;
        self.since = Timestamp::now();
        if let Some(payload) = payload {
            publish(ctx.events(), &payload);
        }
        Ok(())
    }

    fn service(&self) -> &ServiceRef {
        &self.shared.service
    }

    fn status(&self) -> InstanceStatus {
        let state = match &self.state {
            InstanceState::Stopping { cause, .. } => InstanceState::Stopping {
                strategy: self.stop_progress.as_ref().and_then(|p| *p.borrow()),
                cause: *cause,
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
            restarts: self.restarts.total(),
            next_restart_at: self.next_restart_at,
            health: self.health.clone(),
            log_file: self.shared.spec.logs.file.clone(),
        }
    }

    fn cancel_restart_timer(&mut self) {
        self.restart_timer = None;
        self.restart_generation += 1;
        self.next_restart_at = None;
    }

    /// Pending start requests are superseded by a stop.
    fn cancel_start_waiters(&mut self, why: &str) {
        let err = ForgeError::Cancelled(why.to_owned());
        fail_all(std::mem::take(&mut self.pending_start), &err);
    }

    /// A restart queued after the current stop is superseded (stop or maintenance).
    fn cancel_queued_start(&mut self, why: &str) {
        if self.after_stop == AfterStop::Start {
            self.after_stop = AfterStop::Nothing;
            let err = ForgeError::Cancelled(why.to_owned());
            fail_all(std::mem::take(&mut self.pending_restart), &err);
        }
    }

    /// A new run (user start, restart or restart policy).
    fn begin_start(&mut self, ctx: &mut Context<Self>) {
        self.cancel_restart_timer();
        let run = self.shared.runs.load(Ordering::SeqCst) + 1;
        let payload = ServiceEventPayload::starting(self.service(), run);
        if let Err(err) = self.enter(Trigger::Start, Some(payload), ctx) {
            fail_all(std::mem::take(&mut self.pending_start), &err);
            return;
        }
        self.shared.runs.store(run, Ordering::SeqCst);
        self.run = run;
        self.pid = None;
        self.exited = None;
        self.stop_progress = None;
        self.stop_failure = None;
        self.stop_after_spawn = None;
        self.probes = None;
        self.validation = None;
        self.health = self
            .shared
            .spec
            .health
            .as_ref()
            .map(|h| HealthStatus::new(&h.probe));
        if self.shared.spec.precheck.is_empty() {
            self.launch(ctx);
        } else {
            self.prechecks = Some(TaskGuard(tokio::spawn(precheck_task(
                Arc::clone(&self.shared),
                ctx.myself_weak(),
                run,
                PrecheckScope::Start,
            ))));
        }
    }

    /// Creates the process of the current run (the pre-checks passed): the start
    /// deadline counts from here.
    fn launch(&mut self, ctx: &mut Context<Self>) {
        let run = self.run;
        self.start_deadline = self.shared.spec.health.as_ref().map(|h| {
            deliver_after(
                ctx.myself_weak(),
                h.start_timeout,
                Notice::StartDeadline { run },
            )
        });
        tokio::spawn(run_process(
            Arc::clone(&self.shared),
            ctx.myself_weak(),
            run,
            std::mem::take(&mut self.job_cleanups),
            ctx.events().clone(),
        ));
    }

    /// End of the pre-checks of the current run.
    fn on_prechecked(
        &mut self,
        result: std::result::Result<(), FailureReason>,
        ctx: &mut Context<Self>,
    ) {
        self.prechecks = None;
        match result {
            Ok(()) => self.launch(ctx),
            Err(reason) => {
                tracing::warn!(
                    instance = %self.shared.service.instance,
                    tool = %self.shared.service.tool,
                    %reason,
                    "pre-check failed; the instance is not started"
                );
                self.fail(reason, None, ctx);
            }
        }
    }

    /// The current run is stopped while its pre-checks run: it ends `Stopped` without
    /// ever creating a process (then restarts or enters maintenance if that was asked).
    fn abort_unspawned(&mut self, cause: StopCause, ctx: &mut Context<Self>) {
        self.prechecks = None;
        self.start_deadline = None;
        // Unhealthy and start-timeout stops need a process; anything else ends Stopped.
        let cause = match cause {
            StopCause::Restart | StopCause::Maintenance => cause,
            _ => StopCause::User,
        };
        if cause == StopCause::Restart {
            let waiting = std::mem::take(&mut self.pending_start);
            self.pending_restart.extend(waiting);
        } else {
            self.cancel_start_waiters("a stop was requested before the instance was running");
        }
        if self.enter(Trigger::Stop { cause }, None, ctx).is_err() {
            return;
        }
        let payload = ServiceEventPayload::idle(self.service());
        let _ = self.enter(Trigger::StopFinished { exit_code: 0 }, Some(payload), ctx);
        self.answer_stops(&StopReport::not_running());
        match std::mem::replace(&mut self.after_stop, AfterStop::Nothing) {
            AfterStop::Nothing => {}
            AfterStop::Start => {
                self.restarts.reset();
                self.pending_start.append(&mut self.pending_restart);
                self.begin_start(ctx);
            }
            AfterStop::Maintenance => self.enter_maintenance(ctx),
        }
    }

    /// Whether a restart has something to validate before stopping the running process.
    fn restart_needs_validation(&self) -> bool {
        let spec = &self.shared.spec.precheck;
        spec.check.is_some() || !spec.depends_on.is_empty() || !spec.runtime_deps.is_empty()
    }

    /// End of the validation of a restart requested while `Running`.
    fn on_restart_validated(
        &mut self,
        result: std::result::Result<(), FailureReason>,
        ctx: &mut Context<Self>,
    ) {
        self.validation = None;
        if self.state != InstanceState::Running || self.after_stop != AfterStop::Start {
            return;
        }
        match result {
            Ok(()) => self.begin_stop(StopCause::Restart, ctx),
            Err(reason) => {
                tracing::warn!(
                    instance = %self.shared.service.instance,
                    tool = %self.shared.service.tool,
                    %reason,
                    "restart refused by the pre-checks; the instance keeps running"
                );
                self.after_stop = AfterStop::Nothing;
                fail_all(
                    std::mem::take(&mut self.pending_restart),
                    &reason.to_error(),
                );
            }
        }
    }

    /// `Starting` → `Running`: health passed (or there is none).
    fn become_running(&mut self, ctx: &mut Context<Self>) {
        let Some(pid) = self.pid else {
            return;
        };
        self.start_deadline = None;
        let payload = ServiceEventPayload::running(self.service(), self.run, pid);
        if self.enter(Trigger::Healthy, Some(payload), ctx).is_ok() {
            for reply in std::mem::take(&mut self.pending_start) {
                let _ = reply.send(Ok(pid));
            }
        }
    }

    /// Starts the stop cascade (the process exists). Without a process yet, the stop
    /// waits for the spawn.
    fn begin_stop(&mut self, cause: StopCause, ctx: &mut Context<Self>) {
        let (Some(pid), Some(exited)) = (self.pid, self.exited.clone()) else {
            if self.prechecks.is_some() {
                // Still pre-checking: no process will be created.
                self.abort_unspawned(cause, ctx);
            } else {
                self.stop_after_spawn = Some(cause);
            }
            return;
        };
        self.probes = None;
        self.start_deadline = None;
        // Any stop supersedes the validation of a restart.
        self.validation = None;
        match cause {
            StopCause::Restart => {
                // They asked for a started instance: they get the next run.
                let waiting = std::mem::take(&mut self.pending_start);
                self.pending_restart.extend(waiting);
            }
            StopCause::StartTimeout => {
                if let Some(reason) = &self.stop_failure {
                    let err = reason.to_error();
                    fail_all(std::mem::take(&mut self.pending_start), &err);
                }
            }
            StopCause::User | StopCause::Maintenance | StopCause::Unhealthy => {
                self.cancel_start_waiters("a stop was requested before the instance was running");
            }
        }
        if self.enter(Trigger::Stop { cause }, None, ctx).is_err() {
            return;
        }
        let (progress_tx, progress_rx) = watch::channel(None);
        self.stop_progress = Some(progress_rx);
        let events = ctx.events().clone();
        let service = self.shared.service.clone();
        let on_step: StepObserver = Box::new(move |strategy: StopStrategy, wait: Duration| {
            progress_tx.send_replace(Some(strategy));
            publish(
                &events,
                &ServiceEventPayload::stopping(&service, pid, strategy, wait, cause),
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
        let InstanceState::Stopping { cause, .. } = self.state else {
            return;
        };
        let pid = self.pid.take();
        self.stop_progress = None;
        let after = std::mem::replace(&mut self.after_stop, AfterStop::Nothing);
        let Some(code) = report.exit_code else {
            // The process outlived TerminateJobObject: nothing new goes into a job whose
            // processes may still be dying.
            let err = ForgeError::Timeout {
                operation: "the process did not exit after TerminateJobObject".to_owned(),
                after: self.shared.spec.stop.worst_case(),
            };
            fail_all(std::mem::take(&mut self.pending_restart), &err);
            fail_all(std::mem::take(&mut self.pending_maintenance), &err);
            self.stop_failure = None;
            self.fail(FailureReason::from_error(&err), pid, ctx);
            self.answer_stops(&report);
            return;
        };
        match cause {
            StopCause::User | StopCause::Restart | StopCause::Maintenance => {
                let payload = ServiceEventPayload::stopped(self.service(), pid, &report);
                let _ = self.enter(
                    Trigger::StopFinished { exit_code: code },
                    Some(payload),
                    ctx,
                );
            }
            StopCause::Unhealthy => {
                self.crashed(
                    Trigger::StopFinished { exit_code: code },
                    Failure::Unhealthy,
                    pid,
                    code,
                    report.leftovers_killed,
                    ctx,
                );
            }
            StopCause::StartTimeout => {
                let reason = self
                    .stop_failure
                    .take()
                    .unwrap_or(FailureReason::HealthTimeout {
                        timeout_ms: self
                            .shared
                            .spec
                            .health
                            .as_ref()
                            .map_or(0, |h| millis(h.start_timeout)),
                        last_error: None,
                    });
                self.fail(reason, pid, ctx);
            }
        }
        self.answer_stops(&report);
        match after {
            AfterStop::Nothing => {}
            AfterStop::Start => {
                self.restarts.reset();
                self.pending_start.append(&mut self.pending_restart);
                self.begin_start(ctx);
            }
            AfterStop::Maintenance => self.enter_maintenance(ctx),
        }
    }

    fn answer_stops(&mut self, report: &StopReport) {
        for reply in std::mem::take(&mut self.pending_stop) {
            let _ = reply.send(Ok(report.clone()));
        }
    }

    /// Enters `Failed(reason)` and publishes `service.failed` (`pid`: a process left
    /// behind, if any).
    fn fail(&mut self, reason: FailureReason, pid: Option<u32>, ctx: &mut Context<Self>) {
        self.probes = None;
        self.start_deadline = None;
        self.cancel_restart_timer();
        let payload = ServiceEventPayload::failed_with(self.service(), pid, &reason);
        let err = reason.to_error();
        if self
            .enter(Trigger::Fail { reason }, Some(payload), ctx)
            .is_ok()
        {
            fail_all(std::mem::take(&mut self.pending_start), &err);
        }
    }

    /// The run ended without being asked to: `Crashed`, then what the restart policy
    /// says.
    fn crashed(
        &mut self,
        trigger: Trigger,
        failure: Failure,
        pid: Option<u32>,
        exit_code: u32,
        leftovers_killed: u32,
        ctx: &mut Context<Self>,
    ) {
        self.probes = None;
        self.start_deadline = None;
        let mut payload =
            ServiceEventPayload::crashed(self.service(), pid, exit_code, leftovers_killed);
        if failure == Failure::Unhealthy {
            payload = payload.with_cause(StopCause::Unhealthy);
        }
        // Restarting cannot fix a missing DLL or an access denied.
        let hopeless = match failure {
            Failure::Exit(code) => FailureReason::for_exit_code(code),
            Failure::Unhealthy => None,
        };
        let decision = match hopeless {
            Some(_) => RestartDecision::Stay,
            None => self.restarts.decide(
                &self.shared.spec.restart,
                failure,
                tokio::time::Instant::now(),
            ),
        };
        if let RestartDecision::Restart { delay, .. } = decision {
            payload = payload.with_restart_in(delay);
        }
        tracing::warn!(
            instance = %self.shared.service.instance,
            tool = %self.shared.service.tool,
            exit_code,
            ?failure,
            ?decision,
            "instance crashed"
        );
        if self.enter(trigger, Some(payload), ctx).is_err() {
            return;
        }
        if let Some(reason) = hopeless {
            self.fail(reason, None, ctx);
            return;
        }
        match decision {
            RestartDecision::Restart { delay, .. } => {
                self.cancel_restart_timer();
                let generation = self.restart_generation;
                self.next_restart_at = Timestamp::now().checked_add(delay).ok();
                self.restart_timer = Some(deliver_after(
                    ctx.myself_weak(),
                    delay,
                    Notice::RestartDue { generation },
                ));
            }
            RestartDecision::Stay => {}
            RestartDecision::GiveUp { restarts, window } => {
                let reason = FailureReason::CrashLoop {
                    restarts,
                    window_ms: millis(window),
                    exit_code: match failure {
                        Failure::Exit(code) => Some(crate::events::wire_exit_code(code)),
                        Failure::Unhealthy => None,
                    },
                };
                self.fail(reason, None, ctx);
            }
        }
    }

    fn enter_maintenance(&mut self, ctx: &mut Context<Self>) {
        self.cancel_restart_timer();
        let payload = ServiceEventPayload::maintenance(self.service());
        let result = self.enter(Trigger::EnterMaintenance, Some(payload), ctx);
        for reply in std::mem::take(&mut self.pending_maintenance) {
            let _ = reply.send(match &result {
                Ok(()) => Ok(()),
                Err(err) => Err(duplicate(err)),
            });
        }
    }

    fn on_start(&mut self, reply: Reply<Result<u32>>, ctx: &mut Context<Self>) {
        let entering_maintenance = || {
            Err(ForgeError::Conflict(
                "the instance is entering maintenance".into(),
            ))
        };
        match self.state {
            InstanceState::Running => {
                let _ =
                    reply.send(self.pid.ok_or_else(|| {
                        ForgeError::Internal("running instance without a pid".into())
                    }));
            }
            InstanceState::Starting if self.stop_after_spawn.is_none() => {
                self.pending_start.push(reply);
            }
            InstanceState::Starting | InstanceState::Stopping { .. } => {
                if self.after_stop == AfterStop::Maintenance {
                    let _ = reply.send(entering_maintenance());
                } else {
                    self.after_stop = AfterStop::Start;
                    self.pending_restart.push(reply);
                }
            }
            InstanceState::Stopped
            | InstanceState::Crashed { .. }
            | InstanceState::Failed { .. } => {
                self.restarts.reset();
                self.pending_start.push(reply);
                self.begin_start(ctx);
            }
            InstanceState::Maintenance => {
                let _ = reply.send(Err(ForgeError::Conflict(
                    "the instance is under maintenance".into(),
                )));
            }
        }
    }

    fn on_stop(&mut self, reply: Reply<Result<StopReport>>, ctx: &mut Context<Self>) {
        match self.state {
            InstanceState::Running => {
                // A restart still being validated is superseded.
                self.cancel_queued_start("a stop was requested before the restart");
                self.pending_stop.push(reply);
                self.begin_stop(StopCause::User, ctx);
            }
            InstanceState::Starting => {
                self.cancel_queued_start("a stop was requested before the restart");
                self.pending_stop.push(reply);
                match self.stop_after_spawn {
                    // Already going down for maintenance or a missed deadline.
                    Some(StopCause::Maintenance | StopCause::StartTimeout) => {}
                    _ => self.begin_stop(StopCause::User, ctx),
                }
            }
            InstanceState::Stopping { ref mut cause, .. } => {
                // A requested stop ends in Stopped, not in Crashed.
                if *cause == StopCause::Unhealthy {
                    *cause = StopCause::User;
                }
                self.cancel_queued_start("a stop was requested before the restart");
                self.pending_stop.push(reply);
            }
            InstanceState::Crashed { .. } => {
                // No automatic restart after an explicit stop.
                self.cancel_restart_timer();
                let _ = reply.send(Ok(StopReport::not_running()));
            }
            InstanceState::Stopped | InstanceState::Failed { .. } | InstanceState::Maintenance => {
                let _ = reply.send(Ok(StopReport::not_running()));
            }
        }
    }

    fn on_restart(&mut self, reply: Reply<Result<u32>>, ctx: &mut Context<Self>) {
        match self.state {
            InstanceState::Running => {
                self.pending_restart.push(reply);
                self.after_stop = AfterStop::Start;
                if !self.restart_needs_validation() {
                    self.begin_stop(StopCause::Restart, ctx);
                } else if self.validation.is_none() {
                    self.validation = Some(TaskGuard(tokio::spawn(precheck_task(
                        Arc::clone(&self.shared),
                        ctx.myself_weak(),
                        self.run,
                        PrecheckScope::Restart,
                    ))));
                }
                // Otherwise a validation is already running for an earlier request.
            }
            InstanceState::Starting | InstanceState::Stopping { .. } => {
                if self.after_stop == AfterStop::Maintenance {
                    let _ = reply.send(Err(ForgeError::Conflict(
                        "the instance is entering maintenance".into(),
                    )));
                    return;
                }
                self.pending_restart.push(reply);
                self.after_stop = AfterStop::Start;
                if self.state == InstanceState::Starting && self.stop_after_spawn.is_none() {
                    self.begin_stop(StopCause::Restart, ctx);
                }
            }
            InstanceState::Stopped
            | InstanceState::Crashed { .. }
            | InstanceState::Failed { .. } => {
                self.restarts.reset();
                self.pending_start.push(reply);
                self.begin_start(ctx);
            }
            InstanceState::Maintenance => {
                let _ = reply.send(Err(ForgeError::Conflict(
                    "the instance is under maintenance".into(),
                )));
            }
        }
    }

    fn on_enter_maintenance(&mut self, reply: Reply<Result<()>>, ctx: &mut Context<Self>) {
        match self.state {
            InstanceState::Maintenance => {
                let _ = reply.send(Ok(()));
            }
            InstanceState::Stopped
            | InstanceState::Crashed { .. }
            | InstanceState::Failed { .. } => {
                self.pending_maintenance.push(reply);
                self.enter_maintenance(ctx);
            }
            InstanceState::Running | InstanceState::Starting => {
                self.cancel_queued_start("maintenance was requested before the restart");
                self.pending_maintenance.push(reply);
                self.after_stop = AfterStop::Maintenance;
                match self.stop_after_spawn {
                    Some(_) => self.stop_after_spawn = Some(StopCause::Maintenance),
                    None => self.begin_stop(StopCause::Maintenance, ctx),
                }
            }
            InstanceState::Stopping { ref mut cause, .. } => {
                if *cause == StopCause::Unhealthy {
                    *cause = StopCause::Maintenance;
                }
                self.cancel_queued_start("maintenance was requested before the restart");
                self.pending_maintenance.push(reply);
                self.after_stop = AfterStop::Maintenance;
            }
        }
    }

    fn on_exit_maintenance(&mut self, reply: Reply<Result<()>>, ctx: &mut Context<Self>) {
        let result = if self.state == InstanceState::Maintenance {
            let payload = ServiceEventPayload::idle(self.service());
            self.enter(Trigger::ExitMaintenance, Some(payload), ctx)
        } else {
            Err(ForgeError::Conflict(format!(
                "the instance is not under maintenance (state {})",
                self.state.name()
            )))
        };
        let _ = reply.send(result);
    }

    fn on_spawned(
        &mut self,
        result: Result<(u32, watch::Receiver<Option<ExitInfo>>)>,
        ctx: &mut Context<Self>,
    ) {
        match result {
            Ok((pid, exited)) => {
                self.pid = Some(pid);
                self.exited = Some(exited);
                if let Some(cause) = self.stop_after_spawn.take() {
                    self.begin_stop(cause, ctx);
                    return;
                }
                let Some(spec) = self.shared.spec.health.clone() else {
                    self.become_running(ctx);
                    return;
                };
                match Prober::new(
                    &spec,
                    pid,
                    Arc::clone(&self.shared.processes),
                    Arc::clone(&self.shared.root),
                ) {
                    Ok(prober) => {
                        self.probes = Some(TaskGuard(tokio::spawn(probe_loop(
                            spec,
                            prober,
                            ctx.myself_weak(),
                            self.run,
                        ))));
                    }
                    Err(err) => {
                        // The process runs but cannot be checked: stop it and fail.
                        self.stop_failure = Some(FailureReason::from_error(&err));
                        self.begin_stop(StopCause::StartTimeout, ctx);
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    instance = %self.shared.service.instance,
                    tool = %self.shared.service.tool,
                    %err,
                    "instance failed to start"
                );
                fail_all(std::mem::take(&mut self.pending_start), &err);
                fail_all(std::mem::take(&mut self.pending_restart), &err);
                for reply in std::mem::take(&mut self.pending_stop) {
                    let _ = reply.send(Ok(StopReport::not_running()));
                }
                self.stop_after_spawn = None;
                self.stop_failure = None;
                let after = std::mem::replace(&mut self.after_stop, AfterStop::Nothing);
                self.fail(FailureReason::from_error(&err), None, ctx);
                if after == AfterStop::Maintenance {
                    self.enter_maintenance(ctx);
                }
            }
        }
    }

    fn on_probe(&mut self, outcome: std::result::Result<(), String>, ctx: &mut Context<Self>) {
        if !matches!(self.state, InstanceState::Starting | InstanceState::Running) {
            return;
        }
        let (Some(spec), Some(health)) = (self.shared.spec.health.as_ref(), self.health.as_mut())
        else {
            return;
        };
        health.record(&outcome, Timestamp::now());
        let healthy = health.is_healthy(spec);
        let newly_unhealthy = health.became_unhealthy(spec);
        let last_error = health.last_error.clone().unwrap_or_default();
        match self.state {
            InstanceState::Starting if healthy && self.stop_after_spawn.is_none() => {
                self.become_running(ctx);
            }
            InstanceState::Running if newly_unhealthy => {
                if self.shared.spec.restart == RestartPolicy::Never {
                    tracing::warn!(
                        instance = %self.shared.service.instance,
                        tool = %self.shared.service.tool,
                        error = %last_error,
                        "instance is unhealthy; restart policy is never, leaving it running"
                    );
                } else {
                    tracing::warn!(
                        instance = %self.shared.service.instance,
                        tool = %self.shared.service.tool,
                        error = %last_error,
                        "instance is unhealthy; stopping it for the restart policy"
                    );
                    self.begin_stop(StopCause::Unhealthy, ctx);
                }
            }
            _ => {}
        }
    }

    fn on_start_deadline(&mut self, ctx: &mut Context<Self>) {
        if self.state != InstanceState::Starting {
            return;
        }
        let timeout_ms = self
            .shared
            .spec
            .health
            .as_ref()
            .map_or(0, |h| millis(h.start_timeout));
        let reason = FailureReason::HealthTimeout {
            timeout_ms,
            last_error: self.health.as_ref().and_then(|h| h.last_error.clone()),
        };
        tracing::warn!(
            instance = %self.shared.service.instance,
            tool = %self.shared.service.tool,
            %reason,
            "instance did not become healthy in time; stopping it"
        );
        self.stop_failure = Some(reason);
        if self.stop_after_spawn.is_none() {
            self.begin_stop(StopCause::StartTimeout, ctx);
        }
        // Otherwise a stop is already queued for the spawn and keeps its cause.
    }

    fn on_exited(&mut self, info: ExitInfo, ctx: &mut Context<Self>) {
        match self.state {
            InstanceState::Running => {
                if self.validation.take().is_some() {
                    self.cancel_queued_start("the instance exited before it could be restarted");
                }
                let pid = self.pid.take();
                self.crashed(
                    Trigger::Exited {
                        exit_code: info.code,
                    },
                    Failure::Exit(info.code),
                    pid,
                    info.code,
                    info.leftovers_killed,
                    ctx,
                );
            }
            InstanceState::Starting => {
                // Exited before it became healthy.
                let pid = self.pid.take();
                self.probes = None;
                self.start_deadline = None;
                let reason = FailureReason::exited_during_start(info.code);
                tracing::warn!(
                    instance = %self.shared.service.instance,
                    tool = %self.shared.service.tool,
                    %reason,
                    "instance exited while starting"
                );
                let err = reason.to_error();
                let payload = ServiceEventPayload::failed_with(self.service(), pid, &reason);
                let trigger = Trigger::Exited {
                    exit_code: info.code,
                };
                if self.enter(trigger, Some(payload), ctx).is_ok() {
                    fail_all(std::mem::take(&mut self.pending_start), &err);
                }
            }
            // Stopping: the cascade observes the same exit and reports.
            _ => {}
        }
    }

    fn on_notice(&mut self, notice: Notice, ctx: &mut Context<Self>) {
        match notice {
            Notice::Spawned { run, result }
                if run == self.run && self.state == InstanceState::Starting =>
            {
                self.on_spawned(result, ctx);
            }
            Notice::Exited { run, info } if run == self.run => self.on_exited(info, ctx),
            Notice::StopFinished { run, report } if run == self.run => {
                self.finish_stop(report, ctx);
            }
            Notice::Probe { run, outcome } if run == self.run => self.on_probe(outcome, ctx),
            Notice::StartDeadline { run } if run == self.run => self.on_start_deadline(ctx),
            Notice::Prechecked { run, result }
                if run == self.run
                    && self.state == InstanceState::Starting
                    && self.prechecks.is_some() =>
            {
                self.on_prechecked(result, ctx);
            }
            Notice::RestartValidated { run, result }
                if run == self.run && self.validation.is_some() =>
            {
                self.on_restart_validated(result, ctx);
            }
            Notice::RestartDue { generation } => {
                if generation == self.restart_generation
                    && matches!(self.state, InstanceState::Crashed { .. })
                {
                    self.begin_start(ctx);
                }
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
        self.shared.metrics.send_replace(None);
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
            InstanceMsg::Metrics(reply) => {
                let latest = self.shared.metrics.borrow().clone();
                let _ = reply.send(latest);
            }
            InstanceMsg::SubscribeMetrics(reply) => {
                let _ = reply.send(self.shared.metrics.subscribe());
            }
            InstanceMsg::EnterMaintenance(reply) => self.on_enter_maintenance(reply, ctx),
            InstanceMsg::ExitMaintenance(reply) => self.on_exit_maintenance(reply, ctx),
            InstanceMsg::Internal(Internal(notice)) => self.on_notice(notice, ctx),
        }
    }

    /// Actor shutdown: the processes go with it (graceful stops are requested before, by
    /// the daemon, with `Stop`).
    async fn on_stop(&mut self, _ctx: &mut Context<Self>) {
        self.probes = None;
        self.start_deadline = None;
        self.restart_timer = None;
        self.prechecks = None;
        self.validation = None;
        self.terminate_job().await;
    }

    /// The previous incarnation panicked and its view of the process is lost: kill what
    /// runs in the job and report `Failed`.
    async fn on_restart(&mut self, ctx: &mut Context<Self>, info: &RestartInfo) {
        self.terminate_job().await;
        self.fail(
            FailureReason::Other {
                code: "internal".to_owned(),
                message: format!(
                    "instance supervisor restarted after an internal error: {}",
                    info.reason
                ),
            },
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

    /// Starts and waits at most `timeout` for the pid of the running instance.
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

    /// Latest metrics sample within `timeout`.
    pub async fn metrics(&self, timeout: Duration) -> Result<Option<InstanceMetrics>> {
        Ok(self.actor().ask(InstanceMsg::Metrics, timeout).await?)
    }

    /// Receiver of the metrics samples within `timeout`.
    pub async fn subscribe_metrics(
        &self,
        timeout: Duration,
    ) -> Result<watch::Receiver<Option<InstanceMetrics>>> {
        Ok(self
            .actor()
            .ask(InstanceMsg::SubscribeMetrics, timeout)
            .await?)
    }

    /// Stops the instance if needed and holds it in `Maintenance`; waits at most
    /// `timeout`.
    pub async fn enter_maintenance(&self, timeout: Duration) -> Result<()> {
        self.actor()
            .ask(InstanceMsg::EnterMaintenance, timeout)
            .await?
    }

    /// Leaves `Maintenance` within `timeout`.
    pub async fn exit_maintenance(&self, timeout: Duration) -> Result<()> {
        self.actor()
            .ask(InstanceMsg::ExitMaintenance, timeout)
            .await?
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
#[derive(Clone)]
pub struct Supervisor {
    jobs: JobTree,
    actors: ActorSupervisor,
    stats: Arc<dyn ProcessStats>,
    ports: Arc<dyn PortTable>,
    runtimes: Arc<dyn RuntimeDeps>,
    dependencies: Arc<dyn DependencyLookup>,
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Supervisor")
            .field("jobs", &self.jobs)
            .field("actors", &self.actors)
            .finish_non_exhaustive()
    }
}

impl Supervisor {
    /// Supervisor reading process counters, the TCP table and the installed runtimes of
    /// the running OS ([`forge_core::platform`]). Dependencies are unknown until
    /// [`Supervisor::with_dependencies`] (every `depends_on` pre-check fails).
    pub fn new(jobs: JobTree, actors: ActorSupervisor) -> Self {
        Self {
            jobs,
            actors,
            stats: process_stats(),
            ports: port_table(),
            runtimes: runtime_deps(),
            dependencies: Arc::new(NoDependencies),
        }
    }

    /// Replaces the TCP table of the port pre-check (fakes in tests).
    pub fn with_port_table(mut self, ports: Arc<dyn PortTable>) -> Self {
        self.ports = ports;
        self
    }

    /// Replaces the runtime detection of the runtime pre-check (fakes in tests).
    pub fn with_runtime_deps(mut self, runtimes: Arc<dyn RuntimeDeps>) -> Self {
        self.runtimes = runtimes;
        self
    }

    /// How the `depends_on` pre-check learns the state of other services (the daemon
    /// answers from its instance actors).
    pub fn with_dependencies(mut self, dependencies: Arc<dyn DependencyLookup>) -> Self {
        self.dependencies = dependencies;
        self
    }

    /// Replaces the process counters (fakes in tests).
    pub fn with_process_stats(mut self, stats: Arc<dyn ProcessStats>) -> Self {
        self.stats = stats;
        self
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
        if let Some(health) = &spec.health {
            health.validate()?;
        }
        spec.precheck.validate()?;
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
            stats: Arc::clone(&self.stats),
            metrics: Arc::new(watch::channel(None).0),
            runs: AtomicU64::new(0),
            precheck: PrecheckEnv {
                ports: Arc::clone(&self.ports),
                runtimes: Arc::clone(&self.runtimes),
                dependencies: Arc::clone(&self.dependencies),
                processes: Arc::clone(self.jobs.processes()),
                root: Arc::clone(self.jobs.root()),
            },
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
    fn spec_defaults() {
        let spec = InstanceSpec::new(
            InstanceId::new(),
            ToolId::new("mysql").unwrap(),
            "default",
            SpawnSpec::new("C:/x.exe", "C:/"),
        );
        assert_eq!(spec.health, None);
        assert_eq!(spec.restart, RestartPolicy::default());
        assert_eq!(spec.metrics.interval, Duration::from_secs(2));
        assert!(!spec.metrics.publish_events);
        let spec = spec
            .with_restart(RestartPolicy::Never)
            .with_health(HealthSpec::new(crate::health::Probe::Pid));
        assert_eq!(spec.restart, RestartPolicy::Never);
        assert!(spec.health.is_some());
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
