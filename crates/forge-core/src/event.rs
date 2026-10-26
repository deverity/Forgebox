//! Domain events and the in-process event bus.
//!
//! Every [`Event`] carries a ULID, a `seq` that grows strictly by one per published
//! event (ADR-0009, rule 3: clients resume with "since seq N"), a UTC timestamp, a
//! canonical [`EventKind`] (docs/03 §8) and a JSON payload. Typed payload structs are
//! versioned in `forge-ipc` (M1-E5-T1); the bus only transports them.
//!
//! [`EventBus`] wraps `tokio::sync::broadcast`: publishing never blocks and never waits
//! for subscribers. A subscriber that falls behind loses the oldest events
//! (drop-oldest, docs/03 §7.3); [`EventStream`] counts what was lost so the client can
//! resynchronise (for example from the `events` table by `seq`).

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use jiff::Timestamp;
use parking_lot::Mutex;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::sync::broadcast;

use crate::ids::EventId;

/// Default number of events buffered per subscriber before the oldest are dropped.
pub const DEFAULT_EVENT_CAPACITY: usize = 4096;

macro_rules! event_kinds {
    ($($(#[$meta:meta])* $variant:ident => $name:literal,)+) => {
        /// Canonical event names (docs/03 §8). Serialized as the dotted name.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[non_exhaustive]
        pub enum EventKind {
            $($(#[$meta])* $variant,)+
        }

        impl EventKind {
            /// Every kind, in declaration order.
            pub const ALL: &'static [EventKind] = &[$(EventKind::$variant,)+];

            /// Canonical dotted name, e.g. `"daemon.ready"`.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(EventKind::$variant => $name,)+
                }
            }
        }

        impl FromStr for EventKind {
            type Err = UnknownEventKind;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($name => Ok(EventKind::$variant),)+
                    _ => Err(UnknownEventKind(s.to_owned())),
                }
            }
        }
    };
}

event_kinds! {
    DaemonReady => "daemon.ready",
    DaemonShutdown => "daemon.shutdown",
    JobStarted => "job.started",
    JobProgress => "job.progress",
    JobDone => "job.done",
    JobFailed => "job.failed",
    JobCancelled => "job.cancelled",
    ActorRestarted => "actor.restarted",
    FsCaseSensitivityApplied => "fs.case_sensitivity_applied",
    FsCaseSensitivityUnavailable => "fs.case_sensitivity_unavailable",
    ToolInstallStarted => "tool.install.started",
    ToolInstallProgress => "tool.install.progress",
    ToolInstallCompleted => "tool.install.completed",
    ToolInstallFailed => "tool.install.failed",
    ToolUpdated => "tool.updated",
    ToolRollback => "tool.rollback",
    ServiceStarting => "service.starting",
    ServiceRunning => "service.running",
    ServiceStopping => "service.stopping",
    ServiceStopped => "service.stopped",
    ServiceCrashed => "service.crashed",
    ServiceFailed => "service.failed",
    PoolScaled => "pool.scaled",
    PoolWorkerRecycled => "pool.worker.recycled",
    SiteCreated => "site.created",
    SiteUpdated => "site.updated",
    SiteRemoved => "site.removed",
    SitePhpChanged => "site.php_changed",
    ConfigRendered => "config.rendered",
    ConfigDriftDetected => "config.drift_detected",
    DnsPolicyApplied => "dns.policy_applied",
    TlsCaCreated => "tls.ca_created",
    TlsLeafIssued => "tls.leaf_issued",
    TlsRenewed => "tls.renewed",
    PortConflict => "port.conflict",
    /// Payload `{name, ok}` (docs/03 §8, `helper.op{name, ok}`).
    HelperOp => "helper.op",
}

/// A name that is not in [`EventKind`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown event kind {0:?}")]
pub struct UnknownEventKind(pub String);

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for EventKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        name.parse().map_err(serde::de::Error::custom)
    }
}

/// Something that happened in the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    /// Strictly increasing by one per published event (gaps only mean lost events).
    pub seq: u64,
    /// UTC instant of publication (RFC 3339 on the wire).
    pub ts: Timestamp,
    pub kind: EventKind,
    pub payload: serde_json::Value,
}

/// Multi-producer, multi-consumer event bus. Cheap to clone; all clones share the
/// same sequence counter and channel.
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Inner>,
}

struct Inner {
    sender: broadcast::Sender<Event>,
    /// Next `seq` to assign. Held only while numbering and handing the event to the
    /// channel (non-blocking), so subscribers always observe `seq` in order. Never held
    /// across `.await` (ADR-0011).
    next_seq: Mutex<u64>,
}

impl fmt::Debug for EventBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventBus")
            .field("next_seq", &*self.inner.next_seq.lock())
            .field("subscribers", &self.subscriber_count())
            .finish()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    /// Bus with [`DEFAULT_EVENT_CAPACITY`], numbering from `seq = 1`.
    pub fn new() -> Self {
        Self::starting_at(DEFAULT_EVENT_CAPACITY, 1)
    }

    /// Bus with a custom per-subscriber buffer, numbering from `seq = 1`.
    pub fn with_capacity(capacity: usize) -> Self {
        Self::starting_at(capacity, 1)
    }

    /// Bus whose first event gets `next_seq`. The daemon seeds it with
    /// `store.events().max_seq()? + 1` so `seq` keeps growing across restarts.
    ///
    /// # Panics
    /// If `capacity` is zero (tokio requirement).
    pub fn starting_at(capacity: usize, next_seq: u64) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            inner: Arc::new(Inner {
                sender,
                next_seq: Mutex::new(next_seq.max(1)),
            }),
        }
    }

    /// Numbers, timestamps and broadcasts an event. Never blocks on subscribers.
    /// Returns the event so the caller can persist it.
    pub fn publish(&self, kind: EventKind, payload: serde_json::Value) -> Event {
        let mut event = Event {
            id: EventId::new(),
            seq: 0,
            ts: Timestamp::now(),
            kind,
            payload,
        };
        // Clone outside the critical section; inside only numbering, the clock read
        // (keeps `ts` non-decreasing with `seq`) and the non-blocking hand-off.
        let mut outgoing = event.clone();
        {
            let mut next_seq = self.inner.next_seq.lock();
            outgoing.seq = *next_seq;
            outgoing.ts = Timestamp::now();
            *next_seq += 1;
            event.seq = outgoing.seq;
            event.ts = outgoing.ts;
            // An error only means there is no subscriber right now; the event is not
            // lost for persistence because the caller gets it back.
            let _ = self.inner.sender.send(outgoing);
        }
        event
    }

    /// New subscription; it receives events published after this call.
    pub fn subscribe(&self) -> EventStream {
        EventStream {
            receiver: self.inner.sender.subscribe(),
            missed: 0,
        }
    }

    /// Number of live subscriptions.
    pub fn subscriber_count(&self) -> usize {
        self.inner.sender.receiver_count()
    }

    /// `seq` of the last published event (0 if none yet).
    pub fn last_seq(&self) -> u64 {
        *self.inner.next_seq.lock() - 1
    }
}

/// What a subscription yields when the caller wants to see gaps explicitly.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamItem {
    Event(Event),
    /// The subscriber was too slow and `missed` events were dropped (oldest first).
    Lagged {
        missed: u64,
    },
}

/// Receiving side of [`EventBus::subscribe`].
#[derive(Debug)]
pub struct EventStream {
    receiver: broadcast::Receiver<Event>,
    missed: u64,
}

impl EventStream {
    /// Next event, skipping over gaps (counted in [`EventStream::missed`]).
    /// `None` when every [`EventBus`] clone was dropped.
    pub async fn recv(&mut self) -> Option<Event> {
        loop {
            match self.recv_item().await? {
                StreamItem::Event(event) => return Some(event),
                StreamItem::Lagged { .. } => continue,
            }
        }
    }

    /// Next item, reporting gaps as [`StreamItem::Lagged`].
    pub async fn recv_item(&mut self) -> Option<StreamItem> {
        match self.receiver.recv().await {
            Ok(event) => Some(StreamItem::Event(event)),
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                self.missed += missed;
                Some(StreamItem::Lagged { missed })
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }

    /// Non-blocking receive. `None` when nothing is buffered or the bus is closed.
    pub fn try_recv(&mut self) -> Option<Event> {
        loop {
            match self.receiver.try_recv() {
                Ok(event) => return Some(event),
                Err(broadcast::error::TryRecvError::Lagged(missed)) => self.missed += missed,
                Err(_) => return None,
            }
        }
    }

    /// Total number of events this subscription lost because it was too slow.
    pub fn missed(&self) -> u64 {
        self.missed
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    #[test]
    fn kinds_round_trip_and_are_unique() {
        let mut names: Vec<_> = EventKind::ALL.iter().map(|k| k.as_str()).collect();
        for kind in EventKind::ALL {
            assert_eq!(kind.as_str().parse::<EventKind>().unwrap(), *kind);
            let json = serde_json::to_string(kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert_eq!(serde_json::from_str::<EventKind>(&json).unwrap(), *kind);
        }
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), EventKind::ALL.len());
        assert!("daemon.exploded".parse::<EventKind>().is_err());
    }

    #[test]
    fn canonical_names_from_engine_doc() {
        for name in [
            "daemon.ready",
            "job.cancelled",
            "fs.case_sensitivity_unavailable",
            "tool.install.completed",
            "pool.worker.recycled",
            "site.php_changed",
            "tls.leaf_issued",
            "helper.op",
        ] {
            assert!(name.parse::<EventKind>().is_ok(), "{name}");
        }
    }

    #[test]
    fn event_serializes_with_rfc3339_ts() {
        let bus = EventBus::new();
        let event = bus.publish(EventKind::SiteCreated, json!({"host": "loja.test"}));
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["kind"], "site.created");
        assert_eq!(value["seq"], 1);
        assert!(value["ts"].as_str().unwrap().ends_with('Z'));
        let back: Event = serde_json::from_value(value).unwrap();
        assert_eq!(back, event);
    }

    #[tokio::test]
    async fn subscribers_receive_in_order_with_increasing_seq() {
        let bus = EventBus::new();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        for i in 0..10 {
            bus.publish(EventKind::JobProgress, json!({ "i": i }));
        }
        for stream in [&mut a, &mut b] {
            for expected in 1..=10u64 {
                let event = stream.recv().await.unwrap();
                assert_eq!(event.seq, expected);
                assert_eq!(event.payload["i"], expected - 1);
            }
            assert_eq!(stream.missed(), 0);
        }
        assert_eq!(bus.last_seq(), 10);
    }

    #[tokio::test]
    async fn concurrent_publishers_keep_seq_gapless_and_ordered() {
        let bus = EventBus::with_capacity(10_000);
        let mut stream = bus.subscribe();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let bus = bus.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..500 {
                    bus.publish(EventKind::JobProgress, serde_json::Value::Null);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        for expected in 1..=4000u64 {
            assert_eq!(stream.recv().await.unwrap().seq, expected);
        }
    }

    #[tokio::test]
    async fn slow_subscriber_does_not_block_publisher_and_reports_lag() {
        let bus = EventBus::with_capacity(16);
        let mut slow = bus.subscribe();
        let publisher = {
            let bus = bus.clone();
            tokio::task::spawn_blocking(move || {
                for _ in 0..1000 {
                    bus.publish(EventKind::ToolInstallProgress, serde_json::Value::Null);
                }
            })
        };
        // The subscriber never reads while publishing: the publisher must still finish.
        tokio::time::timeout(Duration::from_secs(5), publisher)
            .await
            .expect("publisher blocked by a slow subscriber")
            .unwrap();

        match slow.recv_item().await.unwrap() {
            StreamItem::Lagged { missed } => assert_eq!(missed, 1000 - 16),
            other => panic!("expected lag, got {other:?}"),
        }
        let first = slow.recv().await.unwrap();
        assert_eq!(
            first.seq,
            1000 - 16 + 1,
            "oldest events are the ones dropped"
        );
        let mut last = first.seq;
        while let Some(event) = slow.try_recv() {
            assert_eq!(event.seq, last + 1);
            last = event.seq;
        }
        assert_eq!(last, 1000);
        assert_eq!(slow.missed(), 984);
    }

    #[tokio::test]
    async fn recv_skips_lag_and_counts_it() {
        let bus = EventBus::with_capacity(4);
        let mut stream = bus.subscribe();
        for _ in 0..10 {
            bus.publish(EventKind::JobProgress, serde_json::Value::Null);
        }
        assert_eq!(stream.recv().await.unwrap().seq, 7);
        assert_eq!(stream.missed(), 6);
    }

    #[tokio::test]
    async fn stream_ends_when_bus_dropped() {
        let bus = EventBus::new();
        let mut stream = bus.subscribe();
        bus.publish(EventKind::DaemonShutdown, serde_json::Value::Null);
        drop(bus);
        assert!(stream.recv().await.is_some());
        assert!(stream.recv().await.is_none());
    }

    #[test]
    fn starting_at_continues_numbering() {
        let bus = EventBus::starting_at(8, 42);
        assert_eq!(bus.last_seq(), 41);
        assert_eq!(bus.publish(EventKind::DaemonReady, json!({})).seq, 42);
        assert_eq!(bus.subscriber_count(), 0);
    }
}
