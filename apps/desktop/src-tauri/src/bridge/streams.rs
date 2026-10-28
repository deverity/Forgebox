//! Streams to the webview (`ipc_subscribe`): one task per stream pumps a resumable
//! forge-ipc subscription into a [`FrameSink`] (a Tauri `Channel` in the app) in frames.
//!
//! * **Frames**: a frame leaves at most [`PumpOptions::max_delay`] (16 ms) after its first
//!   entry, or as soon as it holds [`PumpOptions::max_items`] (256) entries, so the
//!   webview gets at most one IPC message per frame of the display per stream and never a
//!   giant one.
//! * **Bounded buffer and lag**: at most [`PumpOptions::window`] frames are in flight; the
//!   webview acknowledges each frame (`ipc_ack`) after handling it. While the window is
//!   full the pump stops pulling and items wait in the subscription's own drop-oldest queue
//!   (`ClientOptions::stream_capacity`, 4096): a stalled webview costs bounded memory, and
//!   what was dropped arrives as a `{kind: "lagged", missed}` entry exactly where it was
//!   lost (daemon-side losses arrive the same way).
//! * **Reset**: when the daemon's sequence restarted (new state: the subscription's cursor
//!   went backwards), a `{kind: "reset"}` entry tells the webview that what it holds may
//!   be stale and its snapshots must be fetched again.
//! * **End**: a `{kind: "end", error}` entry is the last one of a stream that ended on its
//!   own (context closed, subscription refused after a reconnection). Streams the webview
//!   unsubscribes from end silently.
//!
//! The registry ([`Streams`]) maps stream ids to their control handles. Lock
//! `desktop.streams` (rank 10, leaf): map operations only, never held across `.await`.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use forge_ipc::ApiError;
use forge_ipc::base::ids::SubscriptionId;
use forge_ipc::base::sync::{LockLevel, OrderedMutex};
use forge_ipc::client::{Subscription, SubscriptionItem};

/// Lock of the stream registry: leaf, map operations only.
pub const STREAMS_LOCK: LockLevel = LockLevel::new(10, "desktop.streams");

/// One entry of a frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamEntry {
    /// A stream item: the notification params as the daemon sent them
    /// (`{subscription_id, seq, event|line|sample}`).
    Item { item: Value },
    /// `missed` items were lost here (slow webview, slow connection or daemon-side lag).
    Lagged { missed: u64 },
    /// The daemon's sequence restarted: refetch snapshots.
    Reset,
    /// Last entry of a stream that ended by itself; `error` says why when it failed.
    End { error: Option<ApiError> },
}

/// What one Channel message carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamFrame {
    /// The bridge's stream id (from `ipc_subscribe`).
    pub stream_id: String,
    /// 1, 2, 3...: acknowledged with `ipc_ack` once handled.
    pub frame: u64,
    pub entries: Vec<StreamEntry>,
}

/// Where frames go. The Tauri `Channel` in the app; collectors in tests.
pub trait FrameSink: Send + Sync + 'static {
    /// Hands one frame over without waiting. `Err` when the receiver is gone.
    fn send(&self, frame: StreamFrame) -> Result<(), SinkClosed>;
}

/// The receiver of a [`FrameSink`] is gone (webview closed or reloaded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkClosed;

impl FrameSink for tauri::ipc::Channel<StreamFrame> {
    fn send(&self, frame: StreamFrame) -> Result<(), SinkClosed> {
        tauri::ipc::Channel::send(self, frame).map_err(|err| {
            tracing::debug!(%err, "stream channel closed");
            SinkClosed
        })
    }
}

/// Framing and flow control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PumpOptions {
    /// Entries per frame at most.
    pub max_items: usize,
    /// A frame leaves at most this long after its first entry.
    pub max_delay: Duration,
    /// Frames sent and not acknowledged at most.
    pub window: u64,
    /// Deadline of the `*.unsubscribe` sent when a stream is closed.
    pub unsubscribe_timeout: Duration,
}

impl Default for PumpOptions {
    fn default() -> Self {
        Self {
            max_items: 256,
            max_delay: Duration::from_millis(16),
            window: 4,
            unsubscribe_timeout: Duration::from_secs(2),
        }
    }
}

/// What the pump reads: a subscription, or a fake in tests.
pub trait ItemSource: Send {
    /// Next item or gap; `None` once the subscription is closed. Cancel safe.
    fn pull(&mut self) -> impl Future<Output = Option<SubscriptionItem<Value>>> + Send;
    /// Where the subscription would resume (its cursor); goes backwards on a reset.
    fn cursor(&self) -> Option<u64>;
}

impl ItemSource for Subscription<Value> {
    fn pull(&mut self) -> impl Future<Output = Option<SubscriptionItem<Value>>> + Send {
        Subscription::next_item(self)
    }

    fn cursor(&self) -> Option<u64> {
        Subscription::resume_point(self)
    }
}

/// Flow control of one stream: cancellation and the acknowledged frame.
#[derive(Debug)]
pub struct PumpControl {
    pub cancel: CancellationToken,
    acked: watch::Receiver<u64>,
    sent: u64,
}

impl PumpControl {
    /// A control and the sender the acknowledgements go to.
    pub fn new(cancel: CancellationToken) -> (Self, watch::Sender<u64>) {
        let (tx, rx) = watch::channel(0);
        (
            Self {
                cancel,
                acked: rx,
                sent: 0,
            },
            tx,
        )
    }

    /// Frames sent so far.
    pub fn sent(&self) -> u64 {
        self.sent
    }

    /// Waits until fewer than `window` frames are unacknowledged. `false` when cancelled
    /// or when the acknowledgement sender is gone (stream removed).
    async fn wait_window(&mut self, window: u64) -> bool {
        loop {
            if self.cancel.is_cancelled() {
                return false;
            }
            let acked = *self.acked.borrow_and_update();
            if self.sent.saturating_sub(acked) < window.max(1) {
                return true;
            }
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => return false,
                changed = self.acked.changed() => {
                    if changed.is_err() {
                        return false;
                    }
                }
            }
        }
    }
}

/// How a pump ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpEnd {
    /// Unsubscribed, or the stream was closed by the bridge.
    Cancelled,
    /// The subscription ended (client closed); an `end` entry was sent.
    SourceEnded,
    /// The webview side is gone.
    SinkClosed,
}

/// Builds the entries of a frame and detects resets.
#[derive(Debug, Default)]
struct Framer {
    /// `seq` of the last item handed to the webview.
    last_seq: Option<u64>,
    entries: Vec<StreamEntry>,
}

impl Framer {
    fn push(&mut self, item: SubscriptionItem<Value>, cursor: Option<u64>) {
        match item {
            SubscriptionItem::Item(value) => {
                let seq = value.get("seq").and_then(Value::as_u64);
                // The client drops overlap duplicates, so a seq that does not grow means
                // its cursor was reset (the daemon came back with a fresh state).
                if let (Some(seq), Some(last)) = (seq, self.last_seq) {
                    if seq <= last {
                        self.entries.push(StreamEntry::Reset);
                    }
                }
                if seq.is_some() {
                    self.last_seq = seq;
                }
                self.entries.push(StreamEntry::Item { item: value });
            }
            SubscriptionItem::Lagged { missed } => {
                // A reset reaches the subscription as a gap while its cursor goes back
                // below what was already delivered: say `reset`, not `lagged`.
                if let (Some(cursor), Some(last)) = (cursor, self.last_seq) {
                    if cursor < last {
                        self.entries.push(StreamEntry::Reset);
                        self.last_seq = None;
                        return;
                    }
                }
                if let Some(StreamEntry::Lagged { missed: total }) = self.entries.last_mut() {
                    *total += missed;
                } else {
                    self.entries.push(StreamEntry::Lagged { missed });
                }
            }
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn take(&mut self) -> Vec<StreamEntry> {
        std::mem::take(&mut self.entries)
    }
}

/// Pumps `source` into `sink` until cancelled, ended or the sink is gone.
pub async fn pump<S: ItemSource>(
    source: &mut S,
    stream_id: &str,
    sink: &dyn FrameSink,
    control: &mut PumpControl,
    options: &PumpOptions,
) -> PumpEnd {
    let mut framer = Framer::default();
    let max_items = options.max_items.max(1);
    loop {
        if !control.wait_window(options.window).await {
            return PumpEnd::Cancelled;
        }
        let first = tokio::select! {
            biased;
            () = control.cancel.cancelled() => return PumpEnd::Cancelled,
            item = source.pull() => item,
        };
        let mut ended = false;
        match first {
            Some(item) => {
                let cursor = source.cursor();
                framer.push(item, cursor);
                let deadline = tokio::time::Instant::now() + options.max_delay;
                while framer.len() < max_items {
                    tokio::select! {
                        biased;
                        () = control.cancel.cancelled() => return PumpEnd::Cancelled,
                        () = tokio::time::sleep_until(deadline) => break,
                        item = source.pull() => match item {
                            Some(item) => {
                                let cursor = source.cursor();
                                framer.push(item, cursor);
                            }
                            None => {
                                ended = true;
                                break;
                            }
                        },
                    }
                }
            }
            None => ended = true,
        }
        let mut entries = framer.take();
        if ended {
            entries.push(StreamEntry::End { error: None });
        }
        control.sent += 1;
        let frame = StreamFrame {
            stream_id: stream_id.to_owned(),
            frame: control.sent,
            entries,
        };
        if sink.send(frame).is_err() {
            return PumpEnd::SinkClosed;
        }
        if ended {
            return PumpEnd::SourceEnded;
        }
    }
}

/// Sends the `end` frame of a stream that could not continue.
pub fn send_end(
    sink: &dyn FrameSink,
    stream_id: &str,
    control: &mut PumpControl,
    error: Option<ApiError>,
) {
    control.sent += 1;
    let _ = sink.send(StreamFrame {
        stream_id: stream_id.to_owned(),
        frame: control.sent,
        entries: vec![StreamEntry::End { error }],
    });
}

/// Control handle of a registered stream.
#[derive(Debug)]
struct StreamCtl {
    context: String,
    /// Webview that subscribed (its streams die with its page).
    owner: String,
    cancel: CancellationToken,
    acked: watch::Sender<u64>,
}

/// Registry of open streams.
#[derive(Debug)]
pub struct Streams {
    /// Lock `desktop.streams` (rank 10, leaf).
    map: OrderedMutex<HashMap<String, StreamCtl>>,
}

impl Default for Streams {
    fn default() -> Self {
        Self {
            map: OrderedMutex::new(STREAMS_LOCK, HashMap::new()),
        }
    }
}

impl Streams {
    /// Registers a new stream; returns its id and its control.
    pub fn register(&self, context: &str, owner: &str) -> (String, PumpControl) {
        let id = SubscriptionId::new().to_string();
        let cancel = CancellationToken::new();
        let (control, acked) = PumpControl::new(cancel.clone());
        self.map.lock().insert(
            id.clone(),
            StreamCtl {
                context: context.to_owned(),
                owner: owner.to_owned(),
                cancel,
                acked,
            },
        );
        (id, control)
    }

    /// Records that the webview handled every frame up to `frame`. `false` for an unknown
    /// stream (already closed).
    pub fn ack(&self, stream_id: &str, frame: u64) -> bool {
        let map = self.map.lock();
        let Some(ctl) = map.get(stream_id) else {
            return false;
        };
        ctl.acked.send_if_modified(|acked| {
            if frame > *acked {
                *acked = frame;
                true
            } else {
                false
            }
        });
        true
    }

    /// Closes `stream_id` of `context`. `false` when there is no such stream.
    pub fn close(&self, context: &str, stream_id: &str) -> bool {
        let removed = {
            let mut map = self.map.lock();
            match map.get(stream_id) {
                Some(ctl) if ctl.context == context => map.remove(stream_id),
                _ => None,
            }
        };
        removed.map(|ctl| ctl.cancel.cancel()).is_some()
    }

    /// Forgets a stream that ended by itself.
    pub fn forget(&self, stream_id: &str) {
        self.map.lock().remove(stream_id);
    }

    /// Closes every stream opened by the webview `owner` (its page is being replaced).
    pub fn close_owner(&self, owner: &str) -> usize {
        let removed: Vec<StreamCtl> = {
            let mut map = self.map.lock();
            let ids: Vec<String> = map
                .iter()
                .filter(|(_, ctl)| ctl.owner == owner)
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter().filter_map(|id| map.remove(id)).collect()
        };
        for ctl in &removed {
            ctl.cancel.cancel();
        }
        removed.len()
    }

    /// Closes every stream.
    pub fn close_all(&self) -> usize {
        let removed: Vec<StreamCtl> = self.map.lock().drain().map(|(_, ctl)| ctl).collect();
        for ctl in &removed {
            ctl.cancel.cancel();
        }
        removed.len()
    }

    /// Open streams.
    pub fn len(&self) -> usize {
        self.map.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use serde_json::json;

    use super::*;

    /// A source with items ready up front; `None` once drained if `ends`.
    struct Fake {
        items: VecDeque<SubscriptionItem<Value>>,
        cursor: Option<u64>,
        ends: bool,
    }

    impl Fake {
        fn seqs(range: std::ops::RangeInclusive<u64>) -> Self {
            Self {
                items: range
                    .map(|seq| SubscriptionItem::Item(json!({ "seq": seq })))
                    .collect(),
                cursor: None,
                ends: true,
            }
        }
    }

    impl ItemSource for Fake {
        async fn pull(&mut self) -> Option<SubscriptionItem<Value>> {
            match self.items.pop_front() {
                Some(item) => Some(item),
                None if self.ends => None,
                None => std::future::pending().await,
            }
        }

        fn cursor(&self) -> Option<u64> {
            self.cursor
        }
    }

    #[derive(Default)]
    struct Collect(Mutex<Vec<(Instant, StreamFrame)>>);

    impl FrameSink for Arc<Collect> {
        fn send(&self, frame: StreamFrame) -> Result<(), SinkClosed> {
            self.0.lock().unwrap().push((Instant::now(), frame));
            Ok(())
        }
    }

    impl Collect {
        fn frames(&self) -> Vec<StreamFrame> {
            self.0.lock().unwrap().iter().map(|(_, f)| f.clone()).collect()
        }
    }

    fn unlimited() -> PumpOptions {
        PumpOptions {
            window: u64::MAX,
            ..PumpOptions::default()
        }
    }

    #[tokio::test]
    async fn ready_items_go_out_in_full_frames_then_end() {
        let sink = Arc::new(Collect::default());
        let (mut control, _acks) = PumpControl::new(CancellationToken::new());
        let mut source = Fake::seqs(1..=1000);
        let end = pump(&mut source, "s1", &sink, &mut control, &unlimited()).await;
        assert_eq!(end, PumpEnd::SourceEnded);
        let frames = sink.frames();
        let sizes: Vec<usize> = frames.iter().map(|f| f.entries.len()).collect();
        assert_eq!(sizes, [256, 256, 256, 233], "232 items + end");
        assert_eq!(
            frames.iter().map(|f| f.frame).collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert_eq!(
            frames.last().unwrap().entries.last(),
            Some(&StreamEntry::End { error: None })
        );
        let seqs: Vec<u64> = frames
            .iter()
            .flat_map(|f| &f.entries)
            .filter_map(|e| match e {
                StreamEntry::Item { item } => item["seq"].as_u64(),
                _ => None,
            })
            .collect();
        assert_eq!(seqs, (1..=1000).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn a_lone_item_leaves_after_the_frame_delay() {
        let sink = Arc::new(Collect::default());
        let (mut control, _acks) = PumpControl::new(CancellationToken::new());
        let cancel = control.cancel.clone();
        let mut source = Fake {
            items: VecDeque::from([SubscriptionItem::Item(json!({"seq": 7}))]),
            cursor: None,
            ends: false,
        };
        let started = Instant::now();
        let stopper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            cancel.cancel();
        });
        let end = pump(&mut source, "s1", &sink, &mut control, &unlimited()).await;
        stopper.await.unwrap();
        assert_eq!(end, PumpEnd::Cancelled);
        let frames = sink.0.lock().unwrap().clone();
        assert_eq!(frames.len(), 1);
        let (at, frame) = &frames[0];
        assert_eq!(frame.entries.len(), 1);
        let waited = at.duration_since(started);
        // Not before 16 ms (it waits for company); well before the 300 ms cancel.
        assert!(waited >= Duration::from_millis(15), "{waited:?}");
        assert!(waited < Duration::from_millis(250), "{waited:?}");
    }

    #[test]
    fn lag_merges_and_a_backwards_cursor_is_a_reset() {
        let mut framer = Framer::default();
        framer.push(SubscriptionItem::Item(json!({"seq": 10})), Some(10));
        framer.push(SubscriptionItem::Lagged { missed: 2 }, Some(14));
        framer.push(SubscriptionItem::Lagged { missed: 3 }, Some(14));
        framer.push(SubscriptionItem::Item(json!({"seq": 15})), Some(15));
        // The daemon came back with a fresh database: the cursor drops to its head.
        framer.push(SubscriptionItem::Lagged { missed: 1 }, Some(3));
        framer.push(SubscriptionItem::Item(json!({"seq": 4})), Some(4));
        // A seq that does not grow without a gap first is a reset too.
        framer.push(SubscriptionItem::Item(json!({"seq": 2})), Some(2));
        let kinds: Vec<String> = framer
            .take()
            .into_iter()
            .map(|e| match e {
                StreamEntry::Item { item } => item["seq"].to_string(),
                StreamEntry::Lagged { missed } => format!("lag{missed}"),
                StreamEntry::Reset => "reset".into(),
                StreamEntry::End { .. } => "end".into(),
            })
            .collect();
        assert_eq!(kinds, ["10", "lag5", "15", "reset", "4", "reset", "2"]);
    }

    #[tokio::test]
    async fn a_full_window_holds_the_pump_until_acknowledged() {
        let sink = Arc::new(Collect::default());
        let (mut control, acks) = PumpControl::new(CancellationToken::new());
        let options = PumpOptions {
            max_items: 10,
            window: 2,
            ..PumpOptions::default()
        };
        let mut source = Fake::seqs(1..=100);
        let task = {
            let sink = Arc::clone(&sink);
            tokio::spawn(async move {
                let end = pump(&mut source, "s1", &sink, &mut control, &options).await;
                (end, control.sent())
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(sink.frames().len(), 2, "two frames, then the window is full");
        acks.send_replace(1);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(sink.frames().len(), 3);
        acks.send_replace(u64::MAX / 2);
        let (end, sent) = task.await.unwrap();
        assert_eq!(end, PumpEnd::SourceEnded);
        assert_eq!(sent, 11, "100 items in frames of 10, plus the end");
    }

    #[test]
    fn the_registry_acks_closes_by_context_and_by_owner() {
        let streams = Streams::default();
        let (a, mut ca) = streams.register("local", "main");
        let (b, cb) = streams.register("local", "main");
        let (c, cc) = streams.register("local", "other");
        assert_eq!(streams.len(), 3);
        assert!(streams.ack(&a, 3));
        assert_eq!(*ca.acked.borrow_and_update(), 3);
        assert!(streams.ack(&a, 2), "older acks are ignored");
        assert_eq!(*ca.acked.borrow(), 3);
        assert!(!streams.close("remote", &c), "wrong context");
        assert!(streams.close("local", &c));
        assert!(cc.cancel.is_cancelled());
        assert!(!streams.ack(&c, 1));
        assert_eq!(streams.close_owner("main"), 2);
        assert!(cb.cancel.is_cancelled());
        assert!(streams.is_empty());
        let _ = b;
    }
}
