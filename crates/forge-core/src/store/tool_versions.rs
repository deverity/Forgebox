//! `tools` / `tool_versions` tables: installed versions of tools (docs/08 §5).
//!
//! Only what discovery and the layout engine need so far: idempotent registration
//! (discovery), recording of an installation (layout engine), lookup and listing. Paths
//! are stored with `/` separators (db conventions of `0001_init.sql`).

use std::collections::BTreeMap;

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};

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

    /// Records a version installed by the layout engine (`forge_provider::layout`):
    /// inserts it, or, when the tool already has a row with the same `canonical_name`
    /// (reinstall, repair, a row left by a removed folder), updates that row's version,
    /// variant, path, state, source, sha256, size and `installed_at`. Creates the `tools`
    /// row when missing. Returns the row id and whether it was inserted.
    pub fn record_install(
        &self,
        new: &NewToolVersion,
        sha256: Option<&str>,
        size_bytes: Option<u64>,
    ) -> Result<(VersionId, bool)> {
        let path = normalize_path(&new.path);
        let now = Timestamp::now().to_string();
        let variant = serde_json::to_string(&new.variant)
            .map_err(|e| ForgeError::InvalidInput(format!("variant: {e}")))?;
        let size = size_bytes
            .map(i64::try_from)
            .transpose()
            .map_err(|_| ForgeError::InvalidInput("size_bytes does not fit in i64".into()))?;
        self.conn.execute(
            "INSERT OR IGNORE INTO tools (id, name, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?3)",
            params![new.tool_id, new.tool_name, now],
        )?;
        if let Some(existing) = self.find(&new.tool_id, &new.canonical_name)? {
            self.conn.execute(
                "UPDATE tool_versions SET version = ?2, variant = ?3, path = ?4, state = ?5, \
                 source = ?6, sha256 = ?7, size_bytes = ?8, installed_at = ?9, updated_at = ?9 \
                 WHERE id = ?1",
                params![
                    existing.id,
                    new.version,
                    variant,
                    path,
                    new.state.as_str(),
                    new.source.as_str(),
                    sha256,
                    size,
                    now
                ],
            )?;
            return Ok((existing.id, false));
        }
        let id = VersionId::new();
        self.conn.execute(
            "INSERT INTO tool_versions \
             (id, tool_id, version, canonical_name, variant, path, state, source, sha256, \
              size_bytes, installed_at, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11, ?11)",
            params![
                id,
                new.tool_id,
                new.version,
                new.canonical_name,
                variant,
                path,
                new.state.as_str(),
                new.source.as_str(),
                sha256,
                size,
                now
            ],
        )?;
        Ok((id, true))
    }

    /// The row of `tool` with folder name `canonical_name`, if any.
    pub fn find(&self, tool: &ToolId, canonical_name: &str) -> Result<Option<ToolVersionRow>> {
        Ok(self
            .list(Some(tool))?
            .into_iter()
            .find(|row| row.canonical_name == canonical_name))
    }

    /// The row with id `id`, if any.
    pub fn get(&self, id: VersionId) -> Result<Option<ToolVersionRow>> {
        let tool: Option<ToolId> = self
            .conn
            .query_row(
                "SELECT tool_id FROM tool_versions WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(match tool {
            Some(tool) => self.list(Some(&tool))?.into_iter().find(|r| r.id == id),
            None => None,
        })
    }

    /// Changes the state of a row (`removing` while its folder is deleted). `NotFound` if
    /// the row does not exist.
    pub fn set_state(&self, id: VersionId, state: VersionState) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE tool_versions SET state = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, state.as_str(), Timestamp::now().to_string()],
        )?;
        if changed == 0 {
            return Err(ForgeError::NotFound(format!("tool version {id}")));
        }
        Ok(())
    }

    /// Service instances bound to the version (`instances.version_id`).
    pub fn instance_count(&self, id: VersionId) -> Result<u64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM instances WHERE version_id = ?1",
            [id],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// Deletes a row (its folder is the caller's business). `Conflict` while instances
    /// still use it; `Ok(false)` if it did not exist. History rows keep their version
    /// text (`update_history.*_version_id` become `NULL`).
    pub fn delete(&self, id: VersionId) -> Result<bool> {
        if self.instance_count(id)? > 0 {
            return Err(ForgeError::Conflict(format!(
                "tool version {id} is used by service instances"
            )));
        }
        Ok(self
            .conn
            .execute("DELETE FROM tool_versions WHERE id = ?1", [id])?
            > 0)
    }

    /// `tools.update_policy`: the user's override of the manifest's `update.policy`
    /// (`manual`, `notify`, `auto-patch`, `auto-minor`, `auto`); `None` when not set or
    /// the tool is unknown.
    pub fn update_policy(&self, tool: &ToolId) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT update_policy FROM tools WHERE id = ?1",
                [tool],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Sets (or clears, with `None`) the override of [`ToolVersionRepo::update_policy`].
    /// `NotFound` if the tool has no row yet; the column `CHECK` refuses unknown values.
    pub fn set_update_policy(&self, tool: &ToolId, policy: Option<&str>) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE tools SET update_policy = ?2, updated_at = ?3 WHERE id = ?1",
            params![tool, policy, Timestamp::now().to_string()],
        )?;
        if changed == 0 {
            return Err(ForgeError::NotFound(format!("tool {tool}")));
        }
        Ok(())
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
    fn record_install_inserts_then_updates_the_same_folder() {
        let store = Store::open_in_memory().unwrap();
        let repo = ToolVersionRepo::new(store.conn());
        let mut v = new_version("8.3.21-nts", r"C:\fb\bin\php\8.3.21-nts");
        v.source = VersionSource::Registry;
        let (id, inserted) = repo.record_install(&v, Some("ab"), Some(42)).unwrap();
        assert!(inserted);
        v.state = VersionState::Broken;
        let (again, inserted) = repo.record_install(&v, None, None).unwrap();
        assert_eq!((again, inserted), (id, false));
        let row = repo.find(&v.tool_id, "8.3.21-nts").unwrap().unwrap();
        assert_eq!(row.state, VersionState::Broken);
        assert_eq!(row.path, "C:/fb/bin/php/8.3.21-nts");
        assert!(repo.find(&v.tool_id, "nope").unwrap().is_none());
        let (sha, size): (Option<String>, Option<i64>) = store
            .conn()
            .query_row(
                "SELECT sha256, size_bytes FROM tool_versions WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((sha, size), (None, None));
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

    #[test]
    fn get_state_delete_and_policy_override() {
        let store = Store::open_in_memory().unwrap();
        let repo = ToolVersionRepo::new(store.conn());
        let v = new_version("8.3.21-nts", "C:/fb/bin/php/8.3.21-nts");
        let (id, _) = repo.record_install(&v, None, None).unwrap();
        assert_eq!(repo.get(id).unwrap().unwrap().canonical_name, "8.3.21-nts");
        repo.set_state(id, VersionState::Removing).unwrap();
        assert_eq!(repo.get(id).unwrap().unwrap().state, VersionState::Removing);
        assert_eq!(
            repo.set_state(VersionId::new(), VersionState::Broken)
                .unwrap_err()
                .code(),
            "not_found"
        );

        assert_eq!(repo.update_policy(&v.tool_id).unwrap(), None);
        repo.set_update_policy(&v.tool_id, Some("manual")).unwrap();
        assert_eq!(
            repo.update_policy(&v.tool_id).unwrap().as_deref(),
            Some("manual")
        );
        assert!(
            repo.set_update_policy(&v.tool_id, Some("sometimes"))
                .is_err()
        );
        let ghost = ToolId::new("ghost").unwrap();
        assert_eq!(repo.update_policy(&ghost).unwrap(), None);
        assert_eq!(
            repo.set_update_policy(&ghost, None).unwrap_err().code(),
            "not_found"
        );

        // An instance pins the version.
        let now = Timestamp::now().to_string();
        store
            .conn()
            .execute(
                "INSERT INTO instances (id, tool_id, version_id, name, created_at, updated_at)                  VALUES ('01J00000000000000000000000', 'php', ?1, 'default', ?2, ?2)",
                params![id, now],
            )
            .unwrap();
        assert_eq!(repo.instance_count(id).unwrap(), 1);
        assert_eq!(repo.delete(id).unwrap_err().code(), "conflict");
        store.conn().execute("DELETE FROM instances", []).unwrap();
        assert!(repo.delete(id).unwrap());
        assert!(!repo.delete(id).unwrap());
        assert!(repo.get(id).unwrap().is_none());
    }
}
