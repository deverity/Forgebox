-- 0001_init: initial schema of state/forgebox.db (docs/01 §4, docs/06 §1, ADR-0007).
-- Published migrations are never edited (docs/02 §5); add 0002_*.sql instead.
-- Conventions: ids are ULID TEXT (ToolId slug for tools); timestamps are RFC 3339 UTC
-- TEXT; booleans are INTEGER 0/1; JSON columns are TEXT checked with json_valid();
-- paths are attributes (never keys) and use '/' separators.

CREATE TABLE schema_migrations (
    version     INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    checksum    TEXT NOT NULL,
    applied_at  TEXT NOT NULL
) STRICT;

-- Manifest registries (official, third party, local dirs).
CREATE TABLE registries (
    id               TEXT PRIMARY KEY,
    name             TEXT NOT NULL UNIQUE,
    url              TEXT NOT NULL,
    kind             TEXT NOT NULL CHECK (kind IN ('git', 'http', 'dir')),
    official         INTEGER NOT NULL DEFAULT 0 CHECK (official IN (0, 1)),
    enabled          INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    priority         INTEGER NOT NULL DEFAULT 100,
    etag             TEXT,
    last_refresh_at  TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
) STRICT;

-- Tools known locally (manifest cache entry or local manifest). id = ToolId slug.
CREATE TABLE tools (
    id               TEXT PRIMARY KEY CHECK (id GLOB '[a-z0-9]*' AND id NOT GLOB '*[^a-z0-9-]*'),
    registry_id      TEXT REFERENCES registries (id) ON DELETE SET NULL,
    name             TEXT NOT NULL,
    category         TEXT,
    manifest_sha256  TEXT,
    update_policy    TEXT CHECK (update_policy IN ('manual', 'notify', 'auto-patch', 'auto-minor', 'auto')),
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
) STRICT;

-- Installed versions: bin/<tool>/<canonical_name> (docs/08 §5).
CREATE TABLE tool_versions (
    id              TEXT PRIMARY KEY,
    tool_id         TEXT NOT NULL REFERENCES tools (id) ON DELETE RESTRICT,
    version         TEXT NOT NULL,
    canonical_name  TEXT NOT NULL,
    variant         TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(variant)),
    path            TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN ('downloading', 'installed', 'broken', 'removing')),
    source          TEXT NOT NULL CHECK (source IN ('registry', 'manual', 'import')),
    sha256          TEXT,
    size_bytes      INTEGER,
    installed_at    TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    UNIQUE (tool_id, canonical_name)
) STRICT;

CREATE INDEX tool_versions_tool ON tool_versions (tool_id);

-- Service instances of a version (mysql 8.4 "default", "legacy"...).
CREATE TABLE instances (
    id             TEXT PRIMARY KEY,
    tool_id        TEXT NOT NULL REFERENCES tools (id) ON DELETE RESTRICT,
    version_id     TEXT NOT NULL REFERENCES tool_versions (id) ON DELETE RESTRICT,
    name           TEXT NOT NULL,
    autostart      INTEGER NOT NULL DEFAULT 0 CHECK (autostart IN (0, 1)),
    desired_state  TEXT NOT NULL DEFAULT 'stopped' CHECK (desired_state IN ('stopped', 'running')),
    data_dir       TEXT,
    settings       TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(settings)),
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    UNIQUE (tool_id, name)
) STRICT;

CREATE INDEX instances_version ON instances (version_id);

-- Sites (docs/06 §1). Hosts are always lowercase (docs/15 §7).
CREATE TABLE sites (
    id                  TEXT PRIMARY KEY,
    slug                TEXT NOT NULL UNIQUE,
    host                TEXT NOT NULL UNIQUE CHECK (host = lower(host)),
    aliases             TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(aliases)),
    kind                TEXT NOT NULL CHECK (kind IN ('php', 'static', 'proxy', 'app')),
    root                TEXT,
    driver              TEXT,
    server              TEXT,
    php_series          TEXT,
    php_exact           TEXT,
    php_pool_overrides  TEXT CHECK (php_pool_overrides IS NULL OR json_valid(php_pool_overrides)),
    upstream            TEXT,
    secure              INTEGER NOT NULL DEFAULT 1 CHECK (secure IN (0, 1)),
    http_redirect       INTEGER NOT NULL DEFAULT 1 CHECK (http_redirect IN (0, 1)),
    env                 TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(env)),
    xdebug              TEXT NOT NULL DEFAULT 'inherit' CHECK (xdebug IN ('inherit', 'off', 'trigger', 'always')),
    parked              INTEGER NOT NULL DEFAULT 0 CHECK (parked IN (0, 1)),
    enabled             INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    case_sensitive      INTEGER NOT NULL DEFAULT 1 CHECK (case_sensitive IN (0, 1)),
    processes           TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(processes)),
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
) STRICT;

-- Logical port reservations (docs/03 §4): a port is never "guessed" twice.
CREATE TABLE ports (
    id               TEXT PRIMARY KEY,
    port             INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    protocol         TEXT NOT NULL DEFAULT 'tcp' CHECK (protocol IN ('tcp', 'udp')),
    range_name       TEXT,
    owner_kind       TEXT NOT NULL CHECK (owner_kind IN ('instance', 'pool', 'app', 'site', 'daemon')),
    owner_id         TEXT NOT NULL,
    owner_port_name  TEXT NOT NULL DEFAULT 'main',
    state            TEXT NOT NULL DEFAULT 'reserved' CHECK (state IN ('reserved', 'bound', 'conflict')),
    conflict_pid     INTEGER,
    conflict_exe     TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    UNIQUE (protocol, port),
    UNIQUE (owner_kind, owner_id, owner_port_name)
) STRICT;

-- Persisted events (docs/01 §6, ring buffer pruned by the daemon). seq is global.
CREATE TABLE events (
    id       TEXT PRIMARY KEY,
    seq      INTEGER NOT NULL UNIQUE CHECK (seq > 0),
    ts       TEXT NOT NULL,
    kind     TEXT NOT NULL,
    payload  TEXT NOT NULL DEFAULT 'null' CHECK (json_valid(payload))
) STRICT;

CREATE INDEX events_kind ON events (kind, seq);

-- Update history with rollback data (docs/05 §4). Version strings are kept as text so
-- the history survives the removal of old versions.
CREATE TABLE update_history (
    id               TEXT PRIMARY KEY,
    tool_id          TEXT NOT NULL REFERENCES tools (id) ON DELETE CASCADE,
    from_version_id  TEXT REFERENCES tool_versions (id) ON DELETE SET NULL,
    to_version_id    TEXT REFERENCES tool_versions (id) ON DELETE SET NULL,
    from_version     TEXT,
    to_version       TEXT NOT NULL,
    policy           TEXT,
    triggered_by     TEXT NOT NULL CHECK (triggered_by IN ('auto', 'user', 'rollback')),
    status           TEXT NOT NULL CHECK (status IN ('started', 'completed', 'failed', 'rolled_back')),
    error            TEXT,
    job_id           TEXT,
    started_at       TEXT NOT NULL,
    finished_at      TEXT
) STRICT;

CREATE INDEX update_history_tool ON update_history (tool_id, started_at);
