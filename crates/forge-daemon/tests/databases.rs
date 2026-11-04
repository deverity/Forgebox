//! Databases backend (M2-E3-T5, docs/09 §db, docs/04 §1.8). The client is the fake of
//! `tests/fixtures/fake_db_client/` (it records its argv and keeps its "server" in a text
//! file), so the exact SQL and arguments are asserted.
//!
//! * **Both families** (`engine_*`): the real `spec.database` blocks of
//!   `registry/tools/{mysql,postgresql}.yaml` drive `forge_daemon::databases::Target`:
//!   argv of list/create/drop, parsing (system databases hidden, only the first column),
//!   the injection guard (the client is never spawned), confirm, stopped instance,
//!   case-sensitive names, url and shell.
//! * **Through the daemon** (`daemon_*`): the real `forgeboxd` with the fake `mysqld`
//!   (`mysql/default` autostarts) answers `db.list|create|drop|url|shell`; a stopped
//!   instance is a conflict with the `forge start` hint; a root without database tools
//!   answers empty lists; the cold `db.list` takes < 1 s.
//! * **Export/import** (M2-E3-T6): the same fake doubles as `mysqldump.exe`/`pg_dump.exe`
//!   (64 MiB of deterministic SQL, paced on demand) and as the restore (stdin copied to
//!   `restored.bin`): sha256 of what crossed, progress, the three input formats, cancel at
//!   half (no process, no `.partial`), the stderr tail of a failure, validation, and the
//!   jobs through `forgeboxd` with `services.status` p99 < 50 ms during a 64 MiB export.
#![cfg(windows)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use forge_daemon::databases::Target;
use forge_ipc::api::methods::{
    db_create, db_drop, db_export, db_import, db_list, db_shell, db_url, events_subscribe,
    jobs_cancel, jobs_get, services_status, services_stop,
};
use forge_ipc::api::{
    DbCreateParams, DbDropParams, DbExportDto, DbExportParams, DbImportParams, DbListParams,
    DbShellParams, DbUrlParams, EventFilter, EventsSubscribeParams, InstanceDto, JobDto,
    JobIdParams, JobState, NoParams, ServiceState, ServiceTarget, ServiceTargetsParams,
};
use forge_ipc::client::SubscriptionItem;
use forge_ipc::{ApiError, Client, ClientError};
use forge_manifest::schema::Manifest;
use forge_manifest::schema::tool::DatabaseSpec;

use common::*;

fn fake(name: &str) -> PathBuf {
    static CLIENT: OnceLock<PathBuf> = OnceLock::new();
    static MYSQLD: OnceLock<PathBuf> = OnceLock::new();
    match name {
        "fake_db_client" => CLIENT.get_or_init(|| compile_fixture(name)).clone(),
        _ => MYSQLD.get_or_init(|| compile_fixture(name)).clone(),
    }
}

fn spec_of(tool: &str) -> DatabaseSpec {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../registry/tools")
        .join(format!("{tool}.yaml"));
    match forge_manifest::parse::load_file(&path).unwrap() {
        Manifest::Tool(tool) => tool.spec.database.expect("spec.database"),
        other => panic!("not a tool: {other:?}"),
    }
}

fn api_error(err: ClientError) -> ApiError {
    match err {
        ClientError::Api(api) => api,
        other => panic!("not an API error: {other:?}"),
    }
}

/// The recorded invocations of the fake client (`argv.log`), one `{:?}` per line.
fn argv_log(bin: &Path) -> Vec<String> {
    std::fs::read_to_string(bin.join("argv.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn debug(args: &[&str]) -> String {
    format!(
        "{:?}",
        args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>()
    )
}

// ------------------------------------------------------------------------ engine

/// A target on a temporary version folder whose client is the fake named `client`.
struct Family {
    _dir: tempfile::TempDir,
    bin: PathBuf,
    target: Target,
}

fn family(tool: &str, client: &str, port: u16, running: bool) -> Family {
    let dir = tempfile::tempdir().unwrap();
    let version = dir.path().join("8.4.13");
    let bin = version.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(fake("fake_db_client"), bin.join(client)).unwrap();
    let target = Target {
        id: forge_core::ids::InstanceId::new(),
        label: format!("{tool}/default"),
        version_dir: Some(version),
        ports: BTreeMap::from([("main".to_owned(), port)]),
        running,
        spec: spec_of(tool),
    };
    Family {
        _dir: dir,
        bin,
        target,
    }
}

const MYSQL_CLIENT: [&str; 6] = [
    "--protocol=tcp",
    "--host=127.0.0.1",
    "--port=3307",
    "--user=root",
    "--batch",
    "--skip-column-names",
];

fn mysql_argv(sql: &str) -> String {
    let mut args: Vec<String> = MYSQL_CLIENT.iter().map(|s| (*s).to_owned()).collect();
    args.push(format!("--execute={sql}"));
    format!("{args:?}")
}

fn psql_argv(sql: &str) -> String {
    debug(&[
        "-X",
        "-h",
        "127.0.0.1",
        "-p",
        "5433",
        "-U",
        "postgres",
        "-d",
        "postgres",
        "-v",
        "ON_ERROR_STOP=1",
        "-At",
        "-F",
        "\t",
        "-c",
        sql,
    ])
}

/// Exact SQL and argv of list/create/drop, parsing and the system databases, for one
/// family.
async fn exercise(fam: &Family, argv: fn(&str) -> String, list: &str, create: &str, drop: &str) {
    let target = &fam.target;
    std::fs::write(fam.bin.join("databases.txt"), "loja\nBlog").unwrap();

    let found = target.list().await.unwrap();
    let names: Vec<_> = found.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["Blog", "loja"], "system databases hidden, sorted");
    assert!(found.iter().all(|d| d.instance == target.label));
    assert_eq!(argv_log(&fam.bin), [argv(list)]);

    // Create with the default charset, then with an explicit one.
    let made = target.create("shop_2", None).await.unwrap();
    assert_eq!(made.name, "shop_2");
    assert_eq!(
        made.charset.as_deref(),
        Some(target.spec.default_charset.as_str())
    );
    assert_eq!(argv_log(&fam.bin)[1], argv(create));
    let made = target.create("Shop_2", Some("latin1")).await.unwrap();
    assert_eq!(made.charset.as_deref(), Some("latin1"));
    let after = target.list().await.unwrap();
    // Names are case-sensitive: `Shop_2` and `shop_2` are two databases.
    let names: Vec<_> = after.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["Blog", "Shop_2", "loja", "shop_2"]);

    // The same name again is a conflict (message of the client).
    let err = target.create("loja", None).await.unwrap_err();
    assert_eq!(err.code(), "conflict", "{err}");

    // Drop needs confirm and never starts the client without it.
    let before = argv_log(&fam.bin).len();
    let err = target.drop_database("loja", false).await.unwrap_err();
    assert_eq!(err.code(), "invalid_input", "{err}");
    assert_eq!(argv_log(&fam.bin).len(), before);
    target.drop_database("loja", true).await.unwrap();
    assert_eq!(argv_log(&fam.bin).last().unwrap(), &argv(drop));
    let err = target.drop_database("loja", true).await.unwrap_err();
    assert_eq!(err.code(), "not_found", "{err}");
    let names: Vec<_> = target
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(names, ["Blog", "Shop_2", "shop_2"]);
}

#[tokio::test]
async fn engine_mysql_family_runs_the_exact_statements() {
    let fam = family("mysql", "mysql.exe", 3307, true);
    exercise(
        &fam,
        mysql_argv,
        "SELECT schema_name FROM information_schema.schemata ORDER BY schema_name",
        "CREATE DATABASE `shop_2` CHARACTER SET utf8mb4",
        "DROP DATABASE `loja`",
    )
    .await;
    let log = argv_log(&fam.bin);
    assert_eq!(
        log[2],
        mysql_argv("CREATE DATABASE `Shop_2` CHARACTER SET latin1")
    );
}

#[tokio::test]
async fn engine_postgresql_family_runs_the_exact_statements() {
    let fam = family("postgresql", "psql.exe", 5433, true);
    exercise(
        &fam,
        psql_argv,
        "SELECT datname FROM pg_database WHERE NOT datistemplate ORDER BY datname",
        "CREATE DATABASE \"shop_2\" ENCODING 'UTF8' TEMPLATE template0",
        "DROP DATABASE \"loja\"",
    )
    .await;
    let log = argv_log(&fam.bin);
    assert_eq!(
        log[2],
        psql_argv("CREATE DATABASE \"Shop_2\" ENCODING 'latin1' TEMPLATE template0")
    );
}

#[tokio::test]
async fn engine_injection_guard_never_spawns_the_client() {
    for (tool, client) in [("mysql", "mysql.exe"), ("postgresql", "psql.exe")] {
        let fam = family(tool, client, 3307, true);
        let t = &fam.target;
        let long = "x".repeat(65);
        for bad in [
            "",
            "a;b",
            "a b",
            "a`b",
            "a\"b",
            "a'b",
            "loja; DROP DATABASE mysql",
            "a\nb",
            "é",
            "..\\x",
            "{{ sql }}",
            long.as_str(),
        ] {
            let err = t.create(bad, None).await.unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{tool} create {bad:?}: {err}");
            let err = t.drop_database(bad, true).await.unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{tool} drop {bad:?}: {err}");
            let err = t.url(bad).unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{tool} url {bad:?}: {err}");
            let err = t.shell(Some(bad)).unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{tool} shell {bad:?}: {err}");
        }
        for bad in ["", "utf8mb4; DROP", "a b", "a'b", &"c".repeat(33)] {
            let err = t.create("ok", Some(bad)).await.unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{tool} charset {bad:?}: {err}");
        }
        // System databases are refused as well.
        for system in t.spec.system_databases.clone() {
            let err = t.create(&system, None).await.unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{err}");
            let err = t.drop_database(&system, true).await.unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{err}");
        }
        assert!(
            argv_log(&fam.bin).is_empty(),
            "{tool}: the client was started: {:?}",
            argv_log(&fam.bin)
        );
        // Invalid names are invalid even when the instance is stopped (not a conflict).
        let stopped = Target {
            running: false,
            ..t.clone()
        };
        let err = stopped.create("a;b", None).await.unwrap_err();
        assert_eq!(err.code(), "invalid_input");
    }
}

#[tokio::test]
async fn engine_stopped_instance_is_a_conflict_with_the_start_hint() {
    let fam = family("mysql", "mysql.exe", 3307, false);
    let t = &fam.target;
    for err in [
        t.list().await.unwrap_err(),
        t.create("loja", None).await.unwrap_err(),
        t.drop_database("loja", true).await.unwrap_err(),
    ] {
        assert_eq!(err.code(), "conflict", "{err}");
        assert!(
            err.to_string().contains("forge start mysql/default"),
            "{err}"
        );
    }
    assert!(argv_log(&fam.bin).is_empty());
    // A DSN needs no running server.
    assert!(t.url("loja").is_ok());
}

#[tokio::test]
async fn engine_url_and_shell_come_from_the_manifest() {
    let my = family("mysql", "mysql.exe", 3307, true);
    let url = my.target.url("loja").unwrap();
    assert_eq!(url.url, "mysql://root@127.0.0.1:3307/loja");
    assert_eq!(url.env.vars["DB_DATABASE"], "loja");
    assert_eq!(url.env.vars["DB_PORT"], "3307");
    assert_eq!(url.env.vars["DB_CONNECTION"], "mysql");

    let shell = my.target.shell(None).unwrap();
    assert!(
        shell.program.ends_with("bin\\mysql.exe"),
        "{}",
        shell.program
    );
    assert_eq!(
        shell.args,
        [
            "--protocol=tcp",
            "--host=127.0.0.1",
            "--port=3307",
            "--user=root"
        ]
    );
    let with_db = my.target.shell(Some("loja")).unwrap();
    assert_eq!(with_db.args.last().unwrap(), "loja");

    let pg = family("postgresql", "psql.exe", 5433, true);
    let url = pg.target.url("loja").unwrap();
    assert_eq!(url.url, "postgresql://postgres@127.0.0.1:5433/loja");
    assert_eq!(url.env.vars["DB_CONNECTION"], "pgsql");
    let shell = pg.target.shell(Some("loja")).unwrap();
    assert_eq!(
        shell.args,
        [
            "-h",
            "127.0.0.1",
            "-p",
            "5433",
            "-U",
            "postgres",
            "-d",
            "loja"
        ]
    );
    let shell = pg.target.shell(None).unwrap();
    assert_eq!(shell.args.last().unwrap(), "postgres");
}

// ------------------------------------------------------------------ export / import

/// The fake dump's SQL (same generator as `fake_db_client::dump_bytes`).
fn dump_bytes(size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut row = 0u64;
    while out.len() < size {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let line = format!(
            "INSERT INTO `t` VALUES ({row},'{state:016x}{:016x}');\n",
            state.rotate_left(29)
        );
        let take = line.len().min(size - out.len());
        out.extend_from_slice(&line.as_bytes()[..take]);
        row += 1;
    }
    out
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn job_tree() -> forge_supervisor::JobTree {
    forge_supervisor::JobTree::new(forge_core::platform::process_control(Default::default()))
        .unwrap()
}

/// Collects the progress reports of a transfer.
#[derive(Clone, Default)]
struct Reports(std::sync::Arc<std::sync::Mutex<Vec<forge_core::jobs::JobProgress>>>);

impl Reports {
    fn sink(&self) -> forge_daemon::databases::transfer::ProgressSink {
        let seen = self.0.clone();
        std::sync::Arc::new(move |p| seen.lock().unwrap().push(p))
    }

    fn all(&self) -> Vec<forge_core::jobs::JobProgress> {
        self.0.lock().unwrap().clone()
    }
}

/// A family with the fake as its dump client too (`mysqldump.exe` / `pg_dump.exe`).
fn dump_family(tool: &str, client: &str, dump: &str, port: u16) -> Family {
    let fam = family(tool, client, port, true);
    std::fs::copy(fake("fake_db_client"), fam.bin.join(dump)).unwrap();
    fam
}

fn pid_in(bin: &Path, file: &str) -> u32 {
    std::fs::read_to_string(bin.join(file))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Waits (≤ 2 s) until `pid` is gone.
async fn gone(pid: u32) -> bool {
    let control = forge_core::platform::process_control(Default::default());
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if !control.is_alive(pid).unwrap_or(false) {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

const DUMP_64_MIB: usize = 64 * 1024 * 1024;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn engine_export_streams_64_mib_into_zstd() {
    let fam = dump_family("mysql", "mysql.exe", "mysqldump.exe", 3307);
    std::fs::write(fam.bin.join("dump.txt"), DUMP_64_MIB.to_string()).unwrap();
    let out_dir = fam._dir.path().join("out").join("deeper");
    let output = out_dir.join("loja.sql.zst");
    let transfer = fam.target.prepare_export("loja", &output).unwrap();
    let reports = Reports::default();
    let cancel = tokio_util::sync::CancellationToken::new();
    let started = Instant::now();
    let dto =
        forge_daemon::databases::transfer::export(transfer, &job_tree(), reports.sink(), &cancel)
            .await
            .unwrap();
    let took = started.elapsed();
    assert_eq!(PathBuf::from(&dto.path), output);
    let compressed = std::fs::read(&output).unwrap();
    assert_eq!(dto.size_bytes, compressed.len() as u64);
    assert_eq!(leftovers(&out_dir), ["loja.sql.zst"], "no .partial left");
    let plain = zstd::decode_all(compressed.as_slice()).unwrap();
    assert_eq!(plain.len(), DUMP_64_MIB);
    assert_eq!(sha256(&plain), sha256(&dump_bytes(DUMP_64_MIB)));
    assert!(
        compressed.len() * 2 < plain.len(),
        "zstd compresses SQL: {} of {}",
        compressed.len(),
        plain.len()
    );

    // Progress: growing uncompressed bytes with a speed, the last one is the total.
    let seen = reports.all();
    assert!(seen.len() >= 5, "{} progress reports", seen.len());
    let bytes: Vec<u64> = seen.iter().map(|p| p.bytes_done.unwrap()).collect();
    // Growing; the final report (with the total and 100 %) may repeat the last count.
    assert!(bytes.windows(2).all(|w| w[0] <= w[1]), "{bytes:?}");
    let mut distinct = bytes.clone();
    distinct.dedup();
    assert!(distinct.len() >= 5, "{bytes:?}");
    assert_eq!(*bytes.last().unwrap(), DUMP_64_MIB as u64);
    assert!(seen.iter().all(|p| p.speed_bps.is_some()));
    assert_eq!(seen.last().unwrap().pct, Some(100.0));

    // The dump ran with the manifest's arguments, and is gone.
    assert_eq!(
        argv_log(&fam.bin).last().unwrap(),
        &debug(&[
            "--protocol=tcp",
            "--host=127.0.0.1",
            "--port=3307",
            "--user=root",
            "--single-transaction",
            "--routines",
            "--events",
            "--triggers",
            "loja",
        ])
    );
    assert!(gone(pid_in(&fam.bin, "dump.pid")).await);
    eprintln!(
        "64 MiB export: {took:?}, {} -> {} bytes, {} reports",
        plain.len(),
        compressed.len(),
        seen.len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn engine_export_by_name_gzip_and_plain_and_postgres_arguments() {
    let fam = dump_family("postgresql", "psql.exe", "pg_dump.exe", 5433);
    let size = 3 * 1024 * 1024 + 17;
    std::fs::write(fam.bin.join("dump.txt"), size.to_string()).unwrap();
    let dir = fam._dir.path().join("out");
    let cancel = tokio_util::sync::CancellationToken::new();
    for (name, decode) in [("Loja.sql.gz", "gzip"), ("Loja.sql", "plain")] {
        let output = dir.join(name);
        let transfer = fam.target.prepare_export("Loja", &output).unwrap();
        forge_daemon::databases::transfer::export(
            transfer,
            &job_tree(),
            Reports::default().sink(),
            &cancel,
        )
        .await
        .unwrap();
        let raw = std::fs::read(&output).unwrap();
        let plain = if decode == "gzip" {
            let mut out = Vec::new();
            std::io::Read::read_to_end(
                &mut flate2::read::MultiGzDecoder::new(raw.as_slice()),
                &mut out,
            )
            .unwrap();
            out
        } else {
            raw
        };
        assert_eq!(sha256(&plain), sha256(&dump_bytes(size)), "{name}");
    }
    assert_eq!(
        argv_log(&fam.bin).last().unwrap(),
        &debug(&[
            "-h",
            "127.0.0.1",
            "-p",
            "5433",
            "-U",
            "postgres",
            "--no-owner",
            "-d",
            "Loja"
        ])
    );
}

/// The three input formats of one dump: the restore client's stdin is the dump, byte for
/// byte; the percentage only grows and ends at 100.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn engine_import_feeds_sql_gz_and_zst() {
    let fam = family("mysql", "mysql.exe", 3307, true);
    std::fs::write(fam.bin.join("restore.txt"), "0 0").unwrap();
    let data = dump_bytes(12 * 1024 * 1024 + 5);
    let dir = fam._dir.path().join("in");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("loja.sql"), &data).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &data).unwrap();
    std::fs::write(dir.join("loja.sql.gz"), gz.finish().unwrap()).unwrap();
    std::fs::write(
        dir.join("loja.SQL.ZST"),
        zstd::encode_all(data.as_slice(), 3).unwrap(),
    )
    .unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    for name in ["loja.sql", "loja.sql.gz", "loja.SQL.ZST"] {
        std::fs::remove_file(fam.bin.join("restored.bin")).ok();
        let transfer = fam.target.prepare_import("loja", &dir.join(name)).unwrap();
        let reports = Reports::default();
        forge_daemon::databases::transfer::import(transfer, &job_tree(), reports.sink(), &cancel)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let restored = std::fs::read(fam.bin.join("restored.bin")).unwrap();
        assert_eq!(
            sha256(&restored),
            sha256(&data),
            "{name}: stdin of the restore"
        );
        let pcts: Vec<f32> = reports.all().iter().map(|p| p.pct.unwrap()).collect();
        assert!(pcts.windows(2).all(|w| w[0] <= w[1]), "{name}: {pcts:?}");
        assert_eq!(*pcts.last().unwrap(), 100.0, "{name}: {pcts:?}");
        assert!(gone(pid_in(&fam.bin, "restore.pid")).await);
    }
    // The restore ran with the manifest's arguments.
    assert_eq!(
        argv_log(&fam.bin).last().unwrap(),
        &debug(&[
            "--protocol=tcp",
            "--host=127.0.0.1",
            "--port=3307",
            "--user=root",
            "loja"
        ])
    );
}

/// Cancel at about half: `cancelled` within 2 s, the client is gone, nothing is left in
/// the output folder (export) and the restore never got its end of file (import).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn engine_cancel_at_half_leaves_no_process_and_no_partial_file() {
    use forge_daemon::databases::transfer;
    let fam = dump_family("mysql", "mysql.exe", "mysqldump.exe", 3307);
    let tree = job_tree();

    // Export: 64 MiB at ~40 ms per MiB (about 2.6 s); cancel past 32 MiB.
    std::fs::write(fam.bin.join("dump.txt"), format!("{DUMP_64_MIB} 40")).unwrap();
    let dir = fam._dir.path().join("out");
    let output = dir.join("loja.sql.zst");
    let prepared = fam.target.prepare_export("loja", &output).unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let reports = Reports::default();
    let watcher = {
        let reports = reports.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                let half = reports
                    .all()
                    .last()
                    .and_then(|p| p.bytes_done)
                    .is_some_and(|b| b >= (DUMP_64_MIB / 2) as u64);
                if half {
                    cancel.cancel();
                    return Instant::now();
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };
    let err = transfer::export(prepared, &tree, reports.sink(), &cancel)
        .await
        .unwrap_err();
    let took = watcher.await.unwrap().elapsed();
    assert_eq!(err.code(), "cancelled", "{err}");
    assert!(took < Duration::from_secs(2), "export cancel took {took:?}");
    assert!(
        gone(pid_in(&fam.bin, "dump.pid")).await,
        "mysqldump survived"
    );
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
    assert!(tree.pids().unwrap().is_empty());

    // Import: 32 MiB at ~60 ms per MiB; cancel at 50 %.
    std::fs::write(fam.bin.join("restore.txt"), "60 0").unwrap();
    let input = fam._dir.path().join("big.sql");
    std::fs::write(&input, dump_bytes(32 * 1024 * 1024)).unwrap();
    let prepared = fam.target.prepare_import("loja", &input).unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let reports = Reports::default();
    let watcher = {
        let reports = reports.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                if reports
                    .all()
                    .last()
                    .and_then(|p| p.pct)
                    .is_some_and(|pct| pct >= 50.0)
                {
                    cancel.cancel();
                    return Instant::now();
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };
    let err = transfer::import(prepared, &tree, reports.sink(), &cancel)
        .await
        .unwrap_err();
    let took = watcher.await.unwrap().elapsed();
    assert_eq!(err.code(), "cancelled", "{err}");
    assert!(took < Duration::from_secs(2), "import cancel took {took:?}");
    assert!(
        gone(pid_in(&fam.bin, "restore.pid")).await,
        "the restore survived"
    );
    let pct = reports.all().last().unwrap().pct.unwrap();
    assert!(pct < 100.0, "{pct}");
    let restored = std::fs::metadata(fam.bin.join("restored.bin"))
        .unwrap()
        .len();
    assert!(restored < 32 * 1024 * 1024, "the restore got everything");
    assert!(tree.pids().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn engine_failures_carry_the_stderr_tail_and_leave_nothing() {
    use forge_daemon::databases::transfer;
    let fam = dump_family("mysql", "mysql.exe", "mysqldump.exe", 3307);
    let cancel = tokio_util::sync::CancellationToken::new();

    // Restore exits 1 after printing 30 lines: the error has the last 20.
    std::fs::write(fam.bin.join("restore.txt"), "0 1").unwrap();
    let input = fam._dir.path().join("loja.sql");
    std::fs::write(&input, dump_bytes(100_000)).unwrap();
    let prepared = fam.target.prepare_import("loja", &input).unwrap();
    let err = transfer::import(prepared, &job_tree(), Reports::default().sink(), &cancel)
        .await
        .unwrap_err();
    let text = err.to_string();
    assert_eq!(err.code(), "conflict", "{text}");
    assert!(text.contains("exit 1"), "{text}");
    assert!(text.contains("restore error line 11\n"), "{text}");
    assert!(text.ends_with("restore error line 30"), "{text}");
    assert!(!text.contains("restore error line 10\n"), "{text}");

    // Dump exits 2: no file at all.
    std::fs::write(fam.bin.join("dump.txt"), "4096 0 2").unwrap();
    let dir = fam._dir.path().join("out");
    let prepared = fam
        .target
        .prepare_export("loja", &dir.join("loja.sql.zst"))
        .unwrap();
    let err = transfer::export(prepared, &job_tree(), Reports::default().sink(), &cancel)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("dump error line 30"), "{err}");
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));

    // A corrupt .sql.gz: invalid_input, and the restore is killed before it could read
    // an end of file (it never exits on its own, so it never "commits").
    std::fs::write(fam.bin.join("restore.txt"), "0 0").unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &dump_bytes(2 * 1024 * 1024)).unwrap();
    let mut bytes = gz.finish().unwrap();
    let len = bytes.len();
    bytes.truncate(len / 2);
    let broken = fam._dir.path().join("broken.sql.gz");
    std::fs::write(&broken, &bytes).unwrap();
    let prepared = fam.target.prepare_import("loja", &broken).unwrap();
    let err = transfer::import(prepared, &job_tree(), Reports::default().sink(), &cancel)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_input", "{err}");
    assert!(err.to_string().contains("not a valid gzip file"), "{err}");
    assert!(gone(pid_in(&fam.bin, "restore.pid")).await);
}

#[tokio::test]
async fn engine_transfers_are_validated_before_anything_starts() {
    let fam = dump_family("mysql", "mysql.exe", "mysqldump.exe", 3307);
    let t = &fam.target;
    let dir = fam._dir.path();
    let ok_out = dir.join("x.sql.zst");
    let input = dir.join("in.sql");
    std::fs::write(&input, "SELECT 1;\n").unwrap();
    for bad in ["", "a;b", "a b", "a`b", "{{ sql }}"] {
        assert_eq!(
            t.prepare_export(bad, &ok_out).unwrap_err().code(),
            "invalid_input"
        );
        assert_eq!(
            t.prepare_import(bad, &input).unwrap_err().code(),
            "invalid_input"
        );
    }
    // Relative paths, unknown extensions and system databases.
    for path in ["x.sql", r"out\x.sql.zst"] {
        assert_eq!(
            t.prepare_export("loja", Path::new(path))
                .unwrap_err()
                .code(),
            "invalid_input"
        );
        assert_eq!(
            t.prepare_import("loja", Path::new(path))
                .unwrap_err()
                .code(),
            "invalid_input"
        );
    }
    assert_eq!(
        t.prepare_export("loja", &dir.join("x.zip"))
            .unwrap_err()
            .code(),
        "invalid_input"
    );
    assert_eq!(
        t.prepare_import("mysql", &input).unwrap_err().code(),
        "invalid_input"
    );
    // Stopped instance: conflict with the start hint.
    let stopped = Target {
        running: false,
        ..t.clone()
    };
    let err = stopped.prepare_export("loja", &ok_out).unwrap_err();
    assert_eq!(err.code(), "conflict");
    assert!(
        err.to_string().contains("forge start mysql/default"),
        "{err}"
    );
    assert_eq!(
        stopped.prepare_import("loja", &input).unwrap_err().code(),
        "conflict"
    );
    // Nothing ran.
    assert!(argv_log(&fam.bin).is_empty(), "{:?}", argv_log(&fam.bin));
    // Default name: backups/<tool>/<instance>/<db>-<UTC ts>.sql.zst.
    let at: forge_core::Timestamp = "2026-11-03T14:05:09Z".parse().unwrap();
    assert_eq!(
        t.default_export_path(Path::new(r"C:\fb\backups"), "loja", at),
        PathBuf::from(r"C:\fb\backups\mysql\default\loja-20261103T140509Z.sql.zst")
    );
}

// ------------------------------------------------------------------------ daemon

fn mysql_root() -> (TestRoot, PathBuf) {
    let root = TestRoot::new();
    let bin = root.root.bin().join("mysql").join("8.4.13").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(fake("fake_mysqld"), bin.join("mysqld.exe")).unwrap();
    std::fs::copy(fake("fake_mysqld"), bin.join("mysqladmin.exe")).unwrap();
    std::fs::copy(fake("fake_db_client"), bin.join("mysql.exe")).unwrap();
    (root, bin)
}

async fn wait_job(client: &Client, id: forge_core::ids::JobId, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let job = client
            .call::<jobs_get>(JobIdParams { job_id: id })
            .await
            .unwrap();
        match job.state {
            JobState::Done => return,
            JobState::Failed | JobState::Cancelled => panic!("job ended {:?}", job.error),
            _ => {}
        }
        assert!(Instant::now() < deadline, "job still {:?}", job.state);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn mysql_instance(client: &Client) -> Option<InstanceDto> {
    client
        .call::<services_status>(NoParams {})
        .await
        .unwrap()
        .instances
        .into_iter()
        .find(|i| i.tool.as_str() == "mysql")
}

async fn wait_state(client: &Client, state: ServiceState, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if mysql_instance(client)
            .await
            .is_some_and(|i| i.state == state)
        {
            return;
        }
        assert!(Instant::now() < deadline, "mysql never reached {state:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_serves_db_methods_from_the_manifest() {
    let (root, bin) = mysql_root();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    wait_state(&client, ServiceState::Running, Duration::from_secs(60)).await;
    let port = mysql_instance(&client)
        .await
        .unwrap()
        .ports
        .iter()
        .find(|p| p.name == "main")
        .map(|p| p.port)
        .expect("main port");

    // Cold cache: the first call answers well under a second, sizes not measured yet.
    let started = Instant::now();
    let first = client
        .call::<db_list>(DbListParams::default())
        .await
        .unwrap();
    let cold = started.elapsed();
    assert!(cold < Duration::from_secs(1), "cold db.list took {cold:?}");
    assert!(first.databases.is_empty());
    assert_eq!(first.instances.len(), 1);
    let instance = &first.instances[0];
    assert_eq!(instance.instance, "mysql/default");
    assert_eq!(instance.protocol, "mysql");
    assert_eq!(instance.version, "8.4.13");
    assert_eq!(instance.state, ServiceState::Running);
    assert_eq!(instance.port, Some(port));
    assert!(instance.capabilities.create && instance.capabilities.shell);
    assert!(instance.capabilities.export && instance.capabilities.import);
    assert_eq!(instance.data_size_bytes, None, "no walk finished yet");
    // The walk ends in the background: the size shows up on a later call.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let again = client
            .call::<db_list>(DbListParams::default())
            .await
            .unwrap();
        if again.instances[0].data_size_bytes.is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "the size was never measured");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Create (case-sensitive names) and list.
    for name in ["loja", "Loja"] {
        let made = client
            .call::<db_create>(DbCreateParams {
                name: name.into(),
                instance: None,
                charset: None,
            })
            .await
            .unwrap();
        assert_eq!(made.name, name);
        assert_eq!(made.instance, "mysql/default");
        assert_eq!(made.charset.as_deref(), Some("utf8mb4"));
    }
    let listed = client
        .call::<db_list>(DbListParams {
            instance: Some("mysql".into()),
        })
        .await
        .unwrap();
    let names: Vec<_> = listed.databases.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["Loja", "loja"]);
    let expected = mysql_argv("CREATE DATABASE `Loja` CHARACTER SET utf8mb4")
        .replace("3307", &port.to_string());
    assert!(
        argv_log(&bin).contains(&expected),
        "{expected} not in {:?}",
        argv_log(&bin)
    );
    let err = api_error(
        client
            .call::<db_create>(DbCreateParams {
                name: "loja".into(),
                instance: Some("mysql/default".into()),
                charset: None,
            })
            .await
            .unwrap_err(),
    );
    assert_eq!(err.code, "conflict", "{err:?}");

    // Injection guard through the API: nothing reaches the client.
    let calls = argv_log(&bin).len();
    for bad in ["a;b", "x`y", "", "é"] {
        let err = api_error(
            client
                .call::<db_create>(DbCreateParams {
                    name: bad.into(),
                    instance: None,
                    charset: None,
                })
                .await
                .unwrap_err(),
        );
        assert_eq!(err.code, "invalid_input", "{bad:?}: {err:?}");
    }
    let err = api_error(
        client
            .call::<db_create>(DbCreateParams {
                name: "ok".into(),
                instance: None,
                charset: Some("utf8; DROP".into()),
            })
            .await
            .unwrap_err(),
    );
    assert_eq!(err.code, "invalid_input");
    assert_eq!(argv_log(&bin).len(), calls, "the client was never spawned");

    // Drop: confirm required.
    let err = api_error(
        client
            .call::<db_drop>(DbDropParams {
                name: "loja".into(),
                instance: None,
                confirm: false,
            })
            .await
            .unwrap_err(),
    );
    assert_eq!(err.code, "invalid_input", "{err:?}");
    assert_eq!(argv_log(&bin).len(), calls);
    client
        .call::<db_drop>(DbDropParams {
            name: "loja".into(),
            instance: None,
            confirm: true,
        })
        .await
        .unwrap();
    let listed = client
        .call::<db_list>(DbListParams::default())
        .await
        .unwrap();
    let names: Vec<_> = listed.databases.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["Loja"]);

    // Url and shell.
    let url = client
        .call::<db_url>(DbUrlParams {
            instance: "mysql/default".into(),
            database: "Loja".into(),
        })
        .await
        .unwrap();
    assert_eq!(url.url, format!("mysql://root@127.0.0.1:{port}/Loja"));
    assert_eq!(url.env.vars["DB_PORT"], port.to_string());
    let shell = client
        .call::<db_shell>(DbShellParams {
            instance: "mysql/default".into(),
            database: Some("Loja".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        PathBuf::from(&shell.program),
        bin.join("mysql.exe")
            .canonicalize()
            .map(|p| forge_core::paths::plain_path(&p))
            .unwrap()
    );
    assert_eq!(shell.args.last().unwrap(), "Loja");
    // Attached run of what the daemon returned (what `forge db shell` does).
    std::fs::write(bin.join("shell-exit.txt"), "7").unwrap();
    let status = std::process::Command::new(&shell.program)
        .args(&shell.args)
        .envs(&shell.env)
        .current_dir(shell.cwd.as_deref().unwrap())
        .output()
        .unwrap();
    assert_eq!(status.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&status.stdout).starts_with("fake-shell --protocol=tcp"));

    // Stopped instance: conflict with the hint; db.list reports it without databases.
    let job = client
        .call::<services_stop>(ServiceTargetsParams {
            targets: vec![ServiceTarget::from("mysql/default")],
        })
        .await
        .unwrap();
    wait_job(&client, job.id, Duration::from_secs(60)).await;
    let before = argv_log(&bin).len();
    for err in [
        client
            .call::<db_create>(DbCreateParams {
                name: "nova".into(),
                instance: None,
                charset: None,
            })
            .await
            .unwrap_err(),
        client
            .call::<db_list>(DbListParams {
                instance: Some("mysql/default".into()),
            })
            .await
            .unwrap_err(),
    ] {
        let err = api_error(err);
        assert_eq!(err.code, "conflict", "{err:?}");
        assert!(err.message.contains("forge start mysql/default"), "{err:?}");
    }
    assert_eq!(argv_log(&bin).len(), before);
    let all = client
        .call::<db_list>(DbListParams::default())
        .await
        .unwrap();
    assert!(all.databases.is_empty());
    assert_eq!(all.instances[0].state, ServiceState::Stopped);
    // The DSN does not need the server.
    client
        .call::<db_url>(DbUrlParams {
            instance: "mysql".into(),
            database: "Loja".into(),
        })
        .await
        .unwrap();

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(30)).await;
    assert!(status.success(), "{status}");
}

/// `job.*` events of one job, collected in the background.
async fn job_events(
    client: &Client,
) -> std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>> {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut sub = client
        .subscribe::<events_subscribe>(EventsSubscribeParams {
            filter: EventFilter::kinds(["job.*"]),
            resume_from: None,
        })
        .await
        .unwrap();
    let sink = seen.clone();
    tokio::spawn(async move {
        while let Some(item) = sub.next_item().await {
            if let SubscriptionItem::Item(envelope) = item {
                sink.lock()
                    .unwrap()
                    .push((envelope.event.kind, envelope.event.payload));
            }
        }
    });
    seen
}

/// Polls the job to its end; returns the final snapshot.
async fn finished(client: &Client, id: forge_core::ids::JobId, timeout: Duration) -> JobDto {
    let deadline = Instant::now() + timeout;
    loop {
        let job = client
            .call::<jobs_get>(JobIdParams { job_id: id })
            .await
            .unwrap();
        if job.state.is_terminal() {
            return job;
        }
        assert!(Instant::now() < deadline, "job still {:?}", job.state);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Budget of `services.status` while an export runs (docs/03 §7.6).
const STATUS_BUDGET: Duration = Duration::from_millis(50);

/// Samples `services.status` until the job ends; returns its final snapshot and the p99.
async fn sample_status_until_done(
    client: &Client,
    id: forge_core::ids::JobId,
) -> (JobDto, Duration) {
    let mut samples = Vec::new();
    let export_started = Instant::now();
    let mut slow = Vec::new();
    let deadline = export_started + Duration::from_secs(120);
    let done = loop {
        let started = Instant::now();
        client.call::<services_status>(NoParams {}).await.unwrap();
        let took = started.elapsed();
        samples.push(took);
        if took > Duration::from_millis(20) {
            slow.push((started.duration_since(export_started), took));
        }
        let now = client
            .call::<jobs_get>(JobIdParams { job_id: id })
            .await
            .unwrap();
        if now.state.is_terminal() {
            break now;
        }
        assert!(Instant::now() < deadline, "the export never ended");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let count = samples.len();
    assert!(count >= 50, "{count} samples");
    let p99 = p99(samples);
    eprintln!("services.status during the export: {count} samples, p99 {p99:?}");
    // Diagnostics: when (from the start of the export) the slow answers came.
    eprintln!("answers over 20 ms (at, took): {slow:?}");
    (done, p99)
}

fn p99(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[(samples.len() * 99 / 100).min(samples.len() - 1)]
}

/// Through `forgeboxd`: a 64 MiB export as a job (default path under `backups/`, ≥ 5
/// `job.progress` events with growing bytes, `services.status` p99 < 50 ms meanwhile),
/// the import of that file back, `jobs.cancel` at half an export (cancelled within 2 s,
/// no process, no `.partial`) and a failing restore (`job.failed` with the stderr tail).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_export_and_import_jobs() {
    // `TestRoot` already gives this daemon a private `databases` port range, so the other
    // daemon test of this binary running at the same time never picks the same port.
    let (root, bin) = mysql_root();
    std::fs::copy(fake("fake_db_client"), bin.join("mysqldump.exe")).unwrap();
    // ~40 ms per MiB: the export takes a few seconds, long enough to sample the API.
    std::fs::write(bin.join("dump.txt"), format!("{DUMP_64_MIB} 40")).unwrap();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    wait_state(&client, ServiceState::Running, Duration::from_secs(60)).await;
    let events = job_events(&client).await;

    let job = client
        .call::<db_export>(DbExportParams {
            name: "loja".into(),
            instance: None,
            output: None,
        })
        .await
        .unwrap();
    assert_eq!(job.name, "db.export loja mysql/default");
    let (done, mut status_p99) = sample_status_until_done(&client, job.id).await;
    assert_eq!(done.state, JobState::Done, "{:?}", done.error);
    let result: DbExportDto = serde_json::from_value(done.result.unwrap()).unwrap();
    let path = PathBuf::from(&result.path);
    let backups = root.root.backups().join("mysql").join("default");
    assert_eq!(
        path.parent().unwrap().canonicalize().unwrap(),
        backups.canonicalize().unwrap()
    );
    let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        file_name.starts_with("loja-") && file_name.ends_with("Z.sql.zst"),
        "{file_name}"
    );
    assert_eq!(leftovers(&backups), std::slice::from_ref(&file_name));
    let compressed = std::fs::read(&path).unwrap();
    assert_eq!(result.size_bytes, compressed.len() as u64);
    let plain = zstd::decode_all(compressed.as_slice()).unwrap();
    assert_eq!(sha256(&plain), sha256(&dump_bytes(DUMP_64_MIB)));
    // Progress events of this job: at least 5, bytes growing.
    let id = job.id.to_string();
    let progress: Vec<u64> = events
        .lock()
        .unwrap()
        .iter()
        .filter(|(kind, p)| kind == "job.progress" && p["job_id"] == id.as_str())
        .filter_map(|(_, p)| p["bytes_done"].as_u64())
        .collect();
    let mut distinct = progress.clone();
    distinct.dedup();
    assert!(distinct.len() >= 5, "{progress:?}");
    assert!(progress.windows(2).all(|w| w[0] <= w[1]), "{progress:?}");
    // Like the budgets of `concurrency.rs`: a measurement over the budget is made once
    // more and only the last one counts (a daemon that waits on the export misses it
    // every time; a burst of other tests on this 4-core machine does not).
    if status_p99 >= STATUS_BUDGET {
        eprintln!("p99 {status_p99:?} over the budget: measuring again");
        let again = client
            .call::<db_export>(DbExportParams {
                name: "loja".into(),
                instance: None,
                output: Some(
                    root.path()
                        .join("again")
                        .join("loja.sql")
                        .display()
                        .to_string(),
                ),
            })
            .await
            .unwrap();
        let (done, p99) = sample_status_until_done(&client, again.id).await;
        assert_eq!(done.state, JobState::Done, "{:?}", done.error);
        status_p99 = p99;
    }
    assert!(
        status_p99 < STATUS_BUDGET,
        "services.status p99 {status_p99:?}"
    );

    // Import the file back: the restore's stdin is the dump.
    std::fs::write(bin.join("restore.txt"), "0 0").unwrap();
    let job = client
        .call::<db_import>(DbImportParams {
            name: "loja".into(),
            instance: Some("mysql/default".into()),
            file: result.path.clone(),
        })
        .await
        .unwrap();
    let done = finished(&client, job.id, Duration::from_secs(60)).await;
    assert_eq!(done.state, JobState::Done, "{:?}", done.error);
    assert_eq!(done.progress.pct, Some(100.0));
    let restored = std::fs::read(bin.join("restored.bin")).unwrap();
    assert_eq!(sha256(&restored), sha256(&plain));

    // Validation answers at once, before any job.
    for (params, code) in [
        (
            DbImportParams {
                name: "loja".into(),
                instance: None,
                file: "relative.sql".into(),
            },
            "invalid_input",
        ),
        (
            DbImportParams {
                name: "loja".into(),
                instance: None,
                file: root.path().join("missing.sql").display().to_string(),
            },
            "not_found",
        ),
        (
            DbImportParams {
                name: "a;b".into(),
                instance: None,
                file: result.path.clone(),
            },
            "invalid_input",
        ),
    ] {
        let err = api_error(client.call::<db_import>(params).await.unwrap_err());
        assert_eq!(err.code, code, "{err:?}");
    }

    // jobs.cancel at about half an export.
    let job = client
        .call::<db_export>(DbExportParams {
            name: "loja".into(),
            instance: None,
            output: Some(
                root.path()
                    .join("out")
                    .join("half.sql.zst")
                    .display()
                    .to_string(),
            ),
        })
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let now = client
            .call::<jobs_get>(JobIdParams { job_id: job.id })
            .await
            .unwrap();
        if now.progress.bytes_done.unwrap_or(0) >= (DUMP_64_MIB / 2) as u64 {
            break;
        }
        assert!(!now.state.is_terminal(), "{now:?}");
        assert!(Instant::now() < deadline, "the export never reached half");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let asked = Instant::now();
    client
        .call::<jobs_cancel>(JobIdParams { job_id: job.id })
        .await
        .unwrap();
    let done = finished(&client, job.id, Duration::from_secs(5)).await;
    let took = asked.elapsed();
    assert_eq!(done.state, JobState::Cancelled, "{:?}", done.error);
    assert!(took < Duration::from_secs(2), "cancel took {took:?}");
    assert!(gone(pid_in(&bin, "dump.pid")).await, "mysqldump survived");
    assert!(
        leftovers(&root.path().join("out")).is_empty(),
        "{:?}",
        leftovers(&root.path().join("out"))
    );
    assert!(
        children_of(&[daemon.pid()])
            .iter()
            .all(|(_, image)| image != "mysqldump.exe")
    );

    // A failing restore: job.failed with the stderr tail.
    std::fs::write(bin.join("restore.txt"), "0 1").unwrap();
    let job = client
        .call::<db_import>(DbImportParams {
            name: "loja".into(),
            instance: None,
            file: result.path.clone(),
        })
        .await
        .unwrap();
    let done = finished(&client, job.id, Duration::from_secs(60)).await;
    assert_eq!(done.state, JobState::Failed);
    let error = done.error.unwrap();
    assert_eq!(error.code, "conflict", "{error:?}");
    assert!(
        error.message.ends_with("restore error line 30"),
        "{error:?}"
    );
    assert!(
        error.message.contains("restore error line 11\n"),
        "{error:?}"
    );
    let failed = events
        .lock()
        .unwrap()
        .iter()
        .any(|(kind, p)| kind == "job.failed" && p["job_id"] == job.id.to_string().as_str());
    assert!(failed, "job.failed was published");

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(30)).await;
    assert!(status.success(), "{status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_without_database_tools_answers_empty_lists() {
    let root = TestRoot::new();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    let started = Instant::now();
    let list = client
        .call::<db_list>(DbListParams::default())
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(list.databases.is_empty() && list.instances.is_empty());
    for err in [
        client
            .call::<db_create>(DbCreateParams {
                name: "loja".into(),
                instance: None,
                charset: None,
            })
            .await
            .unwrap_err(),
        client
            .call::<db_url>(DbUrlParams {
                instance: "mysql/default".into(),
                database: "loja".into(),
            })
            .await
            .unwrap_err(),
    ] {
        assert_eq!(api_error(err).code, "not_found");
    }
    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(30)).await;
    assert!(status.success(), "{status}");
}
