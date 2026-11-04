//! Store integration tests: migrations on real files, WAL, foreign keys, events repo.

use forge_core::event::{EventBus, EventKind};
use forge_core::ids::{SiteId, VersionId};
use forge_core::store::migrations::{self, Migration, latest_version};
use forge_core::store::{MIGRATIONS, Store};
use serde_json::json;

const TABLES: &[&str] = &[
    "schema_migrations",
    "registries",
    "tools",
    "tool_versions",
    "instances",
    "sites",
    "ports",
    "events",
    "update_history",
    "tool_series_settings",
];

fn tables(store: &Store) -> Vec<String> {
    let mut stmt = store
        .conn()
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn new_database_gets_every_table() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state").join("forgebox.db");
    let store = Store::open(&path).unwrap();
    let found = tables(&store);
    for table in TABLES {
        assert!(
            found.iter().any(|t| t == table),
            "missing {table}: {found:?}"
        );
    }
    let report = store.migration_report();
    assert_eq!(report.from_version, 0);
    assert_eq!(report.to_version, latest_version());
    assert_eq!(report.applied, (1..=latest_version()).collect::<Vec<_>>());
    assert_eq!(report.backup, None, "nothing to back up for a new file");
    assert_eq!(store.schema_version().unwrap(), latest_version());
    assert_eq!(store.path(), Some(path.as_path()));
}

#[test]
fn reopening_does_not_reapply() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forgebox.db");
    drop(Store::open(&path).unwrap());
    let store = Store::open(&path).unwrap();
    let report = store.migration_report();
    assert!(report.applied.is_empty());
    assert_eq!(report.from_version, latest_version());
    assert_eq!(report.backup, None, "no backup without pending migrations");
    let rows: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, MIGRATIONS.len() as i64);
}

#[test]
fn pending_migration_on_existing_file_is_backed_up_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forgebox.db");
    {
        let store = Store::open(&path).unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO tools (id, name, created_at, updated_at) \
                 VALUES ('php', 'PHP', 'now', 'now')",
                [],
            )
            .unwrap();
    }

    let mut next: Vec<Migration> = MIGRATIONS.to_vec();
    next.push(Migration {
        version: latest_version() + 1,
        name: "test_extra",
        sql: "ALTER TABLE tools ADD COLUMN extra TEXT;",
    });
    let mut conn = rusqlite::Connection::open(&path).unwrap();
    let report = migrations::apply(&mut conn, &next, Some(&path)).unwrap();
    assert_eq!(report.applied, vec![latest_version() + 1]);
    let backup = report.backup.expect("backup taken");
    let name = backup.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with(&format!("forgebox.db.bak-v{:04}-", latest_version())),
        "{name}"
    );

    // The backup is a consistent copy of the database before the migration.
    let old = rusqlite::Connection::open(&backup).unwrap();
    let tools: i64 = old
        .query_row("SELECT COUNT(*) FROM tools", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tools, 1);
    assert!(old.prepare("SELECT extra FROM tools").is_err());
    assert!(conn.prepare("SELECT extra FROM tools").is_ok());

    // Idempotent: a second run applies nothing and takes no backup.
    let again = migrations::apply(&mut conn, &next, Some(&path)).unwrap();
    assert!(again.applied.is_empty());
    assert_eq!(again.backup, None);
}

#[test]
fn retried_failing_migration_keeps_every_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forgebox.db");
    drop(Store::open(&path).unwrap());
    let mut next: Vec<Migration> = MIGRATIONS.to_vec();
    next.push(Migration {
        version: latest_version() + 1,
        name: "broken",
        sql: "NOT SQL;",
    });
    // Two quick retries usually land in the same second: names must still differ.
    for _ in 0..2 {
        let mut conn = rusqlite::Connection::open(&path).unwrap();
        let err = migrations::apply(&mut conn, &next, Some(&path)).unwrap_err();
        assert_eq!(err.code(), "db");
        assert_eq!(
            migrations::current_version(&conn).unwrap(),
            latest_version()
        );
    }
    let backups: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("forgebox.db.bak-v"))
        .collect();
    assert_eq!(backups.len(), 2, "{backups:?}");
}

#[test]
fn wal_and_foreign_keys_are_on() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("forgebox.db")).unwrap();
    let mode: String = store
        .conn()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");

    let err = store
        .conn()
        .execute(
            "INSERT INTO tool_versions (id, tool_id, version, canonical_name, path, state, \
             source, created_at, updated_at) \
             VALUES (?1, 'ghost', '1.0', '1.0', 'bin/ghost/1.0', 'installed', 'manual', 'n', 'n')",
            [VersionId::new()],
        )
        .unwrap_err();
    assert!(err.to_string().contains("FOREIGN KEY"), "{err}");
}

#[test]
fn schema_constraints_hold() {
    let store = Store::open_in_memory().unwrap();
    let insert_site = |host: &str| {
        store.conn().execute(
            "INSERT INTO sites (id, slug, host, kind, created_at, updated_at) \
             VALUES (?1, ?2, ?2, 'php', 'n', 'n')",
            (SiteId::new(), host),
        )
    };
    insert_site("loja.test").unwrap();
    assert!(insert_site("Loja.test").is_err(), "hosts are lowercase");
    assert!(insert_site("loja.test").is_err(), "hosts are unique");
    let cs: i64 = store
        .conn()
        .query_row(
            "SELECT case_sensitive FROM sites WHERE host = 'loja.test'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cs, 1, "sites are case-sensitive by default");

    let bad_tool = store.conn().execute(
        "INSERT INTO tools (id, name, created_at, updated_at) VALUES ('My_Tool', 'x', 'n', 'n')",
        [],
    );
    assert!(bad_tool.is_err(), "tool ids are slugs");

    let port = |port: i64| {
        store.conn().execute(
            "INSERT INTO ports (id, port, owner_kind, owner_id, created_at, updated_at) \
             VALUES (?1, ?2, 'pool', ?1, 'n', 'n')",
            (forge_core::ids::PortReservationId::new(), port),
        )
    };
    port(19000).unwrap();
    assert!(port(19000).is_err(), "a port is reserved once");
    assert!(port(70000).is_err());
}

#[test]
fn events_round_trip_since_seq() {
    let store = Store::open_in_memory().unwrap();
    let repo = store.events();
    assert_eq!(repo.max_seq().unwrap(), 0);

    let bus = EventBus::starting_at(64, repo.max_seq().unwrap() + 1);
    let published: Vec<_> = (0..5)
        .map(|i| bus.publish(EventKind::SiteCreated, json!({ "n": i })))
        .collect();
    for event in &published {
        repo.insert(event).unwrap();
    }
    assert!(repo.insert(&published[0]).is_err(), "duplicate seq/id");
    assert_eq!(repo.max_seq().unwrap(), 5);
    assert_eq!(repo.count().unwrap(), 5);

    let since = repo.list_since(2, 100).unwrap();
    assert_eq!(since, published[2..].to_vec());
    assert_eq!(repo.list_since(0, 2).unwrap(), published[..2].to_vec());
    assert!(repo.list_since(5, 10).unwrap().is_empty());

    // A restarted daemon keeps numbering after the stored events.
    let restarted = EventBus::starting_at(64, repo.max_seq().unwrap() + 1);
    assert_eq!(restarted.publish(EventKind::DaemonReady, json!({})).seq, 6);

    assert_eq!(repo.prune_keep_last(2).unwrap(), 3);
    let left: Vec<u64> = repo
        .list_since(0, 10)
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(left, vec![4, 5]);
}

#[test]
fn migration_0002_adds_the_registry_loader_columns() {
    let store = Store::open_in_memory().unwrap();
    let columns: Vec<String> = store
        .conn()
        .prepare("SELECT name FROM pragma_table_info('registries')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for wanted in [
        "trusted_unsigned",
        "public_key",
        "ttl_secs",
        "verification",
        "index_sha256",
        "last_error",
    ] {
        assert!(columns.iter().any(|c| c == wanted), "missing {wanted}");
    }
    // Existing rows (from 0001) get the defaults.
    store
        .conn()
        .execute(
            "INSERT INTO registries (id, name, url, kind, created_at, updated_at) \
             VALUES ('r1', 'x', 'u', 'dir', 't', 't')",
            [],
        )
        .unwrap();
    let (unsigned, ttl): (i64, i64) = store
        .conn()
        .query_row(
            "SELECT trusted_unsigned, ttl_secs FROM registries",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((unsigned, ttl), (0, 3600));
}

/// A database left at schema 3 (the 0.1.0 release) with an instance and a PHP version:
/// opening it applies 0004 after a backup, keeps the rows and adds an empty
/// `tool_series_settings` that works with the existing `tools` rows.
#[test]
fn migration_0004_upgrades_a_schema_3_database() {
    use forge_core::ids::ToolId;
    use forge_core::store::ToolSeriesSettingsRepo;
    use std::collections::BTreeMap;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forgebox.db");
    {
        let mut conn = rusqlite::Connection::open(&path).unwrap();
        let report = migrations::apply(&mut conn, &MIGRATIONS[..3], None).unwrap();
        assert_eq!(report.to_version, 3);
        conn.execute_batch(
            r#"INSERT INTO tools (id, name, created_at, updated_at)
                 VALUES ('php', 'PHP', 't', 't'), ('mysql', 'MySQL', 't', 't');
               INSERT INTO tool_versions (id, tool_id, version, canonical_name, path, state, source, created_at, updated_at)
                 VALUES ('01J00000000000000000000001', 'mysql', '8.4.6', '8.4.6', 'C:/fb/bin/mysql/8.4.6', 'installed', 'registry', 't', 't');
               INSERT INTO instances (id, tool_id, version_id, name, settings, created_at, updated_at)
                 VALUES ('01J00000000000000000000002', 'mysql', '01J00000000000000000000001', 'default',
                         '{"max_connections":"300"}', 't', 't');"#,
        )
        .unwrap();
        assert!(
            conn.prepare("SELECT 1 FROM tool_series_settings").is_err(),
            "schema 3 has no settings table"
        );
    }
    let store = Store::open(&path).unwrap();
    let report = store.migration_report();
    assert_eq!(report.from_version, 3);
    assert_eq!(report.applied, vec![4, 5]);
    assert!(report.backup.is_some(), "backed up before migrating");
    let settings: String = store
        .conn()
        .query_row("SELECT settings FROM instances", [], |r| r.get(0))
        .unwrap();
    assert_eq!(settings, r#"{"max_connections":"300"}"#);
    let php = ToolId::new("php").unwrap();
    let repo = ToolSeriesSettingsRepo::new(store.conn());
    assert!(repo.get(&php, "8.4").unwrap().is_empty());
    let stored = repo
        .apply(
            &php,
            "8.4",
            &BTreeMap::from([("memory_limit".to_owned(), Some("1G".to_owned()))]),
        )
        .unwrap();
    assert_eq!(stored["memory_limit"], "1G");
    // The key shape is enforced by the table too, not only by the repository.
    let err = store
        .conn()
        .execute(
            "INSERT INTO tool_series_settings VALUES ('php', '8.4', 'Bad.Key', 'x', 't')",
            [],
        )
        .unwrap_err();
    assert!(err.to_string().contains("CHECK"), "{err}");
}

#[test]
fn migration_0005_upgrades_a_schema_4_database_keeping_sites() {
    use forge_core::store::{SitePoolMode, SiteRepo};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forgebox.db");
    {
        let mut conn = rusqlite::Connection::open(&path).unwrap();
        let report = migrations::apply(&mut conn, &MIGRATIONS[..4], None).unwrap();
        assert_eq!(report.to_version, 4);
        conn.execute_batch(
            "INSERT INTO sites (id, slug, host, kind, created_at, updated_at) VALUES \
             ('01J0000000000000000000000A', 'a-test', 'a.test', 'php', \
              '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'), \
             ('01J0000000000000000000000B', 'b-test', 'b.test', 'static', \
              '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.migration_report().from_version, 4);
    assert_eq!(store.migration_report().applied, vec![5]);
    assert!(store.migration_report().backup.is_some());
    let sites = SiteRepo::new(store.conn()).list().unwrap();
    assert_eq!(sites.len(), 2);
    assert!(
        sites
            .iter()
            .all(|s| s.pool_mode == SitePoolMode::Shared && s.metrics_enabled)
    );
}

#[test]
fn events_between_filters_by_instant_and_kind_with_the_ts_index() {
    use forge_core::event::Event;
    use forge_core::store::EventRepo;

    let store = Store::open_in_memory().unwrap();
    let repo = EventRepo::new(store.conn());
    let ts = |s: &str| -> jiff::Timestamp { s.parse().unwrap() };
    // Fractions of different lengths sort wrongly as text; the exact comparison fixes it.
    let at = [
        "2026-11-01T10:00:00Z",
        "2026-11-01T10:00:00.5Z",
        "2026-11-01T10:00:01.250Z",
        "2026-11-01T10:05:00Z",
        "2026-11-01T11:00:00Z",
    ];
    let kinds = [
        EventKind::SitePhpChanged,
        EventKind::SiteUpdated,
        EventKind::SitePhpChanged,
        EventKind::ConfigRendered,
        EventKind::SitePhpChanged,
    ];
    for (i, (when, kind)) in at.iter().zip(kinds).enumerate() {
        let event = Event {
            id: forge_core::ids::EventId::new(),
            seq: i as u64 + 1,
            ts: ts(when),
            kind,
            payload: json!({ "i": i }),
        };
        repo.insert(&event).unwrap();
    }
    let seqs = |from: &str, to: &str, kinds: &[EventKind], limit: usize| -> Vec<u64> {
        repo.list_between(ts(from), ts(to), kinds, limit)
            .unwrap()
            .iter()
            .map(|e| e.seq)
            .collect()
    };
    let php = [EventKind::SitePhpChanged];
    let both = [EventKind::SitePhpChanged, EventKind::SiteUpdated];
    // `to` is exclusive, `from` inclusive.
    assert_eq!(
        seqs("2026-11-01T10:00:00Z", "2026-11-01T11:00:00Z", &php, 10),
        [1, 3]
    );
    assert_eq!(
        seqs("2026-11-01T10:00:00Z", "2026-11-01T11:00:00.001Z", &php, 10),
        [1, 3, 5]
    );
    assert_eq!(
        seqs("2026-11-01T10:00:00.4Z", "2026-11-01T10:00:02Z", &both, 10),
        [2, 3]
    );
    // The oldest `limit` come back.
    assert_eq!(
        seqs("2026-11-01T09:00:00Z", "2026-11-01T12:00:00Z", &both, 2),
        [1, 2]
    );
    assert!(seqs("2026-11-01T09:00:00Z", "2026-11-01T12:00:00Z", &[], 10).is_empty());
    assert!(seqs("2026-11-01T12:00:00Z", "2026-11-01T09:00:00Z", &php, 10).is_empty());
    let plan: Vec<String> = store
        .conn()
        .prepare("EXPLAIN QUERY PLAN SELECT id FROM events WHERE ts >= ?1 AND ts < ?2")
        .unwrap()
        .query_map(["a", "b"], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(plan.iter().any(|p| p.contains("events_ts")), "{plan:?}");
}
