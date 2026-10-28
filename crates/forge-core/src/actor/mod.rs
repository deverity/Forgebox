//! Actors: one tokio task per service/pool/subsystem, a bounded mailbox, private state
//! (docs/03 §7.2, ADR-0011).
//!
//! * [`Actor`] is the behaviour: `handle` one message at a time, plus `on_start`,
//!   `on_stop` and `on_restart` hooks.
//! * [`ActorRef`] is the address: [`ActorRef::tell`] (fire-and-forget, never waits) and
//!   [`ActorRef::ask`] (request/response with a **mandatory** timeout).
//! * [`ActorSupervisor::spawn`] starts an actor in its own task. A panic is caught at the
//!   task boundary (`catch_unwind`); the supervisor recreates the actor through its
//!   factory following a [`RestartPolicy`] (at most N restarts within a window, with
//!   exponential backoff), keeps the same mailbox (every `ActorRef` stays valid) and
//!   publishes `actor.restarted`. When the budget is exhausted the actor becomes
//!   [`ActorStatus::Failed`], `actor.failed` is published and asks fail with
//!   [`AskError::Stopped`].
//! * [`ActorHandle::stop`] is graceful: the mailbox is closed, queued messages are
//!   drained and `on_stop` runs, all within the actor's stop timeout; past it the task is
//!   aborted.
//!
//! A handler that blocks its thread (`std::thread::sleep`, slow Win32) only stalls its
//! own actor: other actors run on other worker threads and every caller is protected by
//! its ask timeout. Blocking work still belongs in `spawn_blocking`.
//!
//! # Lock order
//!
//! Shared state lives inside actors and is reached by messages. The few locks that
//! remain are [`OrderedMutex`](crate::sync::OrderedMutex)es (`parking_lot`, critical
//! sections of nanoseconds to microseconds). A thread may only acquire a lock whose rank
//! is strictly higher than every lock it holds; debug builds check it with
//! `debug_assert!` ([`crate::sync`]):
//!
//! | rank | lock | held while |
//! |---|---|---|
//! | 10 | `actor.children` ([`ActorSupervisor`]) | registering or listing children; publishes nothing |
//! | 10 | `jobs.table` ([`crate::jobs::JobRegistry`]) | updating a job; publishes `job.*`, so it takes `event_bus.seq` inside |
//! | 10 | `store.readers` ([`crate::store::StoreReader`]) | checking a read connection in or out; never runs a query |
//! | 10 | `ipc.outbox` (`forge_ipc` server connection) | queueing a response or stream item, or taking a batch for the writer; publishes nothing |
//! | 10 | `ipc.idempotency` (`forge_ipc` server) | looking up or recording a `request_id`; never runs a handler |
//! | 10 | `ipc.connections` (`forge_ipc` server) | registering, listing or cancelling connections |
//! | 10 | `ipc.client` (`forge_ipc` client: connection slot, pending calls, routes, subscription queues) | map/queue operations only; each is a separate mutex, never nested |
//! | 10 | `registry.state` (`forge_registry` service: slots, snapshots, refresh marks) | map operations only; leaf lock: never held while starting a job, awaiting or publishing |
//! | 10 | `fs.case_conversions` ([`crate::fs::case`]: folders being converted) | inserting or removing one path; leaf lock |
//! | 10 | `layout.tools` (`forge_provider::layout`: per-tool turn semaphores) | looking up or inserting a semaphore; the semaphore itself is acquired after the guard is dropped |
//! | 10 | `daemon.tables` (`forge-daemon`: instance, pool and log-target tables) | looking up, inserting or removing a handle; leaf lock |
//! | 200 | `event_bus.seq` ([`crate::event::EventBus`]) | numbering an event and handing it to the broadcast channel |
//! | 200 | `log_hub.seq` (`forge_supervisor::logs`, one per instance) | numbering a log line and handing it to the broadcast channel; never nested |
//!
//! Rules: no lock is ever held across `.await` (clippy `await_holding_lock` and
//! `await_holding_invalid_type` are errors; the guard is also `!Send`); rank-10 locks
//! are never nested with each other; nothing is acquired while `event_bus.seq` is held.
//! Internal locks of tokio primitives (`watch`, `broadcast`, `mpsc`) are leaves outside
//! this order. SQLite connections are not shared: the writer thread owns its own and
//! readers are checked out of the pool.

mod mailbox;
mod supervisor;

use std::any::Any;
use std::fmt;
use std::future::Future;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::event::EventBus;

pub use mailbox::{ActorRef, AskError, DEFAULT_MAILBOX_CAPACITY, Reply, TellError, WeakActorRef};
pub use supervisor::{
    ACTOR_EVENT_VERSION, ActorFailedPayload, ActorHandle, ActorRestartedPayload, ActorSpec,
    ActorStatus, ActorSupervisor, ChildStatus, DEFAULT_STOP_TIMEOUT, RestartPolicy, StopOutcome,
};

/// Behaviour of an actor. Implementations may write `async fn handle(...)`.
///
/// The returned futures must be `Send` (they run on the multi-thread runtime). A panic
/// in any method is caught by the supervisor: the instance is discarded and, within the
/// [`RestartPolicy`], a new one is built by the factory.
pub trait Actor: Send + Sized + 'static {
    /// Messages accepted by the mailbox. Requests carry a [`Reply`].
    type Msg: Send + 'static;

    /// Handles one message. Messages are processed strictly one at a time, in order.
    fn handle(
        &mut self,
        msg: Self::Msg,
        ctx: &mut Context<Self>,
    ) -> impl Future<Output = ()> + Send;

    /// Runs before the first message of every instance (initial start and restarts).
    fn on_start(&mut self, _ctx: &mut Context<Self>) -> impl Future<Output = ()> + Send {
        async {}
    }

    /// Runs after the mailbox was drained on a graceful stop, within the stop timeout.
    /// Not called on an instance that panicked.
    fn on_stop(&mut self, _ctx: &mut Context<Self>) -> impl Future<Output = ()> + Send {
        async {}
    }

    /// Runs on a fresh instance created after a panic, before `on_start`.
    fn on_restart(
        &mut self,
        _ctx: &mut Context<Self>,
        _info: &RestartInfo,
    ) -> impl Future<Output = ()> + Send {
        async {}
    }
}

/// Why an instance is being (re)started after a panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartInfo {
    /// Restarts of this actor so far, this one included (1 on the first restart).
    pub restart_count: u32,
    /// Panic message of the previous instance.
    pub reason: String,
}

/// What an actor can see of itself while handling messages.
pub struct Context<A: Actor> {
    name: Arc<str>,
    myself: mpsc::WeakSender<A::Msg>,
    events: EventBus,
    restart_count: u32,
    stop_requested: bool,
}

impl<A: Actor> Context<A> {
    pub(crate) fn new(
        name: Arc<str>,
        myself: mpsc::WeakSender<A::Msg>,
        events: EventBus,
        restart_count: u32,
    ) -> Self {
        Self {
            name,
            myself,
            events,
            restart_count,
            stop_requested: false,
        }
    }

    /// Name given in the [`ActorSpec`].
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Address of this actor, to hand out to others. `None` once every external
    /// reference was dropped (the actor is then winding down).
    pub fn myself(&self) -> Option<ActorRef<A::Msg>> {
        ActorRef::from_weak(Arc::clone(&self.name), &self.myself)
    }

    /// Address of this actor that does not keep it alive, for the tasks it spawns.
    pub fn myself_weak(&self) -> WeakActorRef<A::Msg> {
        WeakActorRef::new(Arc::clone(&self.name), self.myself.clone())
    }

    /// Event bus of the supervisor, for domain events.
    pub fn events(&self) -> &EventBus {
        &self.events
    }

    /// How many times this actor was restarted after a panic.
    pub fn restart_count(&self) -> u32 {
        self.restart_count
    }

    /// Asks for a graceful stop after the current message: the mailbox is closed and
    /// what is already queued is drained before `on_stop`.
    pub fn stop(&mut self) {
        self.stop_requested = true;
    }

    /// Whether [`Context::stop`] was called.
    pub fn is_stopping(&self) -> bool {
        self.stop_requested
    }
}

impl<A: Actor> fmt::Debug for Context<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("name", &self.name)
            .field("restart_count", &self.restart_count)
            .field("stop_requested", &self.stop_requested)
            .finish_non_exhaustive()
    }
}

/// Human readable message of a caught panic payload.
pub fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "panic with a non-string payload".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_messages_are_extracted() {
        let caught = std::panic::catch_unwind(|| panic!("static text")).unwrap_err();
        assert_eq!(panic_message(&*caught), "static text");
        let n = 42;
        let caught = std::panic::catch_unwind(|| panic!("formatted {n}")).unwrap_err();
        assert_eq!(panic_message(&*caught), "formatted 42");
        let caught = std::panic::catch_unwind(|| std::panic::panic_any(7_u8)).unwrap_err();
        assert_eq!(panic_message(&*caught), "panic with a non-string payload");
    }
}
