//! Captured stdout/stderr (docs/03 §1.2 and §7.3).
//!
//! ```text
//! stdout pipe ─▶ pump ─┐                       ┌─▶ broadcast (drop-oldest) ─▶ subscribers
//!                      ├─▶ hub (seq, ts) ──────┤
//! stderr pipe ─▶ pump ─┘   bounded mpsc        └─▶ bounded mpsc ─▶ file writer (rotation)
//! ```
//!
//! * **Pumps** (one task per pipe) read the overlapped pipe, split lines (`\n`, a
//!   trailing `\r` is dropped, lines longer than [`LogOptions::max_line_bytes`] are cut)
//!   and send them to the hub. They never hold a thread while the pipe is idle.
//! * The **hub** (one task per instance, shared by its runs) numbers lines, stamps them,
//!   broadcasts them and queues them for the file. `seq` continues the instance's file
//!   set ([`set`], ADR-0046): line `n` of `.out.log.4` … `.out.log` (plus the lines
//!   rotation deleted) is `seq` `n`, across runs, daemon restarts and rotations, so a
//!   client can mix the tail backlog, a replay from the files and the live lines.
//! * **File** delivery is guaranteed with backpressure: when the writer falls behind, the
//!   queue fills, the hub waits, the pumps stop reading, the 64 KiB pipe buffer fills and
//!   the child blocks in `WriteFile`, exactly as on a slow terminal. Writes run in
//!   `spawn_blocking` in batches; `logs/<tool>/<instance>.out.log` is rotated at
//!   [`Rotation::max_bytes`], keeping [`Rotation::max_files`] files in total
//!   (`.out.log`, `.out.log.1` … `.out.log.<max_files-1>`; 10 MiB × 5 by default).
//! * **Subscribers** get a `tokio::sync::broadcast` receiver: publishing never waits; a
//!   subscriber that does not keep up loses the oldest lines and is told how many
//!   ([`LogItem::Lagged`]). A slow client never slows the process, the file or the other
//!   clients.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use forge_core::Timestamp;
use forge_core::platform::PipeReader;
use forge_core::sync::{LockLevel, OrderedMutex};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::sync::{OnceCell, broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

pub mod set;

/// Which stream a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    Stdout,
    Stderr,
}

/// One line written by a supervised process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogLine {
    /// Per instance, strictly increasing by one (also across restarts of the instance
    /// and of the daemon): the line's index in the instance's file set ([`set`]).
    pub seq: u64,
    /// When the supervisor read it (UTC).
    pub ts: Timestamp,
    pub stream: LogStream,
    /// The line without its terminator (invalid UTF-8 replaced).
    pub text: String,
}

/// What [`LogSubscription::recv_item`] yields.
#[derive(Debug, Clone, PartialEq)]
pub enum LogItem {
    Line(LogLine),
    /// This subscriber was too slow: the `missed` oldest lines were dropped for it.
    Lagged {
        missed: u64,
    },
}

/// Live view of an instance's output. Dropping it unsubscribes.
#[derive(Debug)]
pub struct LogSubscription {
    receiver: broadcast::Receiver<LogLine>,
    missed: u64,
    numbering: Arc<Numbering>,
}

impl LogSubscription {
    /// `seq` from which this subscription receives every line (or a
    /// [`LogItem::Lagged`] for it); the lines before it are in the file set (or still
    /// being written to it). Lines below it may also arrive: they are in the files too.
    /// Waits, the first time, until the hub has counted the files.
    pub async fn start_seq(&self) -> u64 {
        self.numbering.ready().await;
        self.numbering.next()
    }

    /// Next line, skipping over gaps (counted in [`LogSubscription::missed`]). `None`
    /// once the instance is gone.
    pub async fn recv(&mut self) -> Option<LogLine> {
        loop {
            match self.recv_item().await? {
                LogItem::Line(line) => return Some(line),
                LogItem::Lagged { .. } => continue,
            }
        }
    }

    /// Next item, reporting gaps as [`LogItem::Lagged`].
    pub async fn recv_item(&mut self) -> Option<LogItem> {
        match self.receiver.recv().await {
            Ok(line) => Some(LogItem::Line(line)),
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                self.missed += missed;
                Some(LogItem::Lagged { missed })
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }

    /// Non-blocking receive (gaps are counted and skipped).
    pub fn try_recv(&mut self) -> Option<LogLine> {
        loop {
            match self.receiver.try_recv() {
                Ok(line) => return Some(line),
                Err(broadcast::error::TryRecvError::Lagged(missed)) => self.missed += missed,
                Err(_) => return None,
            }
        }
    }

    /// Total lines this subscription lost because it was too slow.
    pub fn missed(&self) -> u64 {
        self.missed
    }
}

/// Size-based rotation of the output file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rotation {
    /// Rotate before a line would make the file bigger than this.
    pub max_bytes: u64,
    /// Files kept in total, the current one included (at least 1: plain truncation).
    pub max_files: u32,
}

impl Default for Rotation {
    /// 10 MiB × 5 (docs/03 §1.2).
    fn default() -> Self {
        Self {
            max_bytes: 10 * 1024 * 1024,
            max_files: 5,
        }
    }
}

/// Output handling of one instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogOptions {
    /// `logs/<tool>/<instance>.out.log` (see [`crate::instance_log_path`]); `None` keeps
    /// only the live view.
    pub file: Option<PathBuf>,
    pub rotation: Rotation,
    /// Lines buffered per subscriber before the oldest are dropped.
    pub broadcast_capacity: usize,
    /// Lines queued for the file writer before the process is slowed down.
    pub file_queue: usize,
    /// Longer lines are split.
    pub max_line_bytes: usize,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            file: None,
            rotation: Rotation::default(),
            broadcast_capacity: 1024,
            file_queue: 1024,
            max_line_bytes: 16 * 1024,
        }
    }
}

/// Raw line from a pump.
struct RawLine {
    stream: LogStream,
    bytes: Vec<u8>,
}

/// What the pumps (and the end of a run) send to the hub, in order.
enum RawMsg {
    Line(RawLine),
    /// A run ended: acknowledge once everything before it is in the file (and the file
    /// is closed until the next run).
    Flush(oneshot::Sender<()>),
}

/// What the hub queues for the file writer, in `seq` order.
enum FileMsg {
    Line { seq: u64, bytes: Vec<u8> },
    Flush(oneshot::Sender<()>),
}

/// Lock of a hub's line counter. Innermost: nothing is acquired while it is held (the
/// broadcast send under it does not block).
const LOG_HUB_SEQ: LockLevel = LockLevel::new(200, "log_hub.seq");

/// Numbering of one instance's lines (ADR-0046): `seq` continues the file set
/// ([`set::LogSet::Rotated`] of [`LogOptions::file`]), so the tail backlog, a replay and
/// the live lines share one domain across daemon restarts and rotations.
struct Numbering {
    file: Option<PathBuf>,
    /// Set once `next` continues the file set (counted in `spawn_blocking`).
    ready: OnceCell<()>,
    /// `seq` of the next line. The hub takes a number and broadcasts the line under it.
    next: OrderedMutex<u64>,
}

impl std::fmt::Debug for Numbering {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Numbering")
            .field("file", &self.file)
            .field("ready", &self.ready.initialized())
            .finish_non_exhaustive()
    }
}

impl Numbering {
    /// Counts the file set once (first caller; the others wait).
    async fn ready(&self) {
        self.ready
            .get_or_init(|| async {
                let last = match self.file.clone() {
                    None => 0,
                    Some(file) => {
                        let shown = file.display().to_string();
                        match tokio::task::spawn_blocking(move || set::prepare_append(&file))
                            .await
                        {
                            Ok(Ok(last)) => last,
                            Ok(Err(err)) => {
                                tracing::warn!(path = %shown, %err, "cannot count the instance log; numbering from 1");
                                0
                            }
                            Err(err) => {
                                tracing::warn!(path = %shown, %err, "log count task failed; numbering from 1");
                                0
                            }
                        }
                    }
                };
                *self.next.lock() = last + 1;
            })
            .await;
    }

    /// `seq` of the next line (after [`Numbering::ready`]).
    fn next(&self) -> u64 {
        *self.next.lock()
    }
}

/// Shared by every run of an instance: the broadcast channel, the line counter and the
/// hub task (numbering + file writer) that outlives the runs.
#[derive(Debug, Clone)]
pub(crate) struct LogHub {
    sender: broadcast::Sender<LogLine>,
    numbering: Arc<Numbering>,
    raw: mpsc::Sender<RawMsg>,
    options: LogOptions,
}

/// Tasks of one run; [`LogPipeline::finished`] resolves once both pipes reached EOF and
/// their lines were broadcast and written to the file.
#[derive(Debug)]
pub(crate) struct LogPipeline {
    done: JoinHandle<()>,
}

impl LogPipeline {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn finished(self) {
        let _ = self.done.await;
    }
}

impl LogHub {
    /// Starts the hub task (needs a tokio runtime). It numbers from the file set as soon
    /// as it is counted, and ends when the hub and every run are gone.
    pub(crate) fn new(options: LogOptions) -> Self {
        let (sender, _) = broadcast::channel(options.broadcast_capacity.max(1));
        let numbering = Arc::new(Numbering {
            file: options.file.clone(),
            ready: OnceCell::new(),
            next: OrderedMutex::new(LOG_HUB_SEQ, 1),
        });
        let (raw, raw_rx) = mpsc::channel::<RawMsg>(256);
        tokio::spawn(hub(
            raw_rx,
            sender.clone(),
            Arc::clone(&numbering),
            options.clone(),
        ));
        Self {
            sender,
            numbering,
            raw,
            options,
        }
    }

    pub(crate) fn subscribe(&self) -> LogSubscription {
        LogSubscription {
            receiver: self.sender.subscribe(),
            missed: 0,
            numbering: Arc::clone(&self.numbering),
        }
    }

    /// Starts pumping the pipes of a new run.
    pub(crate) fn attach(
        &self,
        stdout: Option<PipeReader>,
        stderr: Option<PipeReader>,
    ) -> LogPipeline {
        let max_line = self.options.max_line_bytes.max(1);
        let pumps: Vec<JoinHandle<()>> = [(LogStream::Stdout, stdout), (LogStream::Stderr, stderr)]
            .into_iter()
            .filter_map(|(stream, reader)| {
                reader.map(|reader| tokio::spawn(pump(stream, reader, self.raw.clone(), max_line)))
            })
            .collect();
        let raw = self.raw.clone();
        let done = tokio::spawn(async move {
            for pump in pumps {
                let _ = pump.await;
            }
            let (ack, acked) = oneshot::channel();
            if raw.send(RawMsg::Flush(ack)).await.is_ok() {
                let _ = acked.await;
            }
        });
        LogPipeline { done }
    }
}

/// Reads one pipe until EOF and forwards complete lines.
async fn pump(
    stream: LogStream,
    mut reader: PipeReader,
    lines: mpsc::Sender<RawMsg>,
    max_line: usize,
) {
    let mut buf = vec![0_u8; 8 * 1024];
    let mut splitter = LineSplitter::new(max_line);
    loop {
        let read = match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => break,
            Err(err) => {
                tracing::warn!(?stream, %err, "reading a process pipe failed; stop capturing it");
                break;
            }
        };
        for bytes in splitter.push(&buf[..read]) {
            if lines
                .send(RawMsg::Line(RawLine { stream, bytes }))
                .await
                .is_err()
            {
                return;
            }
        }
    }
    if let Some(bytes) = splitter.finish() {
        let _ = lines.send(RawMsg::Line(RawLine { stream, bytes })).await;
    }
}

/// Numbers, broadcasts and queues for the file, for every run of the instance.
async fn hub(
    mut raw: mpsc::Receiver<RawMsg>,
    sender: broadcast::Sender<LogLine>,
    numbering: Arc<Numbering>,
    options: LogOptions,
) {
    // Lines wait (the pipes fill) until the numbering continues the files.
    numbering.ready().await;
    let mut file = options.file.clone().map(|path| {
        let (tx, rx) = mpsc::channel::<FileMsg>(options.file_queue.max(1));
        let last = numbering.next().saturating_sub(1);
        (
            tx,
            tokio::spawn(write_file(path, options.rotation, rx, last)),
        )
    });
    while let Some(msg) = raw.recv().await {
        match msg {
            RawMsg::Line(RawLine { stream, mut bytes }) => {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                let seq = {
                    let mut next = numbering.next.lock();
                    let seq = *next;
                    *next += 1;
                    // Numbered and broadcast atomically: a subscriber that read `next`
                    // receives every line from that number on.
                    // No subscriber is not an error; slow subscribers lag on their own.
                    let _ = sender.send(LogLine {
                        seq,
                        ts: Timestamp::now(),
                        stream,
                        text,
                    });
                    seq
                };
                if let Some((queue, _)) = &file {
                    bytes.push(b'\n');
                    // Backpressure: waits while the writer is behind.
                    if queue.send(FileMsg::Line { seq, bytes }).await.is_err() {
                        tracing::warn!("log file writer stopped; output is only broadcast now");
                        file = None;
                    }
                }
            }
            RawMsg::Flush(ack) => match &file {
                Some((queue, _)) => {
                    if let Err(mpsc::error::SendError(FileMsg::Flush(ack))) =
                        queue.send(FileMsg::Flush(ack)).await
                    {
                        let _ = ack.send(());
                    }
                }
                None => {
                    let _ = ack.send(());
                }
            },
        }
    }
    if let Some((queue, writer)) = file {
        drop(queue);
        let _ = writer.await;
    }
}

/// The file side of a hub: the rotating file (open while a run writes), the count of
/// lines in the set and what to check when it is reopened. Blocking.
struct Writer {
    path: PathBuf,
    rotation: Rotation,
    file: Option<RotatingFile>,
    /// Lines in the set (= `seq` of the last line written).
    last_seq: u64,
    /// Size of the current file when it was closed: a different size at reopen means
    /// someone else truncated or replaced it.
    closed_size: Option<u64>,
    /// A write failed: recount the set before writing again.
    resync: bool,
    /// Warned about a failure that still lasts.
    failing: bool,
}

impl Writer {
    fn open(&mut self) {
        if self.resync {
            match set::prepare_append(&self.path) {
                Ok(last) => {
                    self.last_seq = last;
                    self.resync = false;
                }
                Err(err) => {
                    self.warn("cannot recount the instance log", &err);
                    return;
                }
            }
        }
        match RotatingFile::open(&self.path, self.rotation, self.last_seq) {
            Ok(mut file) => {
                if self
                    .closed_size
                    .take()
                    .is_some_and(|size| size != file.size)
                {
                    file.realign();
                }
                self.last_seq = file.last_seq;
                self.file = Some(file);
            }
            Err(err) => self.warn("cannot open the instance log file", &err),
        }
    }

    fn warn(&mut self, what: &str, err: &io::Error) {
        if !self.failing {
            tracing::warn!(path = %self.path.display(), %err, "{what}");
            self.failing = true;
        }
    }

    fn write(&mut self, seq: u64, bytes: &[u8]) {
        if self.file.is_none() {
            self.open();
        }
        // Without a file the line is dropped: the process is never blocked by it.
        let Some(file) = self.file.as_mut() else {
            return;
        };
        match file.write_line(seq, bytes) {
            Ok(()) => {
                self.last_seq = file.last_seq;
                self.failing = false;
            }
            Err(err) => {
                self.warn("writing the instance log failed", &err);
                self.file = None;
                self.resync = true;
            }
        }
    }

    fn flush(&mut self) {
        if let Some(file) = self.file.as_mut() {
            if let Err(err) = file.flush() {
                self.warn("writing the instance log failed", &err);
                self.file = None;
                self.resync = true;
            }
        }
    }

    /// End of a run: flushed and closed until the next line.
    fn close(&mut self) {
        if let Some(file) = self.file.take() {
            self.last_seq = file.last_seq;
            let size = file.size;
            match file.close() {
                Ok(()) => self.closed_size = Some(size),
                Err(err) => {
                    self.warn("writing the instance log failed", &err);
                    self.resync = true;
                }
            }
        }
    }
}

/// Batches queued lines into blocking writes with rotation.
async fn write_file(
    path: PathBuf,
    rotation: Rotation,
    mut queue: mpsc::Receiver<FileMsg>,
    last_seq: u64,
) {
    const MAX_BATCH: usize = 512;
    let mut writer = Some(Writer {
        path,
        rotation,
        file: None,
        last_seq,
        closed_size: None,
        resync: false,
        failing: false,
    });
    let mut batch = Vec::with_capacity(MAX_BATCH);
    while queue.recv_many(&mut batch, MAX_BATCH).await > 0 {
        let Some(mut current) = writer.take() else {
            // The blocking task panicked: keep draining so the process is never blocked.
            for msg in batch.drain(..) {
                if let FileMsg::Flush(ack) = msg {
                    let _ = ack.send(());
                }
            }
            continue;
        };
        let mut items = std::mem::take(&mut batch);
        let written = tokio::task::spawn_blocking(move || {
            let mut acks = Vec::new();
            for msg in items.drain(..) {
                match msg {
                    FileMsg::Line { seq, bytes } => current.write(seq, &bytes),
                    FileMsg::Flush(ack) => {
                        current.close();
                        acks.push(ack);
                    }
                }
            }
            current.flush();
            (current, acks, items)
        })
        .await;
        match written {
            Ok((current, acks, items)) => {
                writer = Some(current);
                for ack in acks {
                    let _ = ack.send(());
                }
                batch = items;
            }
            Err(err) => tracing::warn!(%err, "log file write task failed"),
        }
    }
    if let Some(mut writer) = writer {
        let _ = tokio::task::spawn_blocking(move || writer.close()).await;
    }
}

// Splits a byte stream into lines.
pub(crate) struct LineSplitter {
    partial: Vec<u8>,
    max_line: usize,
}

impl LineSplitter {
    pub(crate) fn new(max_line: usize) -> Self {
        Self {
            partial: Vec::new(),
            max_line: max_line.max(1),
        }
    }

    /// Complete lines found in `chunk` (terminator and trailing `\r` removed).
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        let mut lines = Vec::new();
        for &byte in chunk {
            if byte == b'\n' {
                let mut line = std::mem::take(&mut self.partial);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                lines.push(line);
            } else {
                self.partial.push(byte);
                if self.partial.len() >= self.max_line {
                    lines.push(std::mem::take(&mut self.partial));
                }
            }
        }
        lines
    }

    /// The unterminated rest, if any.
    pub(crate) fn finish(&mut self) -> Option<Vec<u8>> {
        let mut rest = std::mem::take(&mut self.partial);
        if rest.last() == Some(&b'\r') {
            rest.pop();
        }
        (!rest.is_empty()).then_some(rest)
    }
}

/// Append-only file rotated by size; the newest file of a [`set::LogSet::Rotated`] set,
/// whose sidecar it keeps. Blocking.
pub(crate) struct RotatingFile {
    path: PathBuf,
    /// `None` only transiently during a rotation.
    writer: Option<BufWriter<File>>,
    size: u64,
    rotation: Rotation,
    /// Lines in the set: line `n` of the set is `seq` `n`.
    last_seq: u64,
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn rotated(path: &Path, index: u32) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

/// Placeholders written at most for one gap (lines lost by a failed write).
const MAX_PLACEHOLDERS: u64 = 100_000;

impl RotatingFile {
    /// Opens `path` for appending; the set holds `last_seq` lines.
    pub(crate) fn open(path: &Path, rotation: Rotation, last_seq: u64) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = open_append(path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            path: path.to_path_buf(),
            writer: Some(BufWriter::with_capacity(64 * 1024, file)),
            size,
            rotation,
            last_seq,
        })
    }

    /// The file changed while it was closed (truncated or deleted by someone else):
    /// recounts the set and, if it lost lines, records them as dropped so that the next
    /// line still gets the next `seq` (the numbering never goes back).
    fn realign(&mut self) {
        let Ok(snapshot) = set::LogSet::rotated(&self.path).snapshot() else {
            return;
        };
        if snapshot.last_seq < self.last_seq {
            let missing = self.last_seq - snapshot.last_seq;
            let _ = set::rotating(&self.path, |dropped| {
                *dropped += missing;
                Ok(())
            });
            tracing::info!(path = %self.path.display(), missing, "instance log shrank while closed; numbering continues");
        } else if snapshot.last_seq > self.last_seq {
            tracing::warn!(path = %self.path.display(), "instance log grew while closed; its numbering may be off");
            self.last_seq = snapshot.last_seq;
        }
    }

    /// Writes line `seq` (`line` ends with `\n`), rotating when needed (not flushed).
    /// Lines lost before it (a failed write) become empty lines, so line `n` of the set
    /// stays `seq` `n`.
    pub(crate) fn write_line(&mut self, seq: u64, line: &[u8]) -> io::Result<()> {
        let gap = seq.saturating_sub(self.last_seq + 1);
        if gap > MAX_PLACEHOLDERS {
            tracing::warn!(path = %self.path.display(), gap, "instance log far behind its numbering");
        }
        for _ in 0..gap.min(MAX_PLACEHOLDERS) {
            self.write_raw(b"\n")?;
        }
        self.write_raw(line)
    }

    /// Writes every line (each already ends with `\n`) with the next `seq`s; flushes.
    #[cfg(test)]
    pub(crate) fn write_lines(&mut self, lines: &[Vec<u8>]) -> io::Result<()> {
        for line in lines {
            let seq = self.last_seq + 1;
            self.write_line(seq, line)?;
        }
        self.flush()
    }

    fn write_raw(&mut self, line: &[u8]) -> io::Result<()> {
        let len = line.len() as u64;
        if self.size > 0 && self.size + len > self.rotation.max_bytes {
            self.rotate()?;
        }
        self.writer()?.write_all(line)?;
        self.size += len;
        self.last_seq += 1;
        Ok(())
    }

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        self.writer()?.flush()
    }

    fn writer(&mut self) -> io::Result<&mut BufWriter<File>> {
        self.writer
            .as_mut()
            .ok_or_else(|| io::Error::other("log file is closed"))
    }

    /// Every file from `.<max_files-1>` up is dropped (its lines counted in the sidecar),
    /// every other one shifts by one, the current file becomes `.1` and a new one is
    /// started (`max_files == 1`: the file is emptied). If a rename fails (file locked
    /// by a viewer without `FILE_SHARE_DELETE`) the current file keeps growing and
    /// rotation is retried at the next line.
    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut writer) = self.writer.take() {
            // Closed before renaming (Windows refuses to rename an open file otherwise).
            writer.flush()?;
        }
        let keep = self.rotation.max_files.max(1);
        let path = self.path.clone();
        let shifted = set::rotating(&path, |dropped| {
            let mut drop_file = |file: &Path| -> io::Result<()> {
                let (lines, _) = set::count_lines(file)?;
                std::fs::remove_file(file)?;
                *dropped += lines;
                Ok(())
            };
            if keep == 1 {
                return drop_file(&path);
            }
            // The oldest kept index and anything above it (left by a larger `max_files`).
            for (index, oldest) in set::LogSet::siblings(&path)? {
                if index >= keep - 1 {
                    drop_file(&oldest)?;
                }
            }
            for index in (1..keep - 1).rev() {
                let from = rotated(&path, index);
                if from.exists() {
                    std::fs::rename(&from, rotated(&path, index + 1))?;
                }
            }
            std::fs::rename(&path, rotated(&path, 1))
        });
        if let Err(err) = &shifted {
            tracing::warn!(path = %self.path.display(), %err, "log rotation failed; appending");
        }
        let file = open_append(&self.path)?;
        self.size = file.metadata()?.len();
        self.writer = Some(BufWriter::with_capacity(64 * 1024, file));
        Ok(())
    }

    pub(crate) fn close(mut self) -> io::Result<()> {
        match self.writer.take() {
            Some(mut writer) => writer.flush(),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pretty_assertions::assert_eq;

    use super::*;

    fn text(lines: Vec<Vec<u8>>) -> Vec<String> {
        lines
            .into_iter()
            .map(|l| String::from_utf8(l).unwrap())
            .collect()
    }

    #[test]
    fn splitter_handles_crlf_partials_and_long_lines() {
        let mut s = LineSplitter::new(8);
        assert_eq!(text(s.push(b"one\r\ntw")), vec!["one"]);
        assert_eq!(text(s.push(b"o\nthree")), vec!["two"]);
        assert_eq!(text(s.push(b"\n\n")), vec!["three", ""]);
        assert_eq!(text(s.push(b"0123456789")), vec!["01234567"]);
        assert_eq!(s.finish(), Some(b"89".to_vec()));
        assert_eq!(s.finish(), None);
        let mut s = LineSplitter::new(100);
        s.push(b"tail\r");
        assert_eq!(s.finish(), Some(b"tail".to_vec()));
    }

    #[test]
    fn rotation_keeps_max_files_in_total() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("php").join("default.out.log");
        let rotation = Rotation {
            max_bytes: 100,
            max_files: 3,
        };
        let mut file = RotatingFile::open(&path, rotation, 0).unwrap();
        let line = |i: usize| format!("line {i:04} {}\n", "x".repeat(28)).into_bytes(); // 39 bytes
        let lines: Vec<Vec<u8>> = (0..20).map(line).collect();
        file.write_lines(&lines).unwrap();
        file.close().unwrap();

        let mut names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "default.out.log",
                "default.out.log.1",
                "default.out.log.2",
                "default.out.log.seq"
            ]
        );
        for name in names.iter().filter(|n| !n.ends_with(".seq")) {
            let len = std::fs::metadata(path.parent().unwrap().join(name))
                .unwrap()
                .len();
            assert!(len <= 100, "{name}: {len}");
        }
        // Newest lines are in the current file, the previous ones in .1.
        let current = std::fs::read_to_string(&path).unwrap();
        assert!(
            current.ends_with(&String::from_utf8(line(19)).unwrap()),
            "{current}"
        );
        let previous = std::fs::read_to_string(rotated(&path, 1)).unwrap();
        assert!(previous.contains("line 0017"), "{previous}");

        // Two lines per file, three files: the 14 oldest lines were dropped and counted,
        // so `seq` is still the line's number in everything ever written.
        let state = set::read_state(&path);
        assert_eq!((state.dropped, state.rotations % 2), (14, 0), "{state:?}");
        let (snapshot, tail) = set::LogSet::rotated(&path).tail(100).unwrap();
        assert_eq!((snapshot.first_seq, snapshot.last_seq), (15, 20));
        for (seq, text) in &tail {
            assert!(
                text.starts_with(&format!("line {:04}", seq - 1)),
                "{seq} {text}"
            );
        }
        assert_eq!(tail.len(), 6);

        // Reopening appends and keeps rotating from the existing size and numbering.
        let mut file = RotatingFile::open(&path, rotation, 20).unwrap();
        file.write_lines(&[line(20), line(21)]).unwrap();
        file.close().unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("line 0021")
        );
        let (_, tail) = set::LogSet::rotated(&path).tail(1).unwrap();
        assert_eq!(tail[0].0, 22);
        assert!(tail[0].1.starts_with("line 0021"));
    }

    #[test]
    fn single_file_rotation_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.out.log");
        let mut file = RotatingFile::open(
            &path,
            Rotation {
                max_bytes: 10,
                max_files: 1,
            },
            0,
        )
        .unwrap();
        file.write_lines(&[b"12345678\n".to_vec(), b"abc\n".to_vec()])
            .unwrap();
        file.close().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "abc\n");
        // The file and its sidecar: the truncated line is counted, "abc" is line 2.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        let (_, tail) = set::LogSet::rotated(&path).tail(5).unwrap();
        assert_eq!(tail, [(2, "abc".to_owned())]);
    }

    #[tokio::test]
    async fn pipeline_numbers_broadcasts_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.out.log");
        let hub = LogHub::new(LogOptions {
            file: Some(path.clone()),
            ..LogOptions::default()
        });
        let mut sub = hub.subscribe();
        let stdout: PipeReader = Box::pin(&b"a\r\nb\npartial"[..]);
        let stderr: PipeReader = Box::pin(&b"err\n"[..]);
        hub.attach(Some(stdout), Some(stderr)).finished().await;

        let mut got = Vec::new();
        while let Some(line) = sub.try_recv() {
            got.push((line.seq, line.stream, line.text));
        }
        got.sort_by_key(|(seq, _, _)| *seq);
        let seqs: Vec<u64> = got.iter().map(|(s, _, _)| *s).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4]);
        let mut texts: Vec<&str> = got.iter().map(|(_, _, t)| t.as_str()).collect();
        texts.sort_unstable();
        assert_eq!(texts, vec!["a", "b", "err", "partial"]);
        let file = std::fs::read_to_string(&path).unwrap();
        let mut file_lines: Vec<&str> = file.lines().collect();
        let stdout_order: Vec<&str> = file_lines.iter().copied().filter(|l| *l != "err").collect();
        assert_eq!(stdout_order, vec!["a", "b", "partial"], "{file:?}");
        file_lines.sort_unstable();
        assert_eq!(file_lines, vec!["a", "b", "err", "partial"]);
        assert!(!file.contains('\r'));

        // A second run continues the numbering.
        let again: PipeReader = Box::pin(&b"c\n"[..]);
        hub.attach(Some(again), None).finished().await;
        let next = tokio::time::timeout(Duration::from_secs(1), sub.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!((next.seq, next.text.as_str()), (5, "c"));
    }

    #[tokio::test]
    async fn slow_subscriber_lags_without_blocking_others() {
        use tokio::io::AsyncWriteExt;

        let hub = LogHub::new(LogOptions {
            broadcast_capacity: 8,
            ..LogOptions::default()
        });
        let mut slow = hub.subscribe();
        let mut fast = hub.subscribe();
        let (mut writer, reader) = tokio::io::duplex(1024);
        let pipeline = hub.attach(Some(Box::pin(reader)), None);
        let collector = tokio::spawn(async move {
            let mut seen = Vec::new();
            while let Some(line) = fast.recv().await {
                seen.push(line.seq);
            }
            (seen, fast.missed())
        });
        for i in 0..100 {
            writer.write_all(format!("{i}\n").as_bytes()).await.unwrap();
            // Let pump, hub and the fast subscriber run (current-thread runtime).
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        }
        drop(writer);
        pipeline.finished().await;
        drop(hub); // closes the broadcast: the collector ends
        let (seen, fast_missed) = tokio::time::timeout(Duration::from_secs(5), collector)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(seen, (1..=100).collect::<Vec<u64>>());
        assert_eq!(fast_missed, 0);
        match slow.recv_item().await.unwrap() {
            LogItem::Lagged { missed } => assert_eq!(missed, 92),
            other => panic!("expected lag, got {other:?}"),
        }
        assert_eq!(slow.recv().await.unwrap().seq, 93);
        assert_eq!(slow.missed(), 92);
    }

    #[tokio::test]
    async fn numbering_continues_the_file_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.out.log");
        // A previous daemon left 100 rotated-away lines, 5 in `.1` and 10 in the file,
        // the last one cut by a crash.
        set::write_state(
            &path,
            set::SeqState {
                dropped: 100,
                rotations: 2,
            },
        )
        .unwrap();
        std::fs::write(rotated(&path, 1), "old\n".repeat(5)).unwrap();
        std::fs::write(&path, format!("{}cut", "prev\n".repeat(9))).unwrap();
        let hub = LogHub::new(LogOptions {
            file: Some(path.clone()),
            ..LogOptions::default()
        });
        let mut sub = hub.subscribe();
        assert_eq!(sub.start_seq().await, 116);
        let stdout: PipeReader = Box::pin(&b"new\n"[..]);
        hub.attach(Some(stdout), None).finished().await;
        let line = sub.try_recv().unwrap();
        assert_eq!((line.seq, line.text.as_str()), (116, "new"));
        let (snapshot, tail) = set::LogSet::rotated(&path).tail(3).unwrap();
        assert_eq!(
            tail,
            [
                (114, "prev".to_owned()),
                (115, "cut".to_owned()),
                (116, "new".to_owned())
            ]
        );
        assert_eq!(snapshot.first_seq, 101);
        // A subscription taken now starts after the last line.
        assert_eq!(hub.subscribe().start_seq().await, 117);
    }

    #[tokio::test]
    async fn live_seq_matches_the_files_across_rotations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.out.log");
        let hub = LogHub::new(LogOptions {
            file: Some(path.clone()),
            rotation: Rotation {
                max_bytes: 64,
                max_files: 3,
            },
            ..LogOptions::default()
        });
        let mut sub = hub.subscribe();
        for run in 0..4 {
            let text: String = (0..10).map(|i| format!("run {run} line {i}\n")).collect();
            let stdout: PipeReader = Box::pin(std::io::Cursor::new(text.into_bytes()));
            hub.attach(Some(stdout), None).finished().await;
        }
        let mut live = Vec::new();
        while let Some(line) = sub.try_recv() {
            live.push((line.seq, line.text));
        }
        assert_eq!(live.len(), 40);
        assert!(live.windows(2).all(|w| w[1].0 == w[0].0 + 1));
        // Whatever the files still hold has the seq the live line had.
        let (snapshot, kept) = set::LogSet::rotated(&path).range(1, 100, 100).unwrap();
        assert!(snapshot.first_seq > 1, "rotation deleted something");
        assert!(!kept.is_empty());
        for (seq, text) in &kept {
            assert_eq!(&live[(*seq - 1) as usize], &(*seq, text.clone()));
        }
        assert_eq!(kept.last().unwrap().0, 40);
    }
}
