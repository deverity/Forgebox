-- 0004_tool_series_settings: quick settings of a tool series (M2-E2-T7, docs/04 §1.3,
-- ADR-0042). Instances keep theirs in `instances.settings`; a PHP pool has no row, its
-- settings belong to (tool, series) (`php@8.4`) and survive patch updates and the removal
-- of the last version of the series. Published migrations are never edited.

-- One row per stored setting: `value` is the canonical text of `services.set` (`1G`,
-- `true`, `120`, `debug,develop`); a missing row means "the manifest's default".
CREATE TABLE tool_series_settings (
    tool_id     TEXT NOT NULL REFERENCES tools (id) ON DELETE CASCADE,
    series      TEXT NOT NULL CHECK (series <> ''),
    key         TEXT NOT NULL CHECK (key GLOB '[a-z]*' AND key NOT GLOB '*[^a-z0-9_]*'),
    value       TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (tool_id, series, key)
) STRICT, WITHOUT ROWID;
