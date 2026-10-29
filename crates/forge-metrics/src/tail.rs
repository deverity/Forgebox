//! Access log tailer (docs/14 section 3.2): follows one log file by polling and hands its
//! lines, decoded, to the aggregator in batches.
//!
//! One [`Tailer`] per file, run as its own task. Every tick (250 ms ± 10 %, backing off to
//! 1 s while nothing happens, 50 ms while catching up) it moves its reading state into
//! `spawn_blocking`, reads at most 4 MiB and sends what it decoded through a bounded channel
//! with `try_send`. A full channel never blocks the tailer: it keeps the batch, stops reading
//! (the file is the buffer) and samples once it reads again. The runtime only ever runs the
//! bookkeeping between ticks.
//!
//! # Following the file
//!
//! Only std is used (no `notify`, no Win32): directory change notifications are lazy for a
//! file another process holds open, and polling a size is cheap. `File::open` keeps std's
//! share mode `READ | WRITE | DELETE`, so the server or the daemon can rename or delete the
//! file while it is tailed.
//!
//! The open handle is the file being read; the path is looked at (opened, `metadata`, first
//! [`HEAD_BYTES`] bytes) on every tick to see whether it still names that file:
//!
//! - **Same file**: same creation time and the first bytes of the path are a prefix of the
//!   first bytes of the handle, read right after. The creation time alone is not enough:
//!   NTFS *tunneling* gives a file created under a name removed less than 15 s earlier the
//!   creation time of the removed one, which is exactly what a rename + reopen or a delete +
//!   recreate does (measured on Windows 10 19045, docs/14 section 3.2).
//! - **Replaced** (rename + new file, delete + recreate, a server that rolls its own file):
//!   the old handle is *retired* and drained first on every tick until it has been at its
//!   end for `retire_grace` (2 s: workers that have not reopened yet still write to it);
//!   the new file is read from its first byte.
//! - **Vanished** (renamed or deleted, nothing new yet): the handle keeps being read; it is
//!   released once drained and idle for `orphan_grace` (5 s), or at once when the name is
//!   pending deletion (legacy delete semantics: holding the file would keep the name taken).
//! - **Truncated** (`len < offset`, or its first bytes changed under the handle, which
//!   catches a truncate followed by a quick regrowth): read again from the start; the
//!   partial line of the old content is dropped.
//! - **Missing**: waited for; a file that appears later is read from its first byte.
//!
//! # Checkpoints
//!
//! [`Checkpoint`] `{creation_time, offset, head_len, head_hash}` points right after the last
//! complete line of a batch. Each [`TailBatch`] carries the checkpoint of the file at the
//! path, so persistence stores it in the same transaction as the data it covers. Resuming
//! ([`StartAt::Checkpoint`]) checks the identity (creation time, `len >= offset` and the hash
//! of the first `head_len` bytes) and a lag of at most `resume_max_lag` (64 MiB); otherwise
//! the tailer starts at the end of the file and logs a warning.
//!
//! # Sampling
//!
//! When the backlog is over `sample_backlog` (8 MiB), the previous tick hit the read cap or
//! the channel was full, `k` doubles (up to `max_sample`, 16); otherwise it halves back to 1.
//! With `k > 1` every block of `k` lines yields one decoded line, at a random position in the
//! block (no aliasing with periodic traffic); the batch's `weight` is `k`, so counts scaled by
//! it stay exact up to one partial block per change of `k`. Lines are always counted.

use std::collections::hash_map::RandomState;
use std::fs::{File, Metadata};
use std::hash::BuildHasher;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::parse::{AccessParser, MAX_LINE_BYTES};
use crate::record::AccessRecord;

/// Bytes at the start of a file kept (in memory) and hashed (in checkpoints) to tell files
/// with the same creation time apart.
pub const HEAD_BYTES: usize = 256;

/// Rotated files drained at the same time; a faster rotation drops the oldest.
const MAX_RETIRED: usize = 4;
/// Size of one read.
const CHUNK_BYTES: usize = 64 * 1024;
/// The same I/O error is logged at most this often.
const ERROR_LOG_EVERY: Duration = Duration::from_secs(60);

/// Tuning of a [`Tailer`]. The defaults are the ones of docs/14 section 3.2.
#[derive(Debug, Clone, PartialEq)]
pub struct TailConfig {
    /// Interval between ticks while lines keep coming.
    pub tick: Duration,
    /// Longest interval: idle ticks double the interval up to this.
    pub idle_tick: Duration,
    /// Interval after a tick that hit `max_read`.
    pub catch_up_tick: Duration,
    /// Every interval is multiplied by `1 ± jitter` (uniform).
    pub jitter: f64,
    /// Bytes read per tick, all files together.
    pub max_read: u64,
    /// Backlog above which lines are sampled.
    pub sample_backlog: u64,
    /// Largest `k` (1 line in `k` decoded).
    pub max_sample: u32,
    /// Largest lag (bytes appended since the checkpoint) a resume accepts.
    pub resume_max_lag: u64,
    /// Longest line (without its terminator); longer lines are counted and skipped.
    pub max_line: usize,
    /// A rotated file is drained until it has been at its end for this long.
    pub retire_grace: Duration,
    /// A file whose name went away is released after being at its end for this long.
    pub orphan_grace: Duration,
    /// How long a stop waits for a read in progress.
    pub stop_grace: Duration,
}

impl Default for TailConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_millis(250),
            idle_tick: Duration::from_secs(1),
            catch_up_tick: Duration::from_millis(50),
            jitter: 0.1,
            max_read: 4 * 1024 * 1024,
            sample_backlog: 8 * 1024 * 1024,
            max_sample: 16,
            resume_max_lag: 64 * 1024 * 1024,
            max_line: MAX_LINE_BYTES,
            retire_grace: Duration::from_secs(2),
            orphan_grace: Duration::from_secs(5),
            stop_grace: Duration::from_secs(2),
        }
    }
}

impl TailConfig {
    fn sanitized(mut self) -> Self {
        let min = Duration::from_millis(1);
        self.tick = self.tick.max(min);
        self.idle_tick = self.idle_tick.max(self.tick);
        self.catch_up_tick = self.catch_up_tick.clamp(min, self.tick);
        self.jitter = if self.jitter.is_finite() {
            self.jitter.clamp(0.0, 0.5)
        } else {
            0.0
        };
        self.max_read = self.max_read.max(1);
        self.max_sample = self.max_sample.clamp(1, 1024);
        self.max_line = self.max_line.max(1);
        self
    }
}

/// Where a file is in the stream of lines, stable across daemon restarts.
///
/// Persisted by the metrics store (`metrics_tail`) in the same transaction as the data of the
/// batch that carried it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Creation time of the file: on Windows the FILETIME (100 ns ticks since 1601-01-01 UTC);
    /// elsewhere the birth time in the same unit, or 0 when the platform has none.
    pub creation_time: u64,
    /// Offset right after the last complete line handed out.
    pub offset: u64,
    /// Bytes from the start of the file covered by `head_hash`: `min(offset, 256)`.
    pub head_len: u16,
    /// FNV-1a 64 of the first `head_len` bytes (stable across builds; never the bytes
    /// themselves, which could hold a query string, docs/14 section 7).
    pub head_hash: u64,
}

impl Checkpoint {
    fn of(creation_time: u64, offset: u64, head: &[u8]) -> Self {
        let len = head
            .len()
            .min(HEAD_BYTES)
            .min(usize::try_from(offset).unwrap_or(usize::MAX));
        Self {
            creation_time,
            offset,
            head_len: u16::try_from(len).unwrap_or(u16::MAX),
            head_hash: fnv1a(&head[..len]),
        }
    }
}

/// Where the first file is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartAt {
    /// From its first byte.
    Beginning,
    /// From its end (the rest of a line being written is skipped).
    End,
    /// From a checkpoint when it still applies, else from the end (with a warning).
    Checkpoint(Checkpoint),
}

/// What happened to the [`StartAt`] of a tailer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    /// The first tick has not run yet.
    Pending,
    /// The file did not exist: it is read from its first byte once it appears.
    Missing,
    /// Read from the first byte.
    Beginning,
    /// Read from the end.
    End,
    /// Resumed at the checkpoint; `lag` bytes were behind it.
    Resumed {
        /// Bytes appended since the checkpoint.
        lag: u64,
    },
    /// The checkpoint did not apply: read from the end.
    Rejected(ResumeReject),
}

/// Why a checkpoint was not resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeReject {
    /// Another creation time: another file.
    CreationTime,
    /// The file is shorter than the checkpoint offset.
    Shorter {
        /// Size of the file.
        len: u64,
    },
    /// Same creation time but other first bytes: another file (NTFS tunneling) or rewritten.
    Content,
    /// More than `resume_max_lag` bytes behind.
    Lag {
        /// Bytes appended since the checkpoint.
        lag: u64,
    },
}

/// What a tailer is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailState {
    /// The first tick has not run yet.
    Starting,
    /// No file at the path: waiting for one.
    Waiting,
    /// Reading the file at the path.
    Following,
    /// The path no longer names the file being read (renamed or deleted); still reading it.
    Orphaned,
    /// Stopped (asked to, or the consumer went away).
    Stopped,
    /// A tick panicked; restart it from [`TailStatus::delivered`].
    Failed,
}

/// Counters since the tailer started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TailTotals {
    /// Complete non-blank lines read.
    pub lines: u64,
    /// Items decoded (each worth the `weight` of its batch).
    pub items: u64,
    /// Sum of items × weight: the estimate of the decodable lines.
    pub scaled: u64,
    /// Lines the decoder rejected (only sampled lines are decoded) plus overlong lines.
    pub rejected: u64,
    /// Lines longer than `max_line`.
    pub overlong: u64,
    /// Bytes read.
    pub bytes: u64,
    /// Switches to a new file at the path.
    pub rotations: u64,
    /// Restarts of the same file from its first byte.
    pub truncations: u64,
    /// Batches produced with `weight > 1`.
    pub sampled_batches: u64,
    /// Batches the channel accepted.
    pub batches: u64,
    /// Ticks skipped because the channel was full.
    pub deferred_ticks: u64,
    /// Ticks that ran.
    pub ticks: u64,
}

/// Snapshot of a tailer, published after every tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailStatus {
    /// What it is doing.
    pub state: TailState,
    /// What became of [`StartAt`].
    pub start: StartOutcome,
    /// Checkpoint offset in the file at the path (0 without one).
    pub offset: u64,
    /// Size of that file at the last tick.
    pub len: u64,
    /// Current `k` (1 = every line decoded).
    pub sample: u32,
    /// Counters.
    pub totals: TailTotals,
    /// Checkpoint of the last batch the channel accepted: resume from here after a restart.
    pub delivered: Option<Checkpoint>,
    /// Last I/O problem of the latest tick, if any.
    pub last_error: Option<String>,
}

impl TailStatus {
    /// Bytes of the file at the path not read yet.
    pub fn backlog(&self) -> u64 {
        self.len.saturating_sub(self.offset)
    }
}

/// Lines of one tick, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct TailBatch<T> {
    /// The [`TailSpec::key`] of the tailer.
    pub key: Arc<str>,
    /// Decoded lines, in file order (a rotated file first).
    pub items: Vec<T>,
    /// Lines each item stands for (`k`); 1 when nothing was sampled.
    pub weight: u32,
    /// Complete non-blank lines consumed, decoded or not.
    pub lines: u64,
    /// Lines rejected by the decoder or too long.
    pub rejected: u64,
    /// Bytes read.
    pub bytes: u64,
    /// Position after this batch in the file at the path (`None`: no file there).
    pub checkpoint: Option<Checkpoint>,
}

impl<T> TailBatch<T> {
    /// Whether the lines were sampled: aggregates fed by this batch are estimates.
    pub fn sampled(&self) -> bool {
        self.weight > 1
    }

    /// Items × weight.
    pub fn scaled(&self) -> u64 {
        self.items.len() as u64 * u64::from(self.weight)
    }
}

/// Turns a complete line (without `\n` and a final `\r`) into an item. Runs on the blocking
/// pool, once per sampled line.
pub trait LineDecoder: Send + 'static {
    /// What a line becomes.
    type Item: Send + 'static;
    /// `None` rejects the line (counted in [`TailBatch::rejected`]).
    fn decode(&mut self, line: &[u8]) -> Option<Self::Item>;
}

impl<T, F> LineDecoder for F
where
    F: FnMut(&[u8]) -> Option<T> + Send + 'static,
    T: Send + 'static,
{
    type Item = T;

    fn decode(&mut self, line: &[u8]) -> Option<T> {
        self(line)
    }
}

impl LineDecoder for AccessParser {
    type Item = AccessRecord;

    fn decode(&mut self, line: &[u8]) -> Option<AccessRecord> {
        self.parse(line).ok()
    }
}

/// The file a tailer follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailSpec {
    /// Name of the source in batches and logs (the `path_key` of the persisted checkpoint).
    pub key: Arc<str>,
    /// Log file.
    pub path: PathBuf,
    /// Where to start.
    pub start: StartAt,
}

/// A tailer not running yet: [`Tailer::run`] is its task (see [`spawn`]).
pub struct Tailer<D: LineDecoder> {
    reader: Reader<D>,
    tx: mpsc::Sender<TailBatch<D::Item>>,
    stop: watch::Receiver<bool>,
    status: watch::Sender<TailStatus>,
}

/// Stop switch and status of a tailer. Dropping it stops the tailer.
#[derive(Debug)]
pub struct TailControl {
    stop: watch::Sender<bool>,
    status: watch::Receiver<TailStatus>,
}

impl TailControl {
    /// Latest status.
    pub fn status(&self) -> TailStatus {
        self.status.borrow().clone()
    }

    /// Status updates (one per tick). A `Ref` borrowed from it is a read guard of the
    /// channel: never hold one across `.await`, or the tailer's next update waits for it.
    pub fn watch(&self) -> watch::Receiver<TailStatus> {
        self.status.clone()
    }

    /// Asks the tailer to stop after the current tick (never waits).
    pub fn request_stop(&self) {
        self.stop.send_replace(true);
    }
}

/// A spawned tailer. Dropping it stops the tailer.
#[derive(Debug)]
pub struct TailerHandle {
    control: TailControl,
    join: JoinHandle<TailStatus>,
}

impl TailerHandle {
    /// Stop switch and status.
    pub fn control(&self) -> &TailControl {
        &self.control
    }

    /// Latest status.
    pub fn status(&self) -> TailStatus {
        self.control.status()
    }

    /// Status updates (see [`TailControl::watch`]).
    pub fn watch(&self) -> watch::Receiver<TailStatus> {
        self.control.watch()
    }

    /// Whether the task ended (stopped, consumer gone or failed).
    pub fn is_finished(&self) -> bool {
        self.join.is_finished()
    }

    /// Stops the tailer and returns its final status, waiting at most `timeout` (then the
    /// task is aborted and the last published status returned). Batches not accepted by the
    /// channel are dropped: they are after [`TailStatus::delivered`].
    pub async fn stop(self, timeout: Duration) -> TailStatus {
        let Self { control, mut join } = self;
        control.request_stop();
        match tokio::time::timeout(timeout, &mut join).await {
            Ok(Ok(status)) => status,
            Ok(Err(_)) | Err(_) => {
                join.abort();
                let mut status = control.status();
                if status.state != TailState::Failed {
                    status.state = TailState::Stopped;
                }
                status
            }
        }
    }
}

/// Starts a tailer on the current tokio runtime.
pub fn spawn<D: LineDecoder>(
    spec: TailSpec,
    config: TailConfig,
    decoder: D,
    tx: mpsc::Sender<TailBatch<D::Item>>,
) -> TailerHandle {
    let (tailer, control) = Tailer::new(spec, config, decoder, tx);
    TailerHandle {
        control,
        join: tokio::spawn(tailer.run()),
    }
}

impl<D: LineDecoder> Tailer<D> {
    /// A tailer for `spec` sending to `tx`. No I/O happens before the first tick.
    pub fn new(
        spec: TailSpec,
        config: TailConfig,
        decoder: D,
        tx: mpsc::Sender<TailBatch<D::Item>>,
    ) -> (Self, TailControl) {
        let reader = Reader::new(spec, config.sanitized(), decoder);
        let (stop_tx, stop_rx) = watch::channel(false);
        let (status_tx, status_rx) = watch::channel(reader.status());
        (
            Self {
                reader,
                tx,
                stop: stop_rx,
                status: status_tx,
            },
            TailControl {
                stop: stop_tx,
                status: status_rx,
            },
        )
    }

    /// The tailer's task: ticks until stopped, until the consumer is gone or until a tick
    /// panics. Returns the final status.
    pub async fn run(self) -> TailStatus {
        let Self {
            reader,
            tx,
            mut stop,
            status,
        } = self;
        let config = reader.config.clone();
        let key = reader.key.clone();
        let mut rng = Rng::seeded();
        let mut current = status.borrow().clone();
        let mut reader = Some(reader);
        let mut pending: Option<TailBatch<D::Item>> = None;
        let mut pressure = false;
        let mut delay = Duration::ZERO;
        let mut idle = config.tick;
        let mut out = Delivery::default();
        let mut final_state = TailState::Stopped;

        loop {
            if delay.is_zero() {
                if stop_seen(&mut stop) {
                    break;
                }
            } else {
                tokio::select! {
                    biased;
                    () = stop_requested(&mut stop) => break,
                    () = tokio::time::sleep(delay) => {}
                }
            }

            // The file is the buffer: nothing is read while the consumer is behind.
            if let Some(batch) = pending.take() {
                match out.offer(&tx, batch) {
                    Offer::Sent => {}
                    Offer::Full(batch) => {
                        pending = Some(batch);
                        pressure = true;
                        out.deferred += 1;
                        out.publish(&status, &mut current);
                        delay = jittered(config.tick, config.jitter, &mut rng);
                        continue;
                    }
                    Offer::Closed => break,
                }
            }

            let Some(mut r) = reader.take() else { break };
            let tick_pressure = std::mem::take(&mut pressure);
            let mut job = tokio::task::spawn_blocking(move || {
                let tick = r.tick(tick_pressure);
                (r, tick)
            });
            let (stopping, joined) = tokio::select! {
                biased;
                joined = &mut job => (false, joined),
                () = stop_requested(&mut stop) => {
                    match tokio::time::timeout(config.stop_grace, &mut job).await {
                        Ok(joined) => (true, joined),
                        Err(_) => {
                            current.last_error =
                                Some("stopped while a read was still running".to_owned());
                            break;
                        }
                    }
                }
            };
            let (r, tick) = match joined {
                Ok(done) => done,
                Err(e) => {
                    tracing::error!(
                        key = %key,
                        error = %e,
                        "access log tailer tick failed"
                    );
                    current.last_error = Some(format!("tick failed: {e}"));
                    final_state = TailState::Failed;
                    break;
                }
            };
            reader = Some(r);
            current = tick.status;
            if let Some(batch) = tick.batch {
                match out.offer(&tx, batch) {
                    Offer::Sent => {}
                    Offer::Full(batch) => {
                        pending = Some(batch);
                        pressure = true;
                    }
                    Offer::Closed => break,
                }
            }
            out.publish(&status, &mut current);
            if stopping {
                break;
            }
            let base = if tick.cap_hit {
                config.catch_up_tick
            } else if tick.active {
                idle = config.tick;
                config.tick
            } else {
                let wait = idle;
                idle = (idle * 2).min(config.idle_tick);
                wait
            };
            delay = jittered(base, config.jitter, &mut rng);
        }

        current.state = final_state;
        out.publish(&status, &mut current);
        current
    }
}

/// Delivery side of the task: what the channel accepted.
#[derive(Default)]
struct Delivery {
    delivered: Option<Checkpoint>,
    batches: u64,
    deferred: u64,
}

enum Offer<T> {
    Sent,
    Full(TailBatch<T>),
    Closed,
}

impl Delivery {
    fn offer<T>(&mut self, tx: &mpsc::Sender<TailBatch<T>>, batch: TailBatch<T>) -> Offer<T> {
        let checkpoint = batch.checkpoint;
        match tx.try_send(batch) {
            Ok(()) => {
                self.delivered = checkpoint;
                self.batches += 1;
                Offer::Sent
            }
            Err(TrySendError::Full(batch)) => Offer::Full(batch),
            Err(TrySendError::Closed(_)) => Offer::Closed,
        }
    }

    fn publish(&self, status: &watch::Sender<TailStatus>, current: &mut TailStatus) {
        current.delivered = self.delivered;
        current.totals.batches = self.batches;
        current.totals.deferred_ticks = self.deferred;
        status.send_replace(current.clone());
    }
}

/// Whether a stop was asked (or the control dropped), without waiting.
fn stop_seen(stop: &mut watch::Receiver<bool>) -> bool {
    match stop.has_changed() {
        Ok(_) => *stop.borrow_and_update(),
        Err(_) => true,
    }
}

/// Resolves once a stop is asked or the control is dropped.
async fn stop_requested(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}

fn jittered(base: Duration, jitter: f64, rng: &mut Rng) -> Duration {
    base.mul_f64(1.0 + jitter * (2.0 * rng.unit() - 1.0))
}

// ---------------------------------------------------------------------------------------
// Blocking side: everything below runs inside `spawn_blocking`.

/// Result of one tick.
struct Tick<T> {
    batch: Option<TailBatch<T>>,
    status: TailStatus,
    /// Bytes were read or the file changed.
    active: bool,
    /// The read cap stopped the tick before the end of the data.
    cap_hit: bool,
}

/// The reading state of a tailer, moved into `spawn_blocking` for each tick.
struct Reader<D: LineDecoder> {
    key: Arc<str>,
    path: PathBuf,
    config: TailConfig,
    /// Consumed by the first look at the path.
    initial: Option<StartAt>,
    start: StartOutcome,
    /// The file the path named last.
    cur: Option<OpenFile>,
    /// Files the path no longer names, drained first.
    retired: Vec<OpenFile>,
    /// A file was opened before: the next one is a rotation.
    had_file: bool,
    sink: Sink<D>,
    chunk: Vec<u8>,
    cap_hit: bool,
    last_checkpoint: Option<Checkpoint>,
    totals: TailTotals,
    last_error: Option<String>,
    logged: Option<(io::ErrorKind, Instant)>,
    ran: bool,
}

impl<D: LineDecoder> Reader<D> {
    fn new(spec: TailSpec, config: TailConfig, decoder: D) -> Self {
        Self {
            key: spec.key,
            path: spec.path,
            initial: Some(spec.start),
            start: StartOutcome::Pending,
            cur: None,
            retired: Vec::new(),
            had_file: false,
            sink: Sink {
                decoder,
                sampler: Sampler::new(Rng::seeded()),
                max_line: config.max_line,
                items: Vec::new(),
                lines: 0,
                rejected: 0,
                overlong: 0,
            },
            chunk: Vec::new(),
            cap_hit: false,
            last_checkpoint: None,
            totals: TailTotals::default(),
            last_error: None,
            logged: None,
            ran: false,
            config,
        }
    }

    fn state(&self) -> TailState {
        match &self.cur {
            _ if !self.ran => TailState::Starting,
            None => TailState::Waiting,
            Some(f) if f.orphan_since.is_some() => TailState::Orphaned,
            Some(_) => TailState::Following,
        }
    }

    fn status(&self) -> TailStatus {
        TailStatus {
            state: self.state(),
            start: self.start,
            offset: self.cur.as_ref().map_or(0, |f| f.line_start),
            len: self.cur.as_ref().map_or(0, |f| f.len),
            sample: self.sink.sampler.k,
            totals: self.totals,
            delivered: None,
            last_error: self.last_error.clone(),
        }
    }

    fn tick(&mut self, pressure: bool) -> Tick<D::Item> {
        let now = Instant::now();
        self.ran = true;
        self.totals.ticks += 1;
        self.last_error = None;
        if self.chunk.is_empty() {
            self.chunk = vec![0; CHUNK_BYTES];
        }
        let changes_before = self.totals.rotations + self.totals.truncations;
        let state_before = self.state();

        self.maintain(now);

        let backlog = self.backlog();
        self.sink.sampler.adjust(
            pressure || self.cap_hit || backlog > self.config.sample_backlog,
            self.config.max_sample,
        );

        let mut budget = self.config.max_read;
        let mut cap_hit = false;
        let mut bytes = 0;

        // Rotated files first: their lines were written before the ones of the new file.
        let mut kept = Vec::with_capacity(self.retired.len());
        for mut f in std::mem::take(&mut self.retired) {
            let idle = now.saturating_duration_since(f.last_growth) >= self.config.retire_grace;
            match f.unchanged() {
                Ok(true) => {}
                Ok(false) => {
                    tracing::debug!(key = %self.key, "rotated access log was truncated; dropped");
                    continue;
                }
                Err(e) => {
                    self.note("check rotated file", &e);
                    if !idle {
                        kept.push(f);
                    }
                    continue;
                }
            }
            let (end, n) = drain(&mut f, &mut self.sink, &mut self.chunk, &mut budget, now);
            bytes += n;
            match end {
                ReadEnd::Budget => {
                    cap_hit = true;
                    kept.push(f);
                }
                ReadEnd::Eof => {
                    if now.saturating_duration_since(f.last_growth) >= self.config.retire_grace {
                        finish(f, &mut self.sink);
                    } else {
                        kept.push(f);
                    }
                }
                ReadEnd::Error(e) => {
                    self.note("read rotated file", &e);
                    if idle {
                        finish(f, &mut self.sink);
                    } else {
                        kept.push(f);
                    }
                }
            }
        }
        self.retired = kept;

        let mut release = false;
        let mut read_error = None;
        if let Some(f) = self.cur.as_mut() {
            let (end, n) = drain(f, &mut self.sink, &mut self.chunk, &mut budget, now);
            bytes += n;
            let at_end = match end {
                ReadEnd::Eof => true,
                ReadEnd::Budget => {
                    cap_hit = true;
                    false
                }
                ReadEnd::Error(e) => {
                    read_error = Some(e);
                    false
                }
            };
            // Small files: keep the identity bytes up to date for the checkpoint.
            if let Err(e) = f.refresh_head() {
                read_error.get_or_insert(e);
            }
            if at_end {
                if let Some(since) = f.orphan_since {
                    let quiet = now.saturating_duration_since(since.max(f.last_growth));
                    release = f.release_asap || quiet >= self.config.orphan_grace;
                }
            }
        }
        if let Some(e) = read_error {
            self.note("read", &e);
        }
        if release {
            if let Some(f) = self.cur.take() {
                tracing::debug!(key = %self.key, "access log removed; released the old file");
                finish(f, &mut self.sink);
            }
        }
        self.cap_hit = cap_hit;

        // Output.
        let checkpoint = self.cur.as_ref().map(OpenFile::checkpoint);
        let sink = &mut self.sink;
        let weight = sink.sampler.k;
        let items = std::mem::take(&mut sink.items);
        let scaled = items.len() as u64 * u64::from(weight);
        self.totals.lines += sink.lines;
        self.totals.items += items.len() as u64;
        self.totals.scaled += scaled;
        self.totals.rejected += sink.rejected + sink.overlong;
        self.totals.overlong += sink.overlong;
        self.totals.bytes += bytes;
        let produced = sink.lines > 0 || checkpoint != self.last_checkpoint;
        let batch = produced.then(|| {
            if weight > 1 {
                self.totals.sampled_batches += 1;
            }
            TailBatch {
                key: self.key.clone(),
                items,
                weight,
                lines: sink.lines,
                rejected: sink.rejected + sink.overlong,
                bytes,
                checkpoint,
            }
        });
        if produced {
            self.last_checkpoint = checkpoint;
        }
        sink.lines = 0;
        sink.rejected = 0;
        sink.overlong = 0;

        let status = self.status();
        let active = bytes > 0
            || self.totals.rotations + self.totals.truncations != changes_before
            || status.state != state_before;
        Tick {
            batch,
            status,
            active,
            cap_hit,
        }
    }

    /// Looks at the path and at the open file: opens, rotates, truncates, orphans.
    fn maintain(&mut self, now: Instant) {
        let Some(cur) = self.cur.as_mut() else {
            match probe(&self.path) {
                Probe::Found(found) => self.open_first(found, now),
                Probe::Missing { pending_delete } => {
                    if self.initial.take().is_some() {
                        self.start = StartOutcome::Missing;
                    }
                    if pending_delete {
                        self.last_error = Some("access denied (or pending deletion)".to_owned());
                    }
                }
                Probe::Failed(e) => self.note("open", &e),
            }
            return;
        };

        // The open file first: shorter than what was read, or other first bytes (truncated in
        // place, possibly written again past the read position before this tick).
        let meta = match cur.file.metadata() {
            Ok(meta) => meta,
            Err(e) => {
                self.note("stat", &e);
                return;
            }
        };
        cur.len = meta.len();
        match read_head(&cur.file) {
            Ok(fresh) => {
                if meta.len() < cur.pos || !fresh.starts_with(&cur.head) {
                    cur.reset();
                    self.totals.truncations += 1;
                    tracing::debug!(key = %self.key, "access log truncated; reading from the start");
                }
                cur.head = fresh;
            }
            Err(e) => {
                self.note("read", &e);
                return;
            }
        }

        // Then the path. The same file answers the same metadata through both handles (no
        // read access is asked for, so antivirus filters do not rescan the file); otherwise,
        // or when a write fell between the two queries, open it and compare first bytes.
        let probe = match std::fs::metadata(&self.path) {
            Ok(stat) if same_stat(&stat, &meta) => {
                cur.orphan_since = None;
                cur.release_asap = false;
                return;
            }
            Ok(_) => probe(&self.path),
            Err(e) => Probe::from_error(e),
        };
        match probe {
            Probe::Missing { pending_delete } => {
                cur.orphan_since.get_or_insert(now);
                cur.release_asap |= pending_delete;
            }
            Probe::Failed(e) => self.note("open", &e),
            Probe::Found(found) => match cur.compare(&found) {
                Ok(Verdict::Same) => {
                    cur.orphan_since = None;
                    cur.release_asap = false;
                }
                Ok(Verdict::Rewritten) => {
                    cur.reset();
                    self.totals.truncations += 1;
                    tracing::debug!(key = %self.key, "access log rewritten; reading from the start");
                }
                Ok(Verdict::Replaced) => {
                    let old = std::mem::replace(cur, OpenFile::new(found, 0, now));
                    self.retire(old);
                    self.totals.rotations += 1;
                    tracing::debug!(key = %self.key, "access log rotated; draining the old file");
                }
                Err(e) => self.note("compare", &e),
            },
        }
    }

    /// First file at the path (at start, after a missing file or after a release).
    fn open_first(&mut self, found: Found, now: Instant) {
        let len = found.len;
        let (pos, outcome) = match self.initial.take() {
            // Appeared after we looked: everything in it is new.
            None => (0, None),
            Some(StartAt::Beginning) => (0, Some(StartOutcome::Beginning)),
            Some(StartAt::End) => (len, Some(StartOutcome::End)),
            Some(StartAt::Checkpoint(cp)) => {
                match check_resume(&found, &cp, self.config.resume_max_lag) {
                    Ok(lag) => (cp.offset, Some(StartOutcome::Resumed { lag })),
                    Err(reason) => {
                        tracing::warn!(
                            key = %self.key,
                            path = %self.path.display(),
                            ?reason,
                            "access log checkpoint does not apply; starting at the end of the file"
                        );
                        (len, Some(StartOutcome::Rejected(reason)))
                    }
                }
            }
        };
        if let Some(outcome) = outcome {
            self.start = outcome;
        }
        if self.had_file {
            self.totals.rotations += 1;
        }
        self.had_file = true;
        let mut file = OpenFile::new(found, pos, now);
        if let Err(e) = file.align() {
            self.note("read", &e);
        }
        self.cur = Some(file);
    }

    fn retire(&mut self, old: OpenFile) {
        if self.retired.len() >= MAX_RETIRED {
            let oldest = self.retired.remove(0);
            tracing::warn!(
                key = %self.key,
                unread = oldest.len.saturating_sub(oldest.pos),
                "access log rotates too fast; stopped draining the oldest rotated file"
            );
            finish(oldest, &mut self.sink);
        }
        self.retired.push(old);
    }

    fn backlog(&self) -> u64 {
        self.cur
            .iter()
            .chain(&self.retired)
            .map(|f| f.len.saturating_sub(f.pos))
            .sum()
    }

    fn note(&mut self, op: &str, e: &io::Error) {
        let now = Instant::now();
        let log = match self.logged {
            Some((kind, at)) => kind != e.kind() || now.duration_since(at) >= ERROR_LOG_EVERY,
            None => true,
        };
        if log {
            tracing::warn!(
                key = %self.key,
                path = %self.path.display(),
                error = %e,
                "access log tailer: {op} failed"
            );
            self.logged = Some((e.kind(), now));
        }
        self.last_error = Some(format!("{op}: {e}"));
    }
}

fn check_resume(found: &Found, cp: &Checkpoint, max_lag: u64) -> Result<u64, ResumeReject> {
    if found.created != cp.creation_time {
        return Err(ResumeReject::CreationTime);
    }
    if found.len < cp.offset {
        return Err(ResumeReject::Shorter { len: found.len });
    }
    let n = usize::from(cp.head_len);
    if n > HEAD_BYTES || found.head.len() < n || fnv1a(&found.head[..n]) != cp.head_hash {
        return Err(ResumeReject::Content);
    }
    let lag = found.len - cp.offset;
    if lag > max_lag {
        return Err(ResumeReject::Lag { lag });
    }
    Ok(lag)
}

/// One file being read.
struct OpenFile {
    file: File,
    created: u64,
    /// Size at the last look.
    len: u64,
    /// Next byte to read.
    pos: u64,
    /// Offset right after the last `\n` consumed.
    line_start: u64,
    /// The incomplete line so far (at most `max_line + 1` bytes).
    carry: Vec<u8>,
    /// Skipping to the next `\n` (overlong line, or a start in the middle of a line).
    discarding: bool,
    /// First bytes of the file as last read (up to [`HEAD_BYTES`]).
    head: Vec<u8>,
    last_growth: Instant,
    /// Since when the path no longer names this file.
    orphan_since: Option<Instant>,
    /// The name is pending deletion: release the file as soon as it is drained.
    release_asap: bool,
}

/// How the path relates to the open file.
enum Verdict {
    Same,
    /// The first bytes changed under the open handle: truncated and written again. The path
    /// is looked at again on the next tick (switching now could read the file twice when the
    /// truncation fell between the two reads of the first bytes).
    Rewritten,
    /// The path names another file.
    Replaced,
}

impl OpenFile {
    fn new(found: Found, pos: u64, now: Instant) -> Self {
        Self {
            file: found.file,
            created: found.created,
            len: found.len,
            pos,
            line_start: pos,
            carry: Vec::new(),
            discarding: false,
            head: found.head,
            last_growth: now,
            orphan_since: None,
            release_asap: false,
        }
    }

    fn checkpoint(&self) -> Checkpoint {
        Checkpoint::of(self.created, self.line_start, &self.head)
    }

    /// Back to the first byte (truncation): the partial line belongs to the old content.
    fn reset(&mut self) {
        self.pos = 0;
        self.line_start = 0;
        self.carry.clear();
        self.discarding = false;
        self.head.clear();
    }

    /// When starting inside a line, skip to the next one.
    fn align(&mut self) -> io::Result<()> {
        if self.pos == 0 {
            return Ok(());
        }
        let mut byte = [0u8];
        let n = read_at(&self.file, &mut byte, self.pos - 1)?;
        self.discarding = n == 1 && byte[0] != b'\n';
        Ok(())
    }

    /// Compares the file at the path (opened just before) with this one.
    fn compare(&mut self, found: &Found) -> io::Result<Verdict> {
        if found.created != self.created {
            return Ok(Verdict::Replaced);
        }
        // Read after the path: if both are the same file, which only grows, the path's
        // first bytes are a prefix of these and its size is at least what was read.
        let fresh = read_head(&self.file)?;
        let rewritten = !fresh.starts_with(&self.head);
        let same = found.len >= self.pos && fresh.starts_with(&found.head);
        self.head = fresh;
        Ok(if rewritten {
            Verdict::Rewritten
        } else if same {
            Verdict::Same
        } else {
            Verdict::Replaced
        })
    }

    /// A rotated file is still the one we were reading (not truncated nor rewritten).
    fn unchanged(&mut self) -> io::Result<bool> {
        self.len = self.file.metadata()?.len();
        if self.len < self.pos {
            return Ok(false);
        }
        let fresh = read_head(&self.file)?;
        Ok(fresh.starts_with(&self.head))
    }

    fn refresh_head(&mut self) -> io::Result<()> {
        if self.head.len() < HEAD_BYTES && self.pos > self.head.len() as u64 {
            self.head = read_head(&self.file)?;
        }
        Ok(())
    }
}

enum ReadEnd {
    Eof,
    Budget,
    Error(io::Error),
}

/// Reads `f` from its position to its end or until the budget runs out, feeding the sink.
fn drain<D: LineDecoder>(
    f: &mut OpenFile,
    sink: &mut Sink<D>,
    chunk: &mut [u8],
    budget: &mut u64,
    now: Instant,
) -> (ReadEnd, u64) {
    let mut read = 0;
    loop {
        if *budget == 0 {
            return (ReadEnd::Budget, read);
        }
        let want = chunk
            .len()
            .min(usize::try_from(*budget).unwrap_or(usize::MAX));
        let n = match read_at(&f.file, &mut chunk[..want], f.pos) {
            Ok(0) => return (ReadEnd::Eof, read),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return (ReadEnd::Error(e), read),
        };
        let base = f.pos;
        f.pos += n as u64;
        f.len = f.len.max(f.pos);
        f.last_growth = now;
        *budget -= n as u64;
        read += n as u64;
        consume(f, sink, &chunk[..n], base);
    }
}

/// Splits `data` (read at offset `base`) into lines, completing the carried partial line.
fn consume<D: LineDecoder>(f: &mut OpenFile, sink: &mut Sink<D>, data: &[u8], base: u64) {
    // The carry may hold a final `\r` on top of `max_line`.
    let limit = sink.max_line + 1;
    let mut start = 0;
    while let Some(i) = data[start..].iter().position(|&b| b == b'\n') {
        let end = start + i;
        let piece = &data[start..end];
        if f.discarding {
            f.discarding = false;
        } else if f.carry.is_empty() {
            sink.line(piece);
        } else if f.carry.len() + piece.len() > limit {
            f.carry.clear();
            sink.overlong();
        } else {
            f.carry.extend_from_slice(piece);
            sink.line(&f.carry);
            f.carry.clear();
        }
        start = end + 1;
        f.line_start = base + start as u64;
    }
    let rest = &data[start..];
    if !f.discarding && !rest.is_empty() {
        if f.carry.len() + rest.len() > limit {
            f.carry.clear();
            f.discarding = true;
            sink.overlong();
        } else {
            f.carry.extend_from_slice(rest);
        }
    }
}

/// A file is let go: its last line may lack the terminator.
fn finish<D: LineDecoder>(mut f: OpenFile, sink: &mut Sink<D>) {
    if !f.discarding && !f.carry.is_empty() {
        let carry = std::mem::take(&mut f.carry);
        sink.line(&carry);
    }
}

/// Where lines go: sampler, decoder and the counters of the tick.
struct Sink<D: LineDecoder> {
    decoder: D,
    sampler: Sampler,
    max_line: usize,
    items: Vec<D::Item>,
    lines: u64,
    rejected: u64,
    overlong: u64,
}

impl<D: LineDecoder> Sink<D> {
    fn line(&mut self, line: &[u8]) {
        let line = match line {
            [rest @ .., b'\r'] => rest,
            _ => line,
        };
        if line.is_empty() {
            return;
        }
        if line.len() > self.max_line {
            self.overlong();
            return;
        }
        self.lines += 1;
        if self.sampler.take() {
            match self.decoder.decode(line) {
                Some(item) => self.items.push(item),
                None => self.rejected += 1,
            }
        }
    }

    fn overlong(&mut self) {
        self.lines += 1;
        self.overlong += 1;
    }
}

/// 1 line in `k`: one random position in every block of `k` lines.
struct Sampler {
    k: u32,
    pos: u32,
    pick: u32,
    rng: Rng,
}

impl Sampler {
    fn new(rng: Rng) -> Self {
        Self {
            k: 1,
            pos: 0,
            pick: 0,
            rng,
        }
    }

    /// Doubles `k` (up to `max`) under pressure, halves it otherwise.
    fn adjust(&mut self, pressure: bool, max: u32) {
        let k = if pressure {
            self.k.saturating_mul(2).min(max.max(1))
        } else {
            (self.k / 2).max(1)
        };
        if k != self.k {
            self.k = k;
            self.pos = 0;
            self.pick = self.rng.below(k);
        }
    }

    /// Whether the next line is decoded.
    fn take(&mut self) -> bool {
        if self.k <= 1 {
            return true;
        }
        let hit = self.pos == self.pick;
        self.pos += 1;
        if self.pos == self.k {
            self.pos = 0;
            self.pick = self.rng.below(self.k);
        }
        hit
    }
}

/// SplitMix64: jitter and sample positions (not cryptographic).
struct Rng(u64);

impl Rng {
    fn seeded() -> Self {
        Self(RandomState::new().hash_one(std::process::id()))
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`).
    fn below(&mut self, n: u32) -> u32 {
        (((self.next_u64() >> 32) * u64::from(n)) >> 32) as u32
    }

    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// FNV-1a 64: a hash that never changes between builds (checkpoints outlive them).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// The file at the path, opened by [`probe`].
struct Found {
    file: File,
    created: u64,
    len: u64,
    head: Vec<u8>,
}

enum Probe {
    Found(Found),
    /// Not there. `pending_delete`: access denied, which is what a name pending deletion
    /// (legacy delete semantics) answers.
    Missing {
        pending_delete: bool,
    },
    Failed(io::Error),
}

impl Probe {
    fn from_error(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::NotFound => Self::Missing {
                pending_delete: false,
            },
            io::ErrorKind::PermissionDenied => Self::Missing {
                pending_delete: true,
            },
            _ => Self::Failed(e),
        }
    }
}

fn probe(path: &Path) -> Probe {
    // std's share mode is READ | WRITE | DELETE: renames and deletes by the server succeed.
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) => return Probe::from_error(e),
    };
    let meta = match file.metadata() {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return Probe::Failed(io::Error::other("not a regular file")),
        Err(e) => return Probe::Failed(e),
    };
    match read_head(&file) {
        Ok(head) => Probe::Found(Found {
            created: creation_time(&meta),
            len: meta.len(),
            head,
            file,
        }),
        Err(e) => Probe::Failed(e),
    }
}

/// Whether two metadata snapshots describe the same unchanged file: a regular file with the
/// same creation time, size and last write time (100 ns on NTFS).
fn same_stat(a: &Metadata, b: &Metadata) -> bool {
    a.is_file()
        && b.is_file()
        && creation_time(a) == creation_time(b)
        && a.len() == b.len()
        && matches!((a.modified(), b.modified()), (Ok(x), Ok(y)) if x == y)
}

/// The first [`HEAD_BYTES`] bytes (fewer if the file is shorter).
fn read_head(file: &File) -> io::Result<Vec<u8>> {
    let mut head = vec![0; HEAD_BYTES];
    let mut n = 0;
    while n < HEAD_BYTES {
        match read_at(file, &mut head[n..], n as u64) {
            Ok(0) => break,
            Ok(m) => n += m,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    head.truncate(n);
    Ok(head)
}

#[cfg(windows)]
fn creation_time(meta: &Metadata) -> u64 {
    std::os::windows::fs::MetadataExt::creation_time(meta)
}

#[cfg(not(windows))]
fn creation_time(meta: &Metadata) -> u64 {
    // FILETIME units (100 ns since 1601) from the birth time, when the platform has one.
    const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;
    meta.created()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| {
            UNIX_EPOCH_AS_FILETIME + u64::try_from(d.as_nanos() / 100).unwrap_or(u64::MAX / 2)
        })
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

#[cfg(not(any(windows, unix)))]
fn read_at(mut file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(offset))?;
    file.read(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(max_line: usize) -> Sink<impl LineDecoder<Item = String>> {
        Sink {
            decoder: |line: &[u8]| Some(String::from_utf8_lossy(line).into_owned()),
            sampler: Sampler::new(Rng(7)),
            max_line,
            items: Vec::new(),
            lines: 0,
            rejected: 0,
            overlong: 0,
        }
    }

    fn open_file(dir: &Path, content: &[u8], pos: u64) -> OpenFile {
        let path = dir.join("a.log");
        std::fs::write(&path, content).unwrap();
        let Probe::Found(found) = probe(&path) else {
            panic!("probe")
        };
        OpenFile::new(found, pos, Instant::now())
    }

    fn feed<D: LineDecoder>(f: &mut OpenFile, sink: &mut Sink<D>, chunks: &[&[u8]]) {
        for chunk in chunks {
            let base = f.pos;
            f.pos += chunk.len() as u64;
            consume(f, sink, chunk, base);
        }
    }

    fn take(sink: &mut Sink<impl LineDecoder<Item = String>>) -> Vec<String> {
        std::mem::take(&mut sink.items)
    }

    #[test]
    fn partial_lines_are_completed_across_reads() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = open_file(dir.path(), b"", 0);
        let mut s = sink(64);
        feed(
            &mut f,
            &mut s,
            &[b"alpha\nbe", b"ta", b"\r\ngam", b"ma\n\n"],
        );
        assert_eq!(take(&mut s), ["alpha", "beta", "gamma"]);
        assert_eq!(s.lines, 3);
        assert_eq!(f.line_start, f.pos);
        assert!(f.carry.is_empty());

        feed(&mut f, &mut s, &[b"delta-without-newline"]);
        assert!(take(&mut s).is_empty());
        assert_eq!(
            f.line_start,
            f.pos - 21,
            "the checkpoint stays before the partial line"
        );
    }

    #[test]
    fn overlong_lines_are_counted_and_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = open_file(dir.path(), b"", 0);
        let mut s = sink(8);
        // One in a single chunk, one across chunks, then a normal line.
        feed(
            &mut f,
            &mut s,
            &[b"0123456789\nok\n01234", b"56789abc", b"def\nfine\r\n"],
        );
        assert_eq!(take(&mut s), ["ok", "fine"]);
        assert_eq!((s.lines, s.overlong), (4, 2));
        // Exactly max_line bytes plus a CR is fine.
        feed(&mut f, &mut s, &[b"12345678\r\n"]);
        assert_eq!(take(&mut s), ["12345678"]);
    }

    #[test]
    fn a_start_inside_a_line_skips_to_the_next_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = open_file(dir.path(), b"first\nsecond-partial", 9);
        f.align().unwrap();
        assert!(f.discarding);
        let mut s = sink(64);
        feed(&mut f, &mut s, &[b"-rest\nthird\n"]);
        assert_eq!(take(&mut s), ["third"]);

        let mut g = open_file(dir.path(), b"first\nsecond\n", 13);
        g.align().unwrap();
        assert!(!g.discarding, "the end of a line is a line boundary");
    }

    #[test]
    fn block_sampling_keeps_scaled_counts_exact() {
        let mut sampler = Sampler::new(Rng(42));
        for _ in 0..4 {
            sampler.adjust(true, 16);
        }
        assert_eq!(sampler.k, 16);
        // Lines in two alternating classes: block sampling has no aliasing.
        let (mut a, mut b) = (0u64, 0u64);
        for i in 0..160_000u64 {
            if sampler.take() {
                if i % 2 == 0 {
                    a += 16;
                } else {
                    b += 16;
                }
            }
        }
        assert_eq!(a + b, 160_000);
        let skew = (a as f64 - b as f64).abs() / 80_000.0;
        assert!(skew < 0.05, "a={a} b={b}");
        // Back down one step per calm tick, and never below 1 or above the cap.
        sampler.adjust(false, 16);
        assert_eq!(sampler.k, 8);
        for _ in 0..10 {
            sampler.adjust(false, 16);
        }
        assert_eq!(sampler.k, 1);
        for _ in 0..10 {
            sampler.adjust(true, 4);
        }
        assert_eq!(sampler.k, 4);
    }

    #[test]
    fn fnv1a_is_the_reference_function() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn checkpoint_covers_at_most_the_offset_and_256_bytes() {
        let head: Vec<u8> = (0..=255u8).collect();
        let cp = Checkpoint::of(9, 10, &head);
        assert_eq!((cp.head_len, cp.head_hash), (10, fnv1a(&head[..10])));
        let cp = Checkpoint::of(9, 1 << 20, &head);
        assert_eq!((cp.head_len, cp.head_hash), (256, fnv1a(&head)));
        let cp = Checkpoint::of(9, 1 << 20, &head[..3]);
        assert_eq!(cp.head_len, 3);
    }

    #[test]
    fn resume_checks_identity_size_content_and_lag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.log");
        std::fs::write(&path, b"line one\nline two\n").unwrap();
        let Probe::Found(found) = probe(&path) else {
            panic!("probe")
        };
        let good = Checkpoint::of(found.created, 9, &found.head);
        assert_eq!(check_resume(&found, &good, 64), Ok(9));
        let other = Checkpoint {
            creation_time: good.creation_time + 1,
            ..good
        };
        assert_eq!(
            check_resume(&found, &other, 64),
            Err(ResumeReject::CreationTime)
        );
        let past = Checkpoint { offset: 99, ..good };
        assert_eq!(
            check_resume(&found, &past, 64),
            Err(ResumeReject::Shorter { len: 18 })
        );
        let content = Checkpoint {
            head_hash: good.head_hash ^ 1,
            ..good
        };
        assert_eq!(
            check_resume(&found, &content, 64),
            Err(ResumeReject::Content)
        );
        assert_eq!(
            check_resume(&found, &good, 8),
            Err(ResumeReject::Lag { lag: 9 })
        );
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let mut rng = Rng(1);
        let base = Duration::from_millis(250);
        for _ in 0..1000 {
            let d = jittered(base, 0.1, &mut rng);
            assert!(d >= Duration::from_millis(225) && d <= Duration::from_millis(275));
        }
        for _ in 0..1000 {
            assert!(rng.below(16) < 16);
        }
    }

    #[test]
    fn config_is_sanitized() {
        let c = TailConfig {
            tick: Duration::ZERO,
            idle_tick: Duration::ZERO,
            catch_up_tick: Duration::from_secs(9),
            jitter: f64::NAN,
            max_read: 0,
            max_sample: 0,
            max_line: 0,
            ..TailConfig::default()
        }
        .sanitized();
        assert_eq!(c.tick, Duration::from_millis(1));
        assert_eq!(c.idle_tick, c.tick);
        assert_eq!(c.catch_up_tick, c.tick);
        assert_eq!(c.jitter, 0.0);
        assert_eq!((c.max_read, c.max_sample, c.max_line), (1, 1, 1));
    }
}
