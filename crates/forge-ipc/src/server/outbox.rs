//! Output side of one connection: responses plus one drop-oldest queue per stream
//! subscription, drained by the connection's writer task.
//!
//! * Responses are never dropped. Each one carries the in-flight permit of its request,
//!   released only once written: a client that sends requests without reading answers
//!   stops being read (backpressure on that client only).
//! * Stream items never block their producer: a full queue drops its oldest item and the
//!   writer emits `<group>.lagged {subscription_id, missed}` at that point.
//! * A stream starts *inactive*: its items are held until the response of the subscribe
//!   request is queued, so a client never sees a notification for a subscription id it
//!   does not know yet.
//!
//! Lock: `ipc.outbox` (rank 10), held for queue operations only; frames are serialized
//! outside it.

use std::collections::{BTreeMap, VecDeque};
use std::ops::Bound;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{Notify, OwnedSemaphorePermit};
use tokio_util::sync::CancellationToken;

use forge_core::error::{ForgeError, Result};
use forge_core::ids::SubscriptionId;
use forge_core::sync::{LockLevel, OrderedMutex};

use crate::api::LaggedDto;
use crate::rpc::notification_frame;
use crate::stream_queue::{DropOldestQueue, Popped};

struct StreamSlot {
    lagged: &'static str,
    active: bool,
    queue: DropOldestQueue<String>,
    token: CancellationToken,
}

struct State {
    closed: bool,
    drain_then_close: bool,
    responses: VecDeque<(String, Option<OwnedSemaphorePermit>)>,
    streams: BTreeMap<SubscriptionId, StreamSlot>,
    /// Round-robin position among streams.
    cursor: Option<SubscriptionId>,
}

/// Outgoing queue of one connection.
pub(crate) struct Outbox {
    state: OrderedMutex<State>,
    /// Wakes the writer (single waiter: `notify_one` keeps a permit, no lost wake-up).
    wake_writer: Notify,
    /// Signalled after each written batch (producers waiting for room).
    space: Notify,
    connection: CancellationToken,
    default_capacity: usize,
    max_streams: usize,
}

/// What the writer gets from [`Outbox::take_batch`].
pub(crate) struct Batch {
    pub frames: Vec<String>,
    pub permits: Vec<OwnedSemaphorePermit>,
    /// Nothing left and the outbox is closing: the writer may exit.
    pub finished: bool,
}

enum Out {
    Frame(String),
    Lagged {
        method: &'static str,
        id: SubscriptionId,
        missed: u64,
    },
}

impl Outbox {
    pub(crate) fn new(
        connection: CancellationToken,
        default_capacity: usize,
        max_streams: usize,
    ) -> Self {
        Self {
            state: OrderedMutex::new(
                LockLevel::IPC_OUTBOX,
                State {
                    closed: false,
                    drain_then_close: false,
                    responses: VecDeque::new(),
                    streams: BTreeMap::new(),
                    cursor: None,
                },
            ),
            wake_writer: Notify::new(),
            space: Notify::new(),
            connection,
            default_capacity,
            max_streams,
        }
    }

    /// Queues a response; `activate` turns on the stream opened by that request.
    pub(crate) fn push_response(
        &self,
        frame: String,
        permit: Option<OwnedSemaphorePermit>,
        activate: Option<SubscriptionId>,
    ) {
        {
            let mut state = self.state.lock();
            if state.closed {
                return;
            }
            state.responses.push_back((frame, permit));
            if let Some(slot) = activate.and_then(|id| state.streams.get_mut(&id)) {
                slot.active = true;
            }
        }
        self.wake_writer.notify_one();
    }

    /// Registers a stream (inactive until its subscribe response is queued).
    pub(crate) fn open_stream(
        &self,
        lagged: &'static str,
        capacity: Option<usize>,
    ) -> Result<(SubscriptionId, CancellationToken)> {
        let id = SubscriptionId::new();
        let token = self.connection.child_token();
        let mut state = self.state.lock();
        if state.closed {
            return Err(ForgeError::Cancelled("connection closed".into()));
        }
        if state.streams.len() >= self.max_streams {
            return Err(ForgeError::Conflict(format!(
                "too many subscriptions on this connection (limit {})",
                self.max_streams
            )));
        }
        state.streams.insert(
            id,
            StreamSlot {
                lagged,
                active: false,
                queue: DropOldestQueue::new(capacity.unwrap_or(self.default_capacity)),
                token: token.clone(),
            },
        );
        Ok((id, token))
    }

    /// Queues an already serialized notification frame. `false` when the stream is gone.
    pub(crate) fn push_item(&self, id: SubscriptionId, frame: String) -> bool {
        let wake = {
            let mut state = self.state.lock();
            match state.streams.get_mut(&id) {
                Some(slot) => {
                    slot.queue.push(frame);
                    slot.active
                }
                None => return false,
            }
        };
        if wake {
            self.wake_writer.notify_one();
        }
        true
    }

    /// Records upstream loss for a stream.
    pub(crate) fn push_lagged(&self, id: SubscriptionId, missed: u64) -> bool {
        let wake = {
            let mut state = self.state.lock();
            match state.streams.get_mut(&id) {
                Some(slot) => {
                    slot.queue.push_lagged(missed);
                    slot.active
                }
                None => return false,
            }
        };
        if wake {
            self.wake_writer.notify_one();
        }
        true
    }

    /// Whether the stream's queue has room (used by replay to avoid self-inflicted drops).
    pub(crate) fn has_room(&self, id: SubscriptionId) -> Option<bool> {
        let state = self.state.lock();
        state.streams.get(&id).map(|slot| !slot.queue.is_full())
    }

    /// Waits until the writer drained a batch (or `max_wait`).
    pub(crate) async fn wait_for_space(&self, max_wait: Duration) {
        let _ = tokio::time::timeout(max_wait, self.space.notified()).await;
    }

    /// Removes a stream and stops its producer. `false` if unknown.
    pub(crate) fn close_stream(&self, id: SubscriptionId) -> bool {
        let slot = self.state.lock().streams.remove(&id);
        match slot {
            Some(slot) => {
                slot.token.cancel();
                true
            }
            None => false,
        }
    }

    /// Closes the outbox once the queued responses are written (the reader is done: the
    /// client left or broke the protocol). Streams stop now, so a busy stream cannot
    /// keep the connection open.
    pub(crate) fn close_after_drain(&self) {
        let streams = {
            let mut state = self.state.lock();
            state.drain_then_close = true;
            std::mem::take(&mut state.streams)
        };
        for slot in streams.into_values() {
            slot.token.cancel();
        }
        self.wake_writer.notify_one();
    }

    /// Closes now: queued data is discarded, every stream producer is stopped.
    pub(crate) fn close(&self) {
        let streams = {
            let mut state = self.state.lock();
            state.closed = true;
            state.responses.clear();
            std::mem::take(&mut state.streams)
        };
        for slot in streams.into_values() {
            slot.token.cancel();
        }
        self.wake_writer.notify_one();
        self.space.notify_waiters();
    }

    pub(crate) async fn writer_wakeup(&self) {
        self.wake_writer.notified().await;
    }

    pub(crate) fn batch_written(&self) {
        self.space.notify_waiters();
    }

    /// Takes up to `max` frames: every queued response first, then stream items in
    /// round-robin.
    pub(crate) fn take_batch(&self, max: usize) -> Batch {
        let mut out: Vec<Out> = Vec::new();
        let mut permits = Vec::new();
        let finished = {
            let mut state = self.state.lock();
            while out.len() < max {
                let Some((frame, permit)) = state.responses.pop_front() else {
                    break;
                };
                out.push(Out::Frame(frame));
                permits.extend(permit);
            }
            // Round-robin over active streams, one entry per stream per pass.
            loop {
                if out.len() >= max {
                    break;
                }
                let mut progressed = false;
                let start = state.cursor;
                let ids: Vec<SubscriptionId> = match start {
                    Some(c) => state
                        .streams
                        .range((Bound::Excluded(c), Bound::Unbounded))
                        .map(|(id, _)| *id)
                        .chain(state.streams.range(..=c).map(|(id, _)| *id))
                        .collect(),
                    None => state.streams.keys().copied().collect(),
                };
                for id in ids {
                    if out.len() >= max {
                        break;
                    }
                    let Some(slot) = state.streams.get_mut(&id) else {
                        continue;
                    };
                    if !slot.active {
                        continue;
                    }
                    if let Some(popped) = slot.queue.pop() {
                        progressed = true;
                        out.push(match popped {
                            Popped::Item(frame) => Out::Frame(frame),
                            Popped::Lagged(missed) => Out::Lagged {
                                method: slot.lagged,
                                id,
                                missed,
                            },
                        });
                        state.cursor = Some(id);
                    }
                }
                if !progressed {
                    break;
                }
            }
            out.is_empty() && (state.closed || state.drain_then_close)
        };
        let frames = out
            .into_iter()
            .filter_map(|o| match o {
                Out::Frame(frame) => Some(frame),
                Out::Lagged { method, id, missed } => notification_frame(
                    method,
                    &LaggedDto {
                        subscription_id: id,
                        missed,
                    },
                )
                .ok(),
            })
            .collect();
        Batch {
            frames,
            permits,
            finished,
        }
    }
}

/// Producer handle of one stream subscription (given to stream handlers through
/// [`super::Ctx::open_stream`]). Cheap to clone. Sending never blocks.
#[derive(Clone)]
pub struct StreamSender {
    id: SubscriptionId,
    notification: &'static str,
    outbox: Arc<Outbox>,
    token: CancellationToken,
}

impl std::fmt::Debug for StreamSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamSender")
            .field("id", &self.id)
            .field("notification", &self.notification)
            .field("closed", &self.token.is_cancelled())
            .finish()
    }
}

impl StreamSender {
    pub(crate) fn new(
        id: SubscriptionId,
        notification: &'static str,
        outbox: Arc<Outbox>,
        token: CancellationToken,
    ) -> Self {
        Self {
            id,
            notification,
            outbox,
            token,
        }
    }

    /// Id of the subscription (put it in every envelope).
    pub fn id(&self) -> SubscriptionId {
        self.id
    }

    /// Queues one item (the params of the item notification). Drops the oldest queued
    /// item when the subscriber is behind. `false` once the stream is closed.
    pub fn send<T: Serialize>(&self, item: &T) -> bool {
        if self.token.is_cancelled() {
            return false;
        }
        match notification_frame(self.notification, item) {
            Ok(frame) => self.outbox.push_item(self.id, frame),
            Err(err) => {
                tracing::error!(%err, notification = self.notification, "stream item not serializable");
                true
            }
        }
    }

    /// Like [`StreamSender::send`] but first waits (up to `max_wait`) for room in the
    /// queue: used when replaying history, so a burst does not drop its own items.
    pub async fn send_wait<T: Serialize>(&self, item: &T, max_wait: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + max_wait;
        loop {
            match self.outbox.has_room(self.id) {
                None => return false,
                Some(true) => break,
                Some(false) => {
                    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if left.is_zero() || self.token.is_cancelled() {
                        break;
                    }
                    tokio::select! {
                        () = self.outbox.wait_for_space(left) => {}
                        () = self.token.cancelled() => return false,
                    }
                }
            }
        }
        self.send(item)
    }

    /// Reports `missed` items lost upstream at this point of the stream.
    pub fn lagged(&self, missed: u64) {
        if missed > 0 {
            self.outbox.push_lagged(self.id, missed);
        }
    }

    /// The subscriber unsubscribed or disconnected.
    pub fn is_closed(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Resolves when the stream is closed; producers `select!` on it.
    pub async fn closed(&self) {
        self.token.cancelled().await;
    }

    /// Ends the stream from the producer side.
    pub fn close(&self) {
        self.outbox.close_stream(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outbox() -> Arc<Outbox> {
        Arc::new(Outbox::new(CancellationToken::new(), 3, 2))
    }

    #[test]
    fn responses_come_first_and_streams_wait_for_activation() {
        let outbox = outbox();
        let (id, _token) = outbox.open_stream("events.lagged", None).unwrap();
        assert!(outbox.push_item(id, "item1".into()));
        outbox.push_response("resp0".into(), None, None);
        let batch = outbox.take_batch(10);
        assert_eq!(batch.frames, ["resp0"], "inactive stream is held");
        outbox.push_response("resp1".into(), None, Some(id));
        let batch = outbox.take_batch(10);
        assert_eq!(batch.frames, ["resp1", "item1"]);
        assert!(!batch.finished);
    }

    #[test]
    fn full_stream_reports_lag() {
        let outbox = outbox();
        let (id, _token) = outbox.open_stream("logs.lagged", None).unwrap();
        outbox.push_response("ok".into(), None, Some(id));
        for i in 0..5 {
            outbox.push_item(id, format!("i{i}"));
        }
        let frames = outbox.take_batch(10).frames;
        assert_eq!(frames[0], "ok");
        let lagged: serde_json::Value = serde_json::from_str(&frames[1]).unwrap();
        assert_eq!(lagged["method"], "logs.lagged");
        assert_eq!(lagged["params"]["missed"], 2);
        assert_eq!(lagged["params"]["subscription_id"], id.to_string());
        assert_eq!(&frames[2..], ["i2", "i3", "i4"]);
    }

    #[test]
    fn stream_limit_and_close() {
        let outbox = outbox();
        let (a, token_a) = outbox.open_stream("y", None).unwrap();
        outbox.open_stream("y", None).unwrap();
        let err = outbox.open_stream("y", None).unwrap_err();
        assert_eq!(err.code(), "conflict");
        assert!(outbox.close_stream(a));
        assert!(token_a.is_cancelled());
        assert!(!outbox.push_item(a, "late".into()));
        outbox.close();
        assert!(outbox.take_batch(10).finished);
        assert!(outbox.open_stream("y", None).is_err());
    }

    #[test]
    fn round_robin_between_streams() {
        let outbox = outbox();
        let (a, _ta) = outbox.open_stream("a.lagged", None).unwrap();
        let (b, _tb) = outbox.open_stream("b.lagged", None).unwrap();
        outbox.push_response("ra".into(), None, Some(a));
        outbox.push_response("rb".into(), None, Some(b));
        let _ = outbox.take_batch(10);
        for i in 0..3 {
            outbox.push_item(a, format!("a{i}"));
            outbox.push_item(b, format!("b{i}"));
        }
        let frames = outbox.take_batch(4).frames;
        assert_eq!(frames.len(), 4);
        assert_eq!(frames.iter().filter(|f| f.starts_with('a')).count(), 2);
        assert_eq!(frames.iter().filter(|f| f.starts_with('b')).count(), 2);
    }
}
