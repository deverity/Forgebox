//! `events` table: persisted [`Event`]s, read back by `seq` (ADR-0009 `resume_from`).

use rusqlite::{Connection, params};

use crate::error::{ForgeError, Result};
use crate::event::Event;

/// Repository over the `events` table.
#[derive(Debug, Clone, Copy)]
pub struct EventRepo<'c> {
    conn: &'c Connection,
}

fn to_i64(seq: u64) -> Result<i64> {
    i64::try_from(seq).map_err(|_| ForgeError::InvalidInput(format!("seq {seq} is too large")))
}

impl<'c> EventRepo<'c> {
    /// Repository on `conn` (maintenance connection, a writer operation or a reader).
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// Stores `event`. Fails on a duplicate id or `seq`.
    pub fn insert(&self, event: &Event) -> Result<()> {
        let payload = serde_json::to_string(&event.payload)
            .map_err(|e| ForgeError::InvalidInput(format!("event payload: {e}")))?;
        self.conn.execute(
            "INSERT INTO events (id, seq, ts, kind, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.id,
                to_i64(event.seq)?,
                event.ts.to_string(),
                event.kind.as_str(),
                payload
            ],
        )?;
        Ok(())
    }

    /// Up to `limit` events with `seq > after_seq`, ascending.
    pub fn list_since(&self, after_seq: u64, limit: usize) -> Result<Vec<Event>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT id, seq, ts, kind, payload FROM events \
             WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![to_i64(after_seq)?, limit], |row| {
            Ok((
                row.get(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (id, seq, ts, kind, payload) = row?;
            let corrupt = |what: &str, e: String| {
                ForgeError::Schema(format!("events row seq {seq}: invalid {what}: {e}"))
            };
            events.push(Event {
                id,
                seq: u64::try_from(seq).map_err(|e| corrupt("seq", e.to_string()))?,
                ts: ts
                    .parse()
                    .map_err(|e: jiff::Error| corrupt("ts", e.to_string()))?,
                kind: kind
                    .parse()
                    .map_err(|e: crate::event::UnknownEventKind| corrupt("kind", e.to_string()))?,
                payload: serde_json::from_str(&payload)
                    .map_err(|e| corrupt("payload", e.to_string()))?,
            });
        }
        Ok(events)
    }

    /// Highest stored `seq` (0 when empty). Seeds `EventBus::starting_at`.
    pub fn max_seq(&self) -> Result<u64> {
        let max: i64 =
            self.conn
                .query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |r| r.get(0))?;
        Ok(u64::try_from(max).unwrap_or(0))
    }

    /// Number of stored events.
    pub fn count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Keeps only the newest `keep` events (ring buffer, docs/01 §6). Returns how many
    /// rows were deleted.
    pub fn prune_keep_last(&self, keep: u64) -> Result<usize> {
        let cutoff = self.max_seq()?.saturating_sub(keep);
        Ok(self
            .conn
            .execute("DELETE FROM events WHERE seq <= ?1", [to_i64(cutoff)?])?)
    }
}
