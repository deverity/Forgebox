//! Advanced instance settings end to end (docs/07 §5.3, M2-E2-T10) against the real
//! `forgeboxd` on a temporary root, with the fake `mysqld.exe` of the quick settings tests
//! (`tests/fixtures/fake_mysqld.rs`: its version is the folder name, and each start appends
//! `version=… argv=… FB_T10=…` to `<datadir>/launch.log`):
//!
//! * `services.update` on a **running** instance stores `args`, `env` and `restart_policy`,
//!   restarts it and the fake sees the new argv (after the manifest's) and environment;
//!   `autostart` alone does not restart; clearing goes back to the manifest's launch.
//! * A version of the **same series** works (8.4.14 → 8.4.13); another series, installed or
//!   not, is `invalid_input` with the reason; a version of the series that is not installed
//!   is `not_found`; bad names/values are `invalid_input` and change nothing.
//! * Everything is in `instances.settings`: a daemon restart launches with it.
//! * `services.create --data-dir` uses that folder (initialized there, nothing under
//!   `data/`), bad folders are refused before any job, and `InstanceDto.size_bytes` is
//!   filled by the periodic scan while `services.status` stays under 50 ms.
#![cfg(windows)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use forge_core::ids::JobId;
use forge_ipc::api::methods::{
    jobs_get, services_create, services_settings, services_status, services_update,
};
use forge_ipc::api::{
    InstanceDto, JobDto, JobIdParams, JobState, LaunchPatchDto, NoParams, RestartOn,
    RestartPolicyPatchDto, ServiceCreateParams, ServiceState, ServiceTargetParams,
    ServiceUpdateParams, ToolSpec,
};
use forge_ipc::{ApiError, Client, ClientError};

use common::*;

const STATUS_BUDGET: Duration = Duration::from_millis(50);

fn fake_mysqld() -> PathBuf {
    static MYSQLD: OnceLock<PathBuf> = OnceLock::new();
    MYSQLD
        .get_or_init(|| compile_fixture("fake_mysqld"))
        .clone()
}

fn long(path: &Path) -> PathBuf {
    forge_core::platform::long_path(path)
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

async fn instances(client: &Client) -> Vec<InstanceDto> {
    client
        .call::<services_status>(NoParams {})
        .await
        .unwrap()
        .instances
}

async fn wait_running(client: &Client, name: &str, timeout: Duration) -> InstanceDto {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(found) = instances(client)
            .await
            .into_iter()
            .find(|i| i.tool.as_str() == "mysql" && i.name == name)
        {
            if found.state == ServiceState::Running {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "mysql/{name} is {:?}: {:?}",
                found.state,
                found.last_error
            );
        }
        assert!(Instant::now() < deadline, "mysql/{name} not listed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A root with three fake MySQL versions: 8.0.40, 8.4.13 and 8.4.14 (the newest, which the
/// default instance runs).
fn root_with_mysql_versions() -> TestRoot {
    let root = TestRoot::new();
    for version in ["8.0.40", "8.4.13", "8.4.14"] {
        let bin = root.root.bin().join("mysql").join(version).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::copy(fake_mysqld(), bin.join("mysqld.exe")).unwrap();
        std::fs::copy(fake_mysqld(), bin.join("mysqladmin.exe")).unwrap();
    }
    root
}

fn launch_log(root: &TestRoot) -> Vec<String> {
    let path = long(&root.root.data()).join("mysql/default/launch.log");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

async fn wait_launches(root: &TestRoot, count: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let lines = launch_log(root);
        if lines.len() >= count {
            return lines;
        }
        assert!(Instant::now() < deadline, "launch.log: {lines:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn update(instance: &str) -> ServiceUpdateParams {
    ServiceUpdateParams {
        instance: instance.into(),
        autostart: None,
        launch: None,
        restart_policy: None,
        version: None,
    }
}

async fn run_update(client: &Client, params: ServiceUpdateParams) -> InstanceDto {
    let job = client.call::<services_update>(params).await.unwrap();
    let done = wait_job(client, job.id, Duration::from_secs(90)).await;
    serde_json::from_value(done.result.expect("the job result")).unwrap()
}

async fn update_error(client: &Client, params: ServiceUpdateParams) -> ApiError {
    api_error(client.call::<services_update>(params).await.unwrap_err())
}

async fn advanced(client: &Client) -> forge_ipc::api::InstanceAdvancedDto {
    client
        .call::<services_settings>(ServiceTargetParams {
            target: "mysql/default".into(),
        })
        .await
        .unwrap()
        .advanced
        .expect("advanced for an admin")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn update_restarts_a_running_instance_with_the_new_launch_and_persists_it() {
    let root = root_with_mysql_versions();
    let before_pids = pids_of("mysqld.exe");
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    let first = wait_running(&client, "default", Duration::from_secs(60)).await;
    assert_eq!(first.version, "8.4.14");
    let lines = wait_launches(&root, 1).await;
    assert!(lines[0].starts_with("version=8.4.14 "), "{lines:?}");
    assert!(lines[0].ends_with("FB_T10=-"), "{lines:?}");

    // What the tab shows: defaults, and the versions of the same series only.
    let before = advanced(&client).await;
    assert!(before.args.is_empty() && before.env.is_empty());
    assert_eq!(before.restart_policy, before.restart_policy_default);
    assert_eq!(
        before.versions,
        ["8.4.14", "8.4.13"],
        "newest first, no 8.0"
    );

    // args + env + restart policy on the running instance: it restarts with them.
    let mut changed = update("mysql/default");
    changed.launch = Some(LaunchPatchDto {
        args: Some(vec!["--fb-extra=1".into(), "--fb-second".into()]),
        env: Some([("FB_T10".to_owned(), "alpha".to_owned())].into()),
    });
    changed.restart_policy = Some(RestartPolicyPatchDto {
        on: Some(RestartOn::Always),
        ..RestartPolicyPatchDto::default()
    });
    let updated = run_update(&client, changed).await;
    assert_eq!(updated.name, "default");
    let second = wait_running(&client, "default", Duration::from_secs(60)).await;
    assert_ne!(second.pid, first.pid, "restarted");
    let lines = wait_launches(&root, 2).await;
    let line = &lines[1];
    assert!(line.ends_with("FB_T10=alpha"), "{line}");
    let argv = line.split(" argv=").nth(1).unwrap();
    let manifest_end = argv.find("--console").expect("the manifest's arguments");
    let extra = argv
        .find("--fb-extra=1|--fb-second")
        .expect("the extra arguments");
    assert!(
        extra > manifest_end,
        "extra arguments come after the manifest's: {argv}"
    );
    let stored = advanced(&client).await;
    assert_eq!(stored.args, ["--fb-extra=1", "--fb-second"]);
    assert_eq!(stored.env["FB_T10"], "alpha");
    assert_eq!(stored.restart_policy.on, RestartOn::Always);
    assert_eq!(
        stored.restart_policy_default.on,
        before.restart_policy_default.on
    );

    // A patch keeps what it does not name: only the window changes.
    let mut window = update("mysql/default");
    window.restart_policy = Some(RestartPolicyPatchDto {
        window_secs: Some(120),
        ..RestartPolicyPatchDto::default()
    });
    run_update(&client, window).await;
    wait_running(&client, "default", Duration::from_secs(60)).await;
    let stored = advanced(&client).await;
    assert_eq!(stored.restart_policy.on, RestartOn::Always);
    assert_eq!(stored.restart_policy.window_secs, 120);
    assert_eq!(stored.args.len(), 2, "launch kept");
    wait_launches(&root, 3).await;

    // autostart alone does not touch the process.
    let pid = wait_running(&client, "default", Duration::from_secs(5))
        .await
        .pid;
    let mut auto = update("mysql/default");
    auto.autostart = Some(true);
    let result = run_update(&client, auto).await;
    assert!(result.autostart);
    assert_eq!(
        wait_running(&client, "default", Duration::from_secs(5))
            .await
            .pid,
        pid,
        "no restart"
    );
    assert_eq!(launch_log(&root).len(), 3);

    // Same series: 8.4.14 -> 8.4.13 (the fake prints the folder it runs from).
    let mut version = update("mysql/default");
    version.version = Some("8.4.13".into());
    let moved = run_update(&client, version).await;
    assert_eq!(moved.version, "8.4.13");
    wait_running(&client, "default", Duration::from_secs(60)).await;
    let lines = wait_launches(&root, 4).await;
    assert!(lines[3].starts_with("version=8.4.13 "), "{lines:?}");
    assert!(
        lines[3].contains("--fb-extra=1"),
        "settings survive: {lines:?}"
    );

    // Refusals change nothing: another series (installed or not), a missing version,
    // bad names and values.
    let pid = wait_running(&client, "default", Duration::from_secs(5))
        .await
        .pid;
    for wanted in ["8.0.40", "9.1.0", "8.0"] {
        let mut cross = update("mysql/default");
        cross.version = Some(wanted.into());
        let err = update_error(&client, cross).await;
        assert_eq!(err.code, "invalid_input", "{wanted}: {err:?}");
        assert!(err.message.contains("series"), "{wanted}: {}", err.message);
        assert!(err.message.contains("8.4"), "{wanted}: {}", err.message);
    }
    let mut missing = update("mysql/default");
    missing.version = Some("8.4.99".into());
    assert_eq!(update_error(&client, missing).await.code, "not_found");
    for launch in [
        LaunchPatchDto {
            args: None,
            env: Some([("PATH".to_owned(), "C:/x".to_owned())].into()),
        },
        LaunchPatchDto {
            args: None,
            env: Some([("A=B".to_owned(), "1".to_owned())].into()),
        },
        LaunchPatchDto {
            args: Some(vec!["bad\nline".into()]),
            env: None,
        },
    ] {
        let mut bad = update("mysql/default");
        bad.launch = Some(launch);
        assert_eq!(update_error(&client, bad).await.code, "invalid_input");
    }
    let mut bad = update("mysql/default");
    bad.restart_policy = Some(RestartPolicyPatchDto {
        window_secs: Some(0),
        ..RestartPolicyPatchDto::default()
    });
    assert_eq!(update_error(&client, bad).await.code, "invalid_input");
    assert_eq!(
        update_error(&client, update("mysql/nope")).await.code,
        "not_found"
    );
    assert_eq!(
        wait_running(&client, "default", Duration::from_secs(5))
            .await
            .pid,
        pid,
        "refusals never restart"
    );
    assert_eq!(launch_log(&root).len(), 4);

    // The row is the source: a new daemon launches with it.
    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    let again = wait_running(&client, "default", Duration::from_secs(60)).await;
    assert_eq!(again.version, "8.4.13");
    let lines = wait_launches(&root, 5).await;
    assert!(lines[4].starts_with("version=8.4.13 "), "{lines:?}");
    assert!(lines[4].ends_with("FB_T10=alpha"), "{lines:?}");
    let stored = advanced(&client).await;
    assert_eq!(stored.restart_policy.on, RestartOn::Always);
    assert!(stored.autostart);

    // Clearing: empty lists go back to the manifest's launch.
    let mut clear = update("mysql/default");
    clear.launch = Some(LaunchPatchDto {
        args: Some(Vec::new()),
        env: Some(Default::default()),
    });
    run_update(&client, clear).await;
    wait_running(&client, "default", Duration::from_secs(60)).await;
    let lines = wait_launches(&root, 6).await;
    assert!(!lines[5].contains("--fb-extra"), "{lines:?}");
    assert!(lines[5].ends_with("FB_T10=-"), "{lines:?}");
    let stored = advanced(&client).await;
    assert!(stored.args.is_empty() && stored.env.is_empty());

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
    let left: Vec<u32> = pids_of("mysqld.exe")
        .into_iter()
        .filter(|p| !before_pids.contains(p))
        .collect();
    assert!(left.is_empty(), "fake mysqld left running: {left:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_with_a_data_dir_uses_it_and_the_size_comes_from_the_scan() {
    let root = root_with_mysql_versions();
    let before_pids = pids_of("mysqld.exe");
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    wait_running(&client, "default", Duration::from_secs(60)).await;

    let create = |dir: &str| ServiceCreateParams {
        tool: ToolSpec::from("mysql"),
        name: "legacy".into(),
        port: None,
        autostart: false,
        data_dir: Some(dir.to_owned()),
    };
    // Refused before any job: relative, `..`, a drive root, inside bin/, the root.
    let elsewhere = tempfile::tempdir().unwrap();
    let wanted = elsewhere.path().join("mysql-legacy");
    let posix = |p: &Path| forge_core::paths::posix_string(p);
    let inside_bin = posix(&root.root.bin().join("mysql").join("8.4.14").join("data"));
    let up = format!("{}/../x", posix(elsewhere.path()));
    for bad in [
        "data\\legacy",
        "relative/dir",
        "C:\\",
        up.as_str(),
        inside_bin.as_str(),
        posix(root.root.path()).as_str(),
    ] {
        let err = api_error(
            client
                .call::<services_create>(create(bad))
                .await
                .map(|_| ())
                .expect_err(bad),
        );
        assert_eq!(err.code, "invalid_input", "{bad}: {err:?}");
        assert!(err.message.contains("data_dir"), "{bad}: {}", err.message);
    }
    assert!(
        instances(&client).await.iter().all(|i| i.name != "legacy"),
        "refused folders create nothing"
    );

    // Overlaps with the managed folders and with default's folder, both directions.
    let default_dir = instances(&client)
        .await
        .into_iter()
        .find(|i| i.name == "default")
        .and_then(|i| i.data_dir)
        .expect("default has a data folder");
    let parent = posix(Path::new(&default_dir).parent().unwrap());
    let inside = format!("{default_dir}/sub");
    for overlap in [
        parent.clone(),
        default_dir.clone(),
        inside,
        posix(&root.root.data()),
    ] {
        let err = api_error(
            client
                .call::<services_create>(create(&overlap))
                .await
                .map(|_| ())
                .expect_err(&overlap),
        );
        assert!(
            matches!(err.code.as_str(), "conflict" | "invalid_input"),
            "{overlap}: {err:?}"
        );
    }

    // A valid folder, outside the root, that does not exist yet.
    let job = client
        .call::<services_create>(create(&wanted.to_string_lossy()))
        .await
        .unwrap();
    let done = wait_job(&client, job.id, Duration::from_secs(60)).await;
    let created: InstanceDto = serde_json::from_value(done.result.unwrap()).unwrap();
    assert_eq!(created.data_dir.as_deref(), Some(posix(&wanted).as_str()));
    assert_eq!(created.size_bytes, None, "not measured yet");

    // Another instance cannot take the same folder.
    let mut again = create(&wanted.to_string_lossy());
    again.name = "other".into();
    let err = api_error(client.call::<services_create>(again).await.unwrap_err());
    assert_eq!(err.code, "conflict", "{err:?}");

    // Removing it with delete_data keeps the chosen folder.
    std::fs::create_dir_all(&wanted).unwrap();
    std::fs::write(wanted.join("keep.txt"), b"x").unwrap();
    let job = client
        .call::<forge_ipc::api::methods::services_remove>(forge_ipc::api::ServiceRemoveParams {
            instance: "mysql/legacy".into(),
            delete_data: true,
            confirm: true,
        })
        .await
        .unwrap();
    wait_job(&client, job.id, Duration::from_secs(60)).await;
    assert!(
        wanted.join("keep.txt").is_file(),
        "external folder survives"
    );
    std::fs::remove_dir_all(&wanted).unwrap();
    let job = client
        .call::<services_create>(create(&wanted.to_string_lossy()))
        .await
        .unwrap();
    wait_job(&client, job.id, Duration::from_secs(60)).await;

    // Starting it initializes the folder there, and nothing lands under data/.
    let job = client
        .call::<forge_ipc::api::methods::services_start>(forge_ipc::api::ServiceTargetsParams {
            targets: vec!["mysql/legacy".into()],
        })
        .await
        .unwrap();
    wait_job(&client, job.id, Duration::from_secs(90)).await;
    let running = wait_running(&client, "legacy", Duration::from_secs(60)).await;
    assert_eq!(running.data_dir.as_deref(), Some(posix(&wanted).as_str()));
    assert!(
        wanted.join("ibdata1").is_file(),
        "initialized in the chosen folder"
    );
    assert!(
        !long(&root.root.data()).join("mysql/legacy").exists(),
        "nothing under data/"
    );

    // size_bytes: filled by the scan, and services.status never waits for it.
    let started = Instant::now();
    let mut worst = Duration::ZERO;
    let mut size = None;
    while started.elapsed() < Duration::from_secs(20) {
        let asked = Instant::now();
        let all = instances(&client).await;
        worst = worst.max(asked.elapsed());
        size = all
            .iter()
            .find(|i| i.name == "legacy")
            .and_then(|i| i.size_bytes);
        if size.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    println!(
        "size scan filled after {:?}; services.status max {worst:?}",
        started.elapsed()
    );
    assert!(size.is_some_and(|s| s >= 4096), "{size:?}");
    assert!(worst < STATUS_BUDGET, "services.status took {worst:?}");
    let default = instances(&client)
        .await
        .into_iter()
        .find(|i| i.name == "default")
        .unwrap();
    assert!(default.data_dir.is_some());

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
    let left: Vec<u32> = pids_of("mysqld.exe")
        .into_iter()
        .filter(|p| !before_pids.contains(p))
        .collect();
    assert!(left.is_empty(), "fake mysqld left running: {left:?}");
}
