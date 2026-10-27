//! `system.import_laragon` through a real `forgeboxd` (M1-E5-T5): the job scans a copy of
//! the Laragon fixture of `forge-sites` (with fake `php.exe`/`mysqld.exe` compiled by this
//! test), a dry run changes nothing, the real run registers the tools and the sites,
//! saves the parked folder in `forgebox.toml` and adopts the database instance in the
//! running service table, and a second run imports nothing new.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use forge_ipc::Client;
use forge_ipc::api::methods::{
    jobs_get, services_status, sites_list, sites_parked, system_import_laragon,
};
use forge_ipc::api::{ImportLaragonParams, ImportReportDto, JobIdParams, JobState, NoParams};

use common::*;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../forge-sites/tests/fixtures")
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Compiles the fake tool with rustc and copies it next to each `fake-version` file.
fn place_fake_tools(dir: &Path, exe: &Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            place_fake_tools(&path, exe);
        } else if path.file_name().is_some_and(|n| n == "fake-version") {
            let text = std::fs::read_to_string(&path).unwrap();
            for name in text.lines().nth(1).unwrap_or("").split(',') {
                if !name.trim().is_empty() {
                    std::fs::copy(exe, path.with_file_name(name.trim())).unwrap();
                }
            }
        }
    }
}

fn build_fake_tool(out_dir: &Path) -> PathBuf {
    let exe = out_dir.join("fake_tool.exe");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .args(["--edition", "2021", "-O", "-o"])
        .arg(&exe)
        .arg(fixture().join("fake_tool.rs"))
        .output()
        .expect("running rustc");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    exe
}

async fn wait_report(
    client: &Client,
    id: forge_core::ids::JobId,
    timeout: Duration,
) -> ImportReportDto {
    let deadline = Instant::now() + timeout;
    loop {
        let job = client
            .call::<jobs_get>(JobIdParams { job_id: id })
            .await
            .unwrap();
        match job.state {
            JobState::Done => {
                return serde_json::from_value(job.result.expect("job result")).unwrap();
            }
            JobState::Failed | JobState::Cancelled => {
                panic!("job {id} ended {:?}: {:?}", job.state, job.error)
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "job {id} still {:?}", job.state);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn import(client: &Client, params: ImportLaragonParams) -> ImportReportDto {
    let job = client.call::<system_import_laragon>(params).await.unwrap();
    wait_report(client, job.id, Duration::from_secs(120)).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn imports_a_laragon_folder_into_the_running_daemon() {
    let scratch = tempfile::Builder::new()
        .prefix("fbd-laragon-")
        .tempdir()
        .unwrap();
    let laragon = forge_core::paths::plain_path(&std::fs::canonicalize(scratch.path()).unwrap())
        .join("laragon");
    copy_tree(&fixture().join("laragon"), &laragon);
    place_fake_tools(&laragon, &build_fake_tool(scratch.path()));
    let www = format!("{}/www", forge_core::paths::posix_string(&laragon));

    let root = TestRoot::new();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
    let params = ImportLaragonParams {
        path: laragon.display().to_string(),
        ..ImportLaragonParams::default()
    };

    // Dry run: decides, writes nothing.
    let dry = import(
        &client,
        ImportLaragonParams {
            dry_run: true,
            ..params.clone()
        },
    )
    .await;
    assert!(dry.dry_run);
    assert_eq!(
        dry.items
            .iter()
            .filter(|i| i.kind == "site" && i.imported)
            .count(),
        2
    );
    assert!(
        client
            .call::<sites_list>(NoParams {})
            .await
            .unwrap()
            .sites
            .is_empty()
    );
    assert!(
        client
            .call::<sites_parked>(NoParams {})
            .await
            .unwrap()
            .dirs
            .iter()
            .all(|d| d.dir != www)
    );
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    assert!(!toml.contains("laragon"), "{toml}");

    // The real import.
    let report = import(&client, params.clone()).await;
    assert!(!report.dry_run);
    let imported = |kind: &str| {
        report
            .items
            .iter()
            .filter(|i| i.kind == kind && i.imported)
            .count()
    };
    assert_eq!(imported("tool_version"), 3, "{report:#?}");
    assert_eq!(imported("site"), 2);
    assert_eq!(imported("database_dir"), 1);
    assert_eq!(imported("parked_dir"), 1);

    let sites = client.call::<sites_list>(NoParams {}).await.unwrap().sites;
    let hosts: Vec<&str> = sites.iter().map(|s| s.host.as_str()).collect();
    assert!(
        hosts.contains(&"app1.test") && hosts.contains(&"app-two.test"),
        "{hosts:?}"
    );
    let app1 = sites.iter().find(|s| s.host == "app1.test").unwrap();
    assert_eq!(app1.driver.as_deref(), Some("laravel"));
    assert_eq!(
        app1.php.as_ref().and_then(|p| p.series.as_deref()),
        Some("8.1"),
        "the active PHP of Laragon is pinned"
    );

    // The database instance is part of the running service table, over Laragon's folder.
    let status = client.call::<services_status>(NoParams {}).await.unwrap();
    let db = status
        .instances
        .iter()
        .find(|i| i.tool.as_str() == "mysql" && i.name == "mysql-8")
        .unwrap_or_else(|| panic!("{:#?}", status.instances));
    assert_eq!(db.version, "8.0.30");
    assert!(!db.autostart);

    // The parked folder was saved and reconciled.
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    assert!(toml.contains(&www), "{toml}");
    let parked = client.call::<sites_parked>(NoParams {}).await.unwrap();
    assert!(parked.dirs.iter().any(|d| d.dir == www), "{parked:?}");

    // Importing again creates nothing.
    let again = import(&client, params).await;
    assert!(
        again
            .items
            .iter()
            .filter(|i| matches!(i.kind.as_str(), "tool_version" | "site" | "database_dir"))
            .all(|i| !i.imported),
        "{again:#?}"
    );
    assert_eq!(
        client
            .call::<sites_list>(NoParams {})
            .await
            .unwrap()
            .sites
            .len(),
        sites.len()
    );

    // Laragon itself was only read.
    assert!(laragon.join("data/mysql-8/auto.cnf").is_file());

    let (exit, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(exit.success(), "{}", daemon.output());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_folder_that_is_not_laragon_fails_with_a_stable_code() {
    let root = TestRoot::new();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
    let empty = tempfile::tempdir().unwrap();
    let job = client
        .call::<system_import_laragon>(ImportLaragonParams {
            path: empty.path().display().to_string(),
            dry_run: true,
            ..ImportLaragonParams::default()
        })
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let failed = loop {
        let info = client
            .call::<jobs_get>(JobIdParams { job_id: job.id })
            .await
            .unwrap();
        if matches!(info.state, JobState::Failed) {
            break info;
        }
        assert!(Instant::now() < deadline, "{:?}", info.state);
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(failed.error.unwrap().code, "invalid_input");

    let relative = client
        .call::<system_import_laragon>(ImportLaragonParams {
            path: "laragon".into(),
            ..ImportLaragonParams::default()
        })
        .await
        .unwrap_err();
    assert!(relative.to_string().contains("absolute"), "{relative}");

    let (exit, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(exit.success());
}
