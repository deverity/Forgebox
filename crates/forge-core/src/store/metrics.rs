//! `metrics_1m`, `metrics_1h` and `metrics_tail` tables (M2-E5-T7, docs/14 section 3.5).
//!
//! The repository is plain storage of rows: it does not know the histogram codec (that is
//! `forge-metrics`, which depends on this crate). A histogram is an opaque blob plus the
//! `format_version` of the codec that wrote it. Writes go through a [`super::StoreWriter`]
//! operation (a [`MetricsRepo`] on its connection), reads through [`super::StoreReader`].
//!
//! Times are Unix seconds UTC. Counters are `u64` in the API and `i64` in SQLite (a count
//! above `i64::MAX` is refused); the tail columns that really are `u64` bit patterns
//! (FILETIME, FNV hash) are stored with `cast_signed` and read back with `cast_unsigned`.

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::{ForgeError, Result};
use crate::ids::SiteId;

/// One row of `metrics_1m` / `metrics_1h`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsRow {
    pub site: SiteId,
    /// Start of the minute/hour, Unix seconds.
    pub start: i64,
    pub requests: u64,
    /// Requests by status class: 1xx, 2xx, 3xx, 4xx, 5xx.
    pub status: [u64; 5],
    pub bytes: u64,
    pub rt_sum_us: u64,
    pub ut_sum_us: u64,
    pub ut_count: u64,
    pub sampled: bool,
    /// Total-duration histogram, empty when there is none.
    pub rt_hist: Vec<u8>,
    /// Upstream-duration histogram, empty when there is none.
    pub ut_hist: Vec<u8>,
    /// Codec of the histograms.
    pub format_version: u32,
}

/// The persisted position of one access log tailer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailRow {
    pub creation_time: u64,
    pub offset: u64,
    pub head_len: u16,
    pub head_hash: u64,
}

/// Which table of rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Minute,
    Hour,
}

impl Tier {
    const fn table(self) -> &'static str {
        match self {
            Self::Minute => "metrics_1m",
            Self::Hour => "metrics_1h",
        }
    }
}

const COLS: &str = "site_id, start, requests, status_1xx, status_2xx, status_3xx, status_4xx, \
    status_5xx, bytes, rt_sum_us, ut_sum_us, ut_count, sampled, rt_hist, ut_hist, \
    format_version";

fn to_i64(n: u64, what: &str) -> Result<i64> {
    i64::try_from(n).map_err(|_| ForgeError::InvalidInput(format!("{what} {n} is too large")))
}

fn read_u64(row: &Row<'_>, i: usize) -> rusqlite::Result<u64> {
    Ok(row.get::<_, i64>(i)?.max(0).cast_unsigned())
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<MetricsRow> {
    Ok(MetricsRow {
        site: row.get(0)?,
        start: row.get(1)?,
        requests: read_u64(row, 2)?,
        status: [
            read_u64(row, 3)?,
            read_u64(row, 4)?,
            read_u64(row, 5)?,
            read_u64(row, 6)?,
            read_u64(row, 7)?,
        ],
        bytes: read_u64(row, 8)?,
        rt_sum_us: read_u64(row, 9)?,
        ut_sum_us: read_u64(row, 10)?,
        ut_count: read_u64(row, 11)?,
        sampled: row.get(12)?,
        rt_hist: row.get::<_, Option<Vec<u8>>>(13)?.unwrap_or_default(),
        ut_hist: row.get::<_, Option<Vec<u8>>>(14)?.unwrap_or_default(),
        format_version: row.get(15)?,
    })
}

fn blob(bytes: &[u8]) -> Option<&[u8]> {
    (!bytes.is_empty()).then_some(bytes)
}

fn tail_from_row(row: &Row<'_>) -> rusqlite::Result<TailRow> {
    Ok(TailRow {
        creation_time: row.get::<_, i64>(0)?.cast_unsigned(),
        offset: read_u64(row, 1)?,
        head_len: row.get(2)?,
        head_hash: row.get::<_, i64>(3)?.cast_unsigned(),
    })
}

/// Repository over the metrics tables.
#[derive(Debug, Clone, Copy)]
pub struct MetricsRepo<'c> {
    conn: &'c Connection,
}

impl<'c> MetricsRepo<'c> {
    /// Repository on `conn` (writer operation, reader or maintenance connection).
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// Inserts or replaces the row of `(site, start)`. `start` must be aligned to the tier.
    pub fn put(&self, tier: Tier, row: &MetricsRow) -> Result<()> {
        let sql = format!(
            "INSERT OR REPLACE INTO {} ({COLS}) VALUES \
             (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            tier.table()
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        stmt.execute(params![
            row.site,
            row.start,
            to_i64(row.requests, "requests")?,
            to_i64(row.status[0], "status_1xx")?,
            to_i64(row.status[1], "status_2xx")?,
            to_i64(row.status[2], "status_3xx")?,
            to_i64(row.status[3], "status_4xx")?,
            to_i64(row.status[4], "status_5xx")?,
            to_i64(row.bytes, "bytes")?,
            to_i64(row.rt_sum_us, "rt_sum_us")?,
            to_i64(row.ut_sum_us, "ut_sum_us")?,
            to_i64(row.ut_count, "ut_count")?,
            row.sampled,
            blob(&row.rt_hist),
            blob(&row.ut_hist),
            row.format_version,
        ])?;
        Ok(())
    }

    /// The row of `(site, start)`.
    pub fn get(&self, tier: Tier, site: SiteId, start: i64) -> Result<Option<MetricsRow>> {
        let sql = format!(
            "SELECT {COLS} FROM {} WHERE site_id = ?1 AND start = ?2",
            tier.table()
        );
        Ok(self
            .conn
            .prepare_cached(&sql)?
            .query_row(params![site, start], from_row)
            .optional()?)
    }

    /// Rows with `from <= start < to`, by `(start, site)`; `site` restricts to one site.
    pub fn range(
        &self,
        tier: Tier,
        site: Option<SiteId>,
        from: i64,
        to: i64,
    ) -> Result<Vec<MetricsRow>> {
        let sql = format!(
            "SELECT {COLS} FROM {} WHERE start >= ?1 AND start < ?2 \
             AND (?3 IS NULL OR site_id = ?3) ORDER BY start, site_id",
            tier.table()
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![from, to, site], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Number of rows of the tier.
    pub fn count(&self, tier: Tier) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            &format!("SELECT count(*) FROM {}", tier.table()),
            [],
            |r| r.get(0),
        )?;
        Ok(n.cast_unsigned())
    }

    /// Largest `start` of the tier.
    pub fn newest(&self, tier: Tier) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            &format!("SELECT max(start) FROM {}", tier.table()),
            [],
            |r| r.get(0),
        )?)
    }

    /// Smallest `start` of the tier.
    pub fn oldest(&self, tier: Tier) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            &format!("SELECT min(start) FROM {}", tier.table()),
            [],
            |r| r.get(0),
        )?)
    }

    /// Deletes rows with `start < before`; returns how many.
    pub fn prune(&self, tier: Tier, before: i64) -> Result<usize> {
        Ok(self.conn.execute(
            &format!("DELETE FROM {} WHERE start < ?1", tier.table()),
            [before],
        )?)
    }

    /// Deletes every row of a removed site from both tiers.
    pub fn delete_site(&self, site: SiteId) -> Result<usize> {
        let mut n = 0;
        for tier in [Tier::Minute, Tier::Hour] {
            n += self.conn.execute(
                &format!("DELETE FROM {} WHERE site_id = ?1", tier.table()),
                [site],
            )?;
        }
        Ok(n)
    }

    /// Stores the position of the tailer `path_key`.
    pub fn put_tail(&self, path_key: &str, tail: &TailRow, updated_at: &str) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT INTO metrics_tail (path_key, creation_time, \"offset\", head_len, \
                 head_hash, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT (path_key) DO UPDATE SET creation_time = excluded.creation_time, \
                 \"offset\" = excluded.\"offset\", head_len = excluded.head_len, \
                 head_hash = excluded.head_hash, updated_at = excluded.updated_at",
            )?
            .execute(params![
                path_key,
                tail.creation_time.cast_signed(),
                to_i64(tail.offset, "offset")?,
                tail.head_len,
                tail.head_hash.cast_signed(),
                updated_at,
            ])?;
        Ok(())
    }

    /// The position stored for `path_key`.
    pub fn tail(&self, path_key: &str) -> Result<Option<TailRow>> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT creation_time, \"offset\", head_len, head_hash FROM metrics_tail \
                 WHERE path_key = ?1",
            )?
            .query_row([path_key], tail_from_row)
            .optional()?)
    }

    /// Every stored tailer position.
    pub fn tails(&self) -> Result<Vec<(String, TailRow)>> {
        let mut stmt = self.conn.prepare(
            "SELECT creation_time, \"offset\", head_len, head_hash, path_key FROM metrics_tail \
             ORDER BY path_key",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(4)?, tail_from_row(r)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Forgets the position of `path_key`.
    pub fn delete_tail(&self, path_key: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM metrics_tail WHERE path_key = ?1", [path_key])?
            > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn row(site: SiteId, start: i64, n: u64) -> MetricsRow {
        MetricsRow {
            site,
            start,
            requests: n,
            status: [0, n, 0, 0, 0],
            bytes: n * 10,
            rt_sum_us: n * 1000,
            ut_sum_us: 0,
            ut_count: 0,
            sampled: false,
            rt_hist: vec![1, 2, 3],
            ut_hist: Vec::new(),
            format_version: 1,
        }
    }

    #[test]
    fn rows_round_trip_range_and_prune() {
        let store = Store::open_in_memory().unwrap();
        let repo = MetricsRepo::new(store.conn());
        let (a, b) = (SiteId::new(), SiteId::new());
        for i in 0..5_i64 {
            repo.put(Tier::Minute, &row(a, i * 60, 7 + i.cast_unsigned()))
                .unwrap();
        }
        repo.put(Tier::Minute, &row(b, 60, 1)).unwrap();
        assert_eq!(repo.count(Tier::Minute).unwrap(), 6);
        let got = repo.get(Tier::Minute, a, 120).unwrap().unwrap();
        assert_eq!(got, row(a, 120, 9));
        assert!(got.ut_hist.is_empty());
        assert_eq!(repo.range(Tier::Minute, Some(a), 60, 240).unwrap().len(), 3);
        assert_eq!(repo.range(Tier::Minute, None, 0, 300).unwrap().len(), 6);
        assert_eq!(repo.oldest(Tier::Minute).unwrap(), Some(0));
        assert_eq!(repo.newest(Tier::Minute).unwrap(), Some(240));
        assert_eq!(repo.prune(Tier::Minute, 120).unwrap(), 3);
        assert_eq!(repo.count(Tier::Minute).unwrap(), 3);
        assert_eq!(repo.delete_site(a).unwrap(), 3);
        assert_eq!(repo.count(Tier::Minute).unwrap(), 0);
        assert_eq!(repo.newest(Tier::Hour).unwrap(), None);
    }

    #[test]
    fn misaligned_starts_are_refused_by_the_table() {
        let store = Store::open_in_memory().unwrap();
        let repo = MetricsRepo::new(store.conn());
        let err = repo
            .put(Tier::Minute, &row(SiteId::new(), 61, 1))
            .unwrap_err();
        assert_eq!(err.code(), "db");
        assert!(repo.put(Tier::Hour, &row(SiteId::new(), 60, 1)).is_err());
        repo.put(Tier::Hour, &row(SiteId::new(), 3600, 1)).unwrap();
    }

    #[test]
    fn tail_positions_keep_the_full_u64_range() {
        let store = Store::open_in_memory().unwrap();
        let repo = MetricsRepo::new(store.conn());
        let tail = TailRow {
            creation_time: 0xfedc_ba98_7654_3210,
            offset: 12_345,
            head_len: 256,
            head_hash: u64::MAX - 5,
        };
        repo.put_tail("c:/x/access.log", &tail, "t").unwrap();
        assert_eq!(repo.tail("c:/x/access.log").unwrap(), Some(tail));
        let newer = TailRow { offset: 99, ..tail };
        repo.put_tail("c:/x/access.log", &newer, "t2").unwrap();
        assert_eq!(
            repo.tails().unwrap(),
            vec![("c:/x/access.log".to_owned(), newer)]
        );
        assert!(repo.delete_tail("c:/x/access.log").unwrap());
        assert_eq!(repo.tail("c:/x/access.log").unwrap(), None);
    }
}
