//! Download engine (docs/05 §2): a priority queue with bounded parallelism, resumable
//! transfers, mirrors, optional bandwidth limit and progress through the job system.
//!
//! * [`DownloadManager`] is the public handle. [`DownloadManager::download`] validates
//!   the request, answers with a [`JobId`] at once and the transfer proceeds as a job
//!   (`job.started|progress|done|failed|cancelled`, docs/03 §7.4). `job.done` carries a
//!   [`DownloadOutcome`] (`path`, `size`, computed `sha256`, `source` and the failures of
//!   mirrors tried before).
//! * The queue is an actor (`forge_core::actor`): its mailbox serializes enqueueing and
//!   slot accounting. At most [`DownloadConfig::max_parallel_downloads`] jobs hold a slot;
//!   the others wait for a grant, [`Priority::UserAction`] before
//!   [`Priority::AutoUpdate`], FIFO within a priority. A job cancelled while queued never
//!   takes a slot. If the actor is restarted after a panic its queue is lost: waiting
//!   jobs fail with `internal` and the caller enqueues again (their `.part` files stay).
//! * Per file: `Range` resume from `tmp/downloads/<key>.part` (`key` = first 16 hex
//!   digits of the expected sha256, or of the sha256 of the first URL); files at or above
//!   [`DownloadConfig::multi_connection_threshold`] (50 MB) use up to
//!   [`DownloadConfig::max_connections`] (4) segments written side by side into the
//!   pre-allocated `.part`, with their positions in `<key>.part.json`; mirrors are tried
//!   in order and each failure is recorded with its reason; transient errors (dropped
//!   connections, timeouts, 408/429/502/503/504) resume the same mirror up to
//!   [`DownloadConfig::max_retries`] times.
//! * The sha256 is computed while streaming (and re-hashed from the `.part` on resume,
//!   or at the end of a segmented transfer), always in `spawn_blocking`, in 1 MiB
//!   buffers. A resumed file that fails verification is discarded and fetched again once
//!   before the mirror is blamed.
//! * Cancellation (`JobRegistry::cancel`) keeps the `.part` and emits no `job.done`.
//! * Network: reqwest with rustls; the system proxy is used unless
//!   [`DownloadConfig::proxy`] says otherwise; [`DownloadManager::set_offline`] makes
//!   `download` fail at once.

mod limiter;
mod messages;
mod progress;
mod transfer;

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::time::Duration;

use forge_core::actor::{Actor, ActorHandle, ActorRef, ActorSpec, ActorSupervisor, Context, Reply};
use forge_core::error::{ForgeError, Result};
use forge_core::jobs::{JobContext, JobId, JobRegistry};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use url::Url;

use self::limiter::RateLimiter;
use self::progress::{Progress, spawn_reporter};
use self::transfer::{Env, Transfer, part_key};

/// Timeout of the enqueue request to the queue actor (ADR-0011: every message has one).
const ENQUEUE_TIMEOUT: Duration = Duration::from_secs(5);
const MAILBOX_CAPACITY: usize = 256;

/// Who asked for the download. Higher goes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Background update check/download.
    AutoUpdate,
    /// The user clicked or typed something.
    UserAction,
}

/// How the HTTP client reaches the network (`prefs.proxy`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProxySetting {
    /// Operating system settings (WinHTTP/IE registry on Windows, environment).
    #[default]
    System,
    /// No proxy at all.
    Direct,
    /// One proxy for http and https.
    Manual(Url),
}

/// Limits and timeouts of a [`DownloadManager`]. [`DownloadConfig::new`] gives the
/// documented defaults.
#[derive(Debug, Clone)]
pub struct DownloadConfig {
    /// `tmp/downloads/` ([`forge_core::paths`]): where the `.part` files live.
    pub tmp_dir: PathBuf,
    /// Downloads in flight at once.
    pub max_parallel_downloads: usize,
    /// Range connections per file.
    pub max_connections: usize,
    /// Files at least this big use several connections.
    pub multi_connection_threshold: u64,
    /// Bytes per second shared by all transfers; `None` = unlimited.
    pub bandwidth_limit: Option<u64>,
    pub proxy: ProxySetting,
    /// Starts in offline mode (`prefs.offline`).
    pub offline: bool,
    /// Interval between `job.progress` reports.
    pub progress_interval: Duration,
    /// Size of the write/hash buffers.
    pub buffer_size: usize,
    pub connect_timeout: Duration,
    /// Time to receive response headers.
    pub request_timeout: Duration,
    /// Longest silence tolerated while streaming a body.
    pub read_timeout: Duration,
    /// Resumes per mirror after transient failures.
    pub max_retries: u32,
    /// Wait before the n-th retry is `retry_backoff * n`.
    pub retry_backoff: Duration,
    pub user_agent: String,
}

impl DownloadConfig {
    /// Defaults of docs/05 §2 with partial files under `tmp_dir`.
    pub fn new(tmp_dir: impl Into<PathBuf>) -> Self {
        Self {
            tmp_dir: tmp_dir.into(),
            max_parallel_downloads: 2,
            max_connections: 4,
            multi_connection_threshold: 50 * 1024 * 1024,
            bandwidth_limit: None,
            proxy: ProxySetting::System,
            offline: false,
            progress_interval: Duration::from_millis(250),
            buffer_size: 1024 * 1024,
            connect_timeout: Duration::from_secs(15),
            request_timeout: Duration::from_secs(30),
            read_timeout: Duration::from_secs(30),
            max_retries: 3,
            retry_backoff: Duration::from_secs(1),
            user_agent: format!("Forgebox/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// What to download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRequest {
    /// Mirrors, in order of preference (at least one, http or https).
    pub urls: Vec<Url>,
    pub expected_size: Option<u64>,
    /// 64 hex digits. Providers always set it (docs/05 §3); `None` is for tests and for
    /// sources that publish no checksum, and is logged.
    pub expected_sha256: Option<String>,
    /// Final path; its directory is created, an existing file is replaced.
    pub dest: PathBuf,
    pub priority: Priority,
}

impl DownloadRequest {
    /// Request for `urls` with the priority of a user action and nothing to verify yet.
    pub fn new(urls: Vec<Url>, dest: impl Into<PathBuf>) -> Self {
        Self {
            urls,
            expected_size: None,
            expected_sha256: None,
            dest: dest.into(),
            priority: Priority::UserAction,
        }
    }

    pub fn expected_size(mut self, size: u64) -> Self {
        self.expected_size = Some(size);
        self
    }

    pub fn expected_sha256(mut self, sha256: impl Into<String>) -> Self {
        self.expected_sha256 = Some(sha256.into());
        self
    }

    pub fn priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    fn validate(&self) -> Result<()> {
        if self.urls.is_empty() {
            return Err(ForgeError::InvalidInput(
                "download needs at least one URL".into(),
            ));
        }
        if let Some(url) = self
            .urls
            .iter()
            .find(|u| !matches!(u.scheme(), "http" | "https") || u.host_str().is_none())
        {
            return Err(ForgeError::InvalidInput(format!(
                "unsupported download URL: {url}"
            )));
        }
        if let Some(sha) = &self.expected_sha256 {
            if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(ForgeError::InvalidInput(format!(
                    "expected_sha256 must be 64 hex digits, got {sha:?}"
                )));
            }
        } else {
            tracing::warn!(dest = %self.dest.display(), "download without expected sha256");
        }
        if self.dest.file_name().is_none() {
            return Err(ForgeError::InvalidInput(format!(
                "download destination has no file name: {}",
                self.dest.display()
            )));
        }
        Ok(())
    }
}

/// Why a mirror was given up on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// HTTP status, connection or timeout problem.
    Network,
    /// The bytes did not match the expected size or sha256.
    Verification,
}

/// A mirror that failed, with the reason shown in the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorFailure {
    pub url: String,
    pub reason: String,
    pub kind: FailureKind,
}

/// Payload (`result`) of `job.done` for a download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadOutcome {
    pub path: PathBuf,
    pub size: u64,
    /// sha256 (lowercase hex) computed over the downloaded bytes.
    pub sha256: String,
    /// URL that served the file.
    pub source: String,
    /// Mirrors tried before `source`, with their failure reasons.
    #[serde(default)]
    pub mirror_failures: Vec<MirrorFailure>,
}

/// Queue occupancy, for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueStats {
    pub running: usize,
    pub queued: usize,
}

/// Public handle of the download engine. Cheap to clone.
#[derive(Clone)]
pub struct DownloadManager {
    actor: ActorRef<Msg>,
    // Keeps the actor supervised for as long as a handle lives.
    handle: Arc<ActorHandle<Msg>>,
    jobs: JobRegistry,
    limiter: Arc<RateLimiter>,
    offline: Arc<AtomicBool>,
}

impl std::fmt::Debug for DownloadManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadManager")
            .field("offline", &self.is_offline())
            .finish_non_exhaustive()
    }
}

impl DownloadManager {
    /// Starts the queue actor under `supervisor`; jobs are registered in `jobs`.
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn spawn(
        supervisor: &ActorSupervisor,
        jobs: JobRegistry,
        config: DownloadConfig,
    ) -> Result<Self> {
        let client = build_client(&config)?;
        let limiter = Arc::new(RateLimiter::new(config.bandwidth_limit));
        let offline = Arc::new(AtomicBool::new(config.offline));
        let max_parallel = config.max_parallel_downloads.max(1);
        let env = Arc::new(Env {
            client,
            limiter: Arc::clone(&limiter),
            config: Arc::new(config),
        });
        let factory_jobs = jobs.clone();
        let handle = supervisor.spawn(
            ActorSpec::new("download-manager").mailbox(MAILBOX_CAPACITY),
            move || QueueActor::new(Arc::clone(&env), factory_jobs.clone(), max_parallel),
        );
        Ok(Self {
            actor: handle.actor_ref().clone(),
            handle: Arc::new(handle),
            jobs,
            limiter,
            offline,
        })
    }

    /// Queues a download and returns its job at once. Fails immediately in offline mode
    /// (`conflict`) or for an invalid request (`invalid_input`). Asking again for a file
    /// that is already queued or running with the same destination returns the same job.
    pub async fn download(&self, req: DownloadRequest) -> Result<JobId> {
        if self.is_offline() {
            return Err(ForgeError::Conflict(
                "offline mode is on: downloads are disabled".into(),
            ));
        }
        req.validate()?;
        self.actor
            .ask(|reply| Msg::Enqueue { req, reply }, ENQUEUE_TIMEOUT)
            .await
            .map_err(ForgeError::from)?
    }

    /// Requests cancellation of a download job (keeps its `.part`).
    pub fn cancel(&self, job: JobId) -> Result<bool> {
        self.jobs.cancel(job)
    }

    /// The registry the download jobs run in.
    pub fn jobs(&self) -> &JobRegistry {
        &self.jobs
    }

    pub fn set_offline(&self, offline: bool) {
        self.offline.store(offline, AtomicOrdering::Relaxed);
    }

    pub fn is_offline(&self) -> bool {
        self.offline.load(AtomicOrdering::Relaxed)
    }

    /// Changes the bandwidth limit (bytes/s) for running and future transfers.
    pub fn set_bandwidth_limit(&self, bytes_per_second: Option<u64>) {
        self.limiter.set_rate(bytes_per_second);
    }

    pub fn bandwidth_limit(&self) -> Option<u64> {
        self.limiter.rate()
    }

    /// Jobs running and waiting.
    pub async fn stats(&self) -> Result<QueueStats> {
        Ok(self
            .actor
            .ask(|reply| Msg::Stats { reply }, ENQUEUE_TIMEOUT)
            .await?)
    }

    /// Stops the queue actor (graceful, see [`ActorHandle::stop`]).
    pub async fn shutdown(&self) {
        self.handle.stop().await;
    }
}

fn build_client(config: &DownloadConfig) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(config.user_agent.clone())
        .connect_timeout(config.connect_timeout);
    builder = match &config.proxy {
        // reqwest's default: system proxy (Windows settings, then environment).
        ProxySetting::System => builder,
        ProxySetting::Direct => builder.no_proxy(),
        ProxySetting::Manual(url) => {
            let proxy = reqwest::Proxy::all(url.as_str())
                .map_err(|e| ForgeError::ConfigInvalid(format!("invalid proxy {url}: {e}")))?;
            builder.proxy(proxy)
        }
    };
    builder
        .build()
        .map_err(|e| ForgeError::Internal(format!("could not build the HTTP client: {e}")))
}

enum Msg {
    Enqueue {
        req: DownloadRequest,
        reply: Reply<Result<JobId>>,
    },
    /// A job is over (done, failed, cancelled, dropped): free its slot or queue entry.
    Ended {
        key: String,
    },
    Stats {
        reply: Reply<QueueStats>,
    },
}

/// Sends `Ended` when the job's future goes away, whatever the reason.
struct EndGuard {
    actor: ActorRef<Msg>,
    key: String,
}

impl Drop for EndGuard {
    fn drop(&mut self) {
        let msg = Msg::Ended {
            key: std::mem::take(&mut self.key),
        };
        match self.actor.tell(msg) {
            Ok(()) => {}
            Err(err) if err.is_full() => {
                // Never lose a slot release: retry from a task until it fits.
                let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                    return;
                };
                let actor = self.actor.clone();
                let mut msg = err.into_inner();
                runtime.spawn(async move {
                    for _ in 0..1200 {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        match actor.tell(msg) {
                            Ok(()) => return,
                            Err(err) if err.is_full() => msg = err.into_inner(),
                            Err(_) => return,
                        }
                    }
                });
            }
            Err(_) => {}
        }
    }
}

struct Active {
    job: JobId,
    dest: PathBuf,
    granted: bool,
}

struct Waiting {
    priority: Priority,
    seq: u64,
    key: String,
    grant: oneshot::Sender<()>,
}

impl PartialEq for Waiting {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Waiting {}
impl PartialOrd for Waiting {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Waiting {
    /// Max-heap: higher priority first, then the older (lower `seq`) entry.
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

struct QueueActor {
    env: Arc<Env>,
    jobs: JobRegistry,
    max_parallel: usize,
    running: usize,
    next_seq: u64,
    active: HashMap<String, Active>,
    waiting: BinaryHeap<Waiting>,
}

impl QueueActor {
    fn new(env: Arc<Env>, jobs: JobRegistry, max_parallel: usize) -> Self {
        Self {
            env,
            jobs,
            max_parallel,
            running: 0,
            next_seq: 0,
            active: HashMap::new(),
            waiting: BinaryHeap::new(),
        }
    }

    /// Gives free slots to the best waiting entries. Entries whose job already ended
    /// are skipped.
    fn dispatch(&mut self) {
        while self.running < self.max_parallel {
            let Some(next) = self.waiting.pop() else {
                return;
            };
            let Some(active) = self.active.get_mut(&next.key) else {
                continue;
            };
            active.granted = true;
            self.running += 1;
            // If the job vanished meanwhile, its EndGuard frees the slot.
            let _ = next.grant.send(());
        }
    }

    fn enqueue(&mut self, req: DownloadRequest, myself: ActorRef<Msg>) -> Result<JobId> {
        let key = part_key(&req);
        if let Some(existing) = self.active.get(&key) {
            if existing.dest == req.dest {
                return Ok(existing.job);
            }
            return Err(ForgeError::Conflict(format!(
                "{} is already being downloaded to {}",
                req.urls[0],
                existing.dest.display()
            )));
        }
        let dest = req.dest.clone();
        let (grant_tx, grant_rx) = oneshot::channel();
        let guard = EndGuard {
            actor: myself,
            key: key.clone(),
        };
        let name = format!(
            "download {}",
            req.dest
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        let env = Arc::clone(&self.env);
        let priority = req.priority;
        let job_key = key.clone();
        let job = self.jobs.start(name, move |ctx| {
            run_job(env, req, job_key, guard, grant_rx, ctx)
        });
        self.active.insert(
            key.clone(),
            Active {
                job,
                dest,
                granted: false,
            },
        );
        self.waiting.push(Waiting {
            priority,
            seq: self.next_seq,
            key,
            grant: grant_tx,
        });
        self.next_seq += 1;
        self.dispatch();
        Ok(job)
    }
}

impl Actor for QueueActor {
    type Msg = Msg;

    async fn handle(&mut self, msg: Msg, ctx: &mut Context<Self>) {
        match msg {
            Msg::Enqueue { req, reply } => {
                let result = match ctx.myself() {
                    Some(myself) => self.enqueue(req, myself),
                    None => Err(ForgeError::Internal("download queue is stopping".into())),
                };
                let _ = reply.send(result);
            }
            Msg::Ended { key } => {
                if let Some(active) = self.active.remove(&key) {
                    if active.granted {
                        self.running = self.running.saturating_sub(1);
                    }
                }
                self.dispatch();
            }
            Msg::Stats { reply } => {
                let _ = reply.send(QueueStats {
                    running: self.running,
                    queued: self.active.len().saturating_sub(self.running),
                });
            }
        }
    }
}

/// Body of a download job: wait for a slot, then transfer.
async fn run_job(
    env: Arc<Env>,
    req: DownloadRequest,
    key: String,
    guard: EndGuard,
    grant: oneshot::Receiver<()>,
    ctx: JobContext,
) -> Result<DownloadOutcome> {
    // Held until the job ends (any way): frees the slot or the queue entry.
    let _guard = guard;
    tokio::select! {
        biased;
        () = ctx.cancelled() => {
            return Err(ForgeError::Cancelled("download cancelled while queued".into()));
        }
        granted = grant => {
            granted.map_err(|_| ForgeError::Internal("download queue restarted".into()))?;
        }
    }
    let progress = Arc::new(Progress::default());
    progress.set_total(req.expected_size);
    let _reporter = spawn_reporter(
        ctx.clone(),
        Arc::clone(&progress),
        env.config.progress_interval,
    );
    let part = env.config.tmp_dir.join(format!("{key}.part"));
    if let Some(dir) = part.parent() {
        let dir = dir.to_path_buf();
        tokio::task::spawn_blocking(move || std::fs::create_dir_all(&dir))
            .await
            .map_err(|e| ForgeError::Internal(format!("blocking task failed: {e}")))?
            .map_err(|e| ForgeError::io("creating the downloads directory", e))?;
    }
    let transfer = Transfer {
        env: &env,
        req: &req,
        part,
        token: ctx.cancellation_token(),
        ctx: &ctx,
        progress,
    };
    transfer.run().await
}
