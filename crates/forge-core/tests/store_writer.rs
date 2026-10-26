//! Store writer thread and read pool (docs/03 §7.1): concurrent writes all land, reads
//! never wait for the writer, timeouts never wedge the writer, failures are isolated per
//! operation and writes are batched.

use std::time::{Duration, Instant};

use forge_core::error::{ForgeError, Result};
use forge_core::event::{EventBus, EventKind};
use forge_core::store::{EventRepo, Store, StoreOptions, StoreReader, StoreWriter};
use serde_json::json;

const SECOND: Duration = Duration::from_secs(1);

fn open_file_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state").join("forgebox.db")).unwrap();
    (dir, store)
}

async fn create_table(writer: &StoreWriter) {
    writer
        .execute(|conn| {
            conn.execute_batch(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, label TEXT NOT NULL, n INTEGER NOT NULL)",
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn insert(writer: &StoreWriter, label: &'static str, n: i64) -> Result<()> {
    writer
        .execute(move |conn| {
            conn.execute("INSERT INTO t (label, n) VALUES (?1, ?2)", (label, n))?;
            Ok(())
        })
        .await
}

async fn labels(reader: &StoreReader) -> Vec<String> {
    reader
        .query(|conn| {
            let mut stmt = conn.prepare("SELECT label FROM t ORDER BY id")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
        })
        .await
        .unwrap()
}

async fn count(reader: &StoreReader) -> i64 {
    reader
        .query(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))?))
        .await
        .unwrap()
}

/// Closing joins the writer thread: do it off the async workers.
async fn close(store: Store) {
    tokio::task::spawn_blocking(move || store.close())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thousand_writes_from_eight_tasks_all_land() {
    let (_dir, store) = open_file_store();
    let writer = store.writer().clone();
    create_table(&writer).await;

    let mut tasks = Vec::new();
    for task in 0..8_i64 {
        let writer = writer.clone();
        tasks.push(tokio::spawn(async move {
            for n in 0..125_i64 {
                writer
                    .execute(move |conn| {
                        conn.execute(
                            "INSERT INTO t (label, n) VALUES (?1, ?2)",
                            (format!("task-{task}"), n),
                        )?;
                        Ok(())
                    })
                    .await
                    .unwrap();
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }

    assert_eq!(count(store.reader()).await, 1000);
    let distinct: i64 = store
        .reader()
        .query(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM (SELECT DISTINCT label, n FROM t)",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(distinct, 1000);
    let stats = writer.stats();
    assert_eq!(stats.ops, 1001, "create + 1000 inserts: {stats:?}");
    assert_eq!(stats.failed, 0);
    assert_eq!(stats.commit_failures, 0);
    assert!(stats.batches <= stats.ops);
    close(store).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reads_do_not_wait_for_a_long_write_transaction() {
    let (_dir, store) = open_file_store();
    let writer = store.writer().clone();
    let reader = store.reader().clone();
    create_table(&writer).await;
    insert(&writer, "before", 1).await.unwrap();

    let slow = tokio::spawn({
        let writer = writer.clone();
        async move {
            writer
                .execute(|conn| {
                    conn.execute("INSERT INTO t (label, n) VALUES ('slow', 2)", [])?;
                    // Holds the write transaction open.
                    std::thread::sleep(Duration::from_millis(600));
                    Ok(())
                })
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    for _ in 0..5 {
        let started = Instant::now();
        let seen = count(&reader).await;
        let took = started.elapsed();
        assert!(took < Duration::from_millis(100), "read took {took:?}");
        assert_eq!(seen, 1, "uncommitted rows are not visible");
    }
    slow.await.unwrap().unwrap();
    assert_eq!(labels(&reader).await, vec!["before", "slow"]);
    assert!(
        reader.idle_connections() >= 1,
        "read connections are pooled"
    );

    // Readers are read-only.
    let err = reader
        .query(|conn| {
            conn.execute("DELETE FROM t", [])?;
            Ok(())
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "db", "{err}");
    close(store).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn short_timeout_behind_slow_write_times_out_without_wedging_the_writer() {
    let (_dir, store) = open_file_store();
    let writer = store.writer().clone();
    create_table(&writer).await;

    let slow = tokio::spawn({
        let writer = writer.clone();
        async move {
            writer
                .execute(|conn| {
                    std::thread::sleep(Duration::from_millis(300));
                    conn.execute("INSERT INTO t (label, n) VALUES ('slow', 1)", [])?;
                    Ok(())
                })
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;

    let started = Instant::now();
    let err = writer
        .execute_timeout(Duration::from_millis(50), |conn| {
            conn.execute("INSERT INTO t (label, n) VALUES ('late', 2)", [])?;
            Ok(())
        })
        .await
        .unwrap_err();
    let took = started.elapsed();
    assert!(matches!(err, ForgeError::Timeout { .. }), "{err:?}");
    assert!(took < Duration::from_millis(250), "{took:?}");

    slow.await.unwrap().unwrap();
    insert(&writer, "after", 3).await.unwrap();
    assert_eq!(
        labels(store.reader()).await,
        vec!["slow", "after"],
        "the abandoned operation was skipped"
    );
    assert_eq!(writer.stats().skipped, 1);

    // An operation that times out while running still completes (outcome unknown).
    let err = writer
        .execute_timeout(Duration::from_millis(30), |conn| {
            std::thread::sleep(Duration::from_millis(150));
            conn.execute("INSERT INTO t (label, n) VALUES ('ran', 4)", [])?;
            Ok(())
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "timeout");
    writer.flush(SECOND).await.unwrap();
    assert_eq!(labels(store.reader()).await, vec!["slow", "after", "ran"]);
    close(store).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failing_or_panicking_operation_rolls_back_alone() {
    let (_dir, store) = open_file_store();
    let writer = store.writer().clone();
    create_table(&writer).await;

    // Queue A, B (fails after writing) and C behind a slow write so that they share
    // a batch; only B is rolled back.
    let blocker = tokio::spawn({
        let writer = writer.clone();
        async move {
            writer
                .execute(|_| {
                    std::thread::sleep(Duration::from_millis(100));
                    Ok(())
                })
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let failing = writer.execute(|conn| {
        conn.execute("INSERT INTO t (label, n) VALUES ('b', 2)", [])?;
        Err::<(), _>(ForgeError::Conflict("rejected after writing".into()))
    });
    let (a, b, c) = tokio::join!(insert(&writer, "a", 1), failing, insert(&writer, "c", 3));
    blocker.await.unwrap().unwrap();
    a.unwrap();
    c.unwrap();
    assert_eq!(b.unwrap_err().code(), "conflict");
    let mut seen = labels(store.reader()).await;
    seen.sort();
    assert_eq!(seen, vec!["a", "c"]);

    // A panicking operation is rolled back and reported; the writer keeps going.
    let err = writer
        .execute(|conn| -> Result<()> {
            conn.execute("INSERT INTO t (label, n) VALUES ('p', 4)", [])?;
            panic!("bad write op");
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "internal", "{err}");
    insert(&writer, "d", 5).await.unwrap();
    let mut seen = labels(store.reader()).await;
    seen.sort();
    assert_eq!(seen, vec!["a", "c", "d"]);

    // An operation that ends the batch transaction itself gets an "outcome unknown"
    // error; the writer recovers.
    let err = writer
        .execute(|conn| {
            conn.execute_batch("COMMIT")?;
            Ok(())
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "internal", "{err}");
    insert(&writer, "e", 6).await.unwrap();

    let stats = writer.stats();
    assert_eq!(stats.failed, 2, "{stats:?}");
    assert_eq!(stats.commit_failures, 1, "{stats:?}");
    close(store).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_writes_are_batched_up_to_the_limit() {
    let (_dir, store) = open_file_store();
    let writer = store.writer().clone();
    create_table(&writer).await;

    let blocker = tokio::spawn({
        let writer = writer.clone();
        async move {
            writer
                .execute(|_| {
                    std::thread::sleep(Duration::from_millis(100));
                    Ok(())
                })
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let before = writer.stats().batches;
    let results = futures::future::join_all((0..200_i64).map(|n| insert(&writer, "q", n))).await;
    blocker.await.unwrap().unwrap();
    assert!(results.iter().all(Result::is_ok));
    let batches = writer.stats().batches - before;
    // ≥ 1 (blocker) + ceil(200 / 64) = 5; far fewer than one per write.
    assert!(batches >= 5, "at most 64 ops per transaction: {batches}");
    assert!(batches <= 60, "writes are grouped: {batches}");
    assert_eq!(count(store.reader()).await, 200);
    close(store).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_memory_store_shares_one_database() {
    let store = Store::open_in_memory().unwrap();
    let bus = EventBus::new();
    let event = bus.publish(EventKind::SiteCreated, json!({"host": "loja.test"}));
    let stored = event.clone();
    store
        .writer()
        .execute(move |conn| EventRepo::new(conn).insert(&stored))
        .await
        .unwrap();
    let back = store
        .reader()
        .query(|conn| EventRepo::new(conn).list_since(0, 10))
        .await
        .unwrap();
    assert_eq!(back, vec![event]);
    assert_eq!(store.events().count().unwrap(), 1, "maintenance connection");
    close(store).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closed_store_rejects_writes_and_reads() {
    let (_dir, store) = open_file_store();
    let writer = store.writer().clone();
    let reader = store.reader().clone();
    create_table(&writer).await;
    insert(&writer, "kept", 1).await.unwrap();
    close(store).await;

    assert!(writer.is_closed());
    let err = insert(&writer, "lost", 2).await.unwrap_err();
    assert_eq!(err.code(), "internal", "{err}");
    let err = reader
        .query(|conn| Ok(conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "internal", "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn options_control_default_timeouts() {
    let dir = tempfile::tempdir().unwrap();
    let options = StoreOptions {
        write_timeout: Duration::from_millis(40),
        ..StoreOptions::default()
    };
    let store = Store::open_with(&dir.path().join("forgebox.db"), options).unwrap();
    let err = store
        .writer()
        .execute(|_| {
            std::thread::sleep(Duration::from_millis(150));
            Ok(())
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "timeout");
    store.writer().flush(SECOND).await.unwrap();
    close(store).await;
}
