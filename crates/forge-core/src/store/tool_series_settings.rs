//! `tool_series_settings` table: quick settings of a tool series (`php@8.4`), the source of
//! truth of the settings of a PHP pool (docs/04 §1.3, ADR-0042). Instances keep theirs in
//! `instances.settings` ([`super::InstanceRepo::update_settings`]).
//!
//! A row is one stored setting with its canonical text (`1G`, `true`, `120`); a key
//! without a row means "the manifest's default". The daemon validates values against the
//! manifest before writing; this repository only checks the key shape (the same rule as
//! `config.quick_settings[].key`).

use std::collections::BTreeMap;

use jiff::Timestamp;
use rusqlite::{Connection, params};

use crate::error::{ForgeError, Result};
use crate::ids::ToolId;

/// Whether `key` has the shape of a quick setting key (`[a-z][a-z0-9_]*`).
pub fn is_setting_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Repository over `tool_series_settings`.
#[derive(Debug, Clone, Copy)]
pub struct ToolSeriesSettingsRepo<'c> {
    conn: &'c Connection,
}

impl<'c> ToolSeriesSettingsRepo<'c> {
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// The stored settings of `tool`@`series` (empty when nothing is stored).
    pub fn get(&self, tool: &ToolId, series: &str) -> Result<BTreeMap<String, String>> {
        let mut stmt = self.conn.prepare(
            "SELECT key, value FROM tool_series_settings WHERE tool_id = ?1 AND series = ?2",
        )?;
        let rows = stmt
            .query_map(params![tool, series], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<BTreeMap<String, String>>>()?;
        Ok(rows)
    }

    /// Every stored series of `tool` with its settings.
    pub fn list_tool(&self, tool: &ToolId) -> Result<BTreeMap<String, BTreeMap<String, String>>> {
        let mut stmt = self.conn.prepare(
            "SELECT series, key, value FROM tool_series_settings WHERE tool_id = ?1 \
             ORDER BY series, key",
        )?;
        let rows = stmt
            .query_map(params![tool], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<Vec<(String, String, String)>>>()?;
        let mut out: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for (series, key, value) in rows {
            out.entry(series).or_default().insert(key, value);
        }
        Ok(out)
    }

    /// Applies `changes` (`Some` = store this text, `None` = forget the key, back to the
    /// default) and returns the settings now stored. Every key is checked first; nothing
    /// is written when one is malformed. Run it in one writer operation (one `SAVEPOINT`)
    /// so the change is all or nothing.
    pub fn apply(
        &self,
        tool: &ToolId,
        series: &str,
        changes: &BTreeMap<String, Option<String>>,
    ) -> Result<BTreeMap<String, String>> {
        if series.trim().is_empty() {
            return Err(ForgeError::InvalidInput("empty series".into()));
        }
        if let Some(bad) = changes.keys().find(|k| !is_setting_key(k)) {
            return Err(ForgeError::InvalidInput(format!(
                "invalid setting key {bad:?} ([a-z][a-z0-9_]*)"
            )));
        }
        let now = Timestamp::now().to_string();
        for (key, value) in changes {
            match value {
                Some(value) => {
                    self.conn.execute(
                        "INSERT INTO tool_series_settings (tool_id, series, key, value, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5) \
                         ON CONFLICT (tool_id, series, key) \
                         DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                        params![tool, series, key, value, now],
                    )?;
                }
                None => {
                    self.conn.execute(
                        "DELETE FROM tool_series_settings \
                         WHERE tool_id = ?1 AND series = ?2 AND key = ?3",
                        params![tool, series, key],
                    )?;
                }
            }
        }
        self.get(tool, series)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn store_with_tool() -> (Store, ToolId) {
        let store = Store::open_in_memory().unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO tools (id, name, created_at, updated_at) \
                 VALUES ('php', 'PHP', 'now', 'now')",
                [],
            )
            .unwrap();
        (store, ToolId::new("php").unwrap())
    }

    #[test]
    fn apply_sets_updates_and_forgets_keys_per_series() {
        let (store, php) = store_with_tool();
        let repo = ToolSeriesSettingsRepo::new(store.conn());
        assert!(repo.get(&php, "8.4").unwrap().is_empty());
        let now = repo
            .apply(
                &php,
                "8.4",
                &BTreeMap::from([
                    ("memory_limit".to_owned(), Some("1G".to_owned())),
                    ("upload_max_filesize".to_owned(), Some("256M".to_owned())),
                ]),
            )
            .unwrap();
        assert_eq!(now["memory_limit"], "1G");
        assert_eq!(now.len(), 2);
        repo.apply(
            &php,
            "8.3",
            &BTreeMap::from([("memory_limit".to_owned(), Some("256M".to_owned()))]),
        )
        .unwrap();
        let now = repo
            .apply(
                &php,
                "8.4",
                &BTreeMap::from([
                    ("memory_limit".to_owned(), Some("-1".to_owned())),
                    ("upload_max_filesize".to_owned(), None),
                ]),
            )
            .unwrap();
        assert_eq!(
            now,
            BTreeMap::from([("memory_limit".to_owned(), "-1".to_owned())])
        );
        assert_eq!(repo.get(&php, "8.3").unwrap()["memory_limit"], "256M");
        let all = repo.list_tool(&php).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all["8.4"]["memory_limit"], "-1");
    }

    #[test]
    fn malformed_keys_write_nothing() {
        let (store, php) = store_with_tool();
        let repo = ToolSeriesSettingsRepo::new(store.conn());
        let err = repo
            .apply(
                &php,
                "8.4",
                &BTreeMap::from([
                    ("memory_limit".to_owned(), Some("1G".to_owned())),
                    ("forgebox.secret".to_owned(), Some("x".to_owned())),
                ]),
            )
            .unwrap_err();
        assert_eq!(err.code(), "invalid_input");
        assert!(repo.get(&php, "8.4").unwrap().is_empty());
        assert!(repo.apply(&php, " ", &BTreeMap::new()).is_err());
        for (key, ok) in [
            ("memory_limit", true),
            ("a1_b", true),
            ("1a", false),
            ("A", false),
            ("ports.main", false),
            ("", false),
        ] {
            assert_eq!(is_setting_key(key), ok, "{key}");
        }
    }

    #[test]
    fn unknown_tool_is_refused_by_the_foreign_key() {
        let store = Store::open_in_memory().unwrap();
        let repo = ToolSeriesSettingsRepo::new(store.conn());
        let err = repo
            .apply(
                &ToolId::new("ghost").unwrap(),
                "1.0",
                &BTreeMap::from([("a".to_owned(), Some("b".to_owned()))]),
            )
            .unwrap_err();
        assert_eq!(err.code(), "db");
    }
}
