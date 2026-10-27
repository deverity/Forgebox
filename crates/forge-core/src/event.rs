//! Domain events and the in-process event bus.
//!
//! Every [`Event`] carries a ULID, a `seq` that grows strictly by one per published
//! event (ADR-0009, rule 3: clients resume with "since seq N"), a UTC timestamp, a
//! canonical [`EventKind`] (docs/03 §8) and a JSON payload. Typed payload structs are
//! serde structs with a version field `v`: the ones produced by forge-core itself live
//! next to their producer ([`crate::jobs::payload`], [`crate::actor::ActorRestartedPayload`]);
//! the others are versioned in `forge-ipc` (M1-E5-T1). The bus only transports them.
//!
//! [`EventBus`] wraps `tokio::sync::broadcast`: publishing never blocks and never waits
//! for subscribers. A subscriber that falls behind loses the oldest events
//! (drop-oldest, docs/03 §7.3); [`EventStream`] counts what was lost so the client can
//! resynchronise (for example from the `events` table by `seq`).
//!
//! The bus also keeps the last [`EventBusOptions::history`] events in memory (a ring
//! buffer). [`EventBus::subscribe_from`] atomically snapshots that ring and subscribes, so
//! an API client resuming from `seq` N gets every event after N exactly once and in order:
//! first what the ring still holds, then the live stream; whatever is older than the ring
//! is reported as a gap to be read from the `events` table (ADR-0009, rule 3).

use std::collections::VecDeque;
use std::fmt;
use std::ops::RangeInclusive;
use std::str::FromStr;
use std::sync::Arc;

use jiff::Timestamp;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::sync::broadcast;

use crate::ids::EventId;
use crate::sync::{LockLevel, OrderedMutex};

/// Default number of events buffered per subscriber before the oldest are dropped.
pub const DEFAULT_EVENT_CAPACITY: usize = 4096;
/// Default number of recent events kept in memory for [`EventBus::subscribe_from`].
pub const DEFAULT_EVENT_HISTORY: usize = 4096;

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
    /// An actor exceeded its restart budget and stays down (docs/03 §7.2).
    ActorFailed => "actor.failed",
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
    /// Install/upgrade in progress: the instance is held down (docs/03 §1.4).
    ServiceMaintenance => "service.maintenance",
    /// Periodic CPU/memory sample of an instance; opt-in per instance, off by default
    /// (docs/03 §6).
    ServiceMetrics => "service.metrics",
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
    history_capacity: usize,
    /// Next `seq` and the in-memory history. Held only while numbering, recording and
    /// handing the event to the channel (non-blocking), so subscribers always observe
    /// `seq` in order. Never held across `.await` (ADR-0011). Innermost lock
    /// ([`LockLevel::EVENT_BUS_SEQ`]): callers may publish while holding an outer lock
    /// (e.g. the job table) but never the reverse.
    state: OrderedMutex<BusState>,
}

struct BusState {
    next_seq: u64,
    /// Last published events, oldest first, at most `history_capacity`.
    history: VecDeque<Event>,
}

/// Construction parameters of an [`EventBus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventBusOptions {
    /// Events buffered per subscriber before the oldest are dropped (must be > 0).
    pub capacity: usize,
    /// Recent events kept in memory for [`EventBus::subscribe_from`] (0 disables replay
    /// from memory).
    pub history: usize,
    /// `seq` of the first event published by this bus.
    pub next_seq: u64,
}

impl Default for EventBusOptions {
    /// [`DEFAULT_EVENT_CAPACITY`], [`DEFAULT_EVENT_HISTORY`], numbering from 1.
    fn default() -> Self {
        Self {
            capacity: DEFAULT_EVENT_CAPACITY,
            history: DEFAULT_EVENT_HISTORY,
            next_seq: 1,
        }
    }
}

/// Result of [`EventBus::subscribe_from`].
#[derive(Debug)]
pub struct Resume {
    /// Events after the requested `seq` that are still in memory, oldest first.
    pub replay: Vec<Event>,
    /// Live subscription; its first event is the one right after the last of `replay`
    /// (or after the bus's last `seq` at the time of the call).
    pub stream: EventStream,
    /// Events the caller asked for that are older than the in-memory history (to be read
    /// from the `events` table). `None` when memory covers everything requested.
    pub gap: Option<RangeInclusive<u64>>,
    /// `seq` of the last event published before the subscription (0 if none).
    pub last_seq: u64,
}

impl fmt::Debug for EventBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (next_seq, history) = {
            let state = self.inner.state.lock();
            (state.next_seq, state.history.len())
        };
        f.debug_struct("EventBus")
            .field("next_seq", &next_seq)
            .field("history", &history)
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
    /// Bus with [`DEFAULT_EVENT_CAPACITY`] and [`DEFAULT_EVENT_HISTORY`], numbering from
    /// `seq = 1`.
    pub fn new() -> Self {
        Self::with_options(EventBusOptions::default())
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
        Self::with_options(EventBusOptions {
            capacity,
            next_seq,
            ..EventBusOptions::default()
        })
    }

    /// Bus with explicit [`EventBusOptions`].
    ///
    /// # Panics
    /// If `options.capacity` is zero (tokio requirement).
    pub fn with_options(options: EventBusOptions) -> Self {
        let (sender, _) = broadcast::channel(options.capacity);
        Self {
            inner: Arc::new(Inner {
                sender,
                history_capacity: options.history,
                state: OrderedMutex::new(
                    LockLevel::EVENT_BUS_SEQ,
                    BusState {
                        next_seq: options.next_seq.max(1),
                        history: VecDeque::with_capacity(options.history.min(1024)),
                    },
                ),
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
        // (keeps `ts` non-decreasing with `seq`), the ring update and the non-blocking
        // hand-off.
        let mut outgoing = event.clone();
        let mut recorded = (self.inner.history_capacity > 0).then(|| event.clone());
        let evicted = {
            let mut state = self.inner.state.lock();
            let seq = state.next_seq;
            let ts = Timestamp::now();
            state.next_seq += 1;
            outgoing.seq = seq;
            outgoing.ts = ts;
            event.seq = seq;
            event.ts = ts;
            let mut evicted = None;
            if let Some(mut copy) = recorded.take() {
                copy.seq = seq;
                copy.ts = ts;
                state.history.push_back(copy);
                if state.history.len() > self.inner.history_capacity {
                    evicted = state.history.pop_front();
                }
            }
            // An error only means there is no subscriber right now; the event is not
            // lost for persistence because the caller gets it back.
            let _ = self.inner.sender.send(outgoing);
            evicted
        };
        // Dropped outside the lock (payloads can be large).
        drop(evicted);
        event
    }

    /// New subscription; it receives events published after this call.
    pub fn subscribe(&self) -> EventStream {
        EventStream {
            receiver: self.inner.sender.subscribe(),
            missed: 0,
        }
    }

    /// Subscription that resumes after `after_seq`: returns the in-memory events with
    /// `seq > after_seq` and a live stream starting right after them, taken atomically
    /// (no duplicate, no hole). Events older than the history are reported in
    /// [`Resume::gap`]. A cursor at or beyond the last published `seq` replays nothing.
    pub fn subscribe_from(&self, after_seq: u64) -> Resume {
        let (receiver, replay, oldest, last_seq) = {
            let state = self.inner.state.lock();
            let receiver = self.inner.sender.subscribe();
            let last_seq = state.next_seq - 1;
            let oldest = state.history.front().map(|e| e.seq);
            // `history` is sorted by `seq`: skip what the caller already has.
            let start = state.history.partition_point(|e| e.seq <= after_seq);
            let replay: Vec<Event> = state.history.range(start..).cloned().collect();
            (receiver, replay, oldest, last_seq)
        };
        let wanted_from = after_seq.saturating_add(1);
        let available_from = oldest.unwrap_or(last_seq.saturating_add(1));
        let gap = (after_seq < last_seq && wanted_from < available_from)
            .then(|| wanted_from..=(available_from - 1).min(last_seq));
        Resume {
            replay,
            stream: EventStream {
                receiver,
                missed: 0,
            },
            gap,
            last_seq,
        }
    }

    /// Up to `limit` in-memory events with `seq > after_seq`, oldest first.
    pub fn history_since(&self, after_seq: u64, limit: usize) -> Vec<Event> {
        let state = self.inner.state.lock();
        let start = state.history.partition_point(|e| e.seq <= after_seq);
        state.history.range(start..).take(limit).cloned().collect()
    }

    /// `seq` of the oldest event still in memory (`None` when the history is empty).
    pub fn oldest_in_history(&self) -> Option<u64> {
        self.inner.state.lock().history.front().map(|e| e.seq)
    }

    /// Number of live subscriptions.
    pub fn subscriber_count(&self) -> usize {
        self.inner.sender.receiver_count()
    }

    /// `seq` of the last published event (0 if none yet).
    pub fn last_seq(&self) -> u64 {
        self.inner.state.lock().next_seq - 1
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
    fn history_keeps_the_last_events() {
        let bus = EventBus::with_options(EventBusOptions {
            capacity: 8,
            history: 3,
            next_seq: 1,
        });
        assert_eq!(bus.oldest_in_history(), None);
        for _ in 0..5 {
            bus.publish(EventKind::JobProgress, serde_json::Value::Null);
        }
        assert_eq!(bus.oldest_in_history(), Some(3));
        let seqs: Vec<u64> = bus.history_since(0, 10).iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [3, 4, 5]);
        let seqs: Vec<u64> = bus.history_since(3, 1).iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [4]);
        assert!(bus.history_since(5, 10).is_empty());
    }

    #[tokio::test]
    async fn subscribe_from_replays_then_streams_without_duplicates() {
        let bus = EventBus::with_options(EventBusOptions {
            capacity: 64,
            history: 4,
            next_seq: 1,
        });
        for _ in 0..6 {
            bus.publish(EventKind::JobProgress, serde_json::Value::Null);
        }
        // Ring holds 3..=6. Resuming after 4 needs 5 and 6 from memory, no gap.
        let mut resume = bus.subscribe_from(4);
        assert_eq!(resume.last_seq, 6);
        assert_eq!(resume.gap, None);
        let replayed: Vec<u64> = resume.replay.iter().map(|e| e.seq).collect();
        assert_eq!(replayed, [5, 6]);
        bus.publish(EventKind::JobProgress, serde_json::Value::Null);
        assert_eq!(resume.stream.recv().await.unwrap().seq, 7);

        // Resuming after 0 needs 1..=2 from the store; memory starts at 4 now.
        let resume = bus.subscribe_from(0);
        assert_eq!(resume.gap, Some(1..=3));
        assert_eq!(resume.replay.first().map(|e| e.seq), Some(4));

        // Up to date or ahead: nothing to replay, no gap.
        for cursor in [7, 100] {
            let resume = bus.subscribe_from(cursor);
            assert!(resume.replay.is_empty());
            assert_eq!(resume.gap, None);
        }
    }

    #[test]
    fn subscribe_from_without_history_reports_the_whole_gap() {
        let bus = EventBus::with_options(EventBusOptions {
            capacity: 8,
            history: 0,
            next_seq: 10,
        });
        for _ in 0..3 {
            bus.publish(EventKind::JobProgress, serde_json::Value::Null);
        }
        let resume = bus.subscribe_from(9);
        assert!(resume.replay.is_empty());
        assert_eq!(resume.gap, Some(10..=12));
        assert_eq!(bus.subscribe_from(12).gap, None);
    }

    #[test]
    fn starting_at_continues_numbering() {
        let bus = EventBus::starting_at(8, 42);
        assert_eq!(bus.last_seq(), 41);
        assert_eq!(bus.publish(EventKind::DaemonReady, json!({})).seq, 42);
        assert_eq!(bus.subscriber_count(), 0);
    }
}
