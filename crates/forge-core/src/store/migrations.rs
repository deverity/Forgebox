//! Schema migrations of `state/forgebox.db`.
//!
//! Migrations are embedded (`include_str!`), numbered and never edited once published
//! (docs/02 §5). The runner is idempotent: each pending migration runs in its own
//! `IMMEDIATE` transaction together with its `schema_migrations` row, and re-checks
//! inside the transaction whether another process applied it meanwhile. Before
//! touching an existing, non-empty database file it takes a consistent copy with
//! `VACUUM INTO` (docs/08 §7, ADR-0007).

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::error::{ForgeError, Result};

/// One embedded migration.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

impl Migration {
    /// FNV-1a 64 of the SQL with line endings normalised to LF, as 16 hex digits. Only
    /// used to warn when a published migration was edited.
    pub fn checksum(&self) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in self.sql.bytes().filter(|&b| b != b'\r') {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{hash:016x}")
    }
}

/// Every migration of this build, in order.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "registry_loader",
        sql: include_str!("migrations/0002_registry_loader.sql"),
    },
];

/// Highest schema version this build knows.
pub fn latest_version() -> u32 {
    MIGRATIONS.last().map_or(0, |m| m.version)
}

/// Outcome of [`apply`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// Schema version before running (0 for a new database).
    pub from_version: u32,
    /// Schema version after running.
    pub to_version: u32,
    /// Versions applied by this call.
    pub applied: Vec<u32>,
    /// Copy of the database taken before applying, if any.
    pub backup: Option<PathBuf>,
}

fn has_migrations_table(conn: &Connection) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// `(version, checksum)` of applied migrations, ascending.
pub fn applied(conn: &Connection) -> Result<Vec<(u32, String)>> {
    if !has_migrations_table(conn)? {
        return Ok(Vec::new());
    }
    let mut stmt =
        conn.prepare("SELECT version, checksum FROM schema_migrations ORDER BY version")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Current schema version (0 when nothing is applied).
pub fn current_version(conn: &Connection) -> Result<u32> {
    Ok(applied(conn)?.last().map_or(0, |(v, _)| *v))
}

/// Applies the pending `migrations`. When `backup_from` is `Some(db_path)`, a copy named
/// `<db>.bak-v<NNNN>-<UTC timestamp>` is written next to it before the first change.
pub fn apply(
    conn: &mut Connection,
    migrations: &[Migration],
    backup_from: Option<&Path>,
) -> Result<MigrationReport> {
    let done = applied(conn)?;
    let from_version = done.last().map_or(0, |(v, _)| *v);
    let latest = migrations.last().map_or(0, |m| m.version);
    if from_version > latest {
        return Err(ForgeError::Schema(format!(
            "database schema version {from_version} is newer than this build supports \
             ({latest}); update Forgebox"
        )));
    }
    for (version, checksum) in &done {
        if let Some(m) = migrations.iter().find(|m| m.version == *version) {
            if m.checksum() != *checksum {
                tracing::warn!(
                    version,
                    name = m.name,
                    "migration was edited after being applied (checksum mismatch)"
                );
            }
        }
    }

    let pending: Vec<&Migration> = migrations
        .iter()
        .filter(|m| !done.iter().any(|(v, _)| *v == m.version))
        .collect();
    let mut report = MigrationReport {
        from_version,
        to_version: from_version,
        ..MigrationReport::default()
    };
    if pending.is_empty() {
        return Ok(report);
    }

    if let Some(db_path) = backup_from {
        report.backup = Some(backup(conn, db_path, from_version)?);
    }

    for migration in pending {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let already: bool = has_migrations_table(&tx)?
            && tx
                .query_row(
                    "SELECT 1 FROM schema_migrations WHERE version = ?1",
                    [migration.version],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
        if already {
            continue;
        }
        tx.execute_batch(migration.sql).map_err(|e| {
            ForgeError::Schema(format!(
                "migration {:04}_{} failed: {e}",
                migration.version, migration.name
            ))
        })?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, checksum, applied_at) \
             VALUES (?1, ?2, ?3, ?4)",
            (
                migration.version,
                migration.name,
                migration.checksum(),
                Timestamp::now().to_string(),
            ),
        )?;
        tx.commit()?;
        tracing::info!(
            version = migration.version,
            name = migration.name,
            "applied migration"
        );
        report.applied.push(migration.version);
    }
    report.to_version = current_version(conn)?;
    Ok(report)
}

/// Consistent copy of the live database (works with WAL) via `VACUUM INTO`.
fn backup(conn: &Connection, db_path: &Path, version: u32) -> Result<PathBuf> {
    let stamp = Timestamp::now().strftime("%Y%m%dT%H%M%SZ").to_string();
    // `VACUUM INTO` refuses to overwrite: add a counter if a retry lands in the same second.
    let target = (0..100)
        .map(|n| {
            let mut name = db_path.as_os_str().to_owned();
            name.push(format!(".bak-v{version:04}-{stamp}"));
            if n > 0 {
                name.push(format!("-{n}"));
            }
            PathBuf::from(name)
        })
        .find(|candidate| !candidate.exists())
        .ok_or_else(|| {
            ForgeError::Conflict(format!("too many backups of {}", db_path.display()))
        })?;
    let target_str = target
        .to_str()
        .ok_or_else(|| ForgeError::InvalidInput(format!("non UTF-8 path {}", target.display())))?;
    conn.execute("VACUUM INTO ?1", [target_str])?;
    tracing::info!(backup = %target.display(), "database backed up before migration");
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_numbered_in_order() {
        for (i, m) in MIGRATIONS.iter().enumerate() {
            assert_eq!(m.version as usize, i + 1, "{}", m.name);
            assert!(!m.sql.trim().is_empty());
        }
        assert_eq!(latest_version(), MIGRATIONS.len() as u32);
    }

    #[test]
    fn checksum_ignores_line_endings() {
        let lf = Migration {
            version: 1,
            name: "x",
            sql: "CREATE TABLE a (b);\nSELECT 1;\n",
        };
        let crlf = Migration {
            sql: "CREATE TABLE a (b);\r\nSELECT 1;\r\n",
            ..lf
        };
        assert_eq!(lf.checksum(), crlf.checksum());
        assert_eq!(lf.checksum().len(), 16);
    }

    #[test]
    fn newer_schema_is_rejected() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply(&mut conn, MIGRATIONS, None).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations VALUES (999, 'future', 'x', 'now')",
            [],
        )
        .unwrap();
        let err = apply(&mut conn, MIGRATIONS, None).unwrap_err();
        assert_eq!(err.code(), "db");
        assert!(err.to_string().contains("999"), "{err}");
    }

    #[test]
    fn failing_migration_rolls_back() {
        const BROKEN: &[Migration] = &[
            Migration {
                version: 1,
                name: "init",
                sql: include_str!("migrations/0001_init.sql"),
            },
            Migration {
                version: 2,
                name: "broken",
                sql: "CREATE TABLE half (x); THIS IS NOT SQL;",
            },
        ];
        let mut conn = Connection::open_in_memory().unwrap();
        let err = apply(&mut conn, BROKEN, None).unwrap_err();
        assert!(err.to_string().contains("0002_broken"), "{err}");
        assert_eq!(current_version(&conn).unwrap(), 1);
        let half: Option<String> = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE name = 'half'",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(half, None, "partial migration rolled back");
    }
}
