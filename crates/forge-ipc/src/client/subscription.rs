//! Client side of streams: a [`Subscription`] survives reconnections. The client keeps
//! the last `seq` received; when the connection drops it reconnects (with backoff) and
//! subscribes again with `resume_from = last seq`, so the consumer sees every item once
//! and in order (duplicates from the overlap are discarded by `seq`). Items the consumer
//! is too slow to take are dropped oldest-first locally and reported as lag.

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::Stream;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::Notify;
use tokio::sync::futures::OwnedNotified;

use forge_core::ids::SubscriptionId;
use forge_core::sync::{LockLevel, OrderedMutex};

use super::Shared;
use crate::api::{MethodInfo, SubscriptionDto, method_info};
use crate::stream_queue::{DropOldestQueue, Popped};

struct SubState {
    queue: DropOldestQueue<Value>,
    last_seq: Option<u64>,
    closed: bool,
    server_id: Option<SubscriptionId>,
    generation: u64,
}

/// Shared state of one subscription (consumer handle, reader task, resubscriber).
pub(crate) struct SubShared {
    pub(crate) local_id: u64,
    pub(crate) method: &'static MethodInfo,
    params: Value,
    /// Lock: `ipc.client` (rank 10), never nested.
    state: OrderedMutex<SubState>,
    notify: Arc<Notify>,
}

impl SubShared {
    pub(crate) fn new(
        local_id: u64,
        method: &'static MethodInfo,
        params: Value,
        capacity: usize,
    ) -> Self {
        let last_seq = params.get("resume_from").and_then(Value::as_u64);
        Self {
            local_id,
            method,
            params,
            state: OrderedMutex::new(
                LockLevel::IPC_CLIENT,
                SubState {
                    queue: DropOldestQueue::new(capacity),
                    last_seq,
                    closed: false,
                    server_id: None,
                    generation: 0,
                },
            ),
            notify: Arc::new(Notify::new()),
        }
    }

    pub(crate) fn item_method(&self) -> &'static str {
        self.method.notification.unwrap_or("")
    }

    pub(crate) fn lagged_method(&self) -> &'static str {
        self.method.lagged_notification.unwrap_or("")
    }

    /// Params for (re)subscribing: the original ones with `resume_from` = last seq.
    pub(crate) fn resume_params(&self) -> Value {
        let last = self.state.lock().last_seq;
        let mut params = self.params.clone();
        if let Value::Object(map) = &mut params {
            map.insert("resume_from".into(), last.map_or(Value::Null, Value::from));
        }
        params
    }

    pub(crate) fn last_seq(&self) -> Option<u64> {
        self.state.lock().last_seq
    }

    /// Records the subscription answer of a connection. If the stream's sequence is
    /// behind our cursor, the daemon's state was reset (new database): the cursor is
    /// dropped and a gap is reported, otherwise every new item would look like a
    /// duplicate.
    pub(crate) fn attach(&self, generation: u64, answer: &SubscriptionDto) {
        let reset = {
            let mut state = self.state.lock();
            state.generation = generation;
            state.server_id = Some(answer.subscription_id);
            let reset = state.last_seq.is_some_and(|last| answer.last_seq < last);
            if reset {
                state.last_seq = None;
                state.queue.push_lagged(1);
            }
            reset
        };
        if reset {
            tracing::warn!(
                method = self.method.name,
                "stream sequence restarted on the daemon; cursor reset"
            );
            self.notify.notify_one();
        }
    }

    /// `(generation, server id)` of the connection this subscription is attached to.
    pub(crate) fn attachment(&self) -> Option<(u64, SubscriptionId)> {
        let state = self.state.lock();
        state.server_id.map(|id| (state.generation, id))
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.state.lock().closed
    }

    /// Item notification from the server (params of the notification).
    pub(crate) fn deliver(&self, params: Value) {
        let seq = params.get("seq").and_then(Value::as_u64);
        {
            let mut state = self.state.lock();
            if state.closed {
                return;
            }
            if let (Some(seq), Some(last)) = (seq, state.last_seq) {
                if seq <= last {
                    // Overlap after a resubscription.
                    return;
                }
            }
            if seq.is_some() {
                state.last_seq = seq;
            }
            state.queue.push(params);
        }
        self.notify.notify_one();
    }

    pub(crate) fn lagged(&self, missed: u64) {
        self.state.lock().queue.push_lagged(missed);
        self.notify.notify_one();
    }

    pub(crate) fn close(&self) {
        self.state.lock().closed = true;
        self.notify.notify_one();
    }

    fn pop(&self) -> (Option<Popped<Value>>, bool) {
        let mut state = self.state.lock();
        (state.queue.pop(), state.closed)
    }
}

/// What a subscription yields when gaps must be visible.
#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptionItem<T> {
    Item(T),
    /// `missed` items were lost (slow consumer here, or slow subscriber on the server).
    Lagged {
        missed: u64,
    },
}

/// A stream subscription (`events.subscribe`, `logs.subscribe`, `metrics.subscribe`).
/// Implements [`Stream`] of items (lag is counted in [`Subscription::missed`]); use
/// [`Subscription::next_item`] to see the gaps. It keeps the client alive; dropping it
/// unsubscribes and [`super::Client::close`] ends it.
pub struct Subscription<T> {
    shared: Arc<SubShared>,
    client: Arc<Shared>,
    notified: Option<Pin<Box<OwnedNotified>>>,
    missed: u64,
    _item: PhantomData<fn() -> T>,
}

impl<T> std::fmt::Debug for Subscription<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("method", &self.shared.method.name)
            .field("last_seq", &self.shared.last_seq())
            .field("missed", &self.missed)
            .finish()
    }
}

impl<T: DeserializeOwned> Subscription<T> {
    pub(crate) fn new(shared: Arc<SubShared>, client: Arc<Shared>) -> Self {
        Self {
            shared,
            client,
            notified: None,
            missed: 0,
            _item: PhantomData,
        }
    }

    /// Last `seq` received from the daemon (resume point).
    pub fn last_seq(&self) -> Option<u64> {
        self.shared.last_seq()
    }

    /// Items lost so far.
    pub fn missed(&self) -> u64 {
        self.missed
    }

    /// Current server-side id (changes after a reconnection).
    pub fn subscription_id(&self) -> Option<SubscriptionId> {
        self.shared.attachment().map(|(_, id)| id)
    }

    /// Next item or gap; `None` once the subscription or the client is closed.
    pub async fn next_item(&mut self) -> Option<SubscriptionItem<T>> {
        std::future::poll_fn(|cx| self.poll_item(cx)).await
    }

    fn poll_item(&mut self, cx: &mut Context<'_>) -> Poll<Option<SubscriptionItem<T>>> {
        loop {
            match self.shared.pop() {
                (Some(Popped::Item(value)), _) => match serde_json::from_value::<T>(value) {
                    Ok(item) => return Poll::Ready(Some(SubscriptionItem::Item(item))),
                    Err(err) => {
                        tracing::warn!(%err, method = self.shared.method.name, "undecodable stream item skipped");
                        self.missed += 1;
                        continue;
                    }
                },
                (Some(Popped::Lagged(missed)), _) => {
                    self.missed += missed;
                    return Poll::Ready(Some(SubscriptionItem::Lagged { missed }));
                }
                (None, true) => return Poll::Ready(None),
                (None, false) => {}
            }
            let notify = Arc::clone(&self.shared.notify);
            let notified = self
                .notified
                .get_or_insert_with(|| Box::pin(notify.notified_owned()));
            match notified.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    self.notified = None;
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }

    /// Unsubscribes and waits for the daemon's acknowledgement (best effort).
    pub async fn unsubscribe(mut self) {
        if let Some((generation, id)) = self.detach() {
            self.client
                .unsubscribe_on(generation, self.shared.method, id)
                .await;
        }
    }

    /// Stops local delivery and forgets the subscription; returns its attachment.
    fn detach(&mut self) -> Option<(u64, SubscriptionId)> {
        if self.shared.is_closed() {
            return None;
        }
        self.shared.close();
        self.client.forget_subscription(self.shared.local_id);
        self.shared.attachment()
    }
}

impl<T: DeserializeOwned> Stream for Subscription<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let this = self.get_mut();
        loop {
            match this.poll_item(cx) {
                Poll::Ready(Some(SubscriptionItem::Item(item))) => return Poll::Ready(Some(item)),
                Poll::Ready(Some(SubscriptionItem::Lagged { .. })) => continue,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl<T> Drop for Subscription<T> {
    fn drop(&mut self) {
        if self.shared.is_closed() {
            return;
        }
        self.shared.close();
        self.client.forget_subscription(self.shared.local_id);
        if let Some((generation, id)) = self.shared.attachment() {
            self.client
                .unsubscribe_detached(generation, self.shared.method, id);
        }
    }
}

/// `events.subscribe` → `events.unsubscribe`.
pub(crate) fn unsubscribe_method(stream: &MethodInfo) -> Option<&'static MethodInfo> {
    method_info(&format!("{}.unsubscribe", stream.group()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sub() -> SubShared {
        SubShared::new(
            1,
            method_info("events.subscribe").unwrap(),
            json!({"filter": {}, "resume_from": 5}),
            3,
        )
    }

    #[test]
    fn dedupes_by_seq_and_tracks_cursor() {
        let s = sub();
        assert_eq!(s.last_seq(), Some(5));
        s.deliver(json!({"seq": 4}));
        s.deliver(json!({"seq": 5}));
        s.deliver(json!({"seq": 6}));
        s.deliver(json!({"seq": 6}));
        assert_eq!(s.last_seq(), Some(6));
        assert!(matches!(s.pop(), (Some(Popped::Item(v)), false) if v["seq"] == 6));
        assert!(matches!(s.pop(), (None, false)));
        assert_eq!(s.resume_params()["resume_from"], 6);
    }

    #[test]
    fn local_overflow_and_reset() {
        let s = sub();
        for seq in 6..12 {
            s.deliver(json!({ "seq": seq }));
        }
        assert!(matches!(s.pop(), (Some(Popped::Lagged(3)), false)));
        // The daemon answers with a sequence behind our cursor: state was reset.
        let id = SubscriptionId::new();
        s.attach(
            2,
            &SubscriptionDto {
                subscription_id: id,
                stream: "events".into(),
                last_seq: 3,
            },
        );
        assert_eq!(s.attachment(), Some((2, id)));
        assert_eq!(s.last_seq(), None);
        assert_eq!(s.resume_params()["resume_from"], Value::Null);
        s.close();
        assert!(s.is_closed());
    }

    #[test]
    fn unsubscribe_names() {
        for stream in ["events.subscribe", "logs.subscribe", "metrics.subscribe"] {
            let info = method_info(stream).unwrap();
            assert!(unsubscribe_method(info).is_some(), "{stream}");
        }
    }
}
