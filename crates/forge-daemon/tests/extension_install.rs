//! `kind: Extension` through the API (M2-E2-T8, ADR-0043) against the real `forgeboxd`
//! with the repository's registry (`xdebug.yaml`):
//!
//! * `tools.info xdebug`: `kind: extension`, `extends: php`, one installed row per target;
//! * `tools.install xdebug --for php@8.0`: `not_found` naming the compatible series (no
//!   download: the curated releases resolve offline); `--for` with a tool and a target of
//!   another tool are `invalid_input`;
//! * `tools.remove xdebug --for php@8.4`: only that DLL and its overlay go.
//!
//! The installations are seeded in the store before the daemon boots (the real
//! downloads are the ignored `xdebug_install_online`; the install pipeline itself is
//! `forge-provider/tests/extension.rs`). PHP versions are fake folders found by the
//! manifest's `discover` (`tests/fixtures/fake_php.rs`).
#![cfg(windows)]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use forge_core::ids::ToolId;
use forge_core::store::{
    NewToolVersion, Store, ToolVersionRepo, ToolVersionRow, VersionSource, VersionState,
};
use forge_ipc::api::methods::{jobs_get, tools_info, tools_install, tools_remove};
use forge_ipc::api::{
    CatalogKind, JobDto, JobIdParams, JobState, ToolInfoDto, ToolInstallParams,
    ToolInstallResultDto, ToolParams, ToolRemoveParams, ToolSpec,
};
use forge_ipc::{ApiError, Client, ClientError};

use common::*;

fn fake_php() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| compile_fixture("fake_php")).clone()
}

fn api_error(err: ClientError) -> ApiError {
    match err {
        ClientError::Api(api) => api,
        other => panic!("not an API error: {other:?}"),
    }
}

/// Waits for the job to end (any way) and returns its final snapshot.
async fn finish(client: &Client, job: JobDto, timeout: Duration) -> JobDto {
    let deadline = Instant::now() + timeout;
    loop {
        let now = client
            .call::<jobs_get>(JobIdParams { job_id: job.id })
            .await
            .unwrap();
        if now.state != JobState::Running {
            return now;
        }
        assert!(Instant::now() < deadline, "job {} still running", job.id);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn install_params(spec: &str, target: Option<&str>) -> ToolInstallParams {
    ToolInstallParams {
        specs: vec![ToolSpec::from(spec)],
        variant: None,
        yes: true,
        target: target.map(ToolSpec::from),
    }
}

fn remove_params(spec: &str, target: Option<&str>) -> ToolRemoveParams {
    ToolRemoveParams {
        spec: ToolSpec::from(spec),
        keep_config: false,
        confirm: true,
        target: target.map(ToolSpec::from),
    }
}

/// `bin/php/<folder>/` with the fake `php.exe`, registered like an installed version.
fn php_version(root: &TestRoot, store: &Store, folder: &str) -> ToolVersionRow {
    let dir = root.root.bin().join("php").join(folder);
    std::fs::create_dir_all(dir.join("ext")).unwrap();
    std::fs::copy(fake_php(), dir.join("php.exe")).unwrap();
    std::fs::copy(fake_php(), dir.join("php-cgi.exe")).unwrap();
    let mut parts = folder.split('-');
    let version = parts.next().unwrap().to_owned();
    let (ts, compiler, arch) = (
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
    );
    let repo = ToolVersionRepo::new(store.conn());
    let (id, _) = repo
        .record_install(
            &NewToolVersion {
                tool_id: ToolId::new("php").unwrap(),
                tool_name: "PHP".into(),
                version,
                canonical_name: folder.into(),
                variant: BTreeMap::from([
                    ("ts".to_owned(), ts.to_owned()),
                    ("compiler".to_owned(), compiler.to_owned()),
                    ("arch".to_owned(), arch.to_owned()),
                ]),
                path: forge_core::paths::posix_string(&dir),
                state: VersionState::Installed,
                source: VersionSource::Registry,
            },
            None,
            None,
        )
        .unwrap();
    repo.get(id).unwrap().unwrap()
}

/// What `Layout::install_extension` leaves behind for `target`: the DLL, the disabled
/// series overlay and the linked row.
fn seed_xdebug(root: &TestRoot, store: &Store, target: &ToolVersionRow, series: &str) {
    let dll = PathBuf::from(&target.path).join("ext/php_xdebug.dll");
    std::fs::write(&dll, format!("xdebug for {}", target.canonical_name)).unwrap();
    let overlay = root
        .root
        .etc()
        .join(format!("php/{series}/conf.d/20-xdebug.ini"));
    std::fs::create_dir_all(overlay.parent().unwrap()).unwrap();
    std::fs::write(&overlay, ";zend_extension = xdebug\n").unwrap();
    let mut variant = BTreeMap::from([
        ("php_series".to_owned(), series.to_owned()),
        ("ts".to_owned(), target.variant["ts"].clone()),
        ("compiler".to_owned(), target.variant["compiler"].clone()),
    ]);
    variant.insert("target_tool".into(), "php".into());
    variant.insert("target_id".into(), target.id.to_string());
    variant.insert("target_name".into(), target.canonical_name.clone());
    ToolVersionRepo::new(store.conn())
        .record_install(
            &NewToolVersion {
                tool_id: ToolId::new("xdebug").unwrap(),
                tool_name: "Xdebug".into(),
                version: "3.5.3".into(),
                canonical_name: format!("php-{}", target.canonical_name),
                variant,
                path: forge_core::paths::posix_string(&dll),
                state: VersionState::Installed,
                source: VersionSource::Registry,
            },
            None,
            None,
        )
        .unwrap();
}

fn root_without_parked_dirs() -> TestRoot {
    let root = TestRoot::new();
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    std::fs::write(
        root.root.forgebox_toml(),
        format!("parked_dirs = []\n{toml}"),
    )
    .unwrap();
    root
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extensions_are_listed_refused_and_removed_per_target() {
    let root = root_without_parked_dirs();
    let (php84, php83) = {
        std::fs::create_dir_all(root.root.db().parent().unwrap()).unwrap();
        let store = Store::open(&root.root.db()).unwrap();
        let php84 = php_version(&root, &store, "8.4.27-nts-vs17-x64");
        let php83 = php_version(&root, &store, "8.3.36-nts-vs16-x64");
        php_version(&root, &store, "8.0.30-nts-vs16-x64");
        seed_xdebug(&root, &store, &php84, "8.4");
        seed_xdebug(&root, &store, &php83, "8.3");
        (php84, php83)
    };
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;

    // tools.info: the extension with one row per target, newest target first.
    let info: ToolInfoDto = client
        .call::<tools_info>(ToolParams {
            tool: ToolId::new("xdebug").unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(info.kind, CatalogKind::Extension);
    assert_eq!(info.extends.as_ref().map(ToolId::as_str), Some("php"));
    assert!(!info.tool.has_service);
    let targets: Vec<&str> = info
        .tool
        .installed
        .iter()
        .map(|v| v.variant["target_name"].as_str())
        .collect();
    assert_eq!(targets, ["8.4.27-nts-vs17-x64", "8.3.36-nts-vs16-x64"]);
    assert!(
        info.tool
            .installed
            .iter()
            .all(|v| v.version == "3.5.3" && v.variant["target_tool"] == "php")
    );
    assert!(info.homepage.is_some() && info.license.is_some());

    // An incompatible explicit target: not_found naming the compatible series, offline.
    let job = client
        .call::<tools_install>(install_params("xdebug", Some("php@8.0")))
        .await
        .unwrap();
    let job = finish(&client, job, Duration::from_secs(30)).await;
    assert_eq!(job.state, JobState::Failed);
    let error = job.error.unwrap();
    assert_eq!(error.code, "not_found", "{}", error.message);
    assert!(
        error.message.contains("8.4 (vs17, nts)"),
        "{}",
        error.message
    );
    assert!(error.message.contains("8.0.30"), "{}", error.message);

    // A target that is not installed: version_not_installed, before any job.
    let err = api_error(
        client
            .call::<tools_install>(install_params("xdebug", Some("php@8.2")))
            .await
            .unwrap_err(),
    );
    assert_eq!(err.code, "not_found", "{err:?}");
    assert_eq!(
        err.details.as_ref().unwrap()["reason"],
        "version_not_installed",
        "{err:?}"
    );

    // Already installed into 8.4 with 3.5.3: nothing downloaded, the row comes back.
    let job = client
        .call::<tools_install>(install_params("xdebug@3.5.3", Some("php@8.4")))
        .await
        .unwrap();
    let job = finish(&client, job, Duration::from_secs(30)).await;
    assert_eq!(job.state, JobState::Done, "{:?}", job.error);
    let result: ToolInstallResultDto = serde_json::from_value(job.result.unwrap()).unwrap();
    assert_eq!(result.installed.len(), 1);
    assert_eq!(
        result.installed[0].canonical_name,
        "php-8.4.27-nts-vs17-x64"
    );

    // --for is for extensions only, and a target must be of the extended tool.
    let err = api_error(
        client
            .call::<tools_install>(install_params("php@8.4", Some("php@8.4")))
            .await
            .unwrap_err(),
    );
    assert_eq!(err.code, "invalid_input", "{err:?}");
    let err = api_error(
        client
            .call::<tools_install>(install_params("xdebug", Some("nginx@1.30")))
            .await
            .unwrap_err(),
    );
    assert_eq!(err.code, "invalid_input", "{err:?}");
    let err = api_error(
        client
            .call::<tools_remove>(remove_params("php@8.4", Some("php@8.4")))
            .await
            .unwrap_err(),
    );
    assert_eq!(err.code, "invalid_input", "{err:?}");

    // remove --for php@8.4: that DLL and overlay only.
    let job = client
        .call::<tools_remove>(remove_params("xdebug", Some("php@8.4")))
        .await
        .unwrap();
    let job = finish(&client, job, Duration::from_secs(30)).await;
    assert_eq!(job.state, JobState::Done, "{:?}", job.error);
    let dll = |row: &ToolVersionRow| PathBuf::from(&row.path).join("ext/php_xdebug.dll");
    assert!(!dll(&php84).exists());
    assert!(dll(&php83).is_file());
    let overlay = |series: &str| {
        root.root
            .etc()
            .join(format!("php/{series}/conf.d/20-xdebug.ini"))
    };
    assert!(!overlay("8.4").exists());
    assert!(overlay("8.3").is_file());
    assert!(PathBuf::from(&php84.path).join("php.exe").is_file());
    let info: ToolInfoDto = client
        .call::<tools_info>(ToolParams {
            tool: ToolId::new("xdebug").unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(info.tool.installed.len(), 1);
    assert_eq!(
        info.tool.installed[0].variant["target_name"],
        "8.3.36-nts-vs16-x64"
    );

    // Nothing left for 8.4.
    let err = api_error(
        client
            .call::<tools_remove>(remove_params("xdebug", Some("php@8.4")))
            .await
            .unwrap_err(),
    );
    assert_eq!(err.code, "not_found", "{err:?}");

    stop(&client, &mut daemon, Duration::from_secs(30)).await;
}

/// Real Xdebug 3.5.3 (xdebug.org, curated sha256) into a real PHP 8.4.27 NTS VS17: the
/// DLL lands in `ext/`, the overlay is rendered disabled, and PHP loads it when enabled
/// by hand (`-d zend_extension=...`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads Xdebug 3.5.3 from xdebug.org and uses the real PHP 8.4.27"]
async fn xdebug_install_online() {
    let root = root_without_parked_dirs();
    let folder = "8.4.27-nts-vs17-x64";
    let php_dir = root.root.bin().join("php").join(folder);
    online::copy_tree(&online::extracted(&online::PHP_8_4), &php_dir);
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;

    let started = Instant::now();
    let job = client
        .call::<tools_install>(install_params("xdebug", Some("php@8.4")))
        .await
        .unwrap();
    let job = finish(&client, job, Duration::from_secs(300)).await;
    assert_eq!(job.state, JobState::Done, "{:?}", job.error);
    let result: ToolInstallResultDto = serde_json::from_value(job.result.unwrap()).unwrap();
    println!(
        "installed in {:?}: {:?}",
        started.elapsed(),
        result.installed
    );
    assert_eq!(result.installed.len(), 1);
    assert_eq!(result.installed[0].version, "3.5.3");
    let dll = php_dir.join("ext/php_xdebug.dll");
    assert_eq!(
        online::sha256_hex(&dll),
        "a94d4fcfe4086a300b2decaf7c04e59f4981aa5e56992d48b55c5e49dfd9b9ee"
    );
    let overlay =
        std::fs::read_to_string(root.root.etc().join("php/8.4/conf.d/20-xdebug.ini")).unwrap();
    println!("{overlay}");
    assert!(
        overlay.contains("\n;zend_extension = xdebug\n"),
        "{overlay}"
    );

    // Enabled by hand, PHP loads it.
    let output = std::process::Command::new(php_dir.join("php.exe"))
        .arg("-n")
        .arg("-d")
        // Quoted: the temporary root may be an 8.3 short path, and `~` is INI syntax.
        .arg(format!(r#"zend_extension="{}""#, dll.display()))
        .arg("-m")
        .output()
        .unwrap();
    let modules = String::from_utf8_lossy(&output.stdout);
    println!("{modules}");
    assert!(
        modules.contains("Xdebug"),
        "{modules}{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // And remove --for takes it out again.
    let job = client
        .call::<tools_remove>(remove_params("xdebug", Some("php@8.4")))
        .await
        .unwrap();
    let job = finish(&client, job, Duration::from_secs(60)).await;
    assert_eq!(job.state, JobState::Done, "{:?}", job.error);
    assert!(!dll.exists());

    stop(&client, &mut daemon, Duration::from_secs(30)).await;
}
