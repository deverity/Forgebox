-- 0005_metrics: persisted per-site metrics (M2-E5-T7, docs/14 section 3.5, ADR-0044 section 3),
-- per-site pool mode and metrics switch (M2-E5-T11 flips them), index for the chart markers.
-- Published migrations are never edited.

-- One row per (site, closed minute), kept 7 days. Counts and sums are the totals of the
-- minute; rt_hist/ut_hist are HDR V2 blobs (no DEFLATE, NULL when empty) in the layout named
-- by format_version, so a later codec can coexist with old rows. Times are Unix seconds UTC.
-- No foreign key to sites: a flush must never fail because a site was just removed; rows of
-- removed sites age out with the retention.
CREATE TABLE metrics_1m (
    site_id         TEXT NOT NULL,
    start           INTEGER NOT NULL CHECK (start >= 0 AND start % 60 = 0),
    requests        INTEGER NOT NULL DEFAULT 0,
    status_1xx      INTEGER NOT NULL DEFAULT 0,
    status_2xx      INTEGER NOT NULL DEFAULT 0,
    status_3xx      INTEGER NOT NULL DEFAULT 0,
    status_4xx      INTEGER NOT NULL DEFAULT 0,
    status_5xx      INTEGER NOT NULL DEFAULT 0,
    bytes           INTEGER NOT NULL DEFAULT 0,
    rt_sum_us       INTEGER NOT NULL DEFAULT 0,
    ut_sum_us       INTEGER NOT NULL DEFAULT 0,
    ut_count        INTEGER NOT NULL DEFAULT 0,
    sampled         INTEGER NOT NULL DEFAULT 0 CHECK (sampled IN (0, 1)),
    rt_hist         BLOB,
    ut_hist         BLOB,
    format_version  INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (site_id, start)
) STRICT, WITHOUT ROWID;

-- Same shape per hour (start % 3600 = 0), rolled up from metrics_1m, kept 90 days.
CREATE TABLE metrics_1h (
    site_id         TEXT NOT NULL,
    start           INTEGER NOT NULL CHECK (start >= 0 AND start % 3600 = 0),
    requests        INTEGER NOT NULL DEFAULT 0,
    status_1xx      INTEGER NOT NULL DEFAULT 0,
    status_2xx      INTEGER NOT NULL DEFAULT 0,
    status_3xx      INTEGER NOT NULL DEFAULT 0,
    status_4xx      INTEGER NOT NULL DEFAULT 0,
    status_5xx      INTEGER NOT NULL DEFAULT 0,
    bytes           INTEGER NOT NULL DEFAULT 0,
    rt_sum_us       INTEGER NOT NULL DEFAULT 0,
    ut_sum_us       INTEGER NOT NULL DEFAULT 0,
    ut_count        INTEGER NOT NULL DEFAULT 0,
    sampled         INTEGER NOT NULL DEFAULT 0 CHECK (sampled IN (0, 1)),
    rt_hist         BLOB,
    ut_hist         BLOB,
    format_version  INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (site_id, start)
) STRICT, WITHOUT ROWID;

CREATE INDEX metrics_1m_start ON metrics_1m (start);
CREATE INDEX metrics_1h_start ON metrics_1h (start);

-- Position of the access log tailers (docs/14 section 3.2), written in the same transaction
-- as the minutes they cover. The u64 columns hold the bit pattern as i64.
CREATE TABLE metrics_tail (
    path_key       TEXT PRIMARY KEY,
    creation_time  INTEGER NOT NULL,
    "offset"       INTEGER NOT NULL CHECK ("offset" >= 0),
    head_len       INTEGER NOT NULL CHECK (head_len BETWEEN 0 AND 256),
    head_hash      INTEGER NOT NULL,
    updated_at     TEXT NOT NULL
) STRICT, WITHOUT ROWID;

ALTER TABLE sites ADD COLUMN pool_mode TEXT NOT NULL DEFAULT 'shared'
    CHECK (pool_mode IN ('shared', 'dedicated'));
ALTER TABLE sites ADD COLUMN metrics_enabled INTEGER NOT NULL DEFAULT 1
    CHECK (metrics_enabled IN (0, 1));

-- Chart markers read events by time window.
CREATE INDEX events_ts ON events (ts);
