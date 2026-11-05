//! Terminal sessions hosted by the daemon: the integrated terminal of the GUI (docs/07
//! §5.10, ADR-0040, docs/03 §7.2). A session is a process in a pseudo console
//! ([`forge_core::platform::PtySpawner`]) inside its own Job (nested in the daemon's
//! JobTree), with the Forgebox environment. It lives in the daemon, not in a client: it
//! survives a GUI reload or a dropped connection, and a client reattaches with
//! `terminal.list` + `terminal.subscribe {resume_from}`.
//!
//! Shape (one actor per session, ADR-0011):
//!
//! * [`TerminalManager`]: the table of sessions (leaf lock `daemon.tables`, like the pool
//!   and instance tables) and the `terminal.max_sessions` limit, counted with the opens in
//!   flight so two concurrent opens never pass it together.
//! * **Session actor**: one task with a bounded mailbox per session. It owns the console
//!   (resize, close), the session's Job, the input queue and the writer map that drops
//!   duplicate `(writer, input_seq)`; it watches the process exit, winds the session down
//!   (Job terminated, console closed while the output drains, `exited` recorded,
//!   `terminal.exited` published) and reaps itself once exited and unwatched for
//!   [`REAP_AFTER`]. Every ask carries a timeout.
//! * **Reader thread** (`forgebox-term-r`): blocking reads of up to [`READ_CHUNK`] bytes
//!   appended to the session's output ring ([`RING_BYTES`]); an incomplete trailing UTF-8
//!   sequence is held back until it completes. `seq` is the absolute byte offset.
//! * **Writer thread** (`forgebox-term-w`): writes the input queue ([`INPUT_QUEUE`]
//!   entries) to the console; a console that does not read stalls only this thread, and a
//!   full queue answers `conflict` (retrying with the same `input_seq` is safe).
//! * **Pump** (one task per subscription): follows the ring from its own cursor and sends
//!   `terminal.output` items, coalesced to at most one every [`DELIVERY_INTERVAL`] or
//!   [`DELIVERY_BYTES`]. It waits for room in its connection's queue instead of
//!   dropping: nothing upstream waits for it (the ring never blocks the reader), so a
//!   subscriber that does not read only falls behind; once its cursor is older than the
//!   ring it gets `terminal.lagged {missed}` (bytes) and continues from the ring start.
//!
//! The ring is the one piece shared outside the actor: the reader thread appends and the
//! pumps copy out, each under a lock held for a memory copy (never across `.await`), with a
//! `watch` announcing progress.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use forge_core::Timestamp;
use forge_core::config::INTEGRATED_SHELLS;
use forge_core::error::{ForgeError, Result};
use forge_core::event::{EventBus, EventKind};
use forge_core::ids::{SiteId, TerminalId, TerminalWriterId};
use forge_core::platform::{
    PTY_CLOSE_TIMEOUT, PTY_RESIZE_TIMEOUT, ProcessExit, ProcessGroup, PtyCommand, PtyConsole,
    PtyReader, PtySize, PtySpawner, PtyWriter,
};
use forge_core::runtime::spawn_blocking_timeout;
use forge_core::sync::{LockLevel, OrderedMutex};
use forge_ipc::api::{
    TERMINAL_MAX_WRITERS, TerminalDto, TerminalEnvelope, TerminalExitedPayload, TerminalItemDto,
};
use forge_ipc::server::StreamSender;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

/// Output kept per session for resuming subscribers.
pub const RING_BYTES: usize = 1 << 20;
/// Largest read of the reader thread.
pub const READ_CHUNK: usize = 64 * 1024;
/// Largest `terminal.output` item.
pub const DELIVERY_BYTES: usize = 32 * 1024;
/// Smallest gap between two `terminal.output` items of one subscription while less than
/// [`DELIVERY_BYTES`] are pending (the first item after a pause goes out at once).
pub const DELIVERY_INTERVAL: Duration = Duration::from_millis(16);
/// An exited session nobody watches is removed after this long.
pub const REAP_AFTER: Duration = Duration::from_secs(10 * 60);
/// Inputs queued for the writer thread of a session.
pub const INPUT_QUEUE: usize = 256;
/// Items queued for one subscription in its connection (32 KiB each at most).
pub const STREAM_QUEUE: usize = 8;
/// Messages queued for a session actor.
const MAILBOX: usize = 64;
/// Answer deadline of quick asks (input).
pub const QUICK_ASK: Duration = Duration::from_millis(900);
/// Answer deadline of a resize (the console's own deadline plus scheduling).
pub const RESIZE_ASK: Duration = Duration::from_secs(4);
/// Answer deadline of a close: the Job terminated, the console closed, the output drained.
pub const CLOSE_ASK: Duration = Duration::from_secs(20);
/// Starting a session: the Job, `CreatePseudoConsole`, `CreateProcess`.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a winding-down session waits for the reader's EOF once the console closed.
const EOF_WAIT: Duration = Duration::from_secs(3);
/// Budget of terminating a session's Job.
const TERMINATE_BUDGET: Duration = Duration::from_secs(12);
/// A pump waits this long for room in its connection's queue (practically forever: the
/// connection's end cancels the wait).
const ROOM_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

/// Tunables of the manager (tests shrink them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalOptions {
    pub ring_bytes: usize,
    pub reap_after: Duration,
}

impl Default for TerminalOptions {
    fn default() -> Self {
        Self {
            ring_bytes: RING_BYTES,
            reap_after: REAP_AFTER,
        }
    }
}

/// Creates the Job of a new session (the daemon nests it in its JobTree).
pub type GroupFactory = Arc<dyn Fn() -> Result<Arc<dyn ProcessGroup>> + Send + Sync>;

/// What `terminal.open` resolved: the process to start and how to show it.
#[derive(Debug, Clone)]
pub struct TerminalSpec {
    pub title: String,
    /// Shell name (`cmd`) or the program of a command (`mysql.exe`).
    pub shell: String,
    pub site_id: Option<SiteId>,
    pub command: PtyCommand,
}

// ------------------------------------------------------------------------ output ring

/// A byte ring addressed by absolute offsets: `start..end` is what it still holds.
#[derive(Debug)]
struct Ring {
    buf: VecDeque<u8>,
    start: u64,
    cap: usize,
}

impl Ring {
    fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::new(),
            start: 0,
            cap: cap.max(1),
        }
    }

    fn end(&self) -> u64 {
        self.start + self.buf.len() as u64
    }

    fn push(&mut self, data: &[u8]) {
        let end = self.end() + data.len() as u64;
        if data.len() >= self.cap {
            self.buf.clear();
            self.buf.extend(&data[data.len() - self.cap..]);
        } else {
            let over = (self.buf.len() + data.len()).saturating_sub(self.cap);
            self.buf.drain(..over);
            self.buf.extend(data);
        }
        self.start = end - self.buf.len() as u64;
    }

    /// At most `max` bytes from `from` (clamped into the ring).
    fn copy(&self, from: u64, max: usize) -> (u64, Vec<u8>) {
        let from = from.clamp(self.start, self.end());
        let offset = usize::try_from(from - self.start).unwrap_or(usize::MAX);
        let len = (self.buf.len() - offset).min(max);
        (
            from,
            self.buf.range(offset..offset + len).copied().collect(),
        )
    }
}

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ended {
    exit_code: Option<i32>,
    at: Timestamp,
}

/// What the output has reached, announced by a `watch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Progress {
    /// Bytes produced so far (the ring's end).
    produced: u64,
    /// The reader saw EOF (the console is gone).
    eof: bool,
    /// The session ended; no more output follows.
    ended: Option<Ended>,
}

/// A slice of the ring for a cursor.
#[derive(Debug, PartialEq, Eq)]
struct Chunk {
    /// Offset of `bytes` (later than the cursor when the cursor is older than the ring).
    from: u64,
    bytes: Vec<u8>,
}

/// Output of one session: the ring, its progress and who watches it.
pub(crate) struct Output {
    /// Lock `daemon.tables` (leaf): appending or copying bytes only.
    ring: OrderedMutex<Ring>,
    progress: watch::Sender<Progress>,
    /// Running pumps (subscriptions) of the session.
    watchers: watch::Sender<usize>,
}

impl Output {
    fn new(cap: usize) -> Self {
        Self {
            ring: OrderedMutex::new(LockLevel::DAEMON_TABLES, Ring::new(cap)),
            progress: watch::Sender::new(Progress::default()),
            watchers: watch::Sender::new(0),
        }
    }

    fn progress(&self) -> Progress {
        *self.progress.borrow()
    }

    fn append(&self, data: &[u8]) {
        if data.is_empty() || self.progress().ended.is_some() {
            return;
        }
        let end = {
            let mut ring = self.ring.lock();
            ring.push(data);
            ring.end()
        };
        self.progress.send_modify(|p| p.produced = end);
    }

    fn mark_eof(&self) {
        self.progress.send_modify(|p| p.eof = true);
    }

    fn end(&self, exit_code: Option<i32>) {
        self.progress.send_modify(|p| {
            if p.ended.is_none() {
                p.ended = Some(Ended {
                    exit_code,
                    at: Timestamp::now(),
                });
            }
        });
    }

    fn bounds(&self) -> (u64, u64) {
        let ring = self.ring.lock();
        (ring.start, ring.end())
    }

    /// Up to `max` bytes for a pump at `cursor`. A cursor older than the ring starts at the
    /// ring's first complete character (the ring may have cut one).
    fn read(&self, cursor: u64, max: usize) -> Chunk {
        let (start, (from, mut bytes)) = {
            let ring = self.ring.lock();
            (ring.start, ring.copy(cursor, max))
        };
        let mut from = from;
        if cursor < start {
            let skip = leading_continuations(&bytes);
            bytes.drain(..skip);
            from += skip as u64;
        }
        Chunk { from, bytes }
    }

    fn watch(self: &Arc<Self>) -> WatchGuard {
        self.watchers.send_modify(|n| *n += 1);
        WatchGuard(Arc::clone(self))
    }
}

/// Counts a running pump.
struct WatchGuard(Arc<Output>);

impl Drop for WatchGuard {
    fn drop(&mut self) {
        self.0.watchers.send_modify(|n| *n = n.saturating_sub(1));
    }
}

/// Length of an incomplete UTF-8 sequence at the end of `bytes` (0 when it ends on a
/// character boundary or with bytes that can never complete).
pub(crate) fn incomplete_utf8_tail(bytes: &[u8]) -> usize {
    for back in 1..=bytes.len().min(3) {
        let b = bytes[bytes.len() - back];
        if b & 0xC0 == 0x80 {
            continue; // continuation byte: keep looking for its lead
        }
        let needed = match b {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => return 0, // ASCII or a byte that starts nothing
        };
        return if back < needed { back } else { 0 };
    }
    0
}

/// Continuation bytes at the start of `bytes` (a character the ring cut), at most 3.
fn leading_continuations(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take(3)
        .take_while(|b| **b & 0xC0 == 0x80)
        .count()
}

fn read_loop(mut reader: PtyReader, output: &Output) {
    let mut buf = vec![0_u8; READ_CHUNK + 4];
    let mut held = 0;
    loop {
        match reader.read(&mut buf[held..held + READ_CHUNK]) {
            Ok(0) => break,
            Ok(n) => {
                let total = held + n;
                let tail = incomplete_utf8_tail(&buf[..total]);
                output.append(&buf[..total - tail]);
                buf.copy_within(total - tail..total, 0);
                held = tail;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                tracing::debug!(error = %e, "terminal output ended");
                break;
            }
        }
    }
    output.append(&buf[..held]);
    output.mark_eof();
}

fn write_loop(mut writer: PtyWriter, mut input: mpsc::Receiver<Vec<u8>>) {
    use std::io::Write;
    while let Some(data) = input.blocking_recv() {
        if let Err(e) = writer.write_all(&data).and_then(|()| writer.flush()) {
            tracing::debug!(error = %e, "terminal input closed");
            break;
        }
    }
}

// ------------------------------------------------------------------------ pump

/// Where a pump sends (the IPC stream; a fake in unit tests).
pub(crate) trait TerminalSink: Send + Sync + 'static {
    fn subscription(&self) -> forge_core::ids::SubscriptionId;
    /// Queues `item` once there is room; `false` once the stream is closed.
    fn send(&self, item: TerminalEnvelope) -> impl std::future::Future<Output = bool> + Send + '_;
    fn lagged(&self, missed: u64);
    fn closed(&self) -> impl std::future::Future<Output = ()> + Send + '_;
    fn is_closed(&self) -> bool;
}

impl TerminalSink for StreamSender {
    fn subscription(&self) -> forge_core::ids::SubscriptionId {
        self.id()
    }

    async fn send(&self, item: TerminalEnvelope) -> bool {
        self.send_wait(&item, ROOM_WAIT).await
    }

    fn lagged(&self, missed: u64) {
        StreamSender::lagged(self, missed);
    }

    async fn closed(&self) {
        StreamSender::closed(self).await;
    }

    fn is_closed(&self) -> bool {
        StreamSender::is_closed(self)
    }
}

/// Where a new subscription starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Attach {
    /// `last_seq` of the subscribe answer.
    pub last_seq: u64,
    /// First byte the pump sends from (`None`: everything was delivered, exited included).
    pub cursor: Option<u64>,
}

/// Plans a subscription from `resume_from`: without it, the whole ring (answer: the ring
/// start); with it, from that byte (older than the ring: lagged first, by the pump); past
/// the output, from the end (answer: the end, below the cursor sent, which tells the
/// client the stream restarted).
fn plan_attach(resume_from: Option<u64>, start: u64, end: u64, ended: bool) -> Attach {
    match resume_from {
        None => Attach {
            last_seq: start,
            cursor: Some(start),
        },
        Some(n) if n <= end => Attach {
            last_seq: n,
            cursor: Some(n),
        },
        // The client already got `exited` (its seq is produced + 1).
        Some(n) if ended && n == end + 1 => Attach {
            last_seq: n,
            cursor: None,
        },
        Some(_) => Attach {
            last_seq: end,
            cursor: Some(end),
        },
    }
}

async fn pump<S: TerminalSink>(output: Arc<Output>, sink: S, mut cursor: u64) {
    let _watching = output.watch();
    let mut progress_rx = output.progress.subscribe();
    let mut last_sent: Option<Instant> = None;
    loop {
        if sink.is_closed() {
            return;
        }
        let progress = *progress_rx.borrow_and_update();
        let chunk = output.read(cursor, DELIVERY_BYTES);
        if chunk.from > cursor {
            sink.lagged(chunk.from - cursor);
            cursor = chunk.from;
        }
        if !chunk.bytes.is_empty() {
            let pending = progress.produced.saturating_sub(cursor);
            let settled = progress.eof || progress.ended.is_some();
            if pending < DELIVERY_BYTES as u64 && !settled {
                if let Some(due) = last_sent.map(|t| t + DELIVERY_INTERVAL) {
                    if Instant::now() < due {
                        tokio::select! {
                            () = tokio::time::sleep_until(due) => {}
                            () = sink.closed() => return,
                        }
                        continue;
                    }
                }
            }
            let mut bytes = chunk.bytes;
            if bytes.len() == DELIVERY_BYTES {
                let tail = incomplete_utf8_tail(&bytes);
                if tail < bytes.len() {
                    bytes.truncate(bytes.len() - tail);
                }
            }
            let n = bytes.len() as u64;
            let item = TerminalEnvelope {
                subscription_id: sink.subscription(),
                seq: cursor + n,
                item: TerminalItemDto::Output {
                    data: String::from_utf8_lossy(&bytes).into_owned(),
                },
            };
            if !sink.send(item).await {
                return;
            }
            cursor += n;
            last_sent = Some(Instant::now());
            continue;
        }
        if let Some(ended) = progress.ended {
            if cursor >= progress.produced {
                let item = TerminalEnvelope {
                    subscription_id: sink.subscription(),
                    seq: progress.produced + 1,
                    item: TerminalItemDto::Exited {
                        exit_code: ended.exit_code,
                    },
                };
                sink.send(item).await;
                return;
            }
            continue;
        }
        tokio::select! {
            changed = progress_rx.changed() => if changed.is_err() { return },
            () = sink.closed() => return,
        }
    }
}

// ------------------------------------------------------------------------ input dedup

/// Last accepted `input_seq` per writer, bounded to [`TERMINAL_MAX_WRITERS`] (the writer
/// idle the longest is forgotten first).
#[derive(Debug, Default)]
struct Writers {
    last: HashMap<TerminalWriterId, (u64, u64)>,
    tick: u64,
}

impl Writers {
    /// Whether `seq` is new for `writer` (a retry otherwise). Does not record it.
    fn is_new(&self, writer: TerminalWriterId, seq: u64) -> bool {
        self.last.get(&writer).is_none_or(|(last, _)| seq > *last)
    }

    fn touch(&mut self, writer: TerminalWriterId) {
        self.tick += 1;
        if let Some(entry) = self.last.get_mut(&writer) {
            entry.1 = self.tick;
        }
    }

    fn accept(&mut self, writer: TerminalWriterId, seq: u64) {
        self.tick += 1;
        self.last.insert(writer, (seq, self.tick));
        while self.last.len() > TERMINAL_MAX_WRITERS {
            let oldest = self
                .last
                .iter()
                .min_by_key(|(_, (_, tick))| *tick)
                .map(|(id, _)| *id);
            match oldest {
                Some(id) => self.last.remove(&id),
                None => break,
            };
        }
    }
}

// ------------------------------------------------------------------------ session actor

enum Msg {
    Input {
        writer: TerminalWriterId,
        input_seq: u64,
        data: Vec<u8>,
        reply: oneshot::Sender<Result<()>>,
    },
    Resize {
        size: PtySize,
        reply: oneshot::Sender<Result<()>>,
    },
    Close {
        reply: oneshot::Sender<()>,
    },
}

/// What `terminal.list` shows of a session, readable without asking its actor.
struct SessionInfo {
    id: TerminalId,
    title: String,
    shell: String,
    cwd: String,
    site_id: Option<SiteId>,
    pid: u32,
    started_at: Timestamp,
    /// `cols << 16 | rows`.
    size: AtomicU32,
    output: Arc<Output>,
}

fn pack(size: PtySize) -> u32 {
    (u32::from(size.cols) << 16) | u32::from(size.rows)
}

impl SessionInfo {
    fn dto(&self) -> TerminalDto {
        let size = self.size.load(Ordering::Relaxed);
        let progress = self.output.progress();
        TerminalDto {
            id: self.id,
            title: self.title.clone(),
            shell: self.shell.clone(),
            cwd: self.cwd.clone(),
            site_id: self.site_id,
            cols: (size >> 16) as u16,
            rows: (size & 0xFFFF) as u16,
            pid: progress.ended.is_none().then_some(self.pid),
            started_at: self.started_at,
            exit_code: progress.ended.and_then(|e| e.exit_code),
            exited_at: progress.ended.map(|e| e.at),
            last_seq: progress.produced,
        }
    }
}

/// A table entry: the shared info and the actor's mailbox.
#[derive(Clone)]
struct SessionHandle {
    info: Arc<SessionInfo>,
    mailbox: mpsc::Sender<Msg>,
    /// Position in the open order (`list` is oldest first; two ULIDs of the same
    /// millisecond do not tell which session came first).
    order: u64,
}

struct Session {
    info: Arc<SessionInfo>,
    console: Arc<dyn PtyConsole>,
    group: Arc<dyn ProcessGroup>,
    input: Option<mpsc::Sender<Vec<u8>>>,
    writers: Writers,
    bus: EventBus,
    manager: Weak<Inner>,
    reap_after: Duration,
}

enum EndedBy {
    Exit(Option<i32>),
    Close(Option<oneshot::Sender<()>>),
}

fn exited_error(id: TerminalId) -> ForgeError {
    ForgeError::Conflict(format!("terminal {id} has exited"))
}

impl Session {
    async fn run(mut self, mut mailbox: mpsc::Receiver<Msg>, mut exit: ProcessExit) {
        let ended_by = loop {
            tokio::select! {
                code = &mut exit => break EndedBy::Exit(match code {
                    // Windows exit codes are DWORDs; the API carries their bits as i32.
                    Ok(code) => Some(code as i32),
                    Err(err) => {
                        tracing::warn!(terminal = %self.info.id, %err, "terminal exit code unknown");
                        None
                    }
                }),
                msg = mailbox.recv() => match msg {
                    // The manager dropped the session (shutdown, or it lost the table).
                    None => break EndedBy::Close(None),
                    Some(Msg::Input { writer, input_seq, data, reply }) => {
                        let _ = reply.send(self.input(writer, input_seq, data));
                    }
                    Some(Msg::Resize { size, reply }) => {
                        let _ = reply.send(self.resize(size).await);
                    }
                    Some(Msg::Close { reply }) => break EndedBy::Close(Some(reply)),
                },
            }
        };
        match ended_by {
            EndedBy::Exit(code) => {
                self.wind_down(code).await;
                self.linger(mailbox).await;
            }
            EndedBy::Close(reply) => {
                self.wind_down(None).await;
                if let Some(reply) = reply {
                    let _ = reply.send(());
                }
            }
        }
    }

    fn input(&mut self, writer: TerminalWriterId, input_seq: u64, data: Vec<u8>) -> Result<()> {
        if !self.writers.is_new(writer, input_seq) {
            // A retry of an input already written.
            self.writers.touch(writer);
            return Ok(());
        }
        let Some(input) = &self.input else {
            return Err(exited_error(self.info.id));
        };
        if !data.is_empty() {
            match input.try_send(data) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    return Err(ForgeError::Conflict(format!(
                        "terminal {} is not reading its input; retry later",
                        self.info.id
                    )));
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return Err(exited_error(self.info.id));
                }
            }
        }
        self.writers.accept(writer, input_seq);
        Ok(())
    }

    async fn resize(&mut self, size: PtySize) -> Result<()> {
        let console = Arc::clone(&self.console);
        spawn_blocking_timeout(
            "resizing a terminal",
            PTY_RESIZE_TIMEOUT + Duration::from_secs(1),
            move || console.resize(size),
        )
        .await?;
        self.info.size.store(pack(size), Ordering::Relaxed);
        Ok(())
    }

    /// Terminates what is left in the session's Job, closes the console while the reader
    /// drains it, records the end and publishes `terminal.exited`.
    async fn wind_down(&mut self, exit_code: Option<i32>) {
        let id = self.info.id;
        self.input = None;
        let group = Arc::clone(&self.group);
        if let Err(err) = spawn_blocking_timeout(
            "terminating a terminal's processes",
            TERMINATE_BUDGET,
            move || group.terminate_all(forge_supervisor::TERMINATED_EXIT_CODE),
        )
        .await
        {
            tracing::warn!(terminal = %id, %err, "terminal processes not terminated");
        }
        let console = Arc::clone(&self.console);
        if let Err(err) = spawn_blocking_timeout(
            "closing a pseudo console",
            PTY_CLOSE_TIMEOUT + Duration::from_secs(1),
            move || console.close(),
        )
        .await
        {
            tracing::warn!(terminal = %id, %err, "pseudo console not closed in time");
        }
        let mut progress = self.info.output.progress.subscribe();
        if tokio::time::timeout(EOF_WAIT, progress.wait_for(|p| p.eof))
            .await
            .is_err()
        {
            tracing::warn!(terminal = %id, "terminal output did not end in time");
        }
        self.info.output.end(exit_code);
        tracing::info!(terminal = %id, ?exit_code, "terminal ended");
        let payload = TerminalExitedPayload {
            v: 1,
            terminal_id: id,
            exit_code,
            site_id: self.info.site_id,
        };
        self.bus.publish(
            EventKind::TerminalExited,
            serde_json::to_value(payload).unwrap_or_default(),
        );
    }

    /// After the process exited: stays listed (its output can still be read) until it is
    /// closed, or reaped once nobody watched it for `reap_after`.
    async fn linger(self, mut mailbox: mpsc::Receiver<Msg>) {
        let id = self.info.id;
        let mut watchers = self.info.output.watchers.subscribe();
        let mut unwatched_since = (*watchers.borrow_and_update() == 0).then(Instant::now);
        loop {
            let deadline = unwatched_since.map(|t| t + self.reap_after);
            tokio::select! {
                () = sleep_until(deadline) => {
                    if let Some(manager) = self.manager.upgrade() {
                        manager.table.lock().sessions.remove(&id);
                    }
                    tracing::info!(terminal = %id, "exited terminal reaped");
                    return;
                }
                changed = watchers.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let n = *watchers.borrow_and_update();
                    if n > 0 {
                        unwatched_since = None;
                    } else if unwatched_since.is_none() {
                        unwatched_since = Some(Instant::now());
                    }
                }
                msg = mailbox.recv() => match msg {
                    None => return,
                    Some(Msg::Input { reply, .. }) => {
                        let _ = reply.send(Err(exited_error(id)));
                    }
                    Some(Msg::Resize { reply, .. }) => {
                        let _ = reply.send(Err(exited_error(id)));
                    }
                    Some(Msg::Close { reply }) => {
                        let _ = reply.send(());
                        return;
                    }
                },
            }
        }
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

async fn ask<R>(
    session: &SessionHandle,
    make: impl FnOnce(oneshot::Sender<R>) -> Msg,
    timeout: Duration,
    operation: &str,
) -> Result<R> {
    let id = session.info.id;
    let (reply, answer) = oneshot::channel();
    session.mailbox.try_send(make(reply)).map_err(|e| match e {
        mpsc::error::TrySendError::Full(_) => {
            ForgeError::Conflict(format!("terminal {id} is busy; retry later"))
        }
        mpsc::error::TrySendError::Closed(_) => exited_error(id),
    })?;
    match tokio::time::timeout(timeout, answer).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err(exited_error(id)),
        Err(_) => Err(ForgeError::Timeout {
            operation: format!("{operation} (terminal {id})"),
            after: timeout,
        }),
    }
}

// ------------------------------------------------------------------------ manager

#[derive(Default)]
struct Table {
    sessions: BTreeMap<TerminalId, SessionHandle>,
    /// `SessionHandle::order` of the next session.
    next_order: u64,
    /// Opens in flight (counted against the limit).
    reserved: u32,
    /// The daemon is going down: no new session.
    closed: bool,
}

struct Inner {
    bus: EventBus,
    groups: GroupFactory,
    pty: Arc<dyn PtySpawner>,
    options: TerminalOptions,
    /// Lock `daemon.tables` (leaf).
    table: OrderedMutex<Table>,
    /// Sessions ever opened (logs).
    opened: AtomicU64,
}

/// The terminal sessions of the daemon. Cheap to clone.
#[derive(Clone)]
pub struct TerminalManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for TerminalManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalManager")
            .field("sessions", &self.inner.table.lock().sessions.len())
            .finish()
    }
}

/// Gives the reserved slot back when an open ends (failed or not).
struct Reservation(Arc<Inner>);

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut table = self.0.table.lock();
        table.reserved = table.reserved.saturating_sub(1);
    }
}

impl TerminalManager {
    pub fn new(
        bus: EventBus,
        groups: GroupFactory,
        pty: Arc<dyn PtySpawner>,
        options: TerminalOptions,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                bus,
                groups,
                pty,
                options,
                table: OrderedMutex::new(LockLevel::DAEMON_TABLES, Table::default()),
                opened: AtomicU64::new(0),
            }),
        }
    }

    fn reserve(&self, max_sessions: u32) -> Result<Reservation> {
        let mut table = self.inner.table.lock();
        if table.closed {
            return Err(ForgeError::Conflict("the daemon is shutting down".into()));
        }
        let open = u32::try_from(table.sessions.len()).unwrap_or(u32::MAX);
        if open.saturating_add(table.reserved) >= max_sessions {
            return Err(ForgeError::Conflict(format!(
                "{max_sessions} terminal sessions are open already (terminal.max_sessions); \
                 close one first"
            )));
        }
        table.reserved += 1;
        Ok(Reservation(Arc::clone(&self.inner)))
    }

    /// Starts a session (at most `max_sessions` at once).
    pub async fn open(&self, spec: TerminalSpec, max_sessions: u32) -> Result<TerminalDto> {
        let reservation = self.reserve(max_sessions)?;
        let size = spec.command.size()?;
        let (groups, pty, command) = (
            Arc::clone(&self.inner.groups),
            Arc::clone(&self.inner.pty),
            spec.command.clone(),
        );
        let (group, session) =
            spawn_blocking_timeout("starting a terminal", SPAWN_TIMEOUT, move || {
                let group = groups()?;
                let session = pty.spawn(&*group, &command)?;
                Ok((group, session))
            })
            .await?;
        let id = TerminalId::new();
        let output = Arc::new(Output::new(self.inner.options.ring_bytes));
        let info = Arc::new(SessionInfo {
            id,
            title: spec.title,
            shell: spec.shell,
            cwd: spec.command.cwd.display().to_string(),
            site_id: spec.site_id,
            pid: session.pid,
            started_at: Timestamp::now(),
            size: AtomicU32::new(pack(size)),
            output: Arc::clone(&output),
        });
        let (input_tx, input_rx) = mpsc::channel(INPUT_QUEUE);
        let reader = session.reader;
        let writer = session.writer;
        let started = std::thread::Builder::new()
            .name("forgebox-term-r".into())
            .stack_size(256 * 1024)
            .spawn({
                let output = Arc::clone(&output);
                move || read_loop(reader, &output)
            })
            .and_then(|_| {
                std::thread::Builder::new()
                    .name("forgebox-term-w".into())
                    .stack_size(256 * 1024)
                    .spawn(move || write_loop(writer, input_rx))
            });
        if let Err(e) = started {
            // Dropping the console and the group ends the process.
            return Err(ForgeError::Internal(format!(
                "starting the terminal threads: {e}"
            )));
        }
        let (mailbox_tx, mailbox_rx) = mpsc::channel(MAILBOX);
        let actor = Session {
            info: Arc::clone(&info),
            console: session.console,
            group,
            input: Some(input_tx),
            writers: Writers::default(),
            bus: self.inner.bus.clone(),
            manager: Arc::downgrade(&self.inner),
            reap_after: self.inner.options.reap_after,
        };
        tokio::spawn(actor.run(mailbox_rx, session.exit));
        let dto = info.dto();
        {
            let mut table = self.inner.table.lock();
            if table.closed {
                // Shut down meanwhile: dropping the mailbox winds the session down.
                return Err(ForgeError::Conflict("the daemon is shutting down".into()));
            }
            let order = table.next_order;
            table.next_order += 1;
            table.sessions.insert(
                id,
                SessionHandle {
                    info,
                    mailbox: mailbox_tx,
                    order,
                },
            );
        }
        drop(reservation);
        let n = self.inner.opened.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::info!(terminal = %id, pid = dto.pid, shell = %dto.shell, opened = n, "terminal opened");
        Ok(dto)
    }

    fn get(&self, id: TerminalId) -> Result<SessionHandle> {
        self.inner
            .table
            .lock()
            .sessions
            .get(&id)
            .cloned()
            .ok_or_else(|| ForgeError::NotFound(format!("terminal {id}")))
    }

    /// Every session, oldest first (in the order they were opened).
    pub fn list(&self) -> Vec<TerminalDto> {
        let mut infos: Vec<(u64, Arc<SessionInfo>)> = self
            .inner
            .table
            .lock()
            .sessions
            .values()
            .map(|h| (h.order, Arc::clone(&h.info)))
            .collect();
        infos.sort_unstable_by_key(|(order, _)| *order);
        infos.iter().map(|(_, i)| i.dto()).collect()
    }

    /// Sessions in the table.
    pub fn len(&self) -> usize {
        self.inner.table.lock().sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Writes `data` unless `(writer, input_seq)` was already written.
    pub async fn input(
        &self,
        id: TerminalId,
        writer: TerminalWriterId,
        input_seq: u64,
        data: String,
    ) -> Result<()> {
        let session = self.get(id)?;
        ask(
            &session,
            |reply| Msg::Input {
                writer,
                input_seq,
                data: data.into_bytes(),
                reply,
            },
            QUICK_ASK,
            "writing to a terminal",
        )
        .await?
    }

    pub async fn resize(&self, id: TerminalId, cols: u16, rows: u16) -> Result<()> {
        let size = PtySize::new(cols, rows)?;
        let session = self.get(id)?;
        ask(
            &session,
            |reply| Msg::Resize { size, reply },
            RESIZE_ASK,
            "resizing a terminal",
        )
        .await?
    }

    /// Ends a session (its processes are terminated) and removes it from the list. Its
    /// subscribers still get the rest of the output and `exited`.
    pub async fn close(&self, id: TerminalId) -> Result<()> {
        let session = self
            .inner
            .table
            .lock()
            .sessions
            .remove(&id)
            .ok_or_else(|| ForgeError::NotFound(format!("terminal {id}")))?;
        ask(
            &session,
            |reply| Msg::Close { reply },
            CLOSE_ASK,
            "closing a terminal",
        )
        .await
    }

    /// Daemon shutdown: refuses new sessions and closes every session in parallel, each
    /// within `budget`. Their processes die with their Jobs (and with the JobTree after).
    pub async fn close_all(&self, budget: Duration) {
        let sessions: Vec<SessionHandle> = {
            let mut table = self.inner.table.lock();
            table.closed = true;
            std::mem::take(&mut table.sessions).into_values().collect()
        };
        if sessions.is_empty() {
            return;
        }
        tracing::info!(sessions = sessions.len(), "closing terminal sessions");
        let closes = sessions.iter().map(|s| {
            ask(
                s,
                |reply| Msg::Close { reply },
                budget.min(CLOSE_ASK),
                "closing a terminal",
            )
        });
        for result in futures::future::join_all(closes).await {
            if let Err(err) = result {
                tracing::warn!(%err, "terminal session not closed in time");
            }
        }
    }

    /// Opens a subscription on `sink` from `resume_from`; answers the `last_seq` of the
    /// subscribe answer.
    pub(crate) fn subscribe<S: TerminalSink>(
        &self,
        id: TerminalId,
        resume_from: Option<u64>,
        sink: impl FnOnce() -> Result<S>,
    ) -> Result<u64> {
        let output = Arc::clone(&self.get(id)?.info.output);
        let (start, end) = output.bounds();
        let ended = output.progress().ended.is_some();
        let attach = plan_attach(resume_from, start, end, ended);
        let sink = sink()?;
        if let Some(cursor) = attach.cursor {
            tokio::spawn(pump(output, sink, cursor));
        }
        Ok(attach.last_seq)
    }
}

// ------------------------------------------------------------------------ resolution

/// Where shells and programs are looked up (pure inputs, so the rules are testable).
pub(crate) struct Lookup<'a> {
    /// `%SystemRoot%\System32`.
    pub system32: &'a Path,
    /// `PATH` of the session.
    pub path: &'a OsStr,
    pub pathext: Option<&'a OsStr>,
    pub program_files: Option<&'a Path>,
    pub comspec: Option<&'a Path>,
    pub is_file: &'a dyn Fn(&Path) -> bool,
}

impl Lookup<'_> {
    fn in_path(&self, file: &str) -> Option<PathBuf> {
        std::env::split_paths(self.path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(file))
            .find(|p| (self.is_file)(p))
    }

    fn cmd(&self) -> PathBuf {
        self.comspec
            .filter(|p| (self.is_file)(p))
            .map_or_else(|| self.system32.join("cmd.exe"), Path::to_path_buf)
    }

    fn shell(&self, name: &str) -> Option<PathBuf> {
        match name {
            "pwsh" => self.in_path("pwsh.exe").or_else(|| {
                self.program_files
                    .map(|p| p.join("PowerShell").join("7").join("pwsh.exe"))
                    .filter(|p| (self.is_file)(p))
            }),
            "powershell" => Some(
                self.system32
                    .join("WindowsPowerShell")
                    .join("v1.0")
                    .join("powershell.exe"),
            )
            .filter(|p| (self.is_file)(p))
            .or_else(|| self.in_path("powershell.exe")),
            "cmd" => Some(self.cmd()).filter(|p| (self.is_file)(p)),
            _ => None,
        }
    }
}

/// The shell of a session: `requested` (`terminal.open {shell}`) must exist; otherwise the
/// configured one, then `pwsh` → `powershell` → `cmd`.
pub(crate) fn find_shell(
    requested: Option<&str>,
    configured: Option<&str>,
    lookup: &Lookup<'_>,
) -> Result<(&'static str, PathBuf)> {
    let known = |name: &str| INTEGRATED_SHELLS.iter().copied().find(|s| *s == name);
    if let Some(name) = requested {
        let name = known(name).ok_or_else(|| {
            ForgeError::InvalidInput(format!(
                "shell {name:?} must be one of {INTEGRATED_SHELLS:?}"
            ))
        })?;
        return lookup
            .shell(name)
            .map(|exe| (name, exe))
            .ok_or_else(|| ForgeError::NotFound(format!("shell {name} is not installed")));
    }
    configured
        .and_then(known)
        .into_iter()
        .chain(INTEGRATED_SHELLS)
        .find_map(|name| lookup.shell(name).map(|exe| (name, exe)))
        .ok_or_else(|| ForgeError::NotFound("no shell found (pwsh, powershell, cmd)".into()))
}

/// Arguments of an interactive `shell`; PowerShell loads the `forge` completions when
/// `forge` (next to the daemon) is given.
pub(crate) fn shell_args(shell: &str, forge: Option<&Path>) -> Vec<OsString> {
    match shell {
        "pwsh" | "powershell" => {
            let mut args = vec![OsString::from("-NoLogo")];
            if let Some(forge) = forge {
                let quoted = forge.display().to_string().replace('\'', "''");
                args.extend([
                    OsString::from("-NoExit"),
                    OsString::from("-Command"),
                    OsString::from(format!(
                        "& '{quoted}' completions powershell | Out-String | Invoke-Expression"
                    )),
                ]);
            }
            args
        }
        _ => Vec::new(),
    }
}

/// The executable and arguments that run `argv` (`terminal.open {command}`): a bare name
/// is looked up in the session's `PATH` with `PATHEXT`, as a shell would; a relative path
/// is taken from `cwd`; batch files run through `cmd.exe /d /c`.
pub(crate) fn resolve_command(
    argv: &[String],
    cwd: &Path,
    lookup: &Lookup<'_>,
) -> Result<(PathBuf, Vec<OsString>)> {
    let (program, rest) = argv
        .split_first()
        .filter(|(p, _)| !p.trim().is_empty())
        .ok_or_else(|| ForgeError::InvalidInput("command needs a program".into()))?;
    let as_path = PathBuf::from(program);
    let found = if program.contains(['\\', '/', ':']) {
        let path = if as_path.is_absolute() {
            as_path
        } else {
            cwd.join(as_path)
        };
        Some(path).filter(|p| (lookup.is_file)(p))
    } else if as_path.extension().is_some() {
        lookup.in_path(program)
    } else {
        let pathext = lookup
            .pathext
            .and_then(OsStr::to_str)
            .filter(|p| !p.trim().is_empty())
            .unwrap_or(".COM;.EXE;.BAT;.CMD");
        pathext
            .split(';')
            .map(str::trim)
            .filter(|e| e.starts_with('.'))
            .find_map(|ext| lookup.in_path(&format!("{program}{}", ext.to_ascii_lowercase())))
    };
    let exe = found.ok_or_else(|| {
        ForgeError::NotFound(format!(
            "program {program:?} was not found in the terminal's PATH"
        ))
    })?;
    let batch = exe
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|e| e.eq_ignore_ascii_case("bat") || e.eq_ignore_ascii_case("cmd"));
    let rest = rest.iter().map(OsString::from);
    if batch {
        let mut args = vec![OsString::from("/d"), OsString::from("/c"), exe.into()];
        args.extend(rest);
        Ok((lookup.cmd(), args))
    } else {
        Ok((exe, rest.collect()))
    }
}

/// The complete environment of a session: `base` (the daemon's own environment) without
/// its `PATH`, then `PATH` = `prepend` + the base `PATH`, the Forgebox `vars`,
/// `FORGEBOX_TERMINAL=1` and the caller's `extra` last (they win). Returns it with the
/// `PATH` in effect.
pub(crate) fn session_env(
    base: impl IntoIterator<Item = (OsString, OsString)>,
    prepend: &[String],
    vars: &BTreeMap<String, String>,
    extra: &BTreeMap<String, String>,
) -> (Vec<(OsString, OsString)>, OsString) {
    let is_path = |name: &OsStr| {
        name.to_str()
            .is_some_and(|n| n.eq_ignore_ascii_case("PATH"))
    };
    let mut base_path = None;
    let mut env: Vec<(OsString, OsString)> = Vec::new();
    for (name, value) in base {
        if is_path(&name) {
            base_path = Some(value);
        } else {
            env.push((name, value));
        }
    }
    let mut entries: Vec<PathBuf> = prepend.iter().map(PathBuf::from).collect();
    if let Some(base_path) = &base_path {
        entries.extend(std::env::split_paths(base_path));
    }
    let mut path = std::env::join_paths(&entries).unwrap_or_default();
    env.push(("PATH".into(), path.clone()));
    for (name, value) in vars {
        env.push((name.into(), value.into()));
    }
    env.push(("FORGEBOX_TERMINAL".into(), "1".into()));
    for (name, value) in extra {
        if is_path(OsStr::new(name)) {
            path = value.into();
        }
        env.push((name.into(), value.into()));
    }
    (env, path)
}

#[cfg(test)]
mod tests;
