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
    assert_eq!(report.applied, vec![1, 2]);
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
