//! `db.*` against the real servers (M2-E3-T5): the official MySQL 8.4.13 and, when its zip
//! is in the shared cache, PostgreSQL 17.11. The daemon boots on a temporary root with the
//! binaries copied into `bin/`, `<tool>/default` initializes its data folder and starts,
//! and the manifest-driven `db.list|create|drop|url|shell` run the real `mysql.exe` /
//! `psql.exe`: system databases hidden, case-sensitive names, `db.shell`'s command runs
//! against the server. Then the round trip of M2-E3-T6: a table of 1000 rows, `db.export`
//! (`mysqldump.exe` / `pg_dump.exe` → `.sql.zst`), drop, create, `db.import` (`mysql.exe` /
//! `psql.exe` fed through stdin) and the same row count.
//!
//! Needs the zips in `C:\apps_src\Forgebox-wt\cache` (or `FORGEBOX_TEST_CACHE`, sha256
//! checked) and the VC++ runtime:
//! `cargo test -p forge-daemon --test databases_online -- --ignored --nocapture`.
#![cfg(windows)]

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use forge_core::ids::JobId;
use forge_ipc::Client;
use forge_ipc::api::methods::{
    db_create, db_drop, db_export, db_import, db_list, db_shell, db_url, jobs_get, services_status,
};
use forge_ipc::api::{
    DbCreateParams, DbDropParams, DbExportDto, DbExportParams, DbImportParams, DbListParams,
    DbShellParams, DbUrlParams, JobDto, JobIdParams, JobState, NoParams, ServiceState,
};

use common::*;

/// The tail of every `*.log` under the root's `logs/` plus the daemon log (diagnostics).
fn service_logs(root: &TestRoot) -> String {
    let mut out = daemon_log(root);
    let mut pending = vec![root.root.logs()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "log") {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                let start = text.len().saturating_sub(3000);
                out.push_str(&format!(
                    "
== {} ==
{}",
                    path.display(),
                    text.get(start..).unwrap_or_default()
                ));
            }
        }
    }
    out
}

async fn wait_running(client: &Client, tool: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let status = client.call::<services_status>(NoParams {}).await.unwrap();
        if let Some(i) = status.instances.iter().find(|i| i.tool.as_str() == tool) {
            assert_ne!(i.state, ServiceState::Failed, "{tool} failed: {i:?}");
            if i.state == ServiceState::Running {
                return;
            }
        }
        assert!(Instant::now() < deadline, "{tool} never ran");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn job_done(client: &Client, id: JobId, root: &TestRoot) -> JobDto {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let job = client
            .call::<jobs_get>(JobIdParams { job_id: id })
            .await
            .unwrap();
        if job.state.is_terminal() {
            assert_eq!(
                job.state,
                JobState::Done,
                "{}: {:?}\n{}",
                job.name,
                job.error,
                service_logs(root)
            );
            return job;
        }
        assert!(Instant::now() < deadline, "{} never ended", job.name);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Runs `sql` with the shell command of `db.shell` on `database`; returns stdout.
async fn run_sql(
    client: &Client,
    instance: &str,
    database: &str,
    exec: fn(&str) -> Vec<String>,
    sql: &str,
) -> String {
    let shell = client
        .call::<db_shell>(DbShellParams {
            instance: instance.into(),
            database: Some(database.into()),
        })
        .await
        .unwrap();
    let out = std::process::Command::new(&shell.program)
        .args(&shell.args)
        .args(exec(sql))
        .current_dir(shell.cwd.as_deref().unwrap())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{sql}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Export → drop → create → import of a 1000-row table; the count survives.
async fn round_trip(
    client: &Client,
    root: &TestRoot,
    instance: &str,
    exec: fn(&str) -> Vec<String>,
) {
    let rows: Vec<String> = (1..=1000).map(|i| format!("({i},'row {i}')")).collect();
    run_sql(
        client,
        instance,
        "shop_2",
        exec,
        "CREATE TABLE t (id INT PRIMARY KEY, v VARCHAR(40))",
    )
    .await;
    run_sql(
        client,
        instance,
        "shop_2",
        exec,
        &format!("INSERT INTO t VALUES {}", rows.join(",")),
    )
    .await;
    let count = "SELECT COUNT(*) FROM t";
    assert_eq!(
        run_sql(client, instance, "shop_2", exec, count).await,
        "1000"
    );

    let job = client
        .call::<db_export>(DbExportParams {
            name: "shop_2".into(),
            instance: Some(instance.into()),
            output: None,
        })
        .await
        .unwrap();
    let done = job_done(client, job.id, root).await;
    let export: DbExportDto = serde_json::from_value(done.result.unwrap()).unwrap();
    assert!(export.path.ends_with(".sql.zst"), "{}", export.path);
    assert!(export.size_bytes > 0);
    let plain = zstd::decode_all(std::fs::read(&export.path).unwrap().as_slice()).unwrap();
    let text = String::from_utf8_lossy(&plain);
    assert!(text.contains("row 1000"), "the dump has the rows");

    client
        .call::<db_drop>(DbDropParams {
            name: "shop_2".into(),
            instance: Some(instance.into()),
            confirm: true,
        })
        .await
        .unwrap();
    client
        .call::<db_create>(DbCreateParams {
            name: "shop_2".into(),
            instance: Some(instance.into()),
            charset: None,
        })
        .await
        .unwrap();
    let job = client
        .call::<db_import>(DbImportParams {
            name: "shop_2".into(),
            instance: Some(instance.into()),
            file: export.path.clone(),
        })
        .await
        .unwrap();
    let done = job_done(client, job.id, root).await;
    assert_eq!(done.progress.pct, Some(100.0));
    assert_eq!(
        run_sql(client, instance, "shop_2", exec, count).await,
        "1000"
    );
}

fn mysql_exec(sql: &str) -> Vec<String> {
    vec![
        "--batch".into(),
        "--skip-column-names".into(),
        format!("--execute={sql}"),
    ]
}

fn psql_exec(sql: &str) -> Vec<String> {
    vec![
        "-At".into(),
        "-v".into(),
        "ON_ERROR_STOP=1".into(),
        "-c".into(),
        sql.into(),
    ]
}

/// The whole flow on one family. `shell_args` run the shell command non-interactively and
/// print the current database; `exec` runs a statement the same way.
async fn flow(
    root: &TestRoot,
    tool: &str,
    url_prefix: &str,
    shell_args: &[&str],
    exec: fn(&str) -> Vec<String>,
) {
    let mut daemon = spawn_daemon(root, &[]);
    let (client, _) = connect(root, &mut daemon, Duration::from_secs(60)).await;
    wait_running(&client, tool, Duration::from_secs(240)).await;
    let instance = format!("{tool}/default");

    // `running` means the health probe connected (TCP): the server may still drop the first
    // handshakes, so the first list is retried for a while.
    let deadline = Instant::now() + Duration::from_secs(60);
    let first = loop {
        match client
            .call::<db_list>(DbListParams {
                instance: Some(instance.as_str().into()),
            })
            .await
        {
            Ok(list) => break list,
            Err(err) => {
                assert!(
                    Instant::now() < deadline,
                    "db.list never worked: {err:?}
{}",
                    service_logs(root)
                );
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    };
    assert!(
        first.databases.is_empty(),
        "system databases are hidden: {:?}",
        first.databases
    );
    assert_eq!(first.instances.len(), 1);
    assert_eq!(first.instances[0].state, ServiceState::Running);

    for name in ["loja", "Loja", "shop_2"] {
        client
            .call::<db_create>(DbCreateParams {
                name: name.into(),
                instance: Some(instance.as_str().into()),
                charset: None,
            })
            .await
            .unwrap_or_else(|e| panic!("creating {name}: {e:?}"));
    }
    let listed = client
        .call::<db_list>(DbListParams::default())
        .await
        .unwrap();
    let mut names: Vec<_> = listed.databases.iter().map(|d| d.name.clone()).collect();
    names.sort();
    assert_eq!(
        names,
        ["Loja", "loja", "shop_2"],
        "names are case-sensitive"
    );

    let url = client
        .call::<db_url>(DbUrlParams {
            instance: instance.as_str().into(),
            database: "Loja".into(),
        })
        .await
        .unwrap();
    assert!(
        url.url.starts_with(url_prefix) && url.url.ends_with("/Loja"),
        "{}",
        url.url
    );

    // The shell command runs against the real server and sees the database.
    let shell = client
        .call::<db_shell>(DbShellParams {
            instance: instance.as_str().into(),
            database: Some("Loja".into()),
        })
        .await
        .unwrap();
    assert!(Path::new(&shell.program).is_file(), "{}", shell.program);
    let out = std::process::Command::new(&shell.program)
        .args(&shell.args)
        .args(shell_args)
        .current_dir(shell.cwd.as_deref().unwrap())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Loja");

    client
        .call::<db_drop>(DbDropParams {
            name: "Loja".into(),
            instance: Some(instance.as_str().into()),
            confirm: true,
        })
        .await
        .unwrap();
    let after = client
        .call::<db_list>(DbListParams::default())
        .await
        .unwrap();
    let mut names: Vec<_> = after.databases.iter().map(|d| d.name.clone()).collect();
    names.sort();
    assert_eq!(names, ["loja", "shop_2"]);

    round_trip(&client, root, &instance, exec).await;

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the official MySQL 8.4.13 zip (shared cache) and VC++ 2022"]
async fn mysql_8_4_13_online() {
    let mysql = online::extracted(&online::MYSQL_8_4);
    let root = TestRoot::new();
    online::copy_tree(&mysql, &root.root.bin().join("mysql").join("8.4.13"));
    // `post_install` (mkdir tmp/mysql, the pid files) does not run for a loose folder.
    std::fs::create_dir_all(root.root.tmp().join("mysql")).unwrap();
    flow(
        &root,
        "mysql",
        "mysql://root@127.0.0.1:",
        &[
            "--batch",
            "--skip-column-names",
            "--execute=SELECT DATABASE()",
        ],
        mysql_exec,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the official PostgreSQL 17.11 zip (shared cache) and VC++ 2022"]
async fn postgresql_17_11_online() {
    let cache = std::env::var_os("FORGEBOX_TEST_CACHE")
        .map_or_else(|| r"C:\apps_src\Forgebox-wt\cache".into(), Into::into);
    let cache: std::path::PathBuf = cache;
    if !cache.join(online::POSTGRESQL_17.file).is_file() {
        eprintln!("PostgreSQL zip not cached: skipped");
        return;
    }
    let pg = online::extracted(&online::POSTGRESQL_17);
    let root = TestRoot::new();
    let target = root.root.bin().join("postgresql").join("17.11");
    // Only what the server and psql need (the zip also carries pgAdmin and StackBuilder).
    for sub in ["bin", "lib", "share"] {
        online::copy_tree(&pg.join(sub), &target.join(sub));
    }
    flow(
        &root,
        "postgresql",
        "postgresql://postgres@127.0.0.1:",
        &["-At", "-c", "SELECT current_database()"],
        psql_exec,
    )
    .await;
}
