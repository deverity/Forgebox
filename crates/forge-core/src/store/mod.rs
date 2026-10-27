//! SQLite store: `state/forgebox.db`, the source of truth for domain entities (ADR-0007).
//!
//! [`Store::open`] opens the database (rusqlite, bundled SQLite) with WAL, foreign keys
//! and a busy timeout, applies pending migrations, then starts the concurrency layer of
//! docs/03 §7.1 (M0-E1-T5, ADR-0014):
//!
//! * [`Store::writer`] — [`StoreWriter`], the async handle to a **dedicated writer
//!   thread** (`std::thread`) that owns its own connection and runs queued operations in
//!   short batched transactions ([`writer`] module docs).
//! * [`Store::reader`] — [`StoreReader`], a small pool of `query_only` connections used
//!   from `spawn_blocking`; with WAL they never wait for the writer.
//!
//! In the daemon no query runs on a tokio worker thread: writes go through the writer,
//! reads through the reader. The synchronous connection of [`Store::conn`] is the
//! *maintenance* connection (migrations at boot, offline tools, tests); it is blocking
//! and must not be used from async code.
//!
//! `Store` owns the writer thread: [`Store::close`] (or drop) flushes what is queued and
//! joins it, after which every `StoreWriter`/`StoreReader` clone fails with `internal`.
//! Closing blocks for at most [`STORE_CLOSE_TIMEOUT`]; do it from a blocking context.
//!
//! Repositories are added per aggregate (docs/02). M0-E1-T4 ships the migration runner
//! and [`events::EventRepo`]; M1-E3-T3 adds [`ports::PortRepo`] (port reservations);
//! M1-E2-T5 adds [`update_history::UpdateHistoryRepo`] (updates and rollbacks).

pub mod events;
pub mod migrations;
pub mod ports;
mod reader;
pub mod sites;
pub mod tool_versions;
pub mod update_history;
pub mod writer;

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::error::{ForgeError, IoResultExt, Result};
pub use events::EventRepo;
pub use migrations::{MIGRATIONS, Migration, MigrationReport};
pub use ports::PortRepo;
pub use reader::StoreReader;
pub use sites::{Site, SiteKind, SitePhp, SiteRepo, SiteXdebug};
pub use tool_versions::{
    NewToolVersion, ToolVersionRepo, ToolVersionRow, VersionSource, VersionState,
};
pub use update_history::{
    NewUpdateHistory, UpdateHistoryRepo, UpdateHistoryRow, UpdateStatus, UpdateTrigger,
};
pub use writer::{StoreWriter, WriterStats};

/// How long a statement waits for a lock held by another connection.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long closing (or dropping) a [`Store`] waits for the writer to finish.
pub const STORE_CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// Tuning of the writer thread and the read pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreOptions {
    /// Operations the writer queue holds before `execute` waits for room (within its
    /// timeout).
    pub queue_capacity: usize,
    /// Maximum operations per write transaction.
    pub max_batch_ops: usize,
    /// Maximum duration of a write transaction before it is committed (checked between
    /// operations).
    pub max_batch_time: Duration,
    /// Default timeout of [`StoreWriter::execute`].
    pub write_timeout: Duration,
    /// Default timeout of [`StoreReader::query`].
    pub read_timeout: Duration,
    /// Idle read connections kept open.
    pub max_idle_readers: usize,
}

impl Default for StoreOptions {
    /// Queue of 1024, batches of up to 64 operations or 5 ms, 5 s timeouts, 4 idle readers.
    fn default() -> Self {
        Self {
            queue_capacity: 1024,
            max_batch_ops: 64,
            max_batch_time: Duration::from_millis(5),
            write_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_secs(5),
            max_idle_readers: 4,
        }
    }
}

/// Where the connections of a store point to.
#[derive(Debug, Clone)]
enum Target {
    File(PathBuf),
    /// URI of a named shared-cache in-memory database (one per `Store`).
    Memory(String),
}

impl Target {
    fn open(&self) -> Result<Connection> {
        Ok(match self {
            Target::File(path) => Connection::open(path)?,
            Target::Memory(uri) => Connection::open_with_flags(
                uri,
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_CREATE
                    | OpenFlags::SQLITE_OPEN_URI
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?,
        })
    }

    fn is_file(&self) -> bool {
        matches!(self, Target::File(_))
    }
}

/// An open, migrated database with its writer thread and read pool.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    path: Option<PathBuf>,
    migration: MigrationReport,
    writer: StoreWriter,
    reader: StoreReader,
    writer_thread: Option<writer::WriterThread>,
}

impl Store {
    /// Opens (creating if needed) the database at `path`, configures it and applies
    /// pending migrations. If the file already existed with content, it is backed up
    /// next to itself before the first migration runs (docs/08 §7). Then starts the
    /// writer thread. Blocking.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, StoreOptions::default())
    }

    /// [`Store::open`] with explicit [`StoreOptions`].
    pub fn open_with(path: &Path, options: StoreOptions) -> Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).at("creating", dir)?;
        }
        let existed = std::fs::metadata(path).is_ok_and(|m| m.len() > 0);
        let mut conn = Connection::open(path)?;
        configure(&conn, true)?;
        let migration = migrations::apply(&mut conn, MIGRATIONS, existed.then_some(path))?;
        Self::start(
            conn,
            Some(path.to_path_buf()),
            migration,
            Target::File(path.to_path_buf()),
            &options,
        )
    }

    /// Migrated in-memory database (tests and dry runs). Every connection of this store
    /// (maintenance, writer, readers) shares it through a uniquely named shared cache.
    pub fn open_in_memory() -> Result<Self> {
        Self::open_in_memory_with(StoreOptions::default())
    }

    /// [`Store::open_in_memory`] with explicit [`StoreOptions`].
    pub fn open_in_memory_with(options: StoreOptions) -> Result<Self> {
        let target = Target::Memory(format!(
            "file:forgebox-mem-{}?mode=memory&cache=shared",
            ulid::Ulid::generate()
        ));
        let mut conn = target.open()?;
        configure(&conn, false)?;
        let migration = migrations::apply(&mut conn, MIGRATIONS, None)?;
        Self::start(conn, None, migration, target, &options)
    }

    fn start(
        conn: Connection,
        path: Option<PathBuf>,
        migration: MigrationReport,
        target: Target,
        options: &StoreOptions,
    ) -> Result<Self> {
        let writer_conn = target.open()?;
        configure(&writer_conn, target.is_file())?;
        let (writer, writer_thread) = writer::spawn(writer_conn, options)?;
        let reader = StoreReader::new(target, options);
        Ok(Self {
            conn,
            path,
            migration,
            writer,
            reader,
            writer_thread: Some(writer_thread),
        })
    }

    /// Async handle to the writer thread (clone it to share).
    pub fn writer(&self) -> &StoreWriter {
        &self.writer
    }

    /// Read pool for `spawn_blocking` (clone it to share).
    pub fn reader(&self) -> &StoreReader {
        &self.reader
    }

    /// Flushes and stops the writer thread, closes the read pool and the maintenance
    /// connection. Blocking (at most [`STORE_CLOSE_TIMEOUT`]).
    pub fn close(mut self) -> Result<()> {
        self.shutdown(STORE_CLOSE_TIMEOUT)
    }

    fn shutdown(&mut self, timeout: Duration) -> Result<()> {
        self.reader.close();
        match self.writer_thread.take() {
            Some(thread) => thread.shutdown(timeout),
            None => Ok(()),
        }
    }

    /// File of the database (`None` in memory).
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// What the migration runner did when this store was opened.
    pub fn migration_report(&self) -> &MigrationReport {
        &self.migration
    }

    /// Current schema version.
    pub fn schema_version(&self) -> Result<u32> {
        migrations::current_version(&self.conn)
    }

    /// Maintenance connection (blocking): migrations, offline tools, tests. The daemon
    /// uses [`Store::writer`] and [`Store::reader`] instead.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Mutable maintenance connection (transactions).
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Events repository on the maintenance connection.
    pub fn events(&self) -> EventRepo<'_> {
        EventRepo::new(&self.conn)
    }

    /// Port reservations repository on the maintenance connection.
    pub fn ports(&self) -> PortRepo<'_> {
        PortRepo::new(&self.conn)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if let Err(err) = self.shutdown(STORE_CLOSE_TIMEOUT) {
            tracing::warn!(%err, "store writer did not stop cleanly");
        }
    }
}

fn configure(conn: &Connection, file_backed: bool) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    if file_backed {
        let mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(ForgeError::Schema(format!(
                "could not enable WAL (journal_mode = {mode})"
            )));
        }
        // Safe with WAL: durable at checkpoints, much faster commits.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
    }
    let fk: i64 = conn.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    if fk != 1 {
        return Err(ForgeError::Schema(
            "foreign keys could not be enabled".to_owned(),
        ));
    }
    Ok(())
}
