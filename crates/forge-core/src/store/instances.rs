//! `instances` table: service instances of an installed version (`mysql 8.4 "default"`,
//! docs/01 §4). The daemon (M1-E5-T2) keeps one actor per row; ports live in `ports`
//! (owner kind `instance`), quick settings in the `settings` JSON object.

use std::collections::BTreeMap;

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::{ForgeError, Result};
use crate::ids::{InstanceId, ToolId, VersionId};

/// `instances.desired_state`: what the daemon restores at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DesiredState {
    #[default]
    Stopped,
    Running,
}

impl DesiredState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Running => "running",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "stopped" => Ok(Self::Stopped),
            "running" => Ok(Self::Running),
            other => Err(ForgeError::Schema(format!(
                "instances.desired_state {other:?}"
            ))),
        }
    }
}

/// A stored instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceRow {
    pub id: InstanceId,
    pub tool_id: ToolId,
    pub version_id: VersionId,
    /// `default`, `legacy`... (`[A-Za-z0-9._-]`, unique per tool).
    pub name: String,
    pub autostart: bool,
    pub desired_state: DesiredState,
    /// Data folder (`/` separators), when the manifest declares `service.daemon.data`.
    pub data_dir: Option<String>,
    /// Quick settings (`memory_limit = 1G`).
    pub settings: BTreeMap<String, String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl InstanceRow {
    /// A new row (fresh id, timestamps now, desired state `stopped`).
    pub fn new(tool_id: ToolId, version_id: VersionId, name: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: InstanceId::new(),
            tool_id,
            version_id,
            name: name.into(),
            autostart: false,
            desired_state: DesiredState::Stopped,
            data_dir: None,
            settings: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

const COLUMNS: &str = "id, tool_id, version_id, name, autostart, desired_state, data_dir, \
    settings, created_at, updated_at";

fn corrupt(what: &str, e: impl ToString) -> ForgeError {
    ForgeError::Schema(format!("instances row: invalid {what}: {}", e.to_string()))
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<Result<InstanceRow>> {
    let id: InstanceId = row.get(0)?;
    let tool_id: ToolId = row.get(1)?;
    let version_id: VersionId = row.get(2)?;
    let name: String = row.get(3)?;
    let autostart: bool = row.get(4)?;
    let desired: String = row.get(5)?;
    let data_dir: Option<String> = row.get(6)?;
    let settings: String = row.get(7)?;
    let created: String = row.get(8)?;
    let updated: String = row.get(9)?;
    let decode = || -> Result<InstanceRow> {
        Ok(InstanceRow {
            id,
            tool_id,
            version_id,
            name,
            autostart,
            desired_state: DesiredState::parse(&desired)?,
            data_dir,
            settings: serde_json::from_str(&settings).map_err(|e| corrupt("settings", e))?,
            created_at: created.parse().map_err(|e| corrupt("created_at", e))?,
            updated_at: updated.parse().map_err(|e| corrupt("updated_at", e))?,
        })
    };
    Ok(decode())
}

/// Instance names: `[A-Za-z0-9._-]{1,64}`, not starting with `.` (they become file and
/// folder names: `logs/<tool>/<name>.out.log`, `data/<tool>/<name>`).
pub fn validate_instance_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(ForgeError::InvalidInput(format!(
            "invalid instance name {name:?} (letters, digits, '.', '_', '-')"
        )))
    }
}

/// Repository over `instances`.
#[derive(Debug, Clone, Copy)]
pub struct InstanceRepo<'c> {
    conn: &'c Connection,
}

impl<'c> InstanceRepo<'c> {
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    fn query(&self, sql: &str, args: impl rusqlite::Params) -> Result<Vec<InstanceRow>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt
            .query_map(args, from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().collect()
    }

    /// Inserts `row`; `conflict` when the tool already has an instance with that name.
    pub fn insert(&self, row: &InstanceRow) -> Result<()> {
        validate_instance_name(&row.name)?;
        if self.find(&row.tool_id, &row.name)?.is_some() {
            return Err(ForgeError::Conflict(format!(
                "{}/{} already exists",
                row.tool_id, row.name
            )));
        }
        let settings = serde_json::to_string(&row.settings)
            .map_err(|e| ForgeError::Internal(format!("instance settings: {e}")))?;
        self.conn.execute(
            &format!(
                "INSERT INTO instances ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
            ),
            params![
                row.id,
                row.tool_id,
                row.version_id,
                row.name,
                row.autostart,
                row.desired_state.as_str(),
                row.data_dir,
                settings,
                row.created_at.to_string(),
                row.updated_at.to_string(),
            ],
        )?;
        Ok(())
    }

    /// Rewrites the mutable columns of `row` (version, autostart, desired state, data dir,
    /// settings); `updated_at` becomes now. `not_found` when the row is gone.
    pub fn update(&self, row: &InstanceRow) -> Result<()> {
        let settings = serde_json::to_string(&row.settings)
            .map_err(|e| ForgeError::Internal(format!("instance settings: {e}")))?;
        let changed = self.conn.execute(
            "UPDATE instances SET version_id = ?2, autostart = ?3, desired_state = ?4, \
             data_dir = ?5, settings = ?6, updated_at = ?7 WHERE id = ?1",
            params![
                row.id,
                row.version_id,
                row.autostart,
                row.desired_state.as_str(),
                row.data_dir,
                settings,
                Timestamp::now().to_string(),
            ],
        )?;
        if changed == 0 {
            return Err(ForgeError::NotFound(format!("instance {}", row.id)));
        }
        Ok(())
    }

    /// Sets only the desired state (what the daemon restores at boot).
    pub fn set_desired_state(&self, id: InstanceId, state: DesiredState) -> Result<()> {
        self.conn.execute(
            "UPDATE instances SET desired_state = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, state.as_str(), Timestamp::now().to_string()],
        )?;
        Ok(())
    }

    pub fn get(&self, id: InstanceId) -> Result<Option<InstanceRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {COLUMNS} FROM instances WHERE id = ?1"))?;
        stmt.query_row(params![id], from_row)
            .optional()?
            .transpose()
    }

    pub fn find(&self, tool: &ToolId, name: &str) -> Result<Option<InstanceRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM instances WHERE tool_id = ?1 AND name = ?2"
        ))?;
        stmt.query_row(params![tool, name], from_row)
            .optional()?
            .transpose()
    }

    /// Every instance, by tool then name.
    pub fn list(&self) -> Result<Vec<InstanceRow>> {
        self.query(
            &format!("SELECT {COLUMNS} FROM instances ORDER BY tool_id, name"),
            [],
        )
    }

    /// Instances of `tool`, by name.
    pub fn list_tool(&self, tool: &ToolId) -> Result<Vec<InstanceRow>> {
        self.query(
            &format!("SELECT {COLUMNS} FROM instances WHERE tool_id = ?1 ORDER BY name"),
            params![tool],
        )
    }

    /// Deletes the row; `false` when it did not exist.
    pub fn delete(&self, id: InstanceId) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM instances WHERE id = ?1", params![id])?
            > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{NewToolVersion, Store, ToolVersionRepo, VersionSource, VersionState};

    fn version(store: &Store) -> (ToolId, VersionId) {
        let tool = ToolId::new("mysql").unwrap();
        let id = ToolVersionRepo::new(store.conn())
            .register_if_absent(&NewToolVersion {
                tool_id: tool.clone(),
                tool_name: "MySQL".into(),
                version: "8.4.6".into(),
                canonical_name: "8.4.6".into(),
                variant: BTreeMap::new(),
                path: "C:/fb/bin/mysql/8.4.6".into(),
                state: VersionState::Installed,
                source: VersionSource::Manual,
            })
            .unwrap()
            .unwrap();
        (tool, id)
    }

    #[test]
    fn insert_find_update_delete() {
        let store = Store::open_in_memory().unwrap();
        let (tool, version_id) = version(&store);
        let repo = InstanceRepo::new(store.conn());
        let mut row = InstanceRow::new(tool.clone(), version_id, "default");
        row.autostart = true;
        row.data_dir = Some("C:/fb/data/mysql/default".into());
        row.settings.insert("max_connections".into(), "200".into());
        repo.insert(&row).unwrap();
        assert_eq!(
            repo.insert(&row).unwrap_err().code(),
            "conflict",
            "same tool and name"
        );
        let found = repo.find(&tool, "default").unwrap().unwrap();
        assert_eq!(found.id, row.id);
        assert_eq!(found.settings["max_connections"], "200");
        assert!(found.autostart);

        repo.set_desired_state(row.id, DesiredState::Running)
            .unwrap();
        assert_eq!(
            repo.get(row.id).unwrap().unwrap().desired_state,
            DesiredState::Running
        );
        let mut changed = found.clone();
        changed.autostart = false;
        repo.update(&changed).unwrap();
        assert!(!repo.get(row.id).unwrap().unwrap().autostart);
        assert_eq!(repo.list().unwrap().len(), 1);
        assert_eq!(repo.list_tool(&tool).unwrap().len(), 1);
        assert!(repo.delete(row.id).unwrap());
        assert!(!repo.delete(row.id).unwrap());
        assert!(repo.get(row.id).unwrap().is_none());
    }

    #[test]
    fn names_are_validated() {
        for bad in ["", ".hidden", "a/b", "x y", &"a".repeat(65)] {
            assert!(validate_instance_name(bad).is_err(), "{bad:?}");
        }
        for good in ["default", "legacy-2", "v8.4_a"] {
            validate_instance_name(good).unwrap();
        }
    }
}
