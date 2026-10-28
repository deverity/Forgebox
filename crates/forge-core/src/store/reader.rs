//! Read connections of the store, used from `spawn_blocking` (docs/03 §7.1).
//!
//! With WAL, readers never wait for the writer: they see the last committed state.
//! Connections are opened on demand, configured `query_only` and kept in a small idle
//! pool. [`StoreReader::query`] hops to the blocking pool with a timeout;
//! [`StoreReader::blocking`] is for code that already runs on a blocking thread.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rusqlite::Connection;

use super::{BUSY_TIMEOUT, StoreOptions, Target};
use crate::error::{ForgeError, Result};
use crate::sync::{LockLevel, OrderedMutex};

/// Read-only access to the store. Cheap to clone; obtained from
/// [`super::Store::reader`]. Fails with `internal` once the store is closed.
#[derive(Clone)]
pub struct StoreReader {
    inner: Arc<Inner>,
}

struct Inner {
    target: Target,
    /// Lock order: `store.readers` (rank 10). Held only to push/pop a connection; never
    /// while a query runs.
    idle: OrderedMutex<Vec<Connection>>,
    max_idle: usize,
    default_timeout: Duration,
    closed: AtomicBool,
}

impl fmt::Debug for StoreReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let idle = self.inner.idle.lock().len();
        f.debug_struct("StoreReader")
            .field("idle", &idle)
            .field("closed", &self.inner.closed.load(Ordering::Relaxed))
            .finish()
    }
}

impl StoreReader {
    pub(super) fn new(target: Target, options: &StoreOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                target,
                idle: OrderedMutex::new(LockLevel::STORE_READERS, Vec::new()),
                max_idle: options.max_idle_readers,
                default_timeout: options.read_timeout,
                closed: AtomicBool::new(false),
            }),
        }
    }

    /// Runs `f` on a read-only connection **on the current thread**. Blocking: call it
    /// only from `spawn_blocking` or a plain thread; async code uses
    /// [`StoreReader::query`].
    pub fn blocking<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.ensure_open()?;
        let pooled = self.inner.idle.lock().pop();
        let conn = match pooled {
            Some(conn) => conn,
            None => self.open()?,
        };
        let result = f(&conn);
        let surplus = if self.inner.closed.load(Ordering::Acquire) {
            Some(conn)
        } else {
            let mut idle = self.inner.idle.lock();
            if idle.len() < self.inner.max_idle {
                idle.push(conn);
                None
            } else {
                Some(conn)
            }
        };
        // Closing a connection is I/O: done outside the lock.
        drop(surplus);
        result
    }

    /// Runs `f` on a read-only connection in `spawn_blocking`, with the default read
    /// timeout ([`super::StoreOptions::read_timeout`]).
    pub async fn query<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        self.query_timeout(self.inner.default_timeout, f).await
    }

    /// Like [`StoreReader::query`] with an explicit timeout. On timeout the query keeps
    /// running on its blocking thread, but the caller is released.
    pub async fn query_timeout<T, F>(&self, timeout: Duration, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let reader = self.clone();
        let task = tokio::task::spawn_blocking(move || reader.blocking(f));
        match tokio::time::timeout(timeout, task).await {
            Ok(Ok(result)) => result,
            Ok(Err(join)) => Err(ForgeError::Internal(format!(
                "store read task failed: {join}"
            ))),
            Err(_) => Err(ForgeError::Timeout {
                operation: "store read".to_owned(),
                after: timeout,
            }),
        }
    }

    /// Idle connections in the pool (diagnostics).
    pub fn idle_connections(&self) -> usize {
        self.inner.idle.lock().len()
    }

    /// Refuses further reads and closes idle connections.
    pub(super) fn close(&self) {
        self.inner.closed.store(true, Ordering::Release);
        let idle = std::mem::take(&mut *self.inner.idle.lock());
        drop(idle);
    }

    fn ensure_open(&self) -> Result<()> {
        if self.inner.closed.load(Ordering::Acquire) {
            Err(ForgeError::Internal("the store is closed".to_owned()))
        } else {
            Ok(())
        }
    }

    fn open(&self) -> Result<Connection> {
        let conn = self.inner.target.open()?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update(None, "query_only", true)?;
        if matches!(self.inner.target, Target::Memory(_)) {
            // Shared-cache in-memory databases use table locks that ignore busy_timeout;
            // read-uncommitted readers do not take them.
            conn.pragma_update(None, "read_uncommitted", true)?;
        }
        Ok(conn)
    }
}
