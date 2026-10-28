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
//! * The **hub** numbers lines (`seq` grows across restarts of the instance), stamps
//!   them, broadcasts them and queues them for the file.
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
use std::sync::atomic::{AtomicU64, Ordering};

use forge_core::Timestamp;
use forge_core::platform::PipeReader;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

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
    /// Per instance, strictly increasing by one (also across restarts).
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
}

impl LogSubscription {
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

/// Shared by every run of an instance: the broadcast channel and the line counter.
#[derive(Debug, Clone)]
pub(crate) struct LogHub {
    sender: broadcast::Sender<LogLine>,
    seq: Arc<AtomicU64>,
    options: LogOptions,
}

/// Tasks of one run; [`LogPipeline::finished`] resolves once both pipes reached EOF and
/// the file was flushed and closed.
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
    pub(crate) fn new(options: LogOptions) -> Self {
        let (sender, _) = broadcast::channel(options.broadcast_capacity.max(1));
        Self {
            sender,
            seq: Arc::new(AtomicU64::new(1)),
            options,
        }
    }

    pub(crate) fn subscribe(&self) -> LogSubscription {
        LogSubscription {
            receiver: self.sender.subscribe(),
            missed: 0,
        }
    }

    /// Starts pumping the pipes of a new run.
    pub(crate) fn attach(
        &self,
        stdout: Option<PipeReader>,
        stderr: Option<PipeReader>,
    ) -> LogPipeline {
        let (raw_tx, raw_rx) = mpsc::channel::<RawLine>(256);
        let max_line = self.options.max_line_bytes.max(1);
        for (stream, reader) in [(LogStream::Stdout, stdout), (LogStream::Stderr, stderr)] {
            if let Some(reader) = reader {
                tokio::spawn(pump(stream, reader, raw_tx.clone(), max_line));
            }
        }
        drop(raw_tx);

        let (file_tx, file_rx) = mpsc::channel::<Vec<u8>>(self.options.file_queue.max(1));
        let writer = self
            .options
            .file
            .clone()
            .map(|path| tokio::spawn(write_file(path, self.options.rotation, file_rx)));
        let hub = tokio::spawn(hub(
            raw_rx,
            self.sender.clone(),
            Arc::clone(&self.seq),
            writer.is_some().then_some(file_tx),
        ));
        let done = tokio::spawn(async move {
            let _ = hub.await;
            if let Some(writer) = writer {
                let _ = writer.await;
            }
        });
        LogPipeline { done }
    }
}

/// Reads one pipe until EOF and forwards complete lines.
async fn pump(
    stream: LogStream,
    mut reader: PipeReader,
    lines: mpsc::Sender<RawLine>,
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
            if lines.send(RawLine { stream, bytes }).await.is_err() {
                return;
            }
        }
    }
    if let Some(bytes) = splitter.finish() {
        let _ = lines.send(RawLine { stream, bytes }).await;
    }
}

/// Numbers, broadcasts and queues for the file.
async fn hub(
    mut lines: mpsc::Receiver<RawLine>,
    sender: broadcast::Sender<LogLine>,
    seq: Arc<AtomicU64>,
    file: Option<mpsc::Sender<Vec<u8>>>,
) {
    while let Some(RawLine { stream, mut bytes }) = lines.recv().await {
        let line = LogLine {
            seq: seq.fetch_add(1, Ordering::Relaxed),
            ts: Timestamp::now(),
            stream,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        };
        // No subscriber is not an error; slow subscribers lag on their own.
        let _ = sender.send(line);
        if let Some(file) = &file {
            bytes.push(b'\n');
            // Backpressure: waits while the writer is behind.
            if file.send(bytes).await.is_err() {
                tracing::warn!("log file writer stopped; output is only broadcast now");
                break;
            }
        }
    }
    // Keep draining to the subscribers if the file writer died.
    while let Some(RawLine { stream, bytes }) = lines.recv().await {
        let _ = sender.send(LogLine {
            seq: seq.fetch_add(1, Ordering::Relaxed),
            ts: Timestamp::now(),
            stream,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
}

/// Batches queued lines into blocking writes with rotation.
async fn write_file(path: PathBuf, rotation: Rotation, mut queue: mpsc::Receiver<Vec<u8>>) {
    const MAX_BATCH: usize = 512;
    let opened = {
        let path = path.clone();
        tokio::task::spawn_blocking(move || RotatingFile::open(&path, rotation)).await
    };
    let mut file = match opened {
        Ok(Ok(file)) => Some(file),
        Ok(Err(err)) => {
            tracing::warn!(path = %path.display(), %err, "cannot open the instance log file");
            None
        }
        Err(err) => {
            tracing::warn!(%err, "log file open task failed");
            None
        }
    };
    let mut batch = Vec::with_capacity(MAX_BATCH);
    while queue.recv_many(&mut batch, MAX_BATCH).await > 0 {
        let Some(mut current) = file.take() else {
            batch.clear();
            continue; // keep draining so the process is never blocked by a dead file
        };
        let lines = std::mem::take(&mut batch);
        let written = tokio::task::spawn_blocking(move || {
            let result = current.write_lines(&lines);
            (current, result, lines)
        })
        .await;
        match written {
            Ok((current, result, mut lines)) => {
                if let Err(err) = result {
                    tracing::warn!(path = %path.display(), %err, "writing the instance log failed");
                }
                file = Some(current);
                lines.clear();
                batch = lines;
            }
            Err(err) => tracing::warn!(%err, "log file write task failed"),
        }
    }
    if let Some(file) = file {
        let _ = tokio::task::spawn_blocking(move || file.close()).await;
    }
}

/// Splits a byte stream into lines.
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

/// Append-only file rotated by size. Blocking.
pub(crate) struct RotatingFile {
    path: PathBuf,
    /// `None` only transiently during a rotation.
    writer: Option<BufWriter<File>>,
    size: u64,
    rotation: Rotation,
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn rotated(path: &Path, index: u32) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

impl RotatingFile {
    pub(crate) fn open(path: &Path, rotation: Rotation) -> io::Result<Self> {
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
        })
    }

    /// Writes every line (each already ends with `\n`), rotating when needed; flushes.
    pub(crate) fn write_lines(&mut self, lines: &[Vec<u8>]) -> io::Result<()> {
        for line in lines {
            let len = line.len() as u64;
            if self.size > 0 && self.size + len > self.rotation.max_bytes {
                self.rotate()?;
            }
            self.writer()?.write_all(line)?;
            self.size += len;
        }
        self.writer()?.flush()
    }

    fn writer(&mut self) -> io::Result<&mut BufWriter<File>> {
        self.writer
            .as_mut()
            .ok_or_else(|| io::Error::other("log file is closed"))
    }

    /// The oldest file (`.<max_files-1>`) is dropped, every other one shifts by one, the
    /// current file becomes `.1` and a new one is started (`max_files == 1`: the file is
    /// emptied). If a rename fails (file locked by a viewer without `FILE_SHARE_DELETE`)
    /// the current file keeps growing and rotation is retried at the next line.
    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut writer) = self.writer.take() {
            // Closed before renaming (Windows refuses to rename an open file otherwise).
            writer.flush()?;
        }
        let keep = self.rotation.max_files.max(1);
        let shifted = if keep == 1 {
            std::fs::remove_file(&self.path)
        } else {
            (|| -> io::Result<()> {
                let oldest = rotated(&self.path, keep - 1);
                if oldest.exists() {
                    std::fs::remove_file(&oldest)?;
                }
                for index in (1..keep - 1).rev() {
                    let from = rotated(&self.path, index);
                    if from.exists() {
                        std::fs::rename(&from, rotated(&self.path, index + 1))?;
                    }
                }
                std::fs::rename(&self.path, rotated(&self.path, 1))
            })()
        };
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
        let mut file = RotatingFile::open(&path, rotation).unwrap();
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
            vec!["default.out.log", "default.out.log.1", "default.out.log.2"]
        );
        for name in &names {
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

        // Reopening appends and keeps rotating from the existing size.
        let mut file = RotatingFile::open(&path, rotation).unwrap();
        file.write_lines(&[line(20), line(21)]).unwrap();
        file.close().unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("line 0021")
        );
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
        )
        .unwrap();
        file.write_lines(&[b"12345678\n".to_vec(), b"abc\n".to_vec()])
            .unwrap();
        file.close().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "abc\n");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
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
}
