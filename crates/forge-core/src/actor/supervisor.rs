//! Supervision: one task per actor, panic isolation, restart policy, graceful stop.
//!
//! The supervising loop runs **inside the actor's own task** (there is no central
//! supervisor task that could become a bottleneck or a single point of failure). It owns
//! the mailbox receiver; each incarnation of the actor borrows it, so a restarted
//! instance keeps the queue and the `ActorRef`s handed out before the panic.
//! [`ActorSupervisor`] keeps a registry of its children for listing and `stop_all`.

use std::collections::VecDeque;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio::task::AbortHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::mailbox::{self, ActorRef, DEFAULT_MAILBOX_CAPACITY};
use super::{Actor, Context, RestartInfo, panic_message};
use crate::error::{ForgeError, Result};
use crate::event::{EventBus, EventKind};
use crate::sync::{LockLevel, OrderedMutex};

/// Default time budget of a graceful stop (drain + `on_stop`).
pub const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// Extra wait, past the stop timeout, before the task is aborted.
const STOP_SLACK: Duration = Duration::from_millis(250);
/// `on_stop` always gets at least this much, even when draining used the whole budget.
const MIN_ON_STOP: Duration = Duration::from_millis(100);

/// Version of the `actor.*` event payloads.
pub const ACTOR_EVENT_VERSION: u32 = 1;

/// Payload of `actor.restarted`.
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorRestartedPayload {
    /// Payload version ([`ACTOR_EVENT_VERSION`]).
    pub v: u32,
    pub actor: String,
    /// Panic message of the instance that died.
    pub reason: String,
    /// Restarts of this actor so far, this one included.
    pub restart_count: u32,
    /// Delay before the new instance starts.
    #[cfg_attr(feature = "specta", specta(type = specta_typescript::Number))]
    pub backoff_ms: u64,
}

/// Payload of `actor.failed`: the restart budget is exhausted and the actor stays down.
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorFailedPayload {
    /// Payload version ([`ACTOR_EVENT_VERSION`]).
    pub v: u32,
    pub actor: String,
    /// Panic message of the last instance.
    pub reason: String,
    /// Restarts performed before giving up.
    pub restart_count: u32,
}

/// How many panics an actor survives and how long it waits before restarting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartPolicy {
    /// Restarts allowed within `window`; one more panic makes the actor `Failed`.
    pub max_restarts: u32,
    /// Sliding window over which restarts are counted.
    pub window: Duration,
    /// Delay before the first restart in the window; doubles at each further restart.
    pub backoff_initial: Duration,
    /// Upper bound of the delay.
    pub backoff_max: Duration,
}

impl RestartPolicy {
    /// No restart: the first panic makes the actor `Failed`.
    pub const fn never() -> Self {
        Self {
            max_restarts: 0,
            window: Duration::ZERO,
            backoff_initial: Duration::ZERO,
            backoff_max: Duration::ZERO,
        }
    }

    /// Delay before the `attempt`-th restart within the window (1-based):
    /// `backoff_initial × 2^(attempt-1)`, capped at `backoff_max`.
    pub fn backoff_for(&self, attempt: u32) -> Duration {
        let shift = attempt.saturating_sub(1).min(20);
        self.backoff_initial
            .saturating_mul(1_u32 << shift)
            .min(self.backoff_max)
    }
}

impl Default for RestartPolicy {
    /// 5 restarts per minute, backoff from 100 ms up to 5 s.
    fn default() -> Self {
        Self {
            max_restarts: 5,
            window: Duration::from_secs(60),
            backoff_initial: Duration::from_millis(100),
            backoff_max: Duration::from_secs(5),
        }
    }
}

/// Name and limits of an actor.
#[derive(Debug, Clone)]
pub struct ActorSpec {
    name: Arc<str>,
    mailbox: usize,
    restart: RestartPolicy,
    stop_timeout: Duration,
}

impl ActorSpec {
    /// Spec with the defaults: mailbox of [`DEFAULT_MAILBOX_CAPACITY`],
    /// [`RestartPolicy::default`], stop timeout of [`DEFAULT_STOP_TIMEOUT`].
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into().into(),
            mailbox: DEFAULT_MAILBOX_CAPACITY,
            restart: RestartPolicy::default(),
            stop_timeout: DEFAULT_STOP_TIMEOUT,
        }
    }

    /// Mailbox capacity (at least 1).
    pub fn mailbox(mut self, capacity: usize) -> Self {
        self.mailbox = capacity.max(1);
        self
    }

    /// Restart policy.
    pub fn restart(mut self, policy: RestartPolicy) -> Self {
        self.restart = policy;
        self
    }

    /// Time budget of a graceful stop (drain + `on_stop`).
    pub fn stop_timeout(mut self, timeout: Duration) -> Self {
        self.stop_timeout = timeout;
        self
    }

    /// Actor name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Lifecycle of a supervised actor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ActorStatus {
    /// Building the instance / running `on_start`.
    Starting,
    /// Handling messages.
    Running,
    /// The last instance panicked; waiting for the backoff before the next one.
    Restarting { restart_count: u32, reason: String },
    /// Draining the mailbox and running `on_stop`.
    Stopping,
    /// Stopped (graceful stop, or every reference dropped). Terminal.
    Stopped,
    /// Restart budget exhausted. Terminal.
    Failed { reason: String },
}

impl ActorStatus {
    /// `Stopped` or `Failed`.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Stopped | Self::Failed { .. })
    }
}

/// Result of [`ActorHandle::stop`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopOutcome {
    /// Drained and stopped within the stop timeout.
    Stopped,
    /// Did not stop in time; the task was aborted (a handler blocking its thread keeps
    /// running until it returns, but the caller is not held).
    Aborted,
    /// It had already stopped or failed.
    AlreadyFinished,
}

/// One row of [`ActorSupervisor::children`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChildStatus {
    pub name: String,
    pub status: ActorStatus,
    /// Whether the task has ended (also true for an aborted task).
    pub finished: bool,
}

/// Stop/status side of a running actor (cloneable, never keeps the mailbox open).
#[derive(Clone)]
struct Control {
    name: Arc<str>,
    stop: CancellationToken,
    status: watch::Receiver<ActorStatus>,
    abort: AbortHandle,
    stop_timeout: Duration,
}

impl Control {
    fn is_finished(&self) -> bool {
        self.abort.is_finished() || self.status.borrow().is_terminal()
    }

    async fn stop(&self) -> StopOutcome {
        if self.is_finished() {
            return StopOutcome::AlreadyFinished;
        }
        self.stop.cancel();
        let mut status = self.status.clone();
        let wait = async move {
            // Err only means the task ended without a terminal status (aborted elsewhere).
            let _ = status.wait_for(ActorStatus::is_terminal).await.is_ok();
        };
        match tokio::time::timeout(self.stop_timeout + STOP_SLACK, wait).await {
            Ok(()) => StopOutcome::Stopped,
            Err(_) => {
                tracing::warn!(actor = %self.name, "actor did not stop in time; aborting its task");
                self.abort.abort();
                StopOutcome::Aborted
            }
        }
    }
}

/// Handle returned by [`ActorSupervisor::spawn`]: address, status and stop.
///
/// Holds an [`ActorRef`], so the actor keeps running while the handle lives.
pub struct ActorHandle<M> {
    actor: ActorRef<M>,
    control: Control,
}

impl<M> fmt::Debug for ActorHandle<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActorHandle")
            .field("name", &self.control.name)
            .field("status", &*self.control.status.borrow())
            .finish()
    }
}

impl<M: Send + 'static> ActorHandle<M> {
    /// Address of the actor (clone it to hand it out).
    pub fn actor_ref(&self) -> &ActorRef<M> {
        &self.actor
    }

    /// Actor name.
    pub fn name(&self) -> &str {
        &self.control.name
    }

    /// Current status.
    pub fn status(&self) -> ActorStatus {
        self.control.status.borrow().clone()
    }

    /// Whether the actor stopped, failed or its task was aborted.
    pub fn is_finished(&self) -> bool {
        self.control.is_finished()
    }

    /// Graceful stop: closes the mailbox, drains it, runs `on_stop`; aborts the task
    /// if that takes longer than the stop timeout (+250 ms).
    pub async fn stop(&self) -> StopOutcome {
        self.control.stop().await
    }

    /// Waits until the status satisfies `pred`, at most `timeout`.
    pub async fn wait_for_status(
        &self,
        pred: impl FnMut(&ActorStatus) -> bool,
        timeout: Duration,
    ) -> Result<ActorStatus> {
        let mut status = self.control.status.clone();
        let wait = async move {
            match status.wait_for(pred).await {
                Ok(found) => Some(found.clone()),
                Err(_) => None,
            }
        };
        match tokio::time::timeout(timeout, wait).await {
            Ok(Some(found)) => Ok(found),
            Ok(None) => Err(ForgeError::Internal(format!(
                "actor {} ended without reaching the expected status",
                self.control.name
            ))),
            Err(_) => Err(ForgeError::Timeout {
                operation: format!("waiting for actor {} status", self.control.name),
                after: timeout,
            }),
        }
    }
}

/// Spawns actors in their own tasks and restarts them after panics, publishing
/// `actor.restarted` / `actor.failed` on its [`EventBus`]. Cheap to clone.
#[derive(Clone)]
pub struct ActorSupervisor {
    inner: Arc<SupervisorInner>,
}

struct SupervisorInner {
    bus: EventBus,
    /// Lock order: `actor.children` (rank 10); nothing else is taken while held.
    children: OrderedMutex<Vec<Control>>,
}

impl fmt::Debug for ActorSupervisor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActorSupervisor")
            .field("children", &self.inner.children.lock().len())
            .finish()
    }
}

impl ActorSupervisor {
    /// Supervisor publishing on `bus`.
    pub fn new(bus: EventBus) -> Self {
        Self {
            inner: Arc::new(SupervisorInner {
                bus,
                children: OrderedMutex::new(LockLevel::ACTOR_CHILDREN, Vec::new()),
            }),
        }
    }

    /// Event bus used for `actor.*` events and exposed to actors via [`Context::events`].
    pub fn events(&self) -> &EventBus {
        &self.inner.bus
    }

    /// Starts an actor built by `factory` in a new task. `factory` is called again for
    /// every restart.
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn spawn<A, F>(&self, spec: ActorSpec, factory: F) -> ActorHandle<A::Msg>
    where
        A: Actor,
        F: FnMut() -> A + Send + 'static,
    {
        let (actor, mailbox) = mailbox::channel(Arc::clone(&spec.name), spec.mailbox);
        let stop = CancellationToken::new();
        let (status_tx, status_rx) = watch::channel(ActorStatus::Starting);
        let runner = Runner {
            name: Arc::clone(&spec.name),
            factory,
            policy: spec.restart.clone(),
            stop_timeout: spec.stop_timeout,
            mailbox,
            myself: actor.downgrade(),
            stop: stop.clone(),
            status: status_tx,
            bus: self.inner.bus.clone(),
        };
        let task = tokio::spawn(runner.run());
        let control = Control {
            name: spec.name,
            stop,
            status: status_rx,
            abort: task.abort_handle(),
            stop_timeout: spec.stop_timeout,
        };
        {
            let mut children = self.inner.children.lock();
            children.retain(|child| !child.abort.is_finished());
            children.push(control.clone());
        }
        ActorHandle { actor, control }
    }

    /// Status of every child spawned by this supervisor that has not been reaped.
    pub fn children(&self) -> Vec<ChildStatus> {
        let children = self.inner.children.lock().clone();
        children
            .iter()
            .map(|child| ChildStatus {
                name: child.name.to_string(),
                status: child.status.borrow().clone(),
                finished: child.is_finished(),
            })
            .collect()
    }

    /// Stops every child concurrently (each within its own stop timeout).
    pub async fn stop_all(&self) -> Vec<(String, StopOutcome)> {
        let children = self.inner.children.lock().clone();
        let outcomes = futures::future::join_all(children.iter().map(Control::stop)).await;
        children
            .iter()
            .map(|child| child.name.to_string())
            .zip(outcomes)
            .collect()
    }
}

/// Why `serve` returned.
#[derive(Debug, Clone, Copy)]
enum Exit {
    StopRequested,
    ContextStop,
    AllRefsDropped,
}

/// State of the actor's task.
struct Runner<A: Actor, F> {
    name: Arc<str>,
    factory: F,
    policy: RestartPolicy,
    stop_timeout: Duration,
    mailbox: mpsc::Receiver<A::Msg>,
    myself: mpsc::WeakSender<A::Msg>,
    stop: CancellationToken,
    status: watch::Sender<ActorStatus>,
    bus: EventBus,
}

impl<A, F> Runner<A, F>
where
    A: Actor,
    F: FnMut() -> A + Send + 'static,
{
    async fn run(mut self) {
        let mut window: VecDeque<Instant> = VecDeque::new();
        let mut restart_count = 0_u32;
        let mut restart: Option<RestartInfo> = None;
        loop {
            let mut ctx = Context::new(
                Arc::clone(&self.name),
                self.myself.clone(),
                self.bus.clone(),
                restart_count,
            );
            let mut stopping = false;
            let outcome =
                AssertUnwindSafe(self.incarnation(&mut ctx, restart.take(), &mut stopping))
                    .catch_unwind()
                    .await;
            let Err(panic) = outcome else {
                self.status.send_replace(ActorStatus::Stopped);
                tracing::debug!(actor = %self.name, "actor stopped");
                return;
            };
            let reason = panic_message(&*panic);
            drop(panic);
            if stopping || self.stop.is_cancelled() {
                tracing::error!(actor = %self.name, %reason, "actor panicked while stopping");
                self.close_mailbox();
                self.status.send_replace(ActorStatus::Stopped);
                return;
            }

            let now = Instant::now();
            while window
                .front()
                .is_some_and(|at| now.duration_since(*at) > self.policy.window)
            {
                window.pop_front();
            }
            if window.len() >= self.policy.max_restarts as usize {
                tracing::error!(
                    actor = %self.name, %reason, restart_count,
                    "actor exceeded its restart budget; giving up"
                );
                self.publish(
                    EventKind::ActorFailed,
                    &ActorFailedPayload {
                        v: ACTOR_EVENT_VERSION,
                        actor: self.name.to_string(),
                        reason: reason.clone(),
                        restart_count,
                    },
                );
                self.close_mailbox();
                self.status.send_replace(ActorStatus::Failed { reason });
                return;
            }

            window.push_back(now);
            restart_count += 1;
            let backoff = self
                .policy
                .backoff_for(u32::try_from(window.len()).unwrap_or(u32::MAX));
            tracing::warn!(
                actor = %self.name, %reason, restart_count, backoff_ms = backoff.as_millis() as u64,
                "actor panicked; restarting"
            );
            self.publish(
                EventKind::ActorRestarted,
                &ActorRestartedPayload {
                    v: ACTOR_EVENT_VERSION,
                    actor: self.name.to_string(),
                    reason: reason.clone(),
                    restart_count,
                    backoff_ms: u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
                },
            );
            self.status.send_replace(ActorStatus::Restarting {
                restart_count,
                reason: reason.clone(),
            });
            tokio::select! {
                biased;
                () = self.stop.cancelled() => {
                    self.close_mailbox();
                    self.status.send_replace(ActorStatus::Stopped);
                    return;
                }
                () = tokio::time::sleep(backoff) => {}
            }
            restart = Some(RestartInfo {
                restart_count,
                reason,
            });
        }
    }

    /// One instance: build, hooks, message loop, graceful stop. Panics escape to `run`.
    async fn incarnation(
        &mut self,
        ctx: &mut Context<A>,
        restart: Option<RestartInfo>,
        stopping: &mut bool,
    ) {
        let mut actor = (self.factory)();
        if let Some(info) = &restart {
            actor.on_restart(ctx, info).await;
        }
        actor.on_start(ctx).await;
        self.status.send_replace(ActorStatus::Running);

        let exit = self.serve(&mut actor, ctx).await;

        *stopping = true;
        self.status.send_replace(ActorStatus::Stopping);
        tracing::debug!(actor = %self.name, ?exit, "actor stopping");
        let deadline = Instant::now() + self.stop_timeout;
        self.mailbox.close();
        let mailbox = &mut self.mailbox;
        let drain = async {
            while let Some(msg) = mailbox.recv().await {
                actor.handle(msg, ctx).await;
            }
        };
        if tokio::time::timeout_at(deadline, drain).await.is_err() {
            tracing::warn!(actor = %self.name, "stop timeout while draining; dropping the rest");
        }
        let budget = deadline
            .saturating_duration_since(Instant::now())
            .max(MIN_ON_STOP);
        if tokio::time::timeout(budget, actor.on_stop(ctx))
            .await
            .is_err()
        {
            tracing::warn!(actor = %self.name, "on_stop exceeded the stop timeout");
        }
    }

    /// Message loop until a stop request, `ctx.stop()` or every reference dropped.
    async fn serve(&mut self, actor: &mut A, ctx: &mut Context<A>) -> Exit {
        loop {
            let msg = tokio::select! {
                biased;
                () = self.stop.cancelled() => return Exit::StopRequested,
                msg = self.mailbox.recv() => msg,
            };
            let Some(msg) = msg else {
                return Exit::AllRefsDropped;
            };
            actor.handle(msg, ctx).await;
            if ctx.is_stopping() {
                return Exit::ContextStop;
            }
        }
    }

    /// Refuses new messages and drops queued ones (their askers get `Stopped`).
    fn close_mailbox(&mut self) {
        self.mailbox.close();
        while self.mailbox.try_recv().is_ok() {}
    }

    fn publish<T: Serialize>(&self, kind: EventKind, payload: &T) {
        match serde_json::to_value(payload) {
            Ok(value) => {
                self.bus.publish(kind, value);
            }
            Err(err) => tracing::error!(%err, %kind, "could not serialize actor event"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_is_capped() {
        let policy = RestartPolicy {
            max_restarts: 10,
            window: Duration::from_secs(60),
            backoff_initial: Duration::from_millis(100),
            backoff_max: Duration::from_millis(700),
        };
        let delays: Vec<u64> = (1..=5)
            .map(|n| policy.backoff_for(n).as_millis() as u64)
            .collect();
        assert_eq!(delays, vec![100, 200, 400, 700, 700]);
        assert_eq!(policy.backoff_for(0), Duration::from_millis(100));
        assert_eq!(policy.backoff_for(u32::MAX), Duration::from_millis(700));
        assert_eq!(RestartPolicy::never().backoff_for(3), Duration::ZERO);
    }

    #[test]
    fn status_serializes_with_state_tag() {
        let json = serde_json::to_value(ActorStatus::Restarting {
            restart_count: 2,
            reason: "boom".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"state": "restarting", "restart_count": 2, "reason": "boom"})
        );
        assert!(ActorStatus::Failed { reason: "x".into() }.is_terminal());
        assert!(ActorStatus::Stopped.is_terminal());
        assert!(!ActorStatus::Stopping.is_terminal());
    }

    #[test]
    fn spec_defaults_and_builders() {
        let spec = ActorSpec::new("mysql/default")
            .mailbox(0)
            .stop_timeout(Duration::from_secs(1))
            .restart(RestartPolicy::never());
        assert_eq!(spec.name(), "mysql/default");
        assert_eq!(spec.mailbox, 1, "capacity is at least 1");
        assert_eq!(spec.stop_timeout, Duration::from_secs(1));
        assert_eq!(spec.restart.max_restarts, 0);
        let default = ActorSpec::new("x");
        assert_eq!(default.mailbox, DEFAULT_MAILBOX_CAPACITY);
        assert_eq!(default.restart, RestartPolicy::default());
    }
}
