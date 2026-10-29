//! `PhpPool`: one actor per PHP version (docs/03 §2.1, §7.2, ADR-0003, ADR-0021).
//!
//! A pool keeps between `scaling.min` and `scaling.max` warm `php-cgi.exe` workers, one
//! per port of the `php-pools` range, and publishes them as an [`UpstreamModel`]. The
//! actor only decides; everything slow runs in tasks that report back by message:
//! launching a worker (port, spawn, warmup: [`crate::worker::launch`]), stopping one,
//! sampling the TCP table and the processes (`spawn_blocking`, one read for every
//! worker), and applying an upstream change through the injected [`UpstreamListener`]
//! (re-render + reload by the daemon). `Status` is always answered at once.
//!
//! **Lifecycle.** `Start` → `Starting` (health script written, stale reservations of a
//! previous run released, `min` workers launched) → `Running` once `min` workers are
//! warm. `Drain` (blue/green, [`crate::swap`]) takes the pool out of the web server and
//! stops each worker once its connections ended (or after `drain_timeout`); `Stop` stops
//! every worker at once. Both end in `Stopped`. A pool that cannot get `min` workers up
//! in `start_attempts` launches ends `Failed`.
//!
//! **Every tick** (`timing.tick`) the pool samples, for all its workers at once, the TCP
//! connections of their ports (`PortTable::connections`, `TIME_WAIT` included) and their
//! processes (alive, working set via `ProcessStats`). From that:
//! * **requests** served by each worker are estimated ([`crate::worker::ConnTracker`]);
//! * **recycle**: a worker past `max_requests`, `max_rss_mb` or `max_age` is replaced
//!   proactively — a new worker is launched and warmed up, the old one leaves the
//!   upstream in the same change that adds the new one, drains, and is stopped
//!   (`pool.worker.recycled`). php-cgi's own `PHP_FCGI_MAX_REQUESTS` is set far above
//!   the threshold ([`crate::worker::hard_request_limit`]) so it never exits under load;
//! * **scaling**: when every worker is busy (established connections ≥ its
//!   `FCGI_MAX_CONNS`), up to the queued connections are added (at most `max`); workers
//!   idle for `idle_timeout` above `min` are retired;
//! * **exits**: a worker that died leaves the upstream at once and is replaced to keep
//!   `min` (the pool also learns it from its workers' `service.crashed` on a private bus).
//!
//! **Upstream changes** are numbered. A worker that leaves the upstream is `Retiring`
//! until the listener applied a change without it, then `Draining`: after
//! `drain_grace` it is stopped once `drain_quiet` samples in a row show no connection
//! and no new one, or after `drain_timeout`. A listener failure is retried with backoff
//! and the leaving workers keep serving meanwhile. A pool started on standby
//! ([`PoolHandle::start_standby`]) does not call the listener until promoted: blue/green
//! between two versions of one series shares the upstream name.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::actor::{
    Actor, ActorHandle, ActorSpec, ActorSupervisor, Context, Reply, RestartInfo, StopOutcome,
    TellError, WeakActorRef,
};
use forge_core::event::{EventBus, EventKind};
use forge_core::ids::{InstanceId, ToolId, VersionId};
use forge_core::model::port::{OwnerKind, PortReservations};
use forge_core::platform::{PortTable, ProcessControl, ProcessStats, RuntimeDep, TcpConnection};
use forge_core::runtime::spawn_blocking_timeout;
use forge_core::{ForgeError, Result, Timestamp};
use forge_supervisor::{EnvMap, InstanceHandle, JobTree, Supervisor};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::events::{PoolScaledEvent, RecycleReason, RecycledWorker, WorkerRecycledEvent, publish};
use crate::probe::HealthScript;
use crate::upstream::{
    LbPolicy, ScaleReason, UpstreamChange, UpstreamListener, UpstreamModel, upstream_name,
};
use crate::worker::{
    self, ConnTracker, LaunchContext, Launched, WorkerPhase, WorkerStatus, connection_keys,
    reservation_owner,
};

/// Timeout of the quick requests of [`PoolHandle`] (`Status`, `Upstream`).
pub const POOL_QUICK_TIMEOUT: Duration = Duration::from_secs(1);

/// Worker count bounds (manifest `service.pool.scaling`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scaling {
    /// Warm workers kept even when idle (at least 1: an upstream needs a server).
    pub min: u32,
    /// Most workers under load.
    pub max: u32,
    /// Idle time after which a worker above `min` is retired.
    pub idle_timeout: Duration,
}

impl Default for Scaling {
    /// `{min: 2, max: 12, idle_timeout: 120s}` (docs/04 §1.7).
    fn default() -> Self {
        Self {
            min: 2,
            max: 12,
            idle_timeout: Duration::from_secs(120),
        }
    }
}

/// Recycle thresholds (manifest `service.pool.recycle`); 0 / `None` disables one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recycle {
    /// Requests (estimated) after which a worker is replaced.
    pub max_requests: u64,
    /// Working set, in MiB, above which a worker is replaced.
    pub max_rss_mb: u64,
    /// Age after which a worker is replaced.
    pub max_age: Option<Duration>,
}

impl Default for Recycle {
    /// `{max_requests: 500, max_rss_mb: 256, max_age: 1h}` (docs/04 §1.7).
    fn default() -> Self {
        Self {
            max_requests: 500,
            max_rss_mb: 256,
            max_age: Some(Duration::from_secs(3600)),
        }
    }
}

/// Timings of a pool. The defaults suit the daemon; tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolTiming {
    /// Sampling period of the TCP table and the processes (and of the scaling/recycle
    /// decisions). Must stay well below 30 s, the shortest `TIME_WAIT`.
    pub tick: Duration,
    /// Budget of one worker launch: port, spawn and warmup.
    pub start_timeout: Duration,
    /// Budget of one FastCGI probe exchange.
    pub probe_timeout: Duration,
    /// Budget of one sample (`spawn_blocking`).
    pub sample_timeout: Duration,
    /// Budget of one call to the upstream listener (re-render + reload).
    pub listener_timeout: Duration,
    /// After a change without a worker was applied, how long new connections to it are
    /// still expected (requests the web server routed just before its reload).
    pub drain_grace: Duration,
    /// Consecutive samples without connections that end a worker's drain.
    pub drain_quiet: u32,
    /// Longest drain (docs/03 §2.2: 30 s); the worker is stopped even with connections.
    pub drain_timeout: Duration,
    /// Wait of the first stop step of a worker (`CTRL_BREAK`).
    pub stop_timeout: Duration,
    /// Backoff after a failed launch (or listener call): `retry_min · 2^n` ≤ `retry_max`.
    pub retry_min: Duration,
    pub retry_max: Duration,
    /// Failed launches before a starting pool gives up (`Failed`).
    pub start_attempts: u32,
}

impl Default for PoolTiming {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(1),
            start_timeout: Duration::from_secs(20),
            probe_timeout: Duration::from_secs(5),
            sample_timeout: Duration::from_secs(5),
            listener_timeout: Duration::from_secs(30),
            drain_grace: Duration::from_secs(1),
            drain_quiet: 2,
            drain_timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(10),
            retry_min: Duration::from_secs(1),
            retry_max: Duration::from_secs(30),
            start_attempts: 3,
        }
    }
}

impl PoolTiming {
    /// Upper bound of a worker's stop cascade (`CTRL_BREAK`, then the job emptied).
    pub fn stop_wait(&self) -> Duration {
        self.stop_timeout + Duration::from_secs(20)
    }

    fn backoff(&self, failures: u32) -> Duration {
        let factor = 2_u32.saturating_pow(failures.saturating_sub(1).min(16));
        self.retry_min.saturating_mul(factor).min(self.retry_max)
    }
}

/// Everything about one pool, already resolved from the manifest by the daemon
/// (`service.pool`, docs/04): absolute paths, rendered arguments (except the port).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSpec {
    /// The installed version this pool runs; owner id of its port reservations.
    pub version_id: VersionId,
    /// `php`.
    pub tool: ToolId,
    /// Exact version (`8.3.36`), for names and events.
    pub version: String,
    /// `8.3`: names the ini dir and, by default, the upstream.
    pub series: String,
    /// Upstream name (`php_8_3`; [`upstream_name`] of the series by default).
    pub upstream: String,
    /// Absolute path of `php-cgi.exe` (inside `bin/php/<version>/`).
    pub exe: PathBuf,
    /// Arguments with one `<ip>:{{ port }}` ([`crate::worker::PORT_PLACEHOLDER`]); the IP
    /// must be loopback. Default: `-b 127.0.0.1:{{ port }}`.
    pub args: Vec<String>,
    /// Working directory (the version folder by default).
    pub cwd: PathBuf,
    /// Manifest variables (already rendered); see [`crate::worker::worker_env`].
    pub env: EnvMap,
    /// `PATH` entries (the version folder by default).
    pub path: Vec<PathBuf>,
    /// `etc/php/<series>`: `PHPRC`, `PHP_INI_SCAN_DIR=<ini_dir>/conf.d`.
    pub ini_dir: PathBuf,
    /// Document root of the health script (`<health_root>/_forgebox/health.php`); the
    /// ini dir by default.
    pub health_root: PathBuf,
    /// Named port range (`php-pools`).
    pub port_range: String,
    pub scaling: Scaling,
    pub recycle: Recycle,
    pub policy: LbPolicy,
    pub timing: PoolTiming,
    /// VC++ runtime checked before each worker starts (`spec.runtime_deps`).
    pub runtime_deps: Vec<RuntimeDep>,
    /// `PHP_VERSION` the workers must report at warmup (`None`: not checked).
    pub expect_php_version: Option<String>,
    /// Folder of the workers' `worker-<port>.out.log` (`None`: live output only).
    pub log_dir: Option<PathBuf>,
}

impl PoolSpec {
    /// Spec of `php-cgi.exe` at `exe` for `version` of `series`, with the defaults of the
    /// manifest (docs/04 §1.7).
    pub fn new(
        version_id: VersionId,
        version: impl Into<String>,
        series: impl Into<String>,
        exe: impl Into<PathBuf>,
        ini_dir: impl Into<PathBuf>,
    ) -> Result<Self> {
        let series = series.into();
        let exe = exe.into();
        let ini_dir = ini_dir.into();
        let dir = exe.parent().map(PathBuf::from).unwrap_or_default();
        Ok(Self {
            version_id,
            tool: ToolId::new("php")?,
            version: version.into(),
            upstream: upstream_name(&series)?,
            series,
            exe,
            args: vec![
                "-b".to_owned(),
                format!("127.0.0.1:{}", worker::PORT_PLACEHOLDER),
            ],
            cwd: dir.clone(),
            env: EnvMap::new(),
            path: vec![dir],
            health_root: ini_dir.clone(),
            ini_dir,
            port_range: "php-pools".to_owned(),
            scaling: Scaling::default(),
            recycle: Recycle::default(),
            policy: LbPolicy::default(),
            timing: PoolTiming::default(),
            runtime_deps: Vec::new(),
            expect_php_version: None,
            log_dir: None,
        })
    }

    /// Checks the values (`InvalidInput`).
    pub fn validate(&self) -> Result<()> {
        let invalid = |m: String| Err(ForgeError::InvalidInput(m));
        if !self.exe.is_absolute() {
            return invalid(format!("pool exe {} is not absolute", self.exe.display()));
        }
        if !self.ini_dir.is_absolute() || !self.health_root.is_absolute() {
            return invalid("pool ini dir and health root must be absolute".to_owned());
        }
        if self.scaling.min == 0 || self.scaling.max < self.scaling.min {
            return invalid(format!(
                "pool scaling needs 1 <= min <= max, got min {} max {}",
                self.scaling.min, self.scaling.max
            ));
        }
        if self.timing.tick.is_zero()
            || self.timing.tick >= Duration::from_secs(30)
            || self.timing.drain_quiet == 0
            || self.timing.start_attempts == 0
        {
            return invalid(
                "pool timing needs 0 < tick < 30 s, drain_quiet >= 1, start_attempts >= 1"
                    .to_owned(),
            );
        }
        if self.version.is_empty() || self.port_range.is_empty() {
            return invalid("pool version and port range cannot be empty".to_owned());
        }
        if self.upstream.is_empty()
            || !self
                .upstream
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return invalid(format!("invalid upstream name {:?}", self.upstream));
        }
        worker::bind_address(&self.args)?;
        Ok(())
    }
}

/// What a pool needs from the daemon.
#[derive(Clone)]
pub struct PoolDeps {
    /// The daemon's supervisor: root job, actor supervisor (and its event bus), TCP table
    /// and process counters. The pool scopes it for its workers (nested job, private bus).
    pub supervisor: Supervisor,
    /// Worker ports (`forge_net::ports::PortAllocator` in the daemon).
    pub ports: Arc<dyn PortReservations>,
    /// Applies upstream changes to the web server; `None`: nothing to apply (tests,
    /// headless pools), changes count as applied at once.
    pub listener: Option<Arc<dyn UpstreamListener>>,
}

impl std::fmt::Debug for PoolDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoolDeps")
            .field("ports", &self.ports)
            .field("listener", &self.listener.is_some())
            .finish_non_exhaustive()
    }
}

/// State of a pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PoolState {
    Stopped,
    /// Launching the first `min` workers.
    Starting,
    Running,
    /// Out of the web server; workers stop as their connections end.
    Draining,
    /// Every worker being stopped.
    Stopping,
    /// Could not start (`code` is a [`ForgeError::code`]).
    Failed {
        code: String,
        message: String,
    },
}

impl PoolState {
    /// Wire name (`running`).
    pub fn name(&self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Draining => "draining",
            Self::Stopping => "stopping",
            Self::Failed { .. } => "failed",
        }
    }
}

/// Snapshot answered by `Status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolStatus {
    pub version_id: VersionId,
    pub tool: ToolId,
    pub version: String,
    pub series: String,
    #[serde(flatten)]
    pub state: PoolState,
    pub since: Timestamp,
    /// Whether upstream changes go to the listener (false on standby and once draining).
    pub serving: bool,
    pub upstream: UpstreamModel,
    pub min_workers: u32,
    pub max_workers: u32,
    pub workers: Vec<WorkerStatus>,
    /// Estimated requests served by every worker of this run, gone ones included.
    pub requests_total: u64,
    /// Workers replaced or lost (`pool.worker.recycled`).
    pub recycled: u64,
    /// Sum of the workers' last working sets.
    pub rss_bytes: u64,
    /// Last launch or listener failure.
    pub last_error: Option<String>,
}

impl PoolStatus {
    /// Workers in `phase`.
    pub fn count(&self, phase: WorkerPhase) -> usize {
        self.workers.iter().filter(|w| w.phase == phase).count()
    }
}

/// Outcome of a drain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrainReport {
    /// Workers stopped by the drain.
    pub workers: u32,
    /// Of them, stopped by `drain_timeout` with connections still open.
    pub forced: u32,
    pub elapsed_ms: u64,
}

/// Mailbox of a [`PhpPool`].
pub enum PoolMsg {
    /// Start (if needed); answered with the upstream once `min` workers are warm.
    /// `serve: false` starts on standby (no listener calls until [`PoolMsg::Promote`]).
    Start {
        serve: bool,
        reply: Reply<Result<UpstreamModel>>,
    },
    /// Leave standby: upstream changes go to the listener from now on. `seen` is the
    /// model the caller already applied (blue/green); a different current model is
    /// published at once.
    Promote {
        seen: UpstreamModel,
        reply: Reply<Result<()>>,
    },
    /// Go on standby: upstream changes stop reaching the listener. Answered once no
    /// listener call of this pool is in flight (none can land after the answer).
    Standby(Reply<Result<()>>),
    /// Snapshot; always answered at once.
    Status(Reply<PoolStatus>),
    /// Current upstream; always answered at once.
    Upstream(Reply<UpstreamModel>),
    /// Rolling replacement of every worker; answered at once with how many.
    Reload(Reply<Result<u32>>),
    /// Leave the web server and stop each worker once its connections end; answered
    /// when the pool is stopped.
    Drain(Reply<Result<DrainReport>>),
    /// Stop every worker now; answered when the pool is stopped.
    Stop(Reply<Result<()>>),
    /// Reports of the pool's own tasks (cannot be built outside this crate).
    #[doc(hidden)]
    Internal(Internal),
}

/// Opaque internal message (boxed: a launch report is much larger than the requests).
#[doc(hidden)]
pub struct Internal(Box<Notice>);

enum Notice {
    Tick,
    Prepared { attempt: u64, result: Result<()> },
    Launched { slot: u64, result: Result<Launched> },
    Sampled { result: Result<Sample> },
    Applied { seq: u64, result: Result<()> },
    RetryPublish,
    Retired { slot: u64 },
    WorkerExited { instance_id: InstanceId },
    Released,
}

/// One read of the TCP table and of the workers' processes.
struct Sample {
    /// `None` when the table cannot list connections (then `established` is used and
    /// no request is counted; `table_error` says why).
    conns: Option<Vec<TcpConnection>>,
    table_error: Option<String>,
    established: HashMap<u16, u32>,
    /// pid → (alive, working set).
    procs: HashMap<u32, (bool, Option<u64>)>,
}

/// Aborts its task when dropped.
struct TaskGuard(JoinHandle<()>);

impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// `retire_seq` of a worker that left the upstream before the change was numbered.
const SEQ_PENDING: u64 = u64::MAX;

/// A worker as the pool tracks it.
struct Worker {
    slot: u64,
    phase: WorkerPhase,
    port: Option<u16>,
    addr: Option<SocketAddr>,
    instance: Option<InstanceHandle>,
    instance_id: Option<InstanceId>,
    pid: Option<u32>,
    started_at: Timestamp,
    active_since: Option<Instant>,
    tracker: ConnTracker,
    established: u32,
    last_activity: Instant,
    rss_bytes: Option<u64>,
    max_conns: u32,
    php_version: Option<String>,
    replaces: Option<u64>,
    replaced_by: Option<u64>,
    recycle: Option<RecycleReason>,
    retire_seq: u64,
    drain_since: Option<Instant>,
    quiet: u32,
    task: Option<TaskGuard>,
}

impl Worker {
    fn status(&self) -> WorkerStatus {
        WorkerStatus {
            slot: self.slot,
            phase: self.phase,
            port: self.port,
            pid: self.pid,
            started_at: self.started_at,
            requests: self.tracker.requests(),
            established: self.established,
            rss_bytes: self.rss_bytes,
            php_version: self.php_version.clone(),
            replaces: self.replaces,
            replaced_by: self.replaced_by,
        }
    }
}

/// State shared by every incarnation of a pool actor.
struct Shared {
    spec: Arc<PoolSpec>,
    launch: Arc<LaunchContext>,
    events: EventBus,
    ports: Arc<dyn PortReservations>,
    table: Arc<dyn PortTable>,
    stats: Arc<dyn ProcessStats>,
    processes: Arc<dyn ProcessControl>,
    listener: Option<Arc<dyn UpstreamListener>>,
    upstream_tx: watch::Sender<UpstreamModel>,
    /// The pool's job (workers' jobs nest in it).
    jobs: JobTree,
}

/// The pool actor. Built by [`PhpPool::spawn`].
pub struct PhpPool {
    shared: Arc<Shared>,
    state: PoolState,
    since: Timestamp,
    serving: bool,
    prepared: bool,
    start_attempt: u64,
    preparing: Option<TaskGuard>,
    workers: BTreeMap<u64, Worker>,
    next_slot: u64,
    upstream: UpstreamModel,
    seq: u64,
    applied_seq: u64,
    publishing: Option<TaskGuard>,
    publish_pending: bool,
    publish_failures: u32,
    publish_retry: Option<TaskGuard>,
    sampling: Option<TaskGuard>,
    launch_failures: u32,
    next_launch_at: Option<Instant>,
    pending_start: Vec<Reply<Result<UpstreamModel>>>,
    pending_drain: Vec<Reply<Result<DrainReport>>>,
    pending_stop: Vec<Reply<Result<()>>>,
    pending_standby: Vec<Reply<Result<()>>>,
    retire_started: Option<Instant>,
    drained: u32,
    forced: u32,
    released: bool,
    retired_requests: u64,
    recycled: u64,
    last_error: Option<String>,
    /// Reason of the next upstream change.
    reason: Option<ScaleReason>,
    /// Reason of the last upstream change (sent with coalesced listener calls).
    last_reason: ScaleReason,
    me: Option<WeakActorRef<PoolMsg>>,
}

/// Hands a report to the pool; retries while its mailbox is full. `false` once the pool
/// is gone.
async fn deliver(me: &WeakActorRef<PoolMsg>, notice: Notice) -> bool {
    let mut msg = PoolMsg::Internal(Internal(Box::new(notice)));
    loop {
        let Some(actor) = me.upgrade() else {
            return false;
        };
        match actor.tell(msg) {
            Ok(()) => return true,
            Err(TellError::MailboxFull { msg: back, .. }) => {
                msg = back;
                drop(actor);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(TellError::Stopped { .. }) => return false,
        }
    }
}

/// Same error for every waiter (`ForgeError` is not `Clone`).
fn duplicate(err: &ForgeError) -> ForgeError {
    match err {
        ForgeError::NotFound(m) => ForgeError::NotFound(m.clone()),
        ForgeError::Conflict(m) => ForgeError::Conflict(m.clone()),
        ForgeError::PortInUse {
            port,
            owner,
            pid,
            exe,
        } => ForgeError::PortInUse {
            port: *port,
            owner: owner.clone(),
            pid: *pid,
            exe: exe.clone(),
        },
        ForgeError::ConfigInvalid(m) => ForgeError::ConfigInvalid(m.clone()),
        ForgeError::InvalidInput(m) => ForgeError::InvalidInput(m.clone()),
        ForgeError::Timeout { operation, after } => ForgeError::Timeout {
            operation: operation.clone(),
            after: *after,
        },
        ForgeError::Cancelled(m) => ForgeError::Cancelled(m.clone()),
        ForgeError::Internal(m) => ForgeError::Internal(m.clone()),
        ForgeError::NotImplemented(m) => ForgeError::NotImplemented(m.clone()),
        ForgeError::NeedsElevation(m) => ForgeError::NeedsElevation(m.clone()),
        ForgeError::Io { context, source } => ForgeError::io(
            context.clone(),
            std::io::Error::new(source.kind(), source.to_string()),
        ),
        other => ForgeError::Internal(other.to_string()),
    }
}

fn fail_all<T>(replies: Vec<Reply<Result<T>>>, err: &ForgeError) {
    for reply in replies {
        let _ = reply.send(Err(duplicate(err)));
    }
}

impl PhpPool {
    fn new(shared: Arc<Shared>) -> Self {
        let upstream = UpstreamModel::empty(shared.spec.upstream.clone(), shared.spec.policy);
        Self {
            shared,
            state: PoolState::Stopped,
            since: Timestamp::now(),
            serving: false,
            prepared: false,
            start_attempt: 0,
            preparing: None,
            workers: BTreeMap::new(),
            next_slot: 1,
            upstream,
            seq: 0,
            applied_seq: 0,
            publishing: None,
            publish_pending: false,
            publish_failures: 0,
            publish_retry: None,
            sampling: None,
            launch_failures: 0,
            next_launch_at: None,
            pending_start: Vec::new(),
            pending_drain: Vec::new(),
            pending_stop: Vec::new(),
            pending_standby: Vec::new(),
            retire_started: None,
            drained: 0,
            forced: 0,
            released: true,
            retired_requests: 0,
            recycled: 0,
            last_error: None,
            reason: None,
            last_reason: ScaleReason::Start,
            me: None,
        }
    }

    /// Validates `spec`, creates the pool's job (nested in the daemon's root) and private
    /// bus, and starts the actor (state `Stopped`). Actor name: `pool/<tool>/<version>`.
    pub fn spawn(spec: PoolSpec, deps: PoolDeps) -> Result<PoolHandle> {
        spec.validate()?;
        let bind: IpAddr = worker::bind_address(&spec.args)?;
        let spec = Arc::new(spec);
        let jobs = deps.supervisor.jobs().nested()?;
        let private = EventBus::with_capacity(1024);
        let scoped = deps
            .supervisor
            .clone()
            .with_jobs(jobs.clone())
            .with_actors(ActorSupervisor::new(private.clone()));
        let launch = Arc::new(LaunchContext {
            spec: Arc::clone(&spec),
            supervisor: scoped,
            ports: Arc::clone(&deps.ports),
            table: Arc::clone(deps.supervisor.port_table()),
            script: HealthScript::new(&spec.health_root),
            bind,
        });
        let (upstream_tx, upstream_rx) =
            watch::channel(UpstreamModel::empty(spec.upstream.clone(), spec.policy));
        let shared = Arc::new(Shared {
            spec: Arc::clone(&spec),
            launch,
            events: deps.supervisor.actors().events().clone(),
            ports: deps.ports,
            table: Arc::clone(deps.supervisor.port_table()),
            stats: Arc::clone(deps.supervisor.process_stats()),
            processes: Arc::clone(deps.supervisor.jobs().processes()),
            listener: deps.listener,
            upstream_tx,
            jobs,
        });
        let name = format!("pool/{}/{}", spec.tool, spec.version);
        let factory_shared = Arc::clone(&shared);
        let handle = deps
            .supervisor
            .actors()
            .spawn(ActorSpec::new(name), move || {
                PhpPool::new(Arc::clone(&factory_shared))
            });
        let weak = handle.actor_ref().weak();
        spawn_ticker(weak.clone(), spec.timing.tick);
        spawn_exit_forwarder(weak, private);
        Ok(PoolHandle {
            handle: Arc::new(handle),
            spec,
            upstream: upstream_rx,
        })
    }

    fn spec(&self) -> Arc<PoolSpec> {
        Arc::clone(&self.shared.spec)
    }

    fn label(&self) -> String {
        format!("{} {}", self.shared.spec.tool, self.shared.spec.version)
    }

    fn set_state(&mut self, state: PoolState) {
        if self.state != state {
            tracing::debug!(pool = %self.label(), from = self.state.name(), to = state.name(), "pool state");
            self.state = state;
            self.since = Timestamp::now();
        }
    }

    fn weak(&self) -> Option<WeakActorRef<PoolMsg>> {
        self.me.clone()
    }

    fn status(&self) -> PoolStatus {
        let spec = &self.shared.spec;
        PoolStatus {
            version_id: spec.version_id,
            tool: spec.tool.clone(),
            version: spec.version.clone(),
            series: spec.series.clone(),
            state: self.state.clone(),
            since: self.since,
            serving: self.serving,
            upstream: self.upstream.clone(),
            min_workers: spec.scaling.min,
            max_workers: spec.scaling.max,
            workers: self.workers.values().map(Worker::status).collect(),
            requests_total: self.retired_requests
                + self
                    .workers
                    .values()
                    .map(|w| w.tracker.requests())
                    .sum::<u64>(),
            recycled: self.recycled,
            rss_bytes: self.workers.values().filter_map(|w| w.rss_bytes).sum(),
            last_error: self.last_error.clone(),
        }
    }

    // ----- start -------------------------------------------------------------------

    fn begin_start(&mut self) {
        self.set_state(PoolState::Starting);
        self.prepared = false;
        self.released = false;
        self.launch_failures = 0;
        self.next_launch_at = None;
        self.last_error = None;
        self.retired_requests = 0;
        self.recycled = 0;
        self.reason = None;
        self.start_attempt += 1;
        let attempt = self.start_attempt;
        let Some(me) = self.weak() else { return };
        let spec = self.spec();
        let ports = Arc::clone(&self.shared.ports);
        let script = self.shared.launch.script.clone();
        self.preparing = Some(TaskGuard(tokio::spawn(async move {
            let result = async {
                spawn_blocking_timeout(
                    "writing the health script",
                    spec.timing.sample_timeout,
                    move || script.write(),
                )
                .await?;
                // Reservations left by a previous run (daemon crash) of this pool.
                let id = spec.version_id.to_string();
                tokio::time::timeout(
                    spec.timing.start_timeout,
                    ports.release_owner(OwnerKind::Pool, &id),
                )
                .await
                .map_err(|_| ForgeError::Timeout {
                    operation: "releasing stale pool ports".to_owned(),
                    after: spec.timing.start_timeout,
                })??;
                Ok(())
            }
            .await;
            deliver(&me, Notice::Prepared { attempt, result }).await;
        })));
    }

    fn fail_start(&mut self, err: ForgeError) {
        tracing::warn!(pool = %self.label(), %err, "pool failed to start");
        self.last_error = Some(err.to_string());
        fail_all(std::mem::take(&mut self.pending_start), &err);
        self.preparing = None;
        self.serving = false;
        self.set_state(PoolState::Failed {
            code: err.code().to_owned(),
            message: err.to_string(),
        });
        let slots: Vec<u64> = self.workers.keys().copied().collect();
        for slot in slots {
            self.stop_worker(slot);
        }
        self.reason = Some(ScaleReason::Stop);
    }

    // ----- workers -----------------------------------------------------------------

    fn launch_allowed(&self, now: Instant) -> bool {
        self.next_launch_at.is_none_or(|at| now >= at)
    }

    /// Starts a launch task; returns the new slot.
    fn launch(&mut self, replaces: Option<u64>) -> u64 {
        let slot = self.next_slot;
        self.next_slot += 1;
        let task = self.weak().map(|me| {
            let ctx = Arc::clone(&self.shared.launch);
            let ports = Arc::clone(&self.shared.ports);
            let owner = reservation_owner(&ctx.spec, slot);
            let stop_wait = ctx.spec.timing.stop_wait();
            TaskGuard(tokio::spawn(async move {
                let result = worker::launch(ctx, slot).await;
                let orphan = match result {
                    Ok(launched) => {
                        let instance = launched.instance.clone();
                        if deliver(
                            &me,
                            Notice::Launched {
                                slot,
                                result: Ok(launched),
                            },
                        )
                        .await
                        {
                            None
                        } else {
                            Some(instance)
                        }
                    }
                    Err(err) => {
                        deliver(
                            &me,
                            Notice::Launched {
                                slot,
                                result: Err(err),
                            },
                        )
                        .await;
                        None
                    }
                };
                // The pool is gone: undo the launch.
                if let Some(instance) = orphan {
                    worker::retire(Some(instance), ports, owner, stop_wait).await;
                }
            }))
        });
        let now = Instant::now();
        self.workers.insert(
            slot,
            Worker {
                slot,
                phase: WorkerPhase::Starting,
                port: None,
                addr: None,
                instance: None,
                instance_id: None,
                pid: None,
                started_at: Timestamp::now(),
                active_since: None,
                tracker: ConnTracker::default(),
                established: 0,
                last_activity: now,
                rss_bytes: None,
                max_conns: 1,
                php_version: None,
                replaces,
                replaced_by: None,
                recycle: None,
                retire_seq: 0,
                drain_since: None,
                quiet: 0,
                task,
            },
        );
        slot
    }

    fn on_launched(&mut self, slot: u64, result: Result<Launched>) {
        let accepting = matches!(self.state, PoolState::Starting | PoolState::Running);
        let known = self
            .workers
            .get(&slot)
            .is_some_and(|w| w.phase == WorkerPhase::Starting);
        if !known || !accepting {
            if let Ok(launched) = result {
                // Launched for a slot the pool no longer wants: stop it.
                self.retire_detached(slot, launched);
            }
            if known {
                self.workers.remove(&slot);
            }
            return;
        }
        match result {
            Ok(launched) => {
                self.launch_failures = 0;
                self.next_launch_at = None;
                let now = Instant::now();
                let replaces = {
                    let w = self.workers.get_mut(&slot).expect("checked above");
                    w.task = None;
                    w.phase = WorkerPhase::Active;
                    w.port = Some(launched.port);
                    w.addr = Some(launched.addr);
                    w.instance_id = Some(launched.instance.id());
                    w.instance = Some(launched.instance);
                    w.pid = Some(launched.pid);
                    w.active_since = Some(now);
                    w.last_activity = now;
                    w.tracker = ConnTracker::with_baseline(launched.baseline);
                    w.max_conns = launched.health.values.max_conns.unwrap_or(1).max(1);
                    w.php_version = Some(launched.health.php_version);
                    w.replaces
                };
                tracing::debug!(pool = %self.label(), slot, port = launched.port, pid = launched.pid, "worker warm");
                if let Some(old) = replaces {
                    self.replace(old, slot);
                } else if self.reason.is_none() {
                    self.reason = Some(if self.state == PoolState::Starting {
                        ScaleReason::Start
                    } else {
                        ScaleReason::ScaleUp
                    });
                }
            }
            Err(err) => {
                let replaces = self.workers.remove(&slot).and_then(|w| w.replaces);
                if let Some(old) = replaces.and_then(|old| self.workers.get_mut(&old)) {
                    old.replaced_by = None; // retried by the next recycle check
                }
                self.launch_failures += 1;
                let delay = self.shared.spec.timing.backoff(self.launch_failures);
                self.next_launch_at = Some(Instant::now() + delay);
                tracing::warn!(pool = %self.label(), slot, %err, retry_in = ?delay, "worker launch failed");
                self.last_error = Some(err.to_string());
                if self.state == PoolState::Starting
                    && self.launch_failures >= self.shared.spec.timing.start_attempts
                {
                    self.fail_start(err);
                }
            }
        }
    }

    /// `new` (warm) takes the place of `old`: `old` leaves the upstream.
    fn replace(&mut self, old: u64, new: u64) {
        let new_port = self.workers.get(&new).and_then(|w| w.port);
        let Some(o) = self.workers.get_mut(&old) else {
            return;
        };
        if o.phase != WorkerPhase::Active {
            return;
        }
        let reason = o.recycle.unwrap_or(RecycleReason::MaxRequests);
        o.phase = WorkerPhase::Retiring;
        o.retire_seq = SEQ_PENDING;
        let event = WorkerRecycledEvent::new(
            self.shared.spec.version_id,
            &self.shared.spec.series,
            &self.shared.spec.version,
            reason,
            RecycledWorker {
                port: o.port.unwrap_or_default(),
                pid: o.pid,
                requests: o.tracker.requests(),
                rss_bytes: o.rss_bytes,
                age: o.active_since.map(|t| t.elapsed()).unwrap_or_default(),
                replacement_port: new_port,
            },
        );
        tracing::debug!(pool = %self.label(), old, new, reason = reason.as_str(), "worker recycled");
        self.recycled += 1;
        publish(&self.shared.events, EventKind::PoolWorkerRecycled, &event);
        self.reason = Some(if reason == RecycleReason::Reload {
            ScaleReason::Reload
        } else {
            ScaleReason::Recycle
        });
    }

    /// Stops a launched worker the pool does not track (late launch).
    fn retire_detached(&self, slot: u64, launched: Launched) {
        let ports = Arc::clone(&self.shared.ports);
        let owner = reservation_owner(&self.shared.spec, slot);
        let wait = self.shared.spec.timing.stop_wait();
        tokio::spawn(worker::retire(Some(launched.instance), ports, owner, wait));
    }

    /// Moves a worker to `Stopping` and starts its stop task.
    fn stop_worker(&mut self, slot: u64) {
        let me = self.weak();
        let ports = Arc::clone(&self.shared.ports);
        let owner = reservation_owner(&self.shared.spec, slot);
        let wait = self.shared.spec.timing.stop_wait();
        let Some(w) = self.workers.get_mut(&slot) else {
            return;
        };
        if w.phase == WorkerPhase::Stopping {
            return;
        }
        if w.phase == WorkerPhase::Starting {
            // Abort the launch; its process dies with the dropped handle and the port is
            // released with the pool's reservations.
            self.workers.remove(&slot);
            return;
        }
        w.phase = WorkerPhase::Stopping;
        let instance = w.instance.take();
        w.task = Some(TaskGuard(tokio::spawn(async move {
            worker::retire(instance, ports, owner, wait).await;
            if let Some(me) = me {
                deliver(&me, Notice::Retired { slot }).await;
            }
        })));
    }

    fn on_retired(&mut self, slot: u64) {
        if let Some(w) = self.workers.remove(&slot) {
            self.retired_requests += w.tracker.requests();
        }
    }

    fn worker_exited(&mut self, slot: u64) {
        let Some(w) = self.workers.get(&slot) else {
            return;
        };
        match w.phase {
            WorkerPhase::Starting | WorkerPhase::Stopping => return,
            WorkerPhase::Active => {
                let (port, pid, next) = (w.port, w.pid, w.replaced_by);
                let event = WorkerRecycledEvent::new(
                    self.shared.spec.version_id,
                    &self.shared.spec.series,
                    &self.shared.spec.version,
                    RecycleReason::Crashed,
                    RecycledWorker {
                        port: port.unwrap_or_default(),
                        pid,
                        requests: w.tracker.requests(),
                        rss_bytes: w.rss_bytes,
                        age: w.active_since.map(|t| t.elapsed()).unwrap_or_default(),
                        replacement_port: None,
                    },
                );
                tracing::warn!(pool = %self.label(), slot, ?port, ?pid, "worker exited on its own");
                publish(&self.shared.events, EventKind::PoolWorkerRecycled, &event);
                self.recycled += 1;
                if let Some(n) = next.and_then(|next| self.workers.get_mut(&next)) {
                    n.replaces = None; // becomes a worker of its own
                }
                self.reason = Some(ScaleReason::WorkerExit);
            }
            WorkerPhase::Retiring | WorkerPhase::Draining => {}
        }
        self.stop_worker(slot);
    }

    // ----- policy ------------------------------------------------------------------

    fn ensure_min(&mut self, now: Instant) {
        if !matches!(self.state, PoolState::Starting | PoolState::Running) || !self.prepared {
            return;
        }
        let live = self
            .workers
            .values()
            .filter(|w| {
                w.phase == WorkerPhase::Active
                    || (w.phase == WorkerPhase::Starting && w.replaces.is_none())
            })
            .count();
        let min = self.shared.spec.scaling.min as usize;
        if live < min && self.launch_allowed(now) {
            for _ in live..min {
                self.launch(None);
            }
        }
    }

    fn check_recycle(&mut self, now: Instant) {
        let spec = self.spec();
        let recycle = spec.recycle;
        for w in self.workers.values_mut() {
            if w.phase != WorkerPhase::Active || w.replaced_by.is_some() || w.recycle.is_some() {
                continue;
            }
            let age = w.active_since.map(|t| now.saturating_duration_since(t));
            w.recycle = if recycle.max_requests > 0 && w.tracker.requests() >= recycle.max_requests
            {
                Some(RecycleReason::MaxRequests)
            } else if recycle.max_rss_mb > 0
                && w.rss_bytes
                    .is_some_and(|rss| rss >= recycle.max_rss_mb.saturating_mul(1024 * 1024))
            {
                Some(RecycleReason::MaxRss)
            } else if recycle
                .max_age
                .is_some_and(|max| age.is_some_and(|age| age >= max))
            {
                Some(RecycleReason::MaxAge)
            } else {
                None
            };
        }
        if !self.launch_allowed(now) {
            return;
        }
        let due: Vec<u64> = self
            .workers
            .values()
            .filter(|w| {
                w.phase == WorkerPhase::Active && w.recycle.is_some() && w.replaced_by.is_none()
            })
            .map(|w| w.slot)
            .collect();
        for old in due {
            let new = self.launch(Some(old));
            if let Some(w) = self.workers.get_mut(&old) {
                w.replaced_by = Some(new);
            }
        }
    }

    fn check_scaling(&mut self, now: Instant) {
        let scaling = self.shared.spec.scaling;
        let active: Vec<&Worker> = self
            .workers
            .values()
            .filter(|w| w.phase == WorkerPhase::Active)
            .collect();
        let fresh_starting = self
            .workers
            .values()
            .filter(|w| w.phase == WorkerPhase::Starting && w.replaces.is_none())
            .count();
        let count = active.len();
        if count == 0 || fresh_starting > 0 {
            return;
        }
        let busy = active
            .iter()
            .filter(|w| w.established >= w.max_conns)
            .count();
        let queued: u32 = active
            .iter()
            .map(|w| w.established.saturating_sub(w.max_conns))
            .sum();
        let max = scaling.max as usize;
        let min = scaling.min as usize;
        if busy == count && count < max {
            if self.launch_allowed(now) {
                let add = (max - count).min((queued.max(1)) as usize);
                tracing::debug!(pool = %self.label(), busy, queued, add, "scaling up");
                for _ in 0..add {
                    self.launch(None);
                }
            }
            return;
        }
        if count > min {
            let mut idle: Vec<(Instant, u64)> = active
                .iter()
                .filter(|w| {
                    w.replaced_by.is_none()
                        && w.recycle.is_none()
                        && w.established == 0
                        && now.saturating_duration_since(w.last_activity) >= scaling.idle_timeout
                })
                .map(|w| (w.last_activity, w.slot))
                .collect();
            // Longest idle first; among equals, the newest worker.
            idle.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
            let retire: Vec<u64> = idle
                .into_iter()
                .take(count - min)
                .map(|(_, slot)| slot)
                .collect();
            if !retire.is_empty() {
                tracing::debug!(pool = %self.label(), ?retire, "scaling down");
                self.reason = Some(ScaleReason::ScaleDown);
            }
            for slot in retire {
                if let Some(w) = self.workers.get_mut(&slot) {
                    w.phase = WorkerPhase::Retiring;
                    w.retire_seq = SEQ_PENDING;
                }
            }
        }
    }

    // ----- upstream ----------------------------------------------------------------

    fn update_upstream(&mut self) {
        let backends: Vec<SocketAddr> = self
            .workers
            .values()
            .filter(|w| w.phase == WorkerPhase::Active)
            .filter_map(|w| w.addr)
            .collect();
        if backends == self.upstream.backends {
            for w in self.workers.values_mut() {
                if w.retire_seq == SEQ_PENDING {
                    w.retire_seq = self.seq;
                }
            }
            return;
        }
        let from = self.upstream.backends.len();
        self.upstream =
            UpstreamModel::new(self.upstream.name.clone(), backends, self.upstream.policy);
        self.seq += 1;
        for w in self.workers.values_mut() {
            if w.retire_seq == SEQ_PENDING {
                w.retire_seq = self.seq;
            }
        }
        let reason = self.reason.take().unwrap_or(ScaleReason::ScaleUp);
        self.last_reason = reason;
        self.shared.upstream_tx.send_replace(self.upstream.clone());
        let spec = &self.shared.spec;
        let event = PoolScaledEvent::new(
            spec.version_id,
            &spec.series,
            &spec.version,
            from,
            &self.upstream,
            reason,
        );
        publish(&self.shared.events, EventKind::PoolScaled, &event);
        tracing::debug!(pool = %self.label(), seq = self.seq, backends = ?self.upstream.backends, reason = reason.as_str(), "upstream changed");
        self.kick_publish();
    }

    /// Hands the current model to the listener (one call in flight at a time).
    fn kick_publish(&mut self) {
        let reason = self.last_reason;
        let listener = match &self.shared.listener {
            Some(listener) if self.serving && !self.upstream.is_empty() => Arc::clone(listener),
            _ => {
                self.applied_seq = self.seq;
                return;
            }
        };
        if self.publishing.is_some() {
            self.publish_pending = true;
            return;
        }
        if self.publish_retry.is_some() {
            return; // the scheduled retry publishes the latest model
        }
        let Some(me) = self.weak() else { return };
        let seq = self.seq;
        let change = UpstreamChange::Scaled {
            model: self.upstream.clone(),
            reason,
        };
        let timeout = self.shared.spec.timing.listener_timeout;
        self.publishing = Some(TaskGuard(tokio::spawn(async move {
            let result =
                match tokio::time::timeout(timeout, listener.upstream_changed(change)).await {
                    Ok(result) => result,
                    Err(_) => Err(ForgeError::Timeout {
                        operation: "applying an upstream change".to_owned(),
                        after: timeout,
                    }),
                };
            deliver(&me, Notice::Applied { seq, result }).await;
        })));
    }

    fn on_applied(&mut self, seq: u64, result: Result<()>) {
        self.publishing = None;
        for reply in std::mem::take(&mut self.pending_standby) {
            let _ = reply.send(Ok(()));
        }
        if !self.serving {
            return;
        }
        match result {
            Ok(()) => {
                self.applied_seq = self.applied_seq.max(seq);
                self.publish_failures = 0;
                if self.publish_pending || seq < self.seq {
                    self.publish_pending = false;
                    self.kick_publish();
                }
            }
            Err(err) => {
                self.publish_failures += 1;
                let delay = self.shared.spec.timing.backoff(self.publish_failures);
                tracing::warn!(pool = %self.label(), %err, retry_in = ?delay, "upstream change not applied");
                self.last_error = Some(format!("upstream change not applied: {err}"));
                self.publish_pending = false;
                if let Some(me) = self.weak() {
                    self.publish_retry = Some(TaskGuard(tokio::spawn(async move {
                        tokio::time::sleep(delay).await;
                        deliver(&me, Notice::RetryPublish).await;
                    })));
                }
            }
        }
    }

    // ----- retire / drain ------------------------------------------------------------

    fn begin_retire(&mut self, stop: bool) {
        self.serving = false;
        self.publish_retry = None;
        self.publish_pending = false;
        self.preparing = None;
        fail_all(
            std::mem::take(&mut self.pending_start),
            &ForgeError::Cancelled(format!("pool {} is being drained or stopped", self.label())),
        );
        if self.retire_started.is_none() {
            self.retire_started = Some(Instant::now());
            self.drained = 0;
            self.forced = 0;
        }
        let now = Instant::now();
        let slots: Vec<u64> = self.workers.keys().copied().collect();
        for slot in slots {
            let phase = self.workers.get(&slot).map(|w| w.phase);
            match phase {
                Some(WorkerPhase::Starting) => {
                    self.workers.remove(&slot);
                }
                Some(WorkerPhase::Active | WorkerPhase::Retiring) if !stop => {
                    if let Some(w) = self.workers.get_mut(&slot) {
                        w.phase = WorkerPhase::Draining;
                        w.drain_since = Some(now);
                        w.quiet = 0;
                    }
                }
                Some(WorkerPhase::Stopping) | None => {}
                Some(_) if stop => {
                    self.drained += 1;
                    self.stop_worker(slot);
                }
                Some(_) => {}
            }
        }
        self.reason = Some(if stop {
            ScaleReason::Stop
        } else {
            ScaleReason::Drain
        });
        self.set_state(if stop {
            PoolState::Stopping
        } else {
            PoolState::Draining
        });
    }

    fn progress_retiring(&mut self, now: Instant) {
        for w in self.workers.values_mut() {
            if w.phase == WorkerPhase::Retiring && w.retire_seq <= self.applied_seq {
                w.phase = WorkerPhase::Draining;
                w.drain_since = Some(now);
                w.quiet = 0;
            }
        }
    }

    fn progress_draining(&mut self, now: Instant) {
        let timing = self.shared.spec.timing;
        let done: Vec<(u64, bool)> = self
            .workers
            .values()
            .filter(|w| w.phase == WorkerPhase::Draining)
            .filter_map(|w| {
                let since = w.drain_since?;
                let quiet = w.quiet >= timing.drain_quiet;
                let expired = now.saturating_duration_since(since) >= timing.drain_timeout;
                (quiet || expired).then_some((w.slot, !quiet))
            })
            .collect();
        for (slot, forced) in done {
            if forced {
                let established = self.workers.get(&slot).map_or(0, |w| w.established);
                tracing::warn!(pool = %self.label(), slot, established, "drain timed out; stopping the worker with open connections");
                self.forced += 1;
            }
            self.drained += 1;
            self.stop_worker(slot);
        }
    }

    fn finish_transitions(&mut self) {
        match self.state {
            PoolState::Starting => {
                let active = self
                    .workers
                    .values()
                    .filter(|w| w.phase == WorkerPhase::Active)
                    .count();
                if self.prepared && active >= self.shared.spec.scaling.min as usize {
                    self.set_state(PoolState::Running);
                    for reply in std::mem::take(&mut self.pending_start) {
                        let _ = reply.send(Ok(self.upstream.clone()));
                    }
                }
            }
            PoolState::Draining | PoolState::Stopping if self.workers.is_empty() => {
                let report = DrainReport {
                    workers: self.drained,
                    forced: self.forced,
                    elapsed_ms: self
                        .retire_started
                        .map(|t| u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX))
                        .unwrap_or_default(),
                };
                self.retire_started = None;
                self.set_state(PoolState::Stopped);
                self.release_ports();
                for reply in std::mem::take(&mut self.pending_drain) {
                    let _ = reply.send(Ok(report));
                }
                for reply in std::mem::take(&mut self.pending_stop) {
                    let _ = reply.send(Ok(()));
                }
            }
            PoolState::Failed { .. } if self.workers.is_empty() => {
                self.release_ports();
                for reply in std::mem::take(&mut self.pending_drain) {
                    let _ = reply.send(Ok(DrainReport::default()));
                }
                for reply in std::mem::take(&mut self.pending_stop) {
                    let _ = reply.send(Ok(()));
                }
            }
            _ => {}
        }
    }

    /// Frees every reservation of the pool once (aborted launches included).
    fn release_ports(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let ports = Arc::clone(&self.shared.ports);
        let id = self.shared.spec.version_id.to_string();
        let me = self.weak();
        tokio::spawn(async move {
            match tokio::time::timeout(
                Duration::from_secs(10),
                ports.release_owner(OwnerKind::Pool, &id),
            )
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(err)) => tracing::warn!(%err, pool = %id, "releasing pool ports failed"),
                Err(_) => tracing::warn!(pool = %id, "releasing pool ports timed out"),
            }
            if let Some(me) = me {
                deliver(&me, Notice::Released).await;
            }
        });
    }

    /// Runs after every message: launches to keep `min`, upstream changes, drains and
    /// state transitions.
    fn drive(&mut self) {
        let now = Instant::now();
        self.ensure_min(now);
        self.update_upstream();
        self.progress_retiring(now);
        self.finish_transitions();
    }

    // ----- sampling ----------------------------------------------------------------

    fn start_sample(&mut self) {
        if self.sampling.is_some() {
            return;
        }
        let targets: Vec<(u16, u32)> = self
            .workers
            .values()
            .filter(|w| {
                matches!(
                    w.phase,
                    WorkerPhase::Active | WorkerPhase::Retiring | WorkerPhase::Draining
                )
            })
            .filter_map(|w| Some((w.port?, w.pid?)))
            .collect();
        if targets.is_empty() {
            return;
        }
        let Some(me) = self.weak() else { return };
        let table = Arc::clone(&self.shared.table);
        let stats = Arc::clone(&self.shared.stats);
        let processes = Arc::clone(&self.shared.processes);
        let timeout = self.shared.spec.timing.sample_timeout;
        self.sampling = Some(TaskGuard(tokio::spawn(async move {
            let result = spawn_blocking_timeout("sampling the pool workers", timeout, move || {
                let ports: Vec<u16> = targets.iter().map(|(port, _)| *port).collect();
                let (conns, table_error, established) = match table.connections(&ports) {
                    Ok(conns) => (Some(conns), None, HashMap::new()),
                    Err(err) => {
                        let mut counts = HashMap::new();
                        for port in &ports {
                            counts.insert(*port, table.established_count(*port).unwrap_or(0));
                        }
                        (None, Some(err.to_string()), counts)
                    }
                };
                let mut procs = HashMap::new();
                for (_, pid) in &targets {
                    let alive = processes.is_alive(*pid).unwrap_or(true);
                    let rss = stats.sample(*pid).ok().map(|s| s.rss_bytes);
                    procs.insert(*pid, (alive, rss));
                }
                Ok(Sample {
                    conns,
                    table_error,
                    established,
                    procs,
                })
            })
            .await;
            deliver(&me, Notice::Sampled { result }).await;
        })));
    }

    fn on_sample(&mut self, sample: Sample) {
        let now = Instant::now();
        if let Some(err) = &sample.table_error {
            let message = format!("TCP connections unavailable, requests not counted: {err}");
            if self.last_error.as_deref() != Some(message.as_str()) {
                tracing::warn!(pool = %self.label(), %err, "TCP connections unavailable");
                self.last_error = Some(message);
            }
        }
        let grace = self.shared.spec.timing.drain_grace;
        let mut exited = Vec::new();
        for w in self.workers.values_mut() {
            if !matches!(
                w.phase,
                WorkerPhase::Active | WorkerPhase::Retiring | WorkerPhase::Draining
            ) {
                continue;
            }
            let (Some(addr), Some(port), Some(pid)) = (w.addr, w.port, w.pid) else {
                continue;
            };
            let (established, new) = match &sample.conns {
                Some(conns) => {
                    let (established, keys) = connection_keys(conns, addr);
                    (established, w.tracker.observe(keys))
                }
                None => (sample.established.get(&port).copied().unwrap_or(0), 0),
            };
            w.established = established;
            if established > 0 || new > 0 {
                w.last_activity = now;
            }
            if let Some((alive, rss)) = sample.procs.get(&pid) {
                if rss.is_some() {
                    w.rss_bytes = *rss;
                }
                if !alive {
                    exited.push(w.slot);
                }
            }
            if w.phase == WorkerPhase::Draining
                && w.drain_since.is_some_and(|since| now >= since + grace)
            {
                if established == 0 && new == 0 {
                    w.quiet += 1;
                } else {
                    w.quiet = 0;
                }
            }
        }
        for slot in exited {
            self.worker_exited(slot);
        }
        if self.state == PoolState::Running {
            // On standby (a swap in progress) the pool neither recycles nor scales: the web
            // server may still route to every worker until the swap is applied, and new
            // workers would get no traffic.
            if self.serving {
                self.check_recycle(now);
                self.check_scaling(now);
            }
        }
        self.progress_draining(now);
    }

    fn on_notice(&mut self, notice: Notice) {
        match notice {
            Notice::Tick => {
                if matches!(
                    self.state,
                    PoolState::Starting
                        | PoolState::Running
                        | PoolState::Draining
                        | PoolState::Stopping
                ) {
                    self.start_sample();
                }
            }
            Notice::Prepared { attempt, result } => {
                if attempt != self.start_attempt || self.state != PoolState::Starting {
                    return;
                }
                self.preparing = None;
                match result {
                    Ok(()) => self.prepared = true,
                    Err(err) => self.fail_start(err),
                }
            }
            Notice::Launched { slot, result } => self.on_launched(slot, result),
            Notice::Sampled { result } => {
                self.sampling = None;
                match result {
                    Ok(sample) => self.on_sample(sample),
                    Err(err) => tracing::debug!(pool = %self.label(), %err, "pool sample failed"),
                }
            }
            Notice::Applied { seq, result } => self.on_applied(seq, result),
            Notice::RetryPublish => {
                self.publish_retry = None;
                if self.serving {
                    self.kick_publish();
                }
            }
            Notice::Retired { slot } => self.on_retired(slot),
            Notice::WorkerExited { instance_id } => {
                let slot = self
                    .workers
                    .values()
                    .find(|w| w.instance_id == Some(instance_id))
                    .map(|w| w.slot);
                if let Some(slot) = slot {
                    self.worker_exited(slot);
                }
            }
            Notice::Released => {}
        }
    }

    /// Kills whatever is left in the pool's job (blocking call off the actor).
    async fn terminate_job(&self) {
        let jobs = self.shared.jobs.clone();
        let result = spawn_blocking_timeout(
            "terminating the pool job",
            Duration::from_secs(10),
            move || jobs.terminate_all(),
        )
        .await;
        if let Err(err) = result {
            tracing::warn!(pool = %self.label(), %err, "terminating the pool job failed");
        }
    }
}

impl Actor for PhpPool {
    type Msg = PoolMsg;

    async fn on_start(&mut self, ctx: &mut Context<Self>) {
        self.me = Some(ctx.myself_weak());
    }

    async fn on_restart(&mut self, ctx: &mut Context<Self>, info: &RestartInfo) {
        self.me = Some(ctx.myself_weak());
        // The previous incarnation's workers are gone with its state; nothing of them
        // may keep running behind the pool's back.
        self.terminate_job().await;
        self.released = false;
        self.release_ports();
        self.state = PoolState::Failed {
            code: "internal".to_owned(),
            message: format!("pool actor restarted after a panic: {}", info.reason),
        };
        self.shared.upstream_tx.send_replace(UpstreamModel::empty(
            self.upstream.name.clone(),
            self.upstream.policy,
        ));
    }

    async fn on_stop(&mut self, _ctx: &mut Context<Self>) {
        for w in self.workers.values_mut() {
            w.task = None;
            if let Some(instance) = w.instance.take() {
                let _ = tokio::time::timeout(Duration::from_secs(2), instance.shutdown()).await;
            }
        }
        self.terminate_job().await;
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            self.shared
                .ports
                .release_owner(OwnerKind::Pool, &self.shared.spec.version_id.to_string()),
        )
        .await;
    }

    async fn handle(&mut self, msg: PoolMsg, _ctx: &mut Context<Self>) {
        match msg {
            PoolMsg::Start { serve, reply } => match self.state {
                PoolState::Running => {
                    if serve && !self.serving {
                        self.serving = true;
                        self.last_reason = ScaleReason::Start;
                        self.kick_publish();
                    }
                    let _ = reply.send(Ok(self.upstream.clone()));
                }
                PoolState::Starting => {
                    self.serving |= serve;
                    self.pending_start.push(reply);
                }
                PoolState::Failed { .. } if !self.workers.is_empty() => {
                    let _ = reply.send(Err(ForgeError::Conflict(format!(
                        "pool {} is still stopping the workers of its failed start",
                        self.label()
                    ))));
                }
                PoolState::Stopped | PoolState::Failed { .. } => {
                    self.serving = serve;
                    self.pending_start.push(reply);
                    self.begin_start();
                }
                PoolState::Draining | PoolState::Stopping => {
                    let _ = reply.send(Err(ForgeError::Conflict(format!(
                        "pool {} is {}",
                        self.label(),
                        self.state.name()
                    ))));
                }
            },
            PoolMsg::Promote { seen, reply } => {
                if matches!(self.state, PoolState::Starting | PoolState::Running) {
                    self.serving = true;
                    if self.upstream != seen {
                        self.last_reason = ScaleReason::Start;
                        self.kick_publish();
                    } else {
                        self.applied_seq = self.seq;
                    }
                    let _ = reply.send(Ok(()));
                } else {
                    let _ = reply.send(Err(ForgeError::Conflict(format!(
                        "pool {} is {}",
                        self.label(),
                        self.state.name()
                    ))));
                }
            }
            PoolMsg::Standby(reply) => {
                self.serving = false;
                self.publish_retry = None;
                self.publish_pending = false;
                if self.publishing.is_some() {
                    self.pending_standby.push(reply);
                } else {
                    let _ = reply.send(Ok(()));
                }
            }
            PoolMsg::Status(reply) => {
                let _ = reply.send(self.status());
            }
            PoolMsg::Upstream(reply) => {
                let _ = reply.send(self.upstream.clone());
            }
            PoolMsg::Reload(reply) => {
                if self.state == PoolState::Running {
                    let mut count = 0;
                    for w in self.workers.values_mut() {
                        if w.phase == WorkerPhase::Active && w.replaced_by.is_none() {
                            w.recycle = Some(RecycleReason::Reload);
                            count += 1;
                        }
                    }
                    self.next_launch_at = None;
                    self.check_recycle(Instant::now());
                    let _ = reply.send(Ok(count));
                } else {
                    let _ = reply.send(Err(ForgeError::Conflict(format!(
                        "pool {} is {}",
                        self.label(),
                        self.state.name()
                    ))));
                }
            }
            PoolMsg::Drain(reply) => match self.state {
                PoolState::Stopped | PoolState::Failed { .. } => {
                    let _ = reply.send(Ok(DrainReport::default()));
                }
                PoolState::Draining | PoolState::Stopping => self.pending_drain.push(reply),
                PoolState::Starting | PoolState::Running => {
                    self.pending_drain.push(reply);
                    self.begin_retire(false);
                }
            },
            PoolMsg::Stop(reply) => match self.state {
                PoolState::Stopped => {
                    let _ = reply.send(Ok(()));
                }
                PoolState::Failed { .. } if self.workers.is_empty() => {
                    let _ = reply.send(Ok(()));
                }
                _ => {
                    self.pending_stop.push(reply);
                    if !matches!(self.state, PoolState::Failed { .. }) {
                        self.begin_retire(true);
                    }
                }
            },
            PoolMsg::Internal(Internal(notice)) => self.on_notice(*notice),
        }
        self.drive();
    }
}

/// Sends `Tick` every `tick` while the pool lives.
fn spawn_ticker(me: WeakActorRef<PoolMsg>, tick: Duration) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tick);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let Some(actor) = me.upgrade() else { break };
            match actor.tell(PoolMsg::Internal(Internal(Box::new(Notice::Tick)))) {
                Ok(()) | Err(TellError::MailboxFull { .. }) => {}
                Err(TellError::Stopped { .. }) => break,
            }
        }
    });
}

/// Forwards the exits of the workers (`service.crashed|failed|stopped` on the private
/// bus) to the pool.
fn spawn_exit_forwarder(me: WeakActorRef<PoolMsg>, private: EventBus) {
    let mut events = private.subscribe();
    drop(private);
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if !matches!(
                event.kind,
                EventKind::ServiceCrashed | EventKind::ServiceFailed | EventKind::ServiceStopped
            ) {
                continue;
            }
            let Some(instance_id) = event.payload["instance_id"]
                .as_str()
                .and_then(|id| id.parse::<InstanceId>().ok())
            else {
                continue;
            };
            if !deliver(&me, Notice::WorkerExited { instance_id }).await {
                break;
            }
        }
    });
}

/// Address of a pool actor plus typed helpers. Cheap to clone; the pool (and its
/// workers) stop when the last clone is dropped.
#[derive(Clone)]
pub struct PoolHandle {
    handle: Arc<ActorHandle<PoolMsg>>,
    spec: Arc<PoolSpec>,
    upstream: watch::Receiver<UpstreamModel>,
}

impl std::fmt::Debug for PoolHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoolHandle")
            .field("version", &self.spec.version)
            .field("upstream", &self.spec.upstream)
            .finish_non_exhaustive()
    }
}

impl PoolHandle {
    pub fn version_id(&self) -> VersionId {
        self.spec.version_id
    }

    pub fn spec(&self) -> &PoolSpec {
        &self.spec
    }

    /// `php_8_3`.
    pub fn upstream_name(&self) -> &str {
        &self.spec.upstream
    }

    /// Every upstream change, latest first readable (in-process renderers).
    pub fn subscribe_upstream(&self) -> watch::Receiver<UpstreamModel> {
        self.upstream.clone()
    }

    async fn ask<R>(&self, make: impl FnOnce(Reply<R>) -> PoolMsg, timeout: Duration) -> Result<R> {
        Ok(self.handle.actor_ref().ask(make, timeout).await?)
    }

    /// Starts serving (listener called on every change); waits at most `timeout` for
    /// `min` warm workers.
    pub async fn start(&self, timeout: Duration) -> Result<UpstreamModel> {
        self.ask(|reply| PoolMsg::Start { serve: true, reply }, timeout)
            .await?
    }

    /// Starts on standby: warm workers, no listener calls until [`PoolHandle::promote`].
    pub async fn start_standby(&self, timeout: Duration) -> Result<UpstreamModel> {
        self.ask(
            |reply| PoolMsg::Start {
                serve: false,
                reply,
            },
            timeout,
        )
        .await?
    }

    /// Leaves standby; `seen` is the model already applied by the caller.
    pub async fn promote(&self, seen: UpstreamModel, timeout: Duration) -> Result<()> {
        self.ask(|reply| PoolMsg::Promote { seen, reply }, timeout)
            .await?
    }

    /// Goes on standby; answered once no listener call of the pool is in flight (give
    /// `timeout` more than `timing.listener_timeout`).
    pub async fn standby(&self, timeout: Duration) -> Result<()> {
        self.ask(PoolMsg::Standby, timeout).await?
    }

    pub async fn status(&self, timeout: Duration) -> Result<PoolStatus> {
        self.ask(PoolMsg::Status, timeout).await
    }

    pub async fn upstream(&self, timeout: Duration) -> Result<UpstreamModel> {
        self.ask(PoolMsg::Upstream, timeout).await
    }

    /// Rolling replacement of every worker (`php.ini` changed); returns how many.
    pub async fn reload(&self, timeout: Duration) -> Result<u32> {
        self.ask(PoolMsg::Reload, timeout).await?
    }

    /// Takes the pool out of the web server and stops it once drained; waits at most
    /// `timeout` (give it more than `timing.drain_timeout`).
    pub async fn drain(&self, timeout: Duration) -> Result<DrainReport> {
        self.ask(PoolMsg::Drain, timeout).await?
    }

    /// Stops every worker now; waits at most `timeout`.
    pub async fn stop(&self, timeout: Duration) -> Result<()> {
        self.ask(PoolMsg::Stop, timeout).await?
    }

    /// Polls the status until `pred` holds, at most `timeout`.
    pub async fn wait_for(
        &self,
        mut pred: impl FnMut(&PoolStatus) -> bool,
        timeout: Duration,
    ) -> Result<PoolStatus> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let status = self.status(POOL_QUICK_TIMEOUT).await?;
            if pred(&status) {
                return Ok(status);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(ForgeError::Timeout {
                    operation: format!(
                        "waiting for pool {} {} (state {}, {} workers)",
                        self.spec.tool,
                        self.spec.version,
                        status.state.name(),
                        status.workers.len()
                    ),
                    after: timeout,
                });
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Stops the actor itself: workers die with their jobs, ports are released.
    pub async fn shutdown(&self) -> StopOutcome {
        self.handle.stop().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> PoolSpec {
        PoolSpec::new(
            VersionId::new(),
            "8.3.36",
            "8.3",
            "C:/F/bin/php/8.3.36/php-cgi.exe",
            "C:/F/etc/php/8.3",
        )
        .unwrap()
    }

    #[test]
    fn spec_defaults_follow_the_manifest() {
        let spec = spec();
        assert_eq!(spec.upstream, "php_8_3");
        assert_eq!(spec.args, ["-b", "127.0.0.1:{{ port }}"]);
        assert_eq!(spec.cwd, PathBuf::from("C:/F/bin/php/8.3.36"));
        assert_eq!(spec.path, vec![PathBuf::from("C:/F/bin/php/8.3.36")]);
        assert_eq!(spec.port_range, "php-pools");
        assert_eq!(
            (
                spec.scaling.min,
                spec.scaling.max,
                spec.scaling.idle_timeout
            ),
            (2, 12, Duration::from_secs(120))
        );
        assert_eq!(spec.recycle.max_requests, 500);
        assert_eq!(spec.timing.drain_timeout, Duration::from_secs(30));
        spec.validate().unwrap();
    }

    #[test]
    fn invalid_specs_are_refused() {
        let mut s = spec();
        s.scaling.min = 0;
        assert!(s.validate().is_err());
        let mut s = spec();
        s.scaling.max = 1;
        assert!(s.validate().is_err());
        let mut s = spec();
        s.args = vec!["-b".into(), "19000".into()];
        assert!(s.validate().is_err());
        let mut s = spec();
        s.exe = PathBuf::from("php-cgi.exe");
        assert!(s.validate().is_err());
        let mut s = spec();
        s.timing.tick = Duration::from_secs(30);
        assert!(s.validate().is_err(), "TIME_WAIT would be missed");
        let mut s = spec();
        s.upstream = "php 8.3".into();
        assert!(s.validate().is_err());
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let timing = PoolTiming::default();
        assert_eq!(timing.backoff(1), Duration::from_secs(1));
        assert_eq!(timing.backoff(2), Duration::from_secs(2));
        assert_eq!(timing.backoff(5), Duration::from_secs(16));
        assert_eq!(timing.backoff(6), Duration::from_secs(30));
        assert_eq!(timing.backoff(60), Duration::from_secs(30));
    }

    #[test]
    fn states_have_wire_names() {
        let failed = PoolState::Failed {
            code: "timeout".into(),
            message: "x".into(),
        };
        let json = serde_json::to_value(&failed).unwrap();
        assert_eq!(json["state"], "failed");
        assert_eq!(json["code"], "timeout");
        assert_eq!(PoolState::Draining.name(), "draining");
    }
}
