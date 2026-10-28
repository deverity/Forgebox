//! Dedicated SQLite writer thread (docs/03 §7.1, ADR-0011).
//!
//! Every write of the daemon goes through one `std::thread` that owns its own
//! connection and reads a bounded queue of operations. Async callers use
//! [`StoreWriter::execute`], which enqueues a closure and awaits its result, always
//! under a timeout. No query ever runs on a tokio worker thread.
//!
//! **Batching.** The writer groups whatever is already queued into one short transaction
//! (`BEGIN IMMEDIATE` … `COMMIT`): at most [`super::StoreOptions::max_batch_ops`]
//! operations (64) or [`super::StoreOptions::max_batch_time`] (5 ms) per transaction. It
//! never waits for more work to fill a batch, so batching adds no latency. Each
//! operation runs inside its own `SAVEPOINT`: an operation that returns an error (or
//! panics) is rolled back alone, the rest of the batch still commits. Results are sent
//! to the callers only **after** `COMMIT`; if the commit fails, every operation of the
//! batch that had succeeded receives the commit error instead.
//!
//! Operations receive `&mut Connection` inside the batch transaction: they must not
//! issue `BEGIN`/`COMMIT`/`ROLLBACK` (use `conn.savepoint()` for a nested atomic unit).
//! An operation that ends the transaction anyway makes the outcome of its batch unknown;
//! all its callers get an `internal` error.
//!
//! **Timeouts.** A caller that times out gets `ForgeError::Timeout` and the writer is not
//! affected. If the operation had not started yet it is skipped (its caller is gone); if
//! it was already running it completes, so a timeout means "outcome unknown" — writes
//! must be idempotent (ADR-0009).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use std::{fmt, thread};

use rusqlite::Connection;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};

use super::StoreOptions;
use crate::actor::panic_message;
use crate::error::{ForgeError, Result};

/// Name of the writer thread.
pub const WRITER_THREAD_NAME: &str = "forgebox-db-writer";

const SAVEPOINT: &str = "SAVEPOINT forgebox_op";
const RELEASE: &str = "RELEASE forgebox_op";
const ROLLBACK_TO: &str = "ROLLBACK TO forgebox_op; RELEASE forgebox_op";

enum Command {
    Op(Box<dyn Op>),
    Shutdown,
}

/// A queued write, type-erased.
trait Op: Send {
    /// The caller stopped waiting (timeout or dropped future).
    fn abandoned(&self) -> bool;
    /// Runs the closure; the reply is held until the batch commits.
    fn run(self: Box<Self>, conn: &mut Connection) -> Box<dyn Completed>;
    /// Answers without running (store closed, transaction could not start).
    fn reject(self: Box<Self>, err: ForgeError);
}

/// An executed write waiting for the outcome of its batch.
trait Completed: Send {
    fn succeeded(&self) -> bool;
    fn finish(self: Box<Self>, commit: &CommitStatus);
}

enum CommitStatus {
    Committed,
    Failed(rusqlite::Error),
    /// An operation issued COMMIT/ROLLBACK itself: the batch outcome is unknown.
    TransactionLost,
}

struct TypedOp<T, F> {
    f: F,
    reply: oneshot::Sender<Result<T>>,
}

impl<T, F> Op for TypedOp<T, F>
where
    T: Send + 'static,
    F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
{
    fn abandoned(&self) -> bool {
        self.reply.is_closed()
    }

    fn run(self: Box<Self>, conn: &mut Connection) -> Box<dyn Completed> {
        let Self { f, reply } = *self;
        let result = f(conn);
        Box::new(TypedDone { result, reply })
    }

    fn reject(self: Box<Self>, err: ForgeError) {
        let _ = self.reply.send(Err(err));
    }
}

struct TypedDone<T> {
    result: Result<T>,
    reply: oneshot::Sender<Result<T>>,
}

impl<T: Send + 'static> Completed for TypedDone<T> {
    fn succeeded(&self) -> bool {
        self.result.is_ok()
    }

    fn finish(self: Box<Self>, commit: &CommitStatus) {
        let result = match (self.result, commit) {
            (Err(err), _) => Err(err),
            (Ok(value), CommitStatus::Committed) => Ok(value),
            (Ok(_), CommitStatus::Failed(err)) => Err(copy_db_error(err)),
            (Ok(_), CommitStatus::TransactionLost) => Err(ForgeError::Internal(
                "outcome unknown: an operation of the same write batch issued COMMIT/ROLLBACK"
                    .to_owned(),
            )),
        };
        let _ = self.reply.send(result);
    }
}

/// The commit error is shared by every operation of the batch; `rusqlite::Error` is not
/// `Clone`, so SQLite failures are rebuilt from their (copyable) code.
fn copy_db_error(err: &rusqlite::Error) -> ForgeError {
    match err {
        rusqlite::Error::SqliteFailure(code, message) => {
            ForgeError::Db(rusqlite::Error::SqliteFailure(*code, message.clone()))
        }
        other => ForgeError::Internal(format!("write batch was not committed: {other}")),
    }
}

fn closed_error() -> ForgeError {
    ForgeError::Internal("the store writer is closed".to_owned())
}

/// Counters of the writer thread (diagnostics and tests).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriterStats {
    /// Operations executed (succeeded or not).
    pub ops: u64,
    /// Operations that returned an error or panicked (rolled back alone).
    pub failed: u64,
    /// Operations skipped because their caller had already given up.
    pub skipped: u64,
    /// Transactions (batches) run.
    pub batches: u64,
    /// Batches whose `COMMIT` failed or whose outcome is unknown.
    pub commit_failures: u64,
}

#[derive(Default)]
struct Counters {
    ops: AtomicU64,
    failed: AtomicU64,
    skipped: AtomicU64,
    batches: AtomicU64,
    commit_failures: AtomicU64,
}

impl Counters {
    fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> WriterStats {
        WriterStats {
            ops: self.ops.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            batches: self.batches.load(Ordering::Relaxed),
            commit_failures: self.commit_failures.load(Ordering::Relaxed),
        }
    }
}

/// Async handle to the writer thread. Cheap to clone; obtained from
/// [`super::Store::writer`]. Fails with `internal` once the store is closed.
#[derive(Clone)]
pub struct StoreWriter {
    tx: mpsc::Sender<Command>,
    counters: Arc<Counters>,
    default_timeout: Duration,
}

impl fmt::Debug for StoreWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreWriter")
            .field("closed", &self.tx.is_closed())
            .field("default_timeout", &self.default_timeout)
            .field("stats", &self.stats())
            .finish()
    }
}

impl StoreWriter {
    /// Runs `f` on the writer thread with the default write timeout
    /// ([`super::StoreOptions::write_timeout`]).
    pub async fn execute<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        self.execute_timeout(self.default_timeout, f).await
    }

    /// Runs `f` on the writer thread; the whole round trip (queueing included) must
    /// finish within `timeout`, otherwise `ForgeError::Timeout` (outcome unknown if the
    /// operation had already started).
    pub async fn execute_timeout<T, F>(&self, timeout: Duration, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let (reply, answer) = oneshot::channel();
        let op: Box<dyn Op> = Box::new(TypedOp { f, reply });
        let round_trip = async {
            self.tx
                .send(Command::Op(op))
                .await
                .map_err(|_| closed_error())?;
            answer.await.map_err(|_| {
                ForgeError::Internal(
                    "the store writer dropped the operation (it panicked or the store was closed)"
                        .to_owned(),
                )
            })?
        };
        tokio::time::timeout(timeout, round_trip)
            .await
            .map_err(|_| ForgeError::Timeout {
                operation: "store write".to_owned(),
                after: timeout,
            })?
    }

    /// Waits until every operation queued before this call has been committed.
    pub async fn flush(&self, timeout: Duration) -> Result<()> {
        self.execute_timeout(timeout, |_| Ok(())).await
    }

    /// Counters of the writer thread.
    pub fn stats(&self) -> WriterStats {
        self.counters.snapshot()
    }

    /// Whether the writer thread has stopped accepting operations.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// Owner side of the writer thread, kept by [`super::Store`].
pub(super) struct WriterThread {
    tx: mpsc::Sender<Command>,
    exited: std::sync::mpsc::Receiver<()>,
    join: Option<JoinHandle<()>>,
}

impl fmt::Debug for WriterThread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriterThread")
            .field(
                "running",
                &self.join.as_ref().is_some_and(|j| !j.is_finished()),
            )
            .finish()
    }
}

impl WriterThread {
    /// Asks the thread to stop after the operations already queued, and waits for it at
    /// most `timeout` (blocking: call from a blocking context).
    pub(super) fn shutdown(mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut command = Command::Shutdown;
        loop {
            match self.tx.try_send(command) {
                Ok(()) | Err(TrySendError::Closed(_)) => break,
                Err(TrySendError::Full(back)) => {
                    if Instant::now() >= deadline {
                        return Err(ForgeError::Timeout {
                            operation: "closing the store writer (queue full)".to_owned(),
                            after: timeout,
                        });
                    }
                    command = back;
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match self.exited.recv_timeout(left) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                return Err(ForgeError::Timeout {
                    operation: "closing the store writer".to_owned(),
                    after: timeout,
                });
            }
        }
        if let Some(join) = self.join.take() {
            join.join().map_err(|panic| {
                ForgeError::Internal(format!(
                    "store writer thread panicked: {}",
                    panic_message(&*panic)
                ))
            })?;
        }
        Ok(())
    }
}

/// Starts the writer thread on `conn` (already configured and migrated).
pub(super) fn spawn(
    conn: Connection,
    options: &StoreOptions,
) -> Result<(StoreWriter, WriterThread)> {
    let (tx, rx) = mpsc::channel(options.queue_capacity.max(1));
    let counters = Arc::new(Counters::default());
    let (exited_tx, exited) = std::sync::mpsc::sync_channel::<()>(1);
    let writer = Writer {
        conn,
        rx,
        max_batch_ops: options.max_batch_ops.max(1),
        max_batch_time: options.max_batch_time,
        counters: Arc::clone(&counters),
    };
    let join = thread::Builder::new()
        .name(WRITER_THREAD_NAME.to_owned())
        .spawn(move || {
            // Dropped when the thread ends (even by panic): `shutdown` sees Disconnected.
            let _exited = exited_tx;
            writer.run();
        })
        .map_err(|e| ForgeError::io("spawning the store writer thread", e))?;
    Ok((
        StoreWriter {
            tx: tx.clone(),
            counters,
            default_timeout: options.write_timeout,
        },
        WriterThread {
            tx,
            exited,
            join: Some(join),
        },
    ))
}

struct Writer {
    conn: Connection,
    rx: mpsc::Receiver<Command>,
    max_batch_ops: usize,
    max_batch_time: Duration,
    counters: Arc<Counters>,
}

impl Writer {
    fn run(mut self) {
        tracing::debug!("store writer started");
        while let Some(command) = self.rx.blocking_recv() {
            let keep_running = match command {
                Command::Shutdown => false,
                Command::Op(op) => self.batch(op),
            };
            if !keep_running {
                break;
            }
        }
        // Closed: refuse what is still queued (callers get `internal`, not a hang).
        self.rx.close();
        while let Ok(command) = self.rx.try_recv() {
            if let Command::Op(op) = command {
                op.reject(closed_error());
            }
        }
        tracing::debug!("store writer stopped");
    }

    /// Runs `first` and whatever is already queued in one transaction. Returns `false`
    /// when a shutdown was dequeued.
    fn batch(&mut self, first: Box<dyn Op>) -> bool {
        if first.abandoned() {
            Counters::bump(&self.counters.skipped);
            return true;
        }
        let started = Instant::now();
        if let Err(err) = self.conn.execute_batch("BEGIN IMMEDIATE") {
            tracing::error!(%err, "store writer could not begin a transaction");
            first.reject(ForgeError::Db(err));
            return true;
        }
        let mut done: Vec<Box<dyn Completed>> = Vec::new();
        let mut executed = 0_usize;
        let mut keep_running = true;
        let mut transaction_lost = false;
        let mut next = Some(first);
        while let Some(op) = next.take() {
            if op.abandoned() {
                Counters::bump(&self.counters.skipped);
            } else {
                executed += 1;
                if let Some(completed) = self.run_op(op) {
                    done.push(completed);
                }
                if self.conn.is_autocommit() {
                    tracing::error!(
                        "a store write operation ended the batch transaction (COMMIT/ROLLBACK \
                         inside a writer op); outcome of the batch is unknown"
                    );
                    transaction_lost = true;
                    break;
                }
            }
            if executed >= self.max_batch_ops || started.elapsed() >= self.max_batch_time {
                break;
            }
            match self.rx.try_recv() {
                Ok(Command::Op(op)) => next = Some(op),
                Ok(Command::Shutdown) => {
                    keep_running = false;
                    break;
                }
                // Empty (batch ends without waiting) or closed (seen by `run`).
                Err(_) => break,
            }
        }

        let status = if transaction_lost {
            CommitStatus::TransactionLost
        } else {
            match self.conn.execute_batch("COMMIT") {
                Ok(()) => CommitStatus::Committed,
                Err(err) => {
                    tracing::error!(%err, ops = done.len(), "store write batch failed to commit");
                    if !self.conn.is_autocommit() {
                        if let Err(err) = self.conn.execute_batch("ROLLBACK") {
                            tracing::error!(%err, "rollback after a failed commit failed");
                        }
                    }
                    CommitStatus::Failed(err)
                }
            }
        };
        if !matches!(status, CommitStatus::Committed) {
            Counters::bump(&self.counters.commit_failures);
        }
        Counters::bump(&self.counters.batches);
        for completed in done {
            completed.finish(&status);
        }
        keep_running
    }

    /// Runs one operation inside its own savepoint. `None` if it could not run or
    /// panicked (its caller is answered or sees the reply dropped).
    fn run_op(&mut self, op: Box<dyn Op>) -> Option<Box<dyn Completed>> {
        if let Err(err) = self.conn.execute_batch(SAVEPOINT) {
            op.reject(ForgeError::Db(err));
            Counters::bump(&self.counters.failed);
            return None;
        }
        Counters::bump(&self.counters.ops);
        let conn = &mut self.conn;
        let outcome = catch_unwind(AssertUnwindSafe(|| op.run(conn)));
        if self.conn.is_autocommit() {
            // The savepoint went away with the transaction; nothing to release.
            return outcome.ok();
        }
        match outcome {
            Ok(completed) => {
                let end = if completed.succeeded() {
                    RELEASE
                } else {
                    Counters::bump(&self.counters.failed);
                    ROLLBACK_TO
                };
                if let Err(err) = self.conn.execute_batch(end) {
                    tracing::error!(%err, "could not close the savepoint of a write operation");
                }
                Some(completed)
            }
            Err(panic) => {
                Counters::bump(&self.counters.failed);
                tracing::error!(
                    reason = %panic_message(&*panic),
                    "store write operation panicked; rolled back"
                );
                if let Err(err) = self.conn.execute_batch(ROLLBACK_TO) {
                    tracing::error!(%err, "could not roll back a panicked write operation");
                }
                None
            }
        }
    }
}
