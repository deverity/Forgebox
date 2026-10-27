//! Mailboxes: bounded `mpsc` queues addressed through [`ActorRef`].
//!
//! A mailbox outlives the actor instances that read it: when the supervisor restarts a
//! panicked actor, the new instance keeps reading the same queue, so every `ActorRef`
//! handed out earlier stays valid.
//!
//! Sending never waits for capacity: [`ActorRef::tell`] and [`ActorRef::ask`] use
//! `try_send` and report a full mailbox immediately (backpressure is the caller's
//! decision, never a hidden stall). [`ActorRef::ask`] always takes a timeout.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};

use crate::error::ForgeError;

/// Default number of messages a mailbox buffers before `tell`/`ask` report it full.
pub const DEFAULT_MAILBOX_CAPACITY: usize = 256;

/// Reply channel carried by request messages (`Msg::Get { reply: Reply<u64> }`).
pub type Reply<R> = oneshot::Sender<R>;

/// Creates a mailbox of `capacity` (at least 1) for the actor called `name`.
pub(crate) fn channel<M>(name: Arc<str>, capacity: usize) -> (ActorRef<M>, mpsc::Receiver<M>) {
    let (tx, rx) = mpsc::channel(capacity.max(1));
    (ActorRef { name, tx }, rx)
}

/// Address of an actor. Cheap to clone; the actor stops once every `ActorRef` (and
/// [`super::ActorHandle`]) is dropped and its mailbox is drained.
pub struct ActorRef<M> {
    name: Arc<str>,
    tx: mpsc::Sender<M>,
}

impl<M> Clone for ActorRef<M> {
    fn clone(&self) -> Self {
        Self {
            name: Arc::clone(&self.name),
            tx: self.tx.clone(),
        }
    }
}

impl<M> fmt::Debug for ActorRef<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActorRef")
            .field("name", &self.name)
            .field("closed", &self.tx.is_closed())
            .finish()
    }
}

impl<M: Send + 'static> ActorRef<M> {
    /// Rebuilds a strong reference from the weak one kept by the actor's context.
    pub(crate) fn from_weak(name: Arc<str>, weak: &mpsc::WeakSender<M>) -> Option<Self> {
        weak.upgrade().map(|tx| Self { name, tx })
    }

    /// Weak sender that does not keep the actor alive (stored in its `Context`).
    pub(crate) fn downgrade(&self) -> mpsc::WeakSender<M> {
        self.tx.downgrade()
    }

    /// Name of the actor (as in `actor.restarted` events).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Address that does not keep the actor alive (see [`WeakActorRef`]).
    pub fn weak(&self) -> WeakActorRef<M> {
        WeakActorRef {
            name: Arc::clone(&self.name),
            tx: self.tx.downgrade(),
        }
    }

    /// Fire-and-forget. Fails at once, giving the message back, when the mailbox is full
    /// or the actor is stopped.
    pub fn tell(&self, msg: M) -> Result<(), TellError<M>> {
        self.tx.try_send(msg).map_err(|err| match err {
            TrySendError::Full(msg) => TellError::MailboxFull {
                actor: Arc::clone(&self.name),
                msg,
            },
            TrySendError::Closed(msg) => TellError::Stopped {
                actor: Arc::clone(&self.name),
                msg,
            },
        })
    }

    /// Request/response. `make` builds the message around the reply channel; the answer
    /// must arrive within `timeout` (mandatory, ADR-0011).
    ///
    /// Errors: [`AskError::MailboxFull`] right away when the queue is full;
    /// [`AskError::Stopped`] when the actor is stopped or dropped the reply without
    /// answering (for example because it panicked while handling this message; a
    /// supervised actor is restarted and later asks succeed); [`AskError::Timeout`] when
    /// no answer came in time (the message may still be processed later).
    pub async fn ask<R>(
        &self,
        make: impl FnOnce(Reply<R>) -> M,
        timeout: Duration,
    ) -> Result<R, AskError> {
        let (reply, answer) = oneshot::channel();
        match self.tx.try_send(make(reply)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(AskError::MailboxFull {
                    actor: Arc::clone(&self.name),
                });
            }
            Err(TrySendError::Closed(_)) => {
                return Err(AskError::Stopped {
                    actor: Arc::clone(&self.name),
                });
            }
        }
        match tokio::time::timeout(timeout, answer).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err(AskError::Stopped {
                actor: Arc::clone(&self.name),
            }),
            Err(_) => Err(AskError::Timeout {
                actor: Arc::clone(&self.name),
                after: timeout,
            }),
        }
    }

    /// Whether the actor stopped reading its mailbox for good (stopped or failed).
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    /// Free slots in the mailbox right now.
    pub fn capacity(&self) -> usize {
        self.tx.capacity()
    }

    /// Size of the mailbox.
    pub fn max_capacity(&self) -> usize {
        self.tx.max_capacity()
    }
}

/// Address that does not keep the actor alive: tasks spawned by an actor (process
/// watchers, timers) hold one so that the actor still stops once every external
/// [`ActorRef`] is gone.
pub struct WeakActorRef<M> {
    name: Arc<str>,
    tx: mpsc::WeakSender<M>,
}

impl<M> Clone for WeakActorRef<M> {
    fn clone(&self) -> Self {
        Self {
            name: Arc::clone(&self.name),
            tx: self.tx.clone(),
        }
    }
}

impl<M> fmt::Debug for WeakActorRef<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WeakActorRef")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl<M: Send + 'static> WeakActorRef<M> {
    pub(crate) fn new(name: Arc<str>, tx: mpsc::WeakSender<M>) -> Self {
        Self { name, tx }
    }

    /// Strong address, or `None` once the actor's mailbox was dropped.
    pub fn upgrade(&self) -> Option<ActorRef<M>> {
        ActorRef::from_weak(Arc::clone(&self.name), &self.tx)
    }

    /// Name of the actor.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Why [`ActorRef::tell`] failed. Carries the message back.
pub enum TellError<M> {
    /// The mailbox is full; try later or drop the message.
    MailboxFull { actor: Arc<str>, msg: M },
    /// The actor is stopped or failed; the message will never be handled.
    Stopped { actor: Arc<str>, msg: M },
}

impl<M> TellError<M> {
    /// The message that could not be delivered.
    pub fn into_inner(self) -> M {
        match self {
            Self::MailboxFull { msg, .. } | Self::Stopped { msg, .. } => msg,
        }
    }

    /// Name of the target actor.
    pub fn actor(&self) -> &str {
        match self {
            Self::MailboxFull { actor, .. } | Self::Stopped { actor, .. } => actor,
        }
    }

    /// Whether the failure was a full mailbox (as opposed to a stopped actor).
    pub fn is_full(&self) -> bool {
        matches!(self, Self::MailboxFull { .. })
    }
}

impl<M> fmt::Debug for TellError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let variant = if self.is_full() {
            "MailboxFull"
        } else {
            "Stopped"
        };
        f.debug_struct(variant)
            .field("actor", &self.actor())
            .finish_non_exhaustive()
    }
}

impl<M> fmt::Display for TellError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_full() {
            write!(f, "mailbox of actor {} is full", self.actor())
        } else {
            write!(f, "actor {} is stopped", self.actor())
        }
    }
}

impl<M> std::error::Error for TellError<M> {}

/// Why [`ActorRef::ask`] failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AskError {
    /// No answer within the timeout. The request may still be handled later.
    #[error("actor {actor} did not answer within {after:?}")]
    Timeout { actor: Arc<str>, after: Duration },
    /// The actor is stopped/failed, or dropped the reply without answering.
    #[error("actor {actor} is stopped or dropped the request")]
    Stopped { actor: Arc<str> },
    /// The mailbox is full; nothing was sent.
    #[error("mailbox of actor {actor} is full")]
    MailboxFull { actor: Arc<str> },
}

impl AskError {
    /// Name of the target actor.
    pub fn actor(&self) -> &str {
        match self {
            Self::Timeout { actor, .. } | Self::Stopped { actor } | Self::MailboxFull { actor } => {
                actor
            }
        }
    }
}

/// API mapping: `Timeout` → `timeout`; `MailboxFull` → `timeout` with `after = 0`
/// (overloaded: the request could not even be queued); `Stopped` → `internal`.
impl From<AskError> for ForgeError {
    fn from(err: AskError) -> Self {
        match err {
            AskError::Timeout { actor, after } => ForgeError::Timeout {
                operation: format!("request to actor {actor}"),
                after,
            },
            AskError::MailboxFull { actor } => ForgeError::Timeout {
                operation: format!("request to actor {actor} (mailbox full)"),
                after: Duration::ZERO,
            },
            AskError::Stopped { actor } => {
                ForgeError::Internal(format!("actor {actor} is stopped or dropped the request"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tell_reports_full_and_returns_message() {
        let (actor, mut rx) = channel::<u32>("unit".into(), 1);
        actor.tell(1).unwrap();
        let err = actor.tell(2).unwrap_err();
        assert!(err.is_full());
        assert_eq!(err.actor(), "unit");
        assert_eq!(err.to_string(), "mailbox of actor unit is full");
        assert_eq!(err.into_inner(), 2);
        assert_eq!(rx.recv().await, Some(1));
        drop(rx);
        let err = actor.tell(3).unwrap_err();
        assert!(!err.is_full());
        assert_eq!(format!("{err:?}"), "Stopped { actor: \"unit\", .. }");
    }

    #[tokio::test]
    async fn weak_ref_does_not_keep_the_mailbox_open() {
        let (actor, mut rx) = channel::<u32>("unit".into(), 4);
        let weak = actor.weak();
        assert_eq!(weak.name(), "unit");
        weak.upgrade().unwrap().tell(1).unwrap();
        assert_eq!(rx.recv().await, Some(1));
        drop(actor);
        assert_eq!(rx.recv().await, None, "only the weak ref is left");
        assert!(weak.upgrade().is_none());
        assert!(format!("{weak:?}").contains("unit"));
    }

    #[tokio::test]
    async fn ask_maps_dropped_reply_to_stopped() {
        let (actor, mut rx) = channel::<Reply<u32>>("unit".into(), 4);
        let server = tokio::spawn(async move {
            let first = rx.recv().await.unwrap();
            first.send(7).unwrap();
            drop(rx.recv().await.unwrap()); // dropped without answering
        });
        assert_eq!(actor.ask(|r| r, Duration::from_secs(1)).await, Ok(7));
        let err = actor.ask(|r| r, Duration::from_secs(1)).await.unwrap_err();
        assert!(matches!(err, AskError::Stopped { .. }), "{err:?}");
        server.await.unwrap();
        let err = actor.ask(|r| r, Duration::from_secs(1)).await.unwrap_err();
        assert!(matches!(err, AskError::Stopped { .. }), "{err:?}");
    }

    #[test]
    fn ask_error_maps_to_api_codes() {
        let actor: Arc<str> = "a".into();
        let timeout = AskError::Timeout {
            actor: actor.clone(),
            after: Duration::from_millis(5),
        };
        assert_eq!(ForgeError::from(timeout).code(), "timeout");
        let full = AskError::MailboxFull {
            actor: actor.clone(),
        };
        assert_eq!(full.actor(), "a");
        assert_eq!(ForgeError::from(full).code(), "timeout");
        assert_eq!(
            ForgeError::from(AskError::Stopped { actor }).code(),
            "internal"
        );
    }
}
