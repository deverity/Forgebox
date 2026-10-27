-- 0003_update_history_rollback: what the update engine (forge-provider, M1-E2-T5,
-- docs/05 §4) needs to find and undo an update. Published migrations are never edited.

-- Series of the replaced version under the manifest's series_key (`8.3`): rollback is per
-- (tool, series).
ALTER TABLE update_history ADD COLUMN series TEXT;
-- On a `triggered_by = 'rollback'` row: the update it undid (that row becomes
-- `rolled_back`).
ALTER TABLE update_history ADD COLUMN rollback_of TEXT REFERENCES update_history (id) ON DELETE SET NULL;

CREATE INDEX update_history_series ON update_history (tool_id, series, started_at);
