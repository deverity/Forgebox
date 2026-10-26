//! SQLite store: `state/forgebox.db`, the source of truth for domain entities (ADR-0007).
//!
//! [`Store`] is a plain synchronous connection (rusqlite, bundled SQLite) configured
//! with WAL, foreign keys and a busy timeout, migrated on open. It is **blocking**:
//! the daemon owns it from a dedicated writer thread with a queue (M0-E1-T5, docs/03
//! §7.1); no query runs on a tokio worker thread.
//!
//! Repositories are added per aggregate (docs/02). M0-E1-T4 ships the migration runner
//! and [`events::EventRepo`].

pub mod events;
pub mod migrations;

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;

use crate::error::{ForgeError, IoResultExt, Result};
pub use events::EventRepo;
pub use migrations::{MIGRATIONS, Migration, MigrationReport};

/// How long a statement waits for a lock held by another connection.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// An open, migrated database.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    path: Option<PathBuf>,
    migration: MigrationReport,
}

impl Store {
    /// Opens (creating if needed) the database at `path`, configures it and applies
    /// pending migrations. If the file already existed with content, it is backed up
    /// next to itself before the first migration runs (docs/08 §7).
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).at("creating", dir)?;
        }
        let existed = std::fs::metadata(path).is_ok_and(|m| m.len() > 0);
        let mut conn = Connection::open(path)?;
        configure(&conn, true)?;
        let migration = migrations::apply(&mut conn, MIGRATIONS, existed.then_some(path))?;
        Ok(Self {
            conn,
            path: Some(path.to_path_buf()),
            migration,
        })
    }

    /// Migrated in-memory database (tests and dry runs).
    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        configure(&conn, false)?;
        let migration = migrations::apply(&mut conn, MIGRATIONS, None)?;
        Ok(Self {
            conn,
            path: None,
            migration,
        })
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

    /// Raw connection, for repositories of other modules and tests.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Mutable connection (transactions).
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Events repository.
    pub fn events(&self) -> EventRepo<'_> {
        EventRepo::new(&self.conn)
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
