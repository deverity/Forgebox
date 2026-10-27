//! Long-running operations as jobs (docs/03 §7.4): the caller gets a [`JobId`] at once;
//! progress and outcome arrive as `job.started|progress|done|failed|cancelled` events.
//!
//! [`JobRegistry::start`] spawns the job's future in its own task and returns
//! immediately. The future receives a [`JobContext`] to report [`JobProgress`] and to
//! observe cancellation. Cancellation is cooperative (`tokio_util` `CancellationToken`):
//! [`JobRegistry::cancel`] signals the token, the job winds down and returns an error,
//! and the job ends as `cancelled`. A job that ignores the request for longer than
//! [`JobConfig::cancel_grace`] has its future dropped (`cancelled` with `forced: true`).
//! A panic inside a job is caught and reported as `failed` with code `internal`.
//!
//! Events of one job are totally ordered by `seq`: `started` is published before the
//! task is spawned, progress is only published while the job is running, and every
//! publication happens under the job table lock (lock order: `jobs.table` →
//! `event_bus.seq`, see [`crate::actor`]). Progress events are throttled to one per
//! [`JobConfig::progress_interval`]; the snapshot ([`JobRegistry::get`]) always has the
//! latest values.
//!
//! Starting with a [`RequestId`] already seen returns the existing job (idempotent API,
//! ADR-0009). The registry keeps every running job and the last
//! [`JobConfig::retain_finished`] finished ones.

pub mod payload;

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::FutureExt;
use serde::Serialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub use self::payload::{
    JOB_EVENT_VERSION, JobCancelledPayload, JobDonePayload, JobErrorInfo, JobFailedPayload,
    JobInfo, JobProgress, JobProgressPayload, JobStartedPayload, JobState,
};
use crate::Timestamp;
use crate::actor::panic_message;
use crate::error::{ForgeError, Result};
use crate::event::{EventBus, EventKind};
pub use crate::ids::JobId;
use crate::ids::RequestId;
use crate::sync::{LockLevel, OrderedMutex};

/// Limits of a [`JobRegistry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobConfig {
    /// Finished jobs kept for `get`/`list` (oldest dropped first).
    pub retain_finished: usize,
    /// Minimum interval between two `job.progress` events of the same job.
    pub progress_interval: Duration,
    /// Time a job has to honour a cancellation before its future is dropped.
    pub cancel_grace: Duration,
}

impl Default for JobConfig {
    /// 100 finished jobs, progress at most every 100 ms, 10 s of cancellation grace.
    fn default() -> Self {
        Self {
            retain_finished: 100,
            progress_interval: Duration::from_millis(100),
            cancel_grace: Duration::from_secs(10),
        }
    }
}

/// Name and optional idempotency key of a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    name: String,
    request_id: Option<RequestId>,
    job_id: Option<JobId>,
}

impl JobSpec {
    /// Job called `name` (e.g. `"tool.install php@8.3"`).
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            request_id: None,
            job_id: None,
        }
    }

    /// Uses a pre-allocated id (from [`JobId::new`]) so the caller can publish the id
    /// before the job exists, e.g. to reserve a slot under a lock and start the job after
    /// releasing it. The id must not belong to another job.
    pub fn job_id(mut self, job_id: JobId) -> Self {
        self.job_id = Some(job_id);
        self
    }

    /// Idempotency key: starting again with the same id returns the same job.
    pub fn request_id(mut self, request_id: RequestId) -> Self {
        self.request_id = Some(request_id);
        self
    }
}

/// Registry of running and recently finished jobs. Cheap to clone.
#[derive(Clone)]
pub struct JobRegistry {
    inner: Arc<Inner>,
}

struct Inner {
    bus: EventBus,
    config: JobConfig,
    /// Lock order: `jobs.table` (rank 10) → `event_bus.seq` (publishing). Never held
    /// across `.await`.
    table: OrderedMutex<Table>,
}

#[derive(Default)]
struct Table {
    jobs: HashMap<JobId, Entry>,
    by_request: HashMap<RequestId, JobId>,
    finished: VecDeque<JobId>,
    /// Start order (ULIDs of the same millisecond are not ordered).
    next_order: u64,
}

struct Entry {
    order: u64,
    /// Source of truth of the snapshot; `JobHandle`s subscribe to it.
    info: watch::Sender<JobInfo>,
    cancel: CancellationToken,
    started: Instant,
    last_progress_event: Option<Instant>,
}

impl fmt::Debug for JobRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let jobs = self.inner.table.lock().jobs.len();
        f.debug_struct("JobRegistry")
            .field("config", &self.inner.config)
            .field("jobs", &jobs)
            .finish()
    }
}

impl JobRegistry {
    /// Registry with [`JobConfig::default`], publishing on `bus`.
    pub fn new(bus: EventBus) -> Self {
        Self::with_config(bus, JobConfig::default())
    }

    /// Registry with explicit limits.
    pub fn with_config(bus: EventBus, config: JobConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                bus,
                config,
                table: OrderedMutex::new(LockLevel::JOB_TABLE, Table::default()),
            }),
        }
    }

    /// Starts a job and returns its id immediately. `factory` is called inside the new
    /// task with the job's [`JobContext`]; the value it resolves to is serialized into
    /// `job.done` (`result`).
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn start<F, Fut, T>(&self, name: impl Into<String>, factory: F) -> JobId
    where
        F: FnOnce(JobContext) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
        T: Serialize + Send + 'static,
    {
        self.spawn(JobSpec::new(name), factory).id()
    }

    /// Like [`JobRegistry::start`], with an idempotency key, returning a [`JobHandle`].
    /// If `spec.request_id` belongs to a retained job, that job's handle is returned and
    /// `factory` is dropped unused.
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn spawn<F, Fut, T>(&self, spec: JobSpec, factory: F) -> JobHandle
    where
        F: FnOnce(JobContext) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
        T: Serialize + Send + 'static,
    {
        let id = spec.job_id.unwrap_or_default();
        let token = CancellationToken::new();
        let info_rx = {
            let mut table = self.inner.table.lock();
            if let Some(request_id) = spec.request_id {
                let existing = table.by_request.get(&request_id).and_then(|known| {
                    table
                        .jobs
                        .get(known)
                        .map(|entry| (*known, entry.info.subscribe()))
                });
                if let Some((known, info)) = existing {
                    tracing::debug!(job_id = %known, %request_id, "job already started for request");
                    return JobHandle {
                        id: known,
                        info,
                        registry: self.clone(),
                    };
                }
            }
            let info = JobInfo {
                id,
                name: spec.name.clone(),
                request_id: spec.request_id,
                state: JobState::Running,
                progress: JobProgress::default(),
                cancel_requested: false,
                started_at: Timestamp::now(),
                finished_at: None,
                result: None,
                error: None,
            };
            let (info_tx, info_rx) = watch::channel(info);
            let order = table.next_order;
            table.next_order += 1;
            table.jobs.insert(
                id,
                Entry {
                    order,
                    info: info_tx,
                    cancel: token.clone(),
                    started: Instant::now(),
                    last_progress_event: None,
                },
            );
            if let Some(request_id) = spec.request_id {
                table.by_request.insert(request_id, id);
            }
            self.inner.publish(
                EventKind::JobStarted,
                &JobStartedPayload {
                    v: JOB_EVENT_VERSION,
                    job_id: id,
                    name: spec.name.clone(),
                    request_id: spec.request_id,
                },
            );
            info_rx
        };
        tracing::debug!(job_id = %id, name = %spec.name, "job started");
        let ctx = JobContext {
            id,
            token: token.clone(),
            inner: Arc::clone(&self.inner),
        };
        tokio::spawn(run_job(Arc::clone(&self.inner), id, token, ctx, factory));
        JobHandle {
            id,
            info: info_rx,
            registry: self.clone(),
        }
    }

    /// Requests cancellation. `Ok(true)` if the job was running, `Ok(false)` if it had
    /// already finished, `NotFound` if unknown (or no longer retained).
    pub fn cancel(&self, id: JobId) -> Result<bool> {
        let table = self.inner.table.lock();
        let entry = table
            .jobs
            .get(&id)
            .ok_or_else(|| ForgeError::NotFound(format!("job {id}")))?;
        let running = entry.info.borrow().state == JobState::Running;
        if !running {
            return Ok(false);
        }
        entry.cancel.cancel();
        entry.info.send_modify(|info| info.cancel_requested = true);
        tracing::debug!(job_id = %id, "job cancellation requested");
        Ok(true)
    }

    /// Snapshot of a job.
    pub fn get(&self, id: JobId) -> Option<JobInfo> {
        let table = self.inner.table.lock();
        table.jobs.get(&id).map(|entry| entry.info.borrow().clone())
    }

    /// Snapshots of every retained job, in start order.
    pub fn list(&self) -> Vec<JobInfo> {
        let mut jobs: Vec<(u64, JobInfo)> = {
            let table = self.inner.table.lock();
            table
                .jobs
                .values()
                .map(|entry| (entry.order, entry.info.borrow().clone()))
                .collect()
        };
        jobs.sort_unstable_by_key(|(order, _)| *order);
        jobs.into_iter().map(|(_, info)| info).collect()
    }

    /// Snapshots of the running jobs, in start order (`forge jobs --active`).
    pub fn active(&self) -> Vec<JobInfo> {
        let mut jobs = self.list();
        jobs.retain(|info| info.state == JobState::Running);
        jobs
    }

    /// Event bus the `job.*` events go to.
    pub fn events(&self) -> &EventBus {
        &self.inner.bus
    }
}

impl Inner {
    /// Publishes a `job.*` event. Called with the table lock held so that events of a
    /// job are ordered with its state (lock order `jobs.table` → `event_bus.seq`).
    fn publish<T: Serialize>(&self, kind: EventKind, payload: &T) {
        match serde_json::to_value(payload) {
            Ok(value) => {
                self.bus.publish(kind, value);
            }
            Err(err) => tracing::error!(%err, %kind, "could not serialize job event"),
        }
    }

    fn progress(&self, id: JobId, update: JobProgress) {
        let mut table = self.table.lock();
        let Some(entry) = table.jobs.get_mut(&id) else {
            return;
        };
        let running = entry.info.borrow().state == JobState::Running;
        if !running {
            return;
        }
        let mut merged = JobProgress::default();
        entry.info.send_modify(|info| {
            info.progress.merge(update);
            merged = info.progress.clone();
        });
        let now = Instant::now();
        let due = entry
            .last_progress_event
            .is_none_or(|last| now.duration_since(last) >= self.config.progress_interval);
        if due {
            entry.last_progress_event = Some(now);
            self.publish(
                EventKind::JobProgress,
                &JobProgressPayload {
                    v: JOB_EVENT_VERSION,
                    job_id: id,
                    progress: merged,
                },
            );
        }
    }

    fn finish(&self, id: JobId, outcome: Outcome) {
        let mut table = self.table.lock();
        let Some(entry) = table.jobs.get(&id) else {
            return;
        };
        let duration_ms = u64::try_from(entry.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let finished_at = Timestamp::now();
        let name = entry.info.borrow().name.clone();
        match outcome {
            Outcome::Done(result) => {
                entry.info.send_modify(|info| {
                    info.state = JobState::Done;
                    info.finished_at = Some(finished_at);
                    info.result = Some(result.clone());
                });
                self.publish(
                    EventKind::JobDone,
                    &JobDonePayload {
                        v: JOB_EVENT_VERSION,
                        job_id: id,
                        name,
                        duration_ms,
                        result,
                    },
                );
            }
            Outcome::Failed(error) => {
                tracing::warn!(
                    job_id = %id, %name, code = %error.code, message = %error.message,
                    "job failed"
                );
                entry.info.send_modify(|info| {
                    info.state = JobState::Failed;
                    info.finished_at = Some(finished_at);
                    info.error = Some(error.clone());
                });
                self.publish(
                    EventKind::JobFailed,
                    &JobFailedPayload {
                        v: JOB_EVENT_VERSION,
                        job_id: id,
                        name,
                        duration_ms,
                        error,
                    },
                );
            }
            Outcome::Cancelled { forced } => {
                if forced {
                    tracing::warn!(job_id = %id, %name, "job ignored cancellation; future dropped");
                }
                entry.info.send_modify(|info| {
                    info.state = JobState::Cancelled;
                    info.finished_at = Some(finished_at);
                });
                self.publish(
                    EventKind::JobCancelled,
                    &JobCancelledPayload {
                        v: JOB_EVENT_VERSION,
                        job_id: id,
                        name,
                        duration_ms,
                        forced,
                    },
                );
            }
        }
        table.finished.push_back(id);
        while table.finished.len() > self.config.retain_finished {
            let Some(old) = table.finished.pop_front() else {
                break;
            };
            if let Some(removed) = table.jobs.remove(&old) {
                let request_id = removed.info.borrow().request_id;
                if let Some(request_id) = request_id {
                    table.by_request.remove(&request_id);
                }
            }
        }
    }
}

/// How a job ended.
enum Outcome {
    Done(serde_json::Value),
    Failed(JobErrorInfo),
    Cancelled { forced: bool },
}

async fn run_job<F, Fut, T>(
    inner: Arc<Inner>,
    id: JobId,
    token: CancellationToken,
    ctx: JobContext,
    factory: F,
) where
    F: FnOnce(JobContext) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T>> + Send + 'static,
    T: Serialize + Send + 'static,
{
    let job = AssertUnwindSafe(async move { factory(ctx).await }).catch_unwind();
    let grace = inner.config.cancel_grace;
    let forced = async {
        token.cancelled().await;
        tokio::time::sleep(grace).await;
    };
    let finished = tokio::select! {
        biased;
        result = job => Some(result),
        () = forced => None,
    };
    let outcome = match finished {
        Some(Ok(Ok(value))) => match serde_json::to_value(&value) {
            Ok(result) => Outcome::Done(result),
            Err(err) => Outcome::Failed(JobErrorInfo::from(&ForgeError::Internal(format!(
                "job result is not serializable: {err}"
            )))),
        },
        Some(Ok(Err(err))) => {
            if token.is_cancelled() || matches!(err, ForgeError::Cancelled(_)) {
                Outcome::Cancelled { forced: false }
            } else {
                Outcome::Failed(JobErrorInfo::from(&err))
            }
        }
        Some(Err(panic)) => Outcome::Failed(JobErrorInfo::from(&ForgeError::Internal(format!(
            "job panicked: {}",
            panic_message(&*panic)
        )))),
        None => Outcome::Cancelled { forced: true },
    };
    inner.finish(id, outcome);
}

/// Given to a job's future: identity, progress reporting and cancellation.
#[derive(Clone)]
pub struct JobContext {
    id: JobId,
    token: CancellationToken,
    inner: Arc<Inner>,
}

impl fmt::Debug for JobContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobContext")
            .field("id", &self.id)
            .field("cancelled", &self.token.is_cancelled())
            .finish()
    }
}

impl JobContext {
    /// Id of this job.
    pub fn id(&self) -> JobId {
        self.id
    }

    /// Merges `update` into the job's progress and publishes `job.progress` (throttled).
    /// Ignored once the job has finished.
    pub fn progress(&self, update: JobProgress) {
        self.inner.progress(self.id, update);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Resolves when cancellation is requested (use in `tokio::select!`).
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    /// `Err(Cancelled)` if cancellation was requested: `ctx.check_cancelled()?` between
    /// steps.
    pub fn check_cancelled(&self) -> Result<()> {
        if self.token.is_cancelled() {
            Err(ForgeError::Cancelled(format!("job {}", self.id)))
        } else {
            Ok(())
        }
    }

    /// Token to pass down to helpers (child tokens via `child_token()`).
    pub fn cancellation_token(&self) -> CancellationToken {
        self.token.clone()
    }
}

/// Handle of a started job: id, live snapshot, cancel, wait.
#[derive(Clone)]
pub struct JobHandle {
    id: JobId,
    info: watch::Receiver<JobInfo>,
    registry: JobRegistry,
}

impl fmt::Debug for JobHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.info.borrow().state;
        f.debug_struct("JobHandle")
            .field("id", &self.id)
            .field("state", &state)
            .finish()
    }
}

impl JobHandle {
    /// Id of the job.
    pub fn id(&self) -> JobId {
        self.id
    }

    /// Current snapshot (kept even after the registry stops retaining the job).
    pub fn info(&self) -> JobInfo {
        self.info.borrow().clone()
    }

    /// Requests cancellation; `false` if the job already finished.
    pub fn cancel(&self) -> bool {
        self.registry.cancel(self.id).unwrap_or(false)
    }

    /// Waits until the job finishes, at most `timeout`; returns the final snapshot.
    pub async fn wait(&self, timeout: Duration) -> Result<JobInfo> {
        let mut info = self.info.clone();
        let wait = async move {
            let finished = info
                .wait_for(|i| i.state.is_terminal())
                .await
                .map(|done| done.clone());
            match finished {
                Ok(done) => done,
                Err(_) => info.borrow().clone(),
            }
        };
        tokio::time::timeout(timeout, wait)
            .await
            .map_err(|_| ForgeError::Timeout {
                operation: format!("waiting for job {}", self.id),
                after: timeout,
            })
    }
}
