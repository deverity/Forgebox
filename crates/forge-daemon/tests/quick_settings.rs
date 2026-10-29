//! Quick settings end to end (docs/04 §1.3, ADR-0042, M2-E2-T7) against the real
//! `forgeboxd` on a temporary root:
//!
//! * **PHP series** (`php@8.4`, the fake `php.exe` found by `discover`): `services.settings`
//!   lists the five quick settings of `registry/tools/php.yaml`; `services.set` writes the
//!   overlay `conf.d/90-forgebox-settings.ini` (snapshot, `post_max_size` through
//!   `also_set`), never the user's `php.ini`, publishes `config.rendered`, and the same
//!   `request_id` returns the same job; invalid values are `invalid_input` naming the key
//!   and write nothing; an empty value goes back to the default.
//! * **An instance with an ini file** (`mysql/default`, `tests/fixtures/fake_mysqld.rs`
//!   reading its port from `my.ini`): values land inside the managed block, the user's
//!   text around it is byte for byte the same, the instance restarts (`apply: restart`);
//!   `ports.main` on a busy port is `port_in_use{pid, exe}` and writes nothing, on a free
//!   one the reservation moves (`services.ports`), `my.ini` is re-rendered and the
//!   service runs on the new port; a block edited by hand refuses the change and a render
//!   still reports `config.drift_detected`.
//! * **Concurrency** (docs/03 §7.6): `services.status` answers in < 50 ms during a
//!   `services.set ports.smtp=…` that restarts the stubborn fake Mailpit (CTRL_C and
//!   CTRL_BREAK ignored: the whole stop cascade runs).
//! * **Online** (ignored; the official PHP 8.4.27 zip): the set recycles the running pool
//!   (`pool.worker.recycled{reason: reload}`) and the new workers read `1G`.
#![cfg(windows)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use forge_core::ids::{JobId, RequestId};
use forge_ipc::api::methods::{
    jobs_get, php_pool_status, services_ports, services_reload, services_set, services_settings,
    services_status,
};
use forge_ipc::api::{
    EventFilter, EventsListParams, InstanceDto, JobDto, JobIdParams, JobState, NoParams,
    PhpPoolStatusParams, QuickSettingKind, ServiceSetParams, ServiceState, ServiceStatusDto,
    ServiceTarget, ServiceTargetParams, SettingApply, SettingValueDto,
};
use forge_ipc::client::CallOptions;
use forge_ipc::{ApiError, Client, ClientError};

use common::*;

const STATUS_BUDGET: Duration = Duration::from_millis(50);

fn fake(name: &str) -> PathBuf {
    static PHP: OnceLock<PathBuf> = OnceLock::new();
    static MYSQLD: OnceLock<PathBuf> = OnceLock::new();
    static MAILPIT: OnceLock<PathBuf> = OnceLock::new();
    let cell = match name {
        "fake_php" => &PHP,
        "fake_mysqld" => &MYSQLD,
        _ => &MAILPIT,
    };
    cell.get_or_init(|| compile_fixture(name)).clone()
}

fn long(path: &Path) -> PathBuf {
    forge_core::platform::long_path(path)
}

fn pairs(items: &[(&str, &str)]) -> BTreeMap<String, String> {
    items
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn set_params(target: &str, items: &[(&str, &str)]) -> ServiceSetParams {
    ServiceSetParams {
        target: ServiceTarget::from(target),
        settings: pairs(items),
    }
}

fn api_error(err: ClientError) -> ApiError {
    match err {
        ClientError::Api(api) => api,
        other => panic!("not an API error: {other:?}"),
    }
}

async fn wait_job(client: &Client, id: JobId, timeout: Duration) -> JobDto {
    let deadline = Instant::now() + timeout;
    loop {
        let job = client
            .call::<jobs_get>(JobIdParams { job_id: id })
            .await
            .unwrap();
        match job.state {
            JobState::Done => return job,
            JobState::Failed | JobState::Cancelled => {
                panic!("job {id} ended {:?}: {:?}", job.state, job.error)
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "job {id} still {:?}", job.state);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn instance(client: &Client, tool: &str) -> Option<InstanceDto> {
    client
        .call::<services_status>(NoParams {})
        .await
        .unwrap()
        .instances
        .into_iter()
        .find(|i| i.tool.as_str() == tool)
}

async fn wait_running(client: &Client, tool: &str, timeout: Duration) -> InstanceDto {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(found) = instance(client, tool).await {
            if found.state == ServiceState::Running {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "{tool} is {:?}: {:?}",
                found.state,
                found.last_error
            );
        }
        assert!(Instant::now() < deadline, "{tool} not listed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn port_of(instance: &InstanceDto, name: &str) -> u16 {
    instance
        .ports
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.port)
        .unwrap_or_else(|| panic!("no port {name}: {:?}", instance.ports))
}

async fn reserved_port(client: &Client, owner: &str, name: &str) -> Option<u16> {
    client
        .call::<services_ports>(NoParams {})
        .await
        .unwrap()
        .ports
        .into_iter()
        .find(|p| p.owner_name == owner && p.name == name)
        .map(|p| p.port)
}

async fn events_of(client: &Client, kind: &str) -> Vec<serde_json::Value> {
    client
        .events_list(EventsListParams {
            after_seq: 0,
            limit: Some(5000),
            filter: EventFilter::kinds([kind]),
        })
        .await
        .unwrap()
        .events
        .into_iter()
        .map(|e| e.payload)
        .collect()
}

/// Text outside the managed block `name` of `text` (before, after).
fn outside_block<'a>(text: &'a str, name: &str) -> (&'a str, &'a str) {
    let begin = text
        .find(&format!("# forgebox:begin {name} "))
        .unwrap_or_else(|| panic!("no block {name}:\n{text}"));
    let end_marker = format!("# forgebox:end {name}\n");
    let end = text.find(&end_marker).expect("end of block") + end_marker.len();
    (&text[..begin], &text[end..])
}

fn block_body(text: &str, name: &str) -> String {
    forge_render::managed::parse_blocks(text, forge_render::CommentStyle::Hash)
        .unwrap()
        .into_iter()
        .find(|b| b.name == name)
        .expect("block")
        .body
}

// ------------------------------------------------------------------------ PHP

/// A root with the fake PHP 8.4.27 in `bin/php/` and a `php.ini` of the user.
fn root_with_fake_php() -> (TestRoot, PathBuf) {
    let root = TestRoot::new();
    let dir = root.root.bin().join("php").join("8.4.27");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fake("fake_php"), dir.join("php.exe")).unwrap();
    std::fs::copy(fake("fake_php"), dir.join("php-cgi.exe")).unwrap();
    let ini = root.root.etc().join("php").join("8.4").join("php.ini");
    std::fs::create_dir_all(ini.parent().unwrap()).unwrap();
    std::fs::write(&ini, "; the user's php.ini\r\nmemory_limit = 64M\r\n").unwrap();
    (root, ini)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn php_series_settings_write_the_overlay_and_refuse_invalid_values() {
    let (root, user_ini) = root_with_fake_php();
    let user_bytes = std::fs::read(&user_ini).unwrap();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    let etc = long(&root.root.etc());
    let overlay = etc.join("php/8.4/conf.d/90-forgebox-settings.ini");

    // The five quick settings of php.yaml, with their defaults.
    let found = client
        .call::<services_settings>(ServiceTargetParams {
            target: "php@8.4".into(),
        })
        .await
        .unwrap();
    assert_eq!(found.target.as_str(), "php@8.4");
    assert_eq!(found.tool.as_str(), "php");
    let keys: Vec<&str> = found.settings.iter().map(|s| s.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "memory_limit",
            "upload_max_filesize",
            "max_execution_time",
            "display_errors",
            "opcache"
        ]
    );
    let memory = &found.settings[0];
    assert_eq!(memory.kind, QuickSettingKind::Select);
    assert_eq!(memory.value, Some(SettingValueDto::Text("512M".into())));
    assert!(memory.is_default);
    assert_eq!(memory.apply, SettingApply::Reload);
    assert_eq!(found.settings[1].also_set, ["post_max_size"]);
    assert_eq!(found.settings[2].value, Some(SettingValueDto::Int(120)));
    assert_eq!(found.settings[3].value, Some(SettingValueDto::Bool(true)));
    assert!(found.ports.is_empty() && found.advanced.is_none());
    let user = found.files.iter().find(|f| f.role == "php_ini").unwrap();
    assert!(!user.managed, "the user's php.ini is not Forgebox's");
    assert_eq!(user.fs_path.as_deref(), Some("etc/php/8.4/php.ini"));
    let file = found
        .files
        .iter()
        .find(|f| f.role == "php_ini.overlay")
        .unwrap();
    assert_eq!(
        file.fs_path.as_deref(),
        Some("etc/php/8.4/conf.d/90-forgebox-settings.ini")
    );

    // Set two values, one with an `also_set`; the same request id is the same job.
    let request = RequestId::new();
    let options = || CallOptions {
        request_id: Some(request),
        ..CallOptions::default()
    };
    let params = set_params(
        "php@8.4",
        &[("memory_limit", "1G"), ("upload_max_filesize", "256M")],
    );
    let job = client
        .call_with::<services_set>(params.clone(), options())
        .await
        .unwrap();
    let done = wait_job(&client, job.id, Duration::from_secs(30)).await;
    let status: ServiceStatusDto = serde_json::from_value(done.result.unwrap()).unwrap();
    assert!(status.pools.is_empty(), "no site, no running pool");
    let again = client
        .call_with::<services_set>(params, options())
        .await
        .unwrap();
    assert_eq!(again.id, job.id, "same request_id, same job");

    let written = std::fs::read_to_string(&overlay).unwrap();
    assert_eq!(
        written,
        "; generated by forgebox — edits will be lost; override via etc/templates/\n\
         memory_limit = 1G\n\
         upload_max_filesize = 256M\n\
         post_max_size = 256M\n\
         max_execution_time = 120\n\
         display_errors = On\n"
    );
    assert_eq!(
        std::fs::read(&user_ini).unwrap(),
        user_bytes,
        "php.ini untouched"
    );
    let base = std::fs::read_to_string(etc.join("php/8.4/conf.d/10-forgebox.ini")).unwrap();
    assert!(
        base.contains("memory_limit = 1G\n") && base.contains("post_max_size = 256M\n"),
        "the whole-file template renders the same values:\n{base}"
    );
    let rendered = events_of(&client, "config.rendered").await;
    assert!(
        rendered.iter().any(
            |p| p["fs_path"] == "etc/php/8.4/conf.d/90-forgebox-settings.ini"
                && p["owner"] == "php@8.4"
        ),
        "{rendered:?}"
    );
    let now = client
        .call::<services_settings>(ServiceTargetParams {
            target: "php@8.4.27".into(),
        })
        .await
        .unwrap();
    assert_eq!(now.target.as_str(), "php@8.4", "a version names its series");
    assert_eq!(
        now.settings[0].value,
        Some(SettingValueDto::Text("1G".into()))
    );
    assert!(!now.settings[0].is_default);

    // Invalid values: `invalid_input` naming the key, nothing written.
    let before = std::fs::read(&overlay).unwrap();
    for (items, key) in [
        (vec![("memory_limit", "3G")], "memory_limit"),
        (vec![("max_execution_time", "-5")], "max_execution_time"),
        (vec![("max_execution_time", "soon")], "max_execution_time"),
        (vec![("display_errors", "maybe")], "display_errors"),
        (
            vec![("memory_limit", "256M"), ("upload_max_filesize", "1\nG")],
            "upload_max_filesize",
        ),
        (vec![("memory_limit", "256M"), ("nope", "1")], "nope"),
        (vec![("ports.main", "3306")], "ports.main"),
    ] {
        let err = api_error(
            client
                .call::<services_set>(set_params("php@8.4", &items))
                .await
                .unwrap_err(),
        );
        assert_eq!(err.code, "invalid_input", "{items:?}: {err:?}");
        assert!(err.message.contains(key), "{items:?}: {}", err.message);
    }
    assert_eq!(std::fs::read(&overlay).unwrap(), before, "nothing written");
    let unchanged = client
        .call::<services_settings>(ServiceTargetParams {
            target: "php@8.4".into(),
        })
        .await
        .unwrap();
    assert_eq!(unchanged.settings, now.settings, "nothing stored");

    // An empty value is the default again.
    let job = client
        .call::<services_set>(set_params("php@8.4", &[("upload_max_filesize", "")]))
        .await
        .unwrap();
    wait_job(&client, job.id, Duration::from_secs(30)).await;
    let written = std::fs::read_to_string(&overlay).unwrap();
    assert!(
        written.contains("\nupload_max_filesize = 128M\npost_max_size = 128M\n")
            && written.contains("\nmemory_limit = 1G\n"),
        "{written}"
    );

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(30)).await;
    assert!(status.success(), "{status}");
}

// ---------------------------------------------------------------------- MySQL

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn instance_settings_and_port_moves_keep_the_users_text() {
    let root = TestRoot::new();
    let bin = root.root.bin().join("mysql").join("8.4.13").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(fake("fake_mysqld"), bin.join("mysqld.exe")).unwrap();
    std::fs::copy(fake("fake_mysqld"), bin.join("mysqladmin.exe")).unwrap();
    let before_pids = pids_of("mysqld.exe");
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    // `autostart: true` in the manifest: `mysql/default` comes up at boot.
    let first = wait_running(&client, "mysql", Duration::from_secs(60)).await;
    let port0 = port_of(&first, "main");
    let my_ini = long(&root.root.etc()).join("mysql/default/my.ini");

    // The user writes around the managed block (CRLF, as Notepad would).
    let rendered = std::fs::read_to_string(&my_ini).unwrap();
    let user_text =
        format!("# my header\r\n\r\n{rendered}\r\n[mysqld]\r\n# my tail\r\nlocal_infile = 0\r\n");
    std::fs::write(&my_ini, &user_text).unwrap();
    let (head, tail) = outside_block(&user_text, "main");
    let (head, tail) = (head.to_owned(), tail.to_owned());

    let found = client
        .call::<services_settings>(ServiceTargetParams {
            target: "mysql/default".into(),
        })
        .await
        .unwrap();
    assert_eq!(found.settings.len(), 4);
    assert_eq!(found.ports.len(), 1);
    assert_eq!(found.ports[0].name, "main");
    assert_eq!(found.ports[0].port, Some(port0));
    assert_eq!(found.ports[0].range.as_deref(), Some("databases"));

    // Values: inside the block only; restart (the manifest's default `apply`).
    let job = client
        .call::<services_set>(set_params(
            "mysql/default",
            &[
                ("max_connections", "300"),
                ("innodb_flush_log_at_trx_commit", "1"),
            ],
        ))
        .await
        .unwrap();
    let done = wait_job(&client, job.id, Duration::from_secs(90)).await;
    let status: ServiceStatusDto = serde_json::from_value(done.result.unwrap()).unwrap();
    assert_eq!(status.instances.len(), 1);
    let text = std::fs::read_to_string(&my_ini).unwrap();
    assert_eq!(outside_block(&text, "main"), (head.as_str(), tail.as_str()));
    let body = block_body(&text, "main");
    assert!(body.contains("\nmax_connections = 300\n"), "{body}");
    assert!(
        body.contains("\ninnodb_flush_log_at_trx_commit = 1\n"),
        "{body}"
    );
    assert!(
        forge_render::drift::detect_in_file_any_style(&my_ini)
            .unwrap()
            .is_empty(),
        "Forgebox's own write is no drift"
    );
    let restarted = wait_running(&client, "mysql", Duration::from_secs(60)).await;
    assert_ne!(restarted.pid, first.pid, "apply: restart");

    // Invalid: nothing written.
    let bytes = std::fs::read(&my_ini).unwrap();
    for (items, key) in [
        (vec![("max_connections", "0")], "max_connections"),
        (
            vec![("innodb_buffer_pool_size", "3G")],
            "innodb_buffer_pool_size",
        ),
        (vec![("ports.http", "8080")], "ports.http"),
    ] {
        let err = api_error(
            client
                .call::<services_set>(set_params("mysql/default", &items))
                .await
                .unwrap_err(),
        );
        assert_eq!(err.code, "invalid_input", "{err:?}");
        assert!(err.message.contains(key), "{}", err.message);
    }

    // A busy port: `port_in_use` with the holder, nothing written or moved.
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy_port = busy.local_addr().unwrap().port();
    let err = api_error(
        client
            .call::<services_set>(set_params(
                "mysql/default",
                &[("ports.main", &busy_port.to_string())],
            ))
            .await
            .unwrap_err(),
    );
    assert_eq!(err.code, "port_in_use", "{err:?}");
    let details = err.details.expect("details");
    assert_eq!(details["args"]["port"], busy_port);
    assert_eq!(details["args"]["pid"], std::process::id());
    let me = std::env::current_exe().unwrap();
    let exe = details["args"]["exe"]
        .as_str()
        .unwrap()
        .to_ascii_lowercase();
    assert!(
        exe.ends_with(
            &me.file_name()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase()
        ),
        "{exe}"
    );
    drop(busy);
    assert_eq!(std::fs::read(&my_ini).unwrap(), bytes, "nothing written");
    assert_eq!(
        reserved_port(&client, "mysql/default", "main").await,
        Some(port0)
    );

    // A free port: the reservation moves, my.ini is re-rendered, the service restarts
    // listening there.
    let port1 = free_port();
    let job = client
        .call::<services_set>(set_params(
            "mysql/default",
            &[("ports.main", &port1.to_string())],
        ))
        .await
        .unwrap();
    wait_job(&client, job.id, Duration::from_secs(90)).await;
    assert_eq!(
        reserved_port(&client, "mysql/default", "main").await,
        Some(port1),
        "services.ports shows the move"
    );
    let moved = wait_running(&client, "mysql", Duration::from_secs(60)).await;
    assert_eq!(port_of(&moved, "main"), port1);
    assert_ne!(moved.pid, restarted.pid);
    let text = std::fs::read_to_string(&my_ini).unwrap();
    assert_eq!(outside_block(&text, "main"), (head.as_str(), tail.as_str()));
    assert!(
        block_body(&text, "main").contains(&format!("\nport = {port1}\n")),
        "{text}"
    );
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port1)).is_ok(),
        "the service listens on the new port"
    );
    let settings = client
        .call::<services_settings>(ServiceTargetParams {
            target: "mysql/default".into(),
        })
        .await
        .unwrap();
    assert_eq!(settings.ports[0].port, Some(port1));

    // A block edited by hand: the change is refused and the file stays as the user left
    // it; a render still reports the drift.
    let edited = text.replace("max_connections = 300", "max_connections = 301");
    std::fs::write(&my_ini, &edited).unwrap();
    let err = api_error(
        client
            .call::<services_set>(set_params("mysql/default", &[("max_connections", "400")]))
            .await
            .unwrap_err(),
    );
    assert_eq!(err.code, "conflict", "{err:?}");
    assert!(err.message.contains("my.ini"), "{}", err.message);
    assert_eq!(std::fs::read_to_string(&my_ini).unwrap(), edited);
    let drifts_before = events_of(&client, "config.drift_detected").await.len();
    let job = client
        .call::<services_reload>(ServiceTargetParams {
            target: "mysql/default".into(),
        })
        .await
        .unwrap();
    wait_job(&client, job.id, Duration::from_secs(90)).await;
    assert_eq!(std::fs::read_to_string(&my_ini).unwrap(), edited, "kept");
    let drifts = events_of(&client, "config.drift_detected").await;
    assert!(drifts.len() > drifts_before, "{drifts:?}");
    assert!(
        drifts.last().unwrap()["fs_path"]
            .as_str()
            .is_some_and(|p| p.ends_with("mysql/default/my.ini")),
        "{drifts:?}"
    );

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
    let left: Vec<u32> = pids_of("mysqld.exe")
        .into_iter()
        .filter(|p| !before_pids.contains(p))
        .collect();
    assert!(left.is_empty(), "fake mysqld left running: {left:?}");
}

// ---------------------------------------------------------------- concurrency

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_answers_while_a_port_move_restarts_a_stubborn_service() {
    let root = TestRoot::new();
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    // The file ends inside `[ports.ranges]`.
    let toml = format!("{toml}mail = [{}, {}]\n", free_port(), free_port());
    std::fs::write(root.root.forgebox_toml(), toml).unwrap();
    let dir = root.root.bin().join("mailpit").join("1.32.0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fake("fake_mailpit"), dir.join("mailpit.exe")).unwrap();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    let first = wait_running(&client, "mailpit", Duration::from_secs(60)).await;

    let smtp = free_port();
    let job = client
        .call::<services_set>(set_params(
            "mailpit/default",
            &[("ports.smtp", &smtp.to_string())],
        ))
        .await
        .unwrap();
    let started = Instant::now();
    let mut worst = Duration::ZERO;
    let mut samples = 0u32;
    let finished = loop {
        let asked = Instant::now();
        client.call::<services_status>(NoParams {}).await.unwrap();
        worst = worst.max(asked.elapsed());
        samples += 1;
        let state = client
            .call::<jobs_get>(JobIdParams { job_id: job.id })
            .await
            .unwrap();
        if state.state.is_terminal() {
            break state;
        }
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "set still running"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let took = started.elapsed();
    println!("ports.smtp move took {took:?}; {samples} samples, services.status max {worst:?}");
    assert_eq!(finished.state, JobState::Done, "{:?}", finished.error);
    assert!(
        took >= Duration::from_secs(5),
        "the stubborn service went through its stop cascade ({took:?})"
    );
    assert!(samples > 50, "{samples} samples");
    assert!(worst < STATUS_BUDGET, "services.status took {worst:?}");
    assert_eq!(
        reserved_port(&client, "mailpit/default", "smtp").await,
        Some(smtp)
    );
    let now = wait_running(&client, "mailpit", Duration::from_secs(60)).await;
    assert_ne!(now.pid, first.pid);
    assert_eq!(port_of(&now, "smtp"), smtp);
    assert!(std::net::TcpStream::connect(("127.0.0.1", smtp)).is_ok());

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
}

// --------------------------------------------------------------------- online

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the official PHP 8.4.27 zip (downloaded into the shared cache) and VC++ 2022"]
async fn php_set_recycles_the_running_pool_online() {
    use forge_core::store::{Site, SiteKind, SiteRepo, Store};
    use forge_fcgi::probe::{FcgiRequest, request};

    let php = online::extracted(&online::PHP_8_4);
    let root = TestRoot::new();
    online::copy_tree(
        &php,
        &root.root.bin().join("php").join("8.4.27-nts-vs17-x64"),
    );
    let app = root.root.www().join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("index.php"),
        "<?php echo ini_get('memory_limit'), ' ', ini_get('upload_max_filesize'), ' ', \
         ini_get('post_max_size');\n",
    )
    .unwrap();
    {
        std::fs::create_dir_all(root.root.state()).unwrap();
        let store = Store::open(&root.root.db()).unwrap();
        let mut site = Site::new("app.test", "app-test");
        site.kind = SiteKind::Php;
        site.root = Some(forge_core::paths::posix_string(&app));
        site.driver = Some("generic-php".into());
        site.php.series = Some("8.4".into());
        site.secure = false;
        site.http_redirect = false;
        SiteRepo::new(store.conn()).insert(&site).unwrap();
        store.close().unwrap();
    }
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    std::fs::write(
        root.root.forgebox_toml(),
        format!("parked_dirs = []\n{toml}"),
    )
    .unwrap();
    let before: Vec<u32> = pids_of("php-cgi.exe");
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    let pool = |min: usize| {
        let client = &client;
        async move {
            let started = Instant::now();
            loop {
                let pools = client
                    .call::<php_pool_status>(PhpPoolStatusParams { version: None })
                    .await
                    .unwrap();
                if let Some(pool) = pools.pools.into_iter().find(|p| {
                    p.state == ServiceState::Running && p.ports.len() >= min && p.workers >= 2
                }) {
                    return pool;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(90),
                    "pool not running"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    let script = app.join("index.php");
    let ask = |port: u16| {
        let script = script.clone();
        let app = app.clone();
        async move {
            let addr = format!("127.0.0.1:{port}").parse().unwrap();
            request(
                addr,
                &FcgiRequest::get("/index.php", &script, &app),
                Duration::from_secs(10),
            )
            .await
            .expect("FastCGI request")
            .body_text()
        }
    };
    let running = pool(2).await;
    assert_eq!(ask(running.ports[0]).await, "512M 128M 128M");

    let job = client
        .call::<services_set>(set_params(
            "php@8.4",
            &[("memory_limit", "1G"), ("upload_max_filesize", "256M")],
        ))
        .await
        .unwrap();
    let done = wait_job(&client, job.id, Duration::from_secs(60)).await;
    let status: ServiceStatusDto = serde_json::from_value(done.result.unwrap()).unwrap();
    assert_eq!(status.pools.len(), 1, "the running pool of the series");
    // Every worker is replaced (rolling recycle) and the new ones read the overlay.
    let started = Instant::now();
    loop {
        let recycled = events_of(&client, "pool.worker.recycled").await;
        let reloads = recycled.iter().filter(|p| p["reason"] == "reload").count();
        if reloads >= running.ports.len() {
            println!("{reloads} workers recycled after {:?}", started.elapsed());
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "workers not recycled: {recycled:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // The status lists draining workers until they stop: wait for only new ones.
    let started = Instant::now();
    let after = loop {
        let now = pool(2).await;
        if now.ports.iter().all(|p| !running.ports.contains(p)) {
            break now;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "old workers still listed: {:?} -> {:?}",
            running.ports,
            now.ports
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    for port in &after.ports {
        assert_eq!(ask(*port).await, "1G 256M 256M", "worker on {port}");
    }

    let workers: Vec<u32> = pids_of("php-cgi.exe")
        .into_iter()
        .filter(|p| !before.contains(p))
        .collect();
    let (exit, _) = stop(&client, &mut daemon, Duration::from_secs(90)).await;
    assert!(exit.success(), "{}", daemon.output());
    let left: Vec<u32> = pids_of("php-cgi.exe")
        .into_iter()
        .filter(|p| workers.contains(p))
        .collect();
    assert!(left.is_empty(), "php-cgi left running: {left:?}");
}
