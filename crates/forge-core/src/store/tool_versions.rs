//! `tools` / `tool_versions` tables: installed versions of tools (docs/08 §5).
//!
//! Only what discovery and the layout engine need so far: idempotent registration and
//! listing. Paths are stored with `/` separators (db conventions of `0001_init.sql`).

use std::collections::BTreeMap;

use jiff::Timestamp;
use rusqlite::{Connection, params};

use crate::error::{ForgeError, Result};
use crate::ids::{ToolId, VersionId};

/// `tool_versions.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionState {
    Downloading,
    Installed,
    Broken,
    Removing,
}

impl VersionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Downloading => "downloading",
            Self::Installed => "installed",
            Self::Broken => "broken",
            Self::Removing => "removing",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "downloading" => Self::Downloading,
            "installed" => Self::Installed,
            "broken" => Self::Broken,
            "removing" => Self::Removing,
            _ => return None,
        })
    }
}

/// `tool_versions.source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionSource {
    Registry,
    Manual,
    Import,
}

impl VersionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Registry => "registry",
            Self::Manual => "manual",
            Self::Import => "import",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "registry" => Self::Registry,
            "manual" => Self::Manual,
            "import" => Self::Import,
            _ => return None,
        })
    }
}

/// A version to register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewToolVersion {
    pub tool_id: ToolId,
    /// Display name used when the `tools` row has to be created.
    pub tool_name: String,
    pub version: String,
    /// Folder name; unique per tool.
    pub canonical_name: String,
    pub variant: BTreeMap<String, String>,
    /// Absolute path of the folder (any separator; stored with `/`).
    pub path: String,
    pub state: VersionState,
    pub source: VersionSource,
}

/// A stored `tool_versions` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolVersionRow {
    pub id: VersionId,
    pub tool_id: ToolId,
    pub version: String,
    pub canonical_name: String,
    pub variant: BTreeMap<String, String>,
    pub path: String,
    pub state: VersionState,
    pub source: VersionSource,
}

/// Repository over `tools` and `tool_versions`.
#[derive(Debug, Clone, Copy)]
pub struct ToolVersionRepo<'c> {
    conn: &'c Connection,
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

impl<'c> ToolVersionRepo<'c> {
    /// Repository on `conn` (a writer operation, a reader or the maintenance connection).
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// Registers `new` unless the tool already has a version with the same
    /// `canonical_name` or the same `path`. Creates the `tools` row when missing. Returns
    /// the id of the new row, or `None` when nothing was inserted (idempotent: existing
    /// rows, whatever their source, are never modified).
    pub fn register_if_absent(&self, new: &NewToolVersion) -> Result<Option<VersionId>> {
        let path = normalize_path(&new.path);
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM tool_versions \
             WHERE tool_id = ?1 AND (canonical_name = ?2 OR path = ?3))",
            params![new.tool_id, new.canonical_name, path],
            |r| r.get(0),
        )?;
        if exists {
            return Ok(None);
        }
        let now = Timestamp::now().to_string();
        self.conn.execute(
            "INSERT OR IGNORE INTO tools (id, name, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?3)",
            params![new.tool_id, new.tool_name, now],
        )?;
        let id = VersionId::new();
        let variant = serde_json::to_string(&new.variant)
            .map_err(|e| ForgeError::InvalidInput(format!("variant: {e}")))?;
        self.conn.execute(
            "INSERT INTO tool_versions \
             (id, tool_id, version, canonical_name, variant, path, state, source, \
              installed_at, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?9)",
            params![
                id,
                new.tool_id,
                new.version,
                new.canonical_name,
                variant,
                path,
                new.state.as_str(),
                new.source.as_str(),
                now
            ],
        )?;
        Ok(Some(id))
    }

    /// Versions of `tool` (all tools when `None`), ordered by tool and folder name.
    pub fn list(&self, tool: Option<&ToolId>) -> Result<Vec<ToolVersionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, tool_id, version, canonical_name, variant, path, state, source \
             FROM tool_versions WHERE (?1 IS NULL OR tool_id = ?1) \
             ORDER BY tool_id, canonical_name",
        )?;
        let rows = stmt.query_map([tool], |r| {
            Ok((
                r.get::<_, VersionId>(0)?,
                r.get::<_, ToolId>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, tool_id, version, canonical_name, variant, path, state, source) = row?;
            let corrupt =
                |what: &str| ForgeError::Schema(format!("tool_versions row {id}: invalid {what}"));
            out.push(ToolVersionRow {
                variant: serde_json::from_str(&variant).map_err(|_| corrupt("variant"))?,
                state: VersionState::parse(&state).ok_or_else(|| corrupt("state"))?,
                source: VersionSource::parse(&source).ok_or_else(|| corrupt("source"))?,
                id,
                tool_id,
                version,
                canonical_name,
                path,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn new_version(name: &str, path: &str) -> NewToolVersion {
        NewToolVersion {
            tool_id: ToolId::new("php").unwrap(),
            tool_name: "PHP".into(),
            version: "8.3.21".into(),
            canonical_name: name.into(),
            variant: BTreeMap::from([("ts".to_owned(), "nts".to_owned())]),
            path: path.into(),
            state: VersionState::Installed,
            source: VersionSource::Manual,
        }
    }

    #[test]
    fn register_is_idempotent_and_roundtrips() {
        let store = Store::open_in_memory().unwrap();
        let repo = ToolVersionRepo::new(store.conn());
        let v = new_version(
            "php-8.3-Win32-vs16-x64",
            r"C:\fb\bin\php\php-8.3-Win32-vs16-x64",
        );
        assert!(repo.register_if_absent(&v).unwrap().is_some());
        assert!(repo.register_if_absent(&v).unwrap().is_none());
        // Same path under another name is the same install.
        let alias = new_version("other", "C:/fb/bin/php/php-8.3-Win32-vs16-x64");
        assert!(repo.register_if_absent(&alias).unwrap().is_none());

        let rows = repo.list(Some(&v.tool_id)).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "C:/fb/bin/php/php-8.3-Win32-vs16-x64");
        assert_eq!(rows[0].source, VersionSource::Manual);
        assert_eq!(rows[0].variant["ts"], "nts");
    }

    #[test]
    fn existing_registry_row_is_not_overwritten() {
        let store = Store::open_in_memory().unwrap();
        let repo = ToolVersionRepo::new(store.conn());
        let mut installed = new_version("8.3.21-nts", "C:/fb/bin/php/8.3.21-nts");
        installed.source = VersionSource::Registry;
        repo.register_if_absent(&installed).unwrap().unwrap();
        let manual = new_version("8.3.21-nts", "C:/fb/bin/php/8.3.21-nts");
        assert!(repo.register_if_absent(&manual).unwrap().is_none());
        assert_eq!(repo.list(None).unwrap()[0].source, VersionSource::Registry);
    }
}
