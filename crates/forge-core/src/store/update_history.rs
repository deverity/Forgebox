//! `update_history`: every update and rollback of a tool version (docs/05 §4), written
//! by the update engine (`forge_provider::update`, M1-E2-T5).
//!
//! An update row is inserted as `started` before anything changes and finished as
//! `completed` or `failed`. A rollback is its own row (`triggered_by = rollback`,
//! `rollback_of` = the update it undid), inserted already `completed`, and the undone row
//! becomes `rolled_back` in the same writer operation. Version strings are text so the
//! history survives the removal of old versions (`*_version_id` become `NULL`).

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::{ForgeError, Result};
use crate::ids::{JobId, ToolId, UpdateId, VersionId};

/// `update_history.triggered_by`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateTrigger {
    /// Applied by the scheduler under an `auto-*` policy.
    Auto,
    /// Asked for by the user (`forge tool update`).
    User,
    /// A rollback row.
    Rollback,
}

impl UpdateTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::User => "user",
            Self::Rollback => "rollback",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "auto" => Self::Auto,
            "user" => Self::User,
            "rollback" => Self::Rollback,
            _ => return None,
        })
    }
}

/// `update_history.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateStatus {
    Started,
    Completed,
    Failed,
    /// A completed update that a later rollback undid.
    RolledBack,
}

impl UpdateStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::RolledBack => "rolled_back",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "started" => Self::Started,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "rolled_back" => Self::RolledBack,
            _ => return None,
        })
    }
}

/// A row to insert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUpdateHistory {
    pub tool_id: ToolId,
    /// Series of the replaced version (`8.3`).
    pub series: Option<String>,
    pub from_version_id: Option<VersionId>,
    pub to_version_id: Option<VersionId>,
    pub from_version: Option<String>,
    pub to_version: String,
    /// Effective policy (`auto-patch`...), as text.
    pub policy: Option<String>,
    pub triggered_by: UpdateTrigger,
    pub status: UpdateStatus,
    pub job_id: Option<JobId>,
    pub rollback_of: Option<UpdateId>,
}

/// A stored row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateHistoryRow {
    pub id: UpdateId,
    pub tool_id: ToolId,
    pub series: Option<String>,
    pub from_version_id: Option<VersionId>,
    pub to_version_id: Option<VersionId>,
    pub from_version: Option<String>,
    pub to_version: String,
    pub policy: Option<String>,
    pub triggered_by: UpdateTrigger,
    pub status: UpdateStatus,
    pub error: Option<String>,
    pub job_id: Option<JobId>,
    pub rollback_of: Option<UpdateId>,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
}

const COLUMNS: &str = "id, tool_id, series, from_version_id, to_version_id, from_version, \
     to_version, policy, triggered_by, status, error, job_id, rollback_of, started_at, \
     finished_at";

fn timestamp(id: &UpdateId, what: &str, text: &str) -> Result<Timestamp> {
    text.parse()
        .map_err(|_| ForgeError::Schema(format!("update_history row {id}: invalid {what}")))
}

/// Raw columns of one row, converted outside the rusqlite closure.
type RawRow = (
    UpdateId,
    ToolId,
    Option<String>,
    Option<VersionId>,
    Option<VersionId>,
    Option<String>,
    String,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<JobId>,
    Option<UpdateId>,
    String,
    Option<String>,
);

fn raw(r: &Row<'_>) -> rusqlite::Result<RawRow> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
        r.get(10)?,
        r.get(11)?,
        r.get(12)?,
        r.get(13)?,
        r.get(14)?,
    ))
}

fn convert(raw: RawRow) -> Result<UpdateHistoryRow> {
    let (
        id,
        tool_id,
        series,
        from_version_id,
        to_version_id,
        from_version,
        to_version,
        policy,
        triggered_by,
        status,
        error,
        job_id,
        rollback_of,
        started_at,
        finished_at,
    ) = raw;
    let corrupt =
        |what: &str| ForgeError::Schema(format!("update_history row {id}: invalid {what}"));
    Ok(UpdateHistoryRow {
        triggered_by: UpdateTrigger::parse(&triggered_by).ok_or_else(|| corrupt("triggered_by"))?,
        status: UpdateStatus::parse(&status).ok_or_else(|| corrupt("status"))?,
        started_at: timestamp(&id, "started_at", &started_at)?,
        finished_at: finished_at
            .map(|t| timestamp(&id, "finished_at", &t))
            .transpose()?,
        id,
        tool_id,
        series,
        from_version_id,
        to_version_id,
        from_version,
        to_version,
        policy,
        error,
        job_id,
        rollback_of,
    })
}

/// Repository over `update_history`.
#[derive(Debug, Clone, Copy)]
pub struct UpdateHistoryRepo<'c> {
    conn: &'c Connection,
}

impl<'c> UpdateHistoryRepo<'c> {
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// Inserts `new` (`started_at` = now; `finished_at` = now unless it is `started`).
    pub fn insert(&self, new: &NewUpdateHistory) -> Result<UpdateId> {
        let id = UpdateId::new();
        let now = Timestamp::now().to_string();
        let finished = (new.status != UpdateStatus::Started).then(|| now.clone());
        self.conn.execute(
            "INSERT INTO update_history \
             (id, tool_id, series, from_version_id, to_version_id, from_version, to_version, \
              policy, triggered_by, status, job_id, rollback_of, started_at, finished_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                id,
                new.tool_id,
                new.series,
                new.from_version_id,
                new.to_version_id,
                new.from_version,
                new.to_version,
                new.policy,
                new.triggered_by.as_str(),
                new.status.as_str(),
                new.job_id,
                new.rollback_of,
                now,
                finished
            ],
        )?;
        Ok(id)
    }

    /// Ends a `started` row: `completed` (with the installed version) or `failed` (with
    /// the error). `NotFound` if the row does not exist or is no longer `started`.
    pub fn finish(
        &self,
        id: UpdateId,
        status: UpdateStatus,
        to_version_id: Option<VersionId>,
        error: Option<&str>,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE update_history SET status = ?2, \
             to_version_id = COALESCE(?3, to_version_id), error = ?4, finished_at = ?5 \
             WHERE id = ?1 AND status = 'started'",
            params![
                id,
                status.as_str(),
                to_version_id,
                error,
                Timestamp::now().to_string()
            ],
        )?;
        if changed == 0 {
            return Err(ForgeError::NotFound(format!(
                "update {id} is not in progress"
            )));
        }
        Ok(())
    }

    /// Records the rollback of the `completed` update `of`: inserts `rollback` (status
    /// `completed`, `rollback_of = of`) and marks `of` as `rolled_back`. Run it in one
    /// writer operation (it is atomic there). `Conflict` if `of` is not `completed`.
    pub fn record_rollback(&self, of: UpdateId, rollback: &NewUpdateHistory) -> Result<UpdateId> {
        let changed = self.conn.execute(
            "UPDATE update_history SET status = 'rolled_back' \
             WHERE id = ?1 AND status = 'completed'",
            [of],
        )?;
        if changed == 0 {
            return Err(ForgeError::Conflict(format!(
                "update {of} is not a completed update"
            )));
        }
        let mut row = rollback.clone();
        row.rollback_of = Some(of);
        row.triggered_by = UpdateTrigger::Rollback;
        row.status = UpdateStatus::Completed;
        self.insert(&row)
    }

    /// One row.
    pub fn get(&self, id: UpdateId) -> Result<Option<UpdateHistoryRow>> {
        let sql = format!("SELECT {COLUMNS} FROM update_history WHERE id = ?1");
        self.conn
            .query_row(&sql, [id], raw)
            .optional()?
            .map(convert)
            .transpose()
    }

    /// Rows of `tool` (every tool when `None`), newest first, at most `limit`.
    pub fn list(&self, tool: Option<&ToolId>, limit: usize) -> Result<Vec<UpdateHistoryRow>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM update_history WHERE (?1 IS NULL OR tool_id = ?1) \
             ORDER BY started_at DESC, rowid DESC LIMIT ?2"
        );
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![tool, limit], raw)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(convert(row?)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{NewToolVersion, Store, ToolVersionRepo, VersionSource, VersionState};

    fn version(store: &Store, name: &str) -> VersionId {
        ToolVersionRepo::new(store.conn())
            .record_install(
                &NewToolVersion {
                    tool_id: ToolId::new("php").unwrap(),
                    tool_name: "PHP".into(),
                    version: name.into(),
                    canonical_name: name.into(),
                    variant: Default::default(),
                    path: format!("C:/fb/bin/php/{name}"),
                    state: VersionState::Installed,
                    source: VersionSource::Registry,
                },
                None,
                None,
            )
            .unwrap()
            .0
    }

    fn update(from: VersionId, to: Option<VersionId>) -> NewUpdateHistory {
        NewUpdateHistory {
            tool_id: ToolId::new("php").unwrap(),
            series: Some("8.3".into()),
            from_version_id: Some(from),
            to_version_id: to,
            from_version: Some("8.3.36".into()),
            to_version: "8.3.37".into(),
            policy: Some("auto-patch".into()),
            triggered_by: UpdateTrigger::Auto,
            status: UpdateStatus::Started,
            job_id: Some(JobId::new()),
            rollback_of: None,
        }
    }

    #[test]
    fn update_then_rollback_roundtrips() {
        let store = Store::open_in_memory().unwrap();
        let (v36, v37) = (version(&store, "8.3.36"), version(&store, "8.3.37"));
        let repo = UpdateHistoryRepo::new(store.conn());
        let id = repo.insert(&update(v36, None)).unwrap();
        let started = repo.get(id).unwrap().unwrap();
        assert_eq!(started.status, UpdateStatus::Started);
        assert!(started.finished_at.is_none());
        repo.finish(id, UpdateStatus::Completed, Some(v37), None)
            .unwrap();
        // Finishing twice is refused.
        assert_eq!(
            repo.finish(id, UpdateStatus::Failed, None, Some("x"))
                .unwrap_err()
                .code(),
            "not_found"
        );
        let done = repo.get(id).unwrap().unwrap();
        assert_eq!(
            (done.status, done.to_version_id),
            (UpdateStatus::Completed, Some(v37))
        );
        assert!(done.finished_at.is_some());

        let back = NewUpdateHistory {
            from_version_id: Some(v37),
            to_version_id: Some(v36),
            from_version: Some("8.3.37".into()),
            to_version: "8.3.36".into(),
            ..update(v37, Some(v36))
        };
        let rollback = repo.record_rollback(id, &back).unwrap();
        let rows = repo.list(Some(&ToolId::new("php").unwrap()), 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, rollback, "newest first");
        assert_eq!(rows[0].rollback_of, Some(id));
        assert_eq!(rows[0].triggered_by, UpdateTrigger::Rollback);
        assert_eq!(rows[0].status, UpdateStatus::Completed);
        assert_eq!(rows[1].status, UpdateStatus::RolledBack);
        // A rolled back update cannot be rolled back again.
        assert_eq!(
            repo.record_rollback(id, &back).unwrap_err().code(),
            "conflict"
        );
    }

    #[test]
    fn removing_a_version_keeps_the_history_text() {
        let store = Store::open_in_memory().unwrap();
        let v36 = version(&store, "8.3.36");
        let repo = UpdateHistoryRepo::new(store.conn());
        let id = repo.insert(&update(v36, None)).unwrap();
        store
            .conn()
            .execute("DELETE FROM tool_versions WHERE id = ?1", [v36])
            .unwrap();
        let row = repo.get(id).unwrap().unwrap();
        assert_eq!(row.from_version_id, None);
        assert_eq!(row.from_version.as_deref(), Some("8.3.36"));
    }
}
