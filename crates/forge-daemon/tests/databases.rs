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
#![cfg(windows)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use forge_daemon::databases::Target;
use forge_ipc::api::methods::{
    db_create, db_drop, db_list, db_shell, db_url, jobs_get, services_status, services_stop,
};
use forge_ipc::api::{
    DbCreateParams, DbDropParams, DbListParams, DbShellParams, DbUrlParams, InstanceDto,
    JobIdParams, JobState, NoParams, ServiceState, ServiceTarget, ServiceTargetsParams,
};
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
