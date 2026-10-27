//! Regressions found by the M1 gate (`scripts/smoke.ps1`, ADR-0032) against the real
//! `forgeboxd` on a temporary root, offline:
//!
//! * `system.config_set ports.ranges.*` reaches the live port allocator (no restart), and
//!   a manifest `default` port outside the configured range is not used;
//! * the `tls` actor writes `certs/ca-bundle.pem` at start (system roots) and again when
//!   the local CA is created (local CA + system roots);
//! * `shell.env` with an explicit version (`forge exec --php 8.4`) returns the tool's
//!   `config.env` for that version (`PHPRC`, `PHP_INI_SCAN_DIR`) and
//!   `FORGEBOX_PHP_VERSION`.
//!
//! Tools are fakes compiled with rustc (`tests/fixtures/`), found by the manifests'
//! `discover` at boot like real installs.
#![cfg(windows)]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use forge_ipc::Client;
use forge_ipc::api::methods::{
    jobs_get, services_start, services_status, shell_env, system_config_set, tls_ca_export,
    tls_issue, tools_discover,
};
use forge_ipc::api::{
    ConfigSetParams, InstanceDto, JobIdParams, JobState, NoParams, ServiceState, ServiceTarget,
    ServiceTargetsParams, ShellEnvParams, ShellFormat, TlsHostParams,
};
use serde_json::json;

use common::*;

fn fake_mailpit() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| compile_fixture("fake_mailpit")).clone()
}

fn fake_php() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| compile_fixture("fake_php")).clone()
}

/// Appends lines to the `[ports.ranges]` table the test root's `forgebox.toml` ends with.
fn add_ranges(root: &TestRoot, lines: &str) {
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    std::fs::write(root.root.forgebox_toml(), format!("{toml}{lines}")).unwrap();
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
            JobState::Failed | JobState::Cancelled => {
                panic!("job {id} ended {:?}: {:?}", job.state, job.error)
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "job {id} still {:?}", job.state);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_running(client: &Client, name: &str, timeout: Duration) -> InstanceDto {
    let deadline = Instant::now() + timeout;
    loop {
        let instance = client
            .call::<services_status>(NoParams {})
            .await
            .unwrap()
            .instances
            .into_iter()
            .find(|i| i.tool.as_str() == "mailpit" && i.name == name);
        if let Some(i) = &instance {
            if i.state == ServiceState::Running {
                return i.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "mailpit/{name} not running: {instance:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn ports_of(instance: &InstanceDto) -> Vec<u16> {
    let mut ports: Vec<u16> = instance.ports.iter().map(|p| p.port).collect();
    ports.sort_unstable();
    ports
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn port_ranges_set_through_the_api_apply_without_a_restart() {
    let root = TestRoot::new();
    let mut boot = [free_port(), free_port()];
    boot.sort_unstable();
    add_ranges(
        &root,
        &format!(
            "mail = [{}, {}]
",
            boot[0], boot[1]
        ),
    );
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;

    // `forge config set ports.ranges.mail [c, d]` while the daemon runs (the M1 gate
    // moved the ports this way and the allocator kept the ranges read at boot).
    let mut moved = [free_port(), free_port()];
    moved.sort_unstable();
    client
        .call::<system_config_set>(ConfigSetParams {
            key: "ports.ranges.mail".into(),
            value: json!(moved),
        })
        .await
        .unwrap();

    // Mailpit appears afterwards (`forge tool discover`): its default instance gets its
    // ports from the new range. The manifest defaults (1025/8025) are outside it, so
    // they are not taken even when free.
    let dir = root.root.bin().join("mailpit").join("1.32.0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fake_mailpit(), dir.join("mailpit.exe")).unwrap();
    let job = client.call::<tools_discover>(NoParams {}).await.unwrap();
    wait_job(&client, job.id, Duration::from_secs(60)).await;
    let job = client
        .call::<services_start>(ServiceTargetsParams {
            targets: vec![ServiceTarget::from("mailpit/default")],
        })
        .await
        .unwrap();
    wait_job(&client, job.id, Duration::from_secs(60)).await;
    let instance = wait_running(&client, "default", Duration::from_secs(30)).await;
    assert_eq!(
        ports_of(&instance),
        moved,
        "the range set through the API applies without a restart (boot range {boot:?})"
    );

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tls_actor_writes_the_ca_bundle_at_start_and_with_a_new_ca() {
    let root = TestRoot::new();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
    let bundle_path = root.root.certs_ca_bundle();
    let read_bundle = |want: Option<String>| {
        let path = bundle_path.clone();
        async move {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if want.as_deref().is_none_or(|pem| text.contains(pem)) {
                        return text;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "{} missing or without {want:?}",
                    path.display()
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };
    let count = |text: &str| text.matches("-----BEGIN CERTIFICATE-----").count();

    // At start, before any CA exists: the roots the OS trusts (Microsoft's at least).
    let at_start = read_bundle(None).await;
    let system = count(&at_start);
    assert!(system >= 1, "system roots in the bundle");
    assert!(!at_start.contains('\r'), "LF only");

    // The first certificate creates the local CA: the bundle is rewritten with it.
    let job = client
        .call::<tls_issue>(TlsHostParams {
            host: "bundle.test".into(),
        })
        .await
        .unwrap();
    wait_job(&client, job.id, Duration::from_secs(60)).await;
    let ca = client.call::<tls_ca_export>(NoParams {}).await.unwrap();
    let with_ca = read_bundle(Some(ca.pem.trim().replace("\r\n", "\n"))).await;
    assert_eq!(
        count(&with_ca),
        system + 1,
        "local CA + the same system roots"
    );

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(30)).await;
    assert!(status.success(), "{status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shell_env_with_an_explicit_version_carries_its_config_env() {
    let root = TestRoot::new();
    let dir = root.root.bin().join("php").join("8.4.27");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fake_php(), dir.join("php.exe")).unwrap();
    std::fs::copy(fake_php(), dir.join("php-cgi.exe")).unwrap();

    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
    let env = client
        .call::<shell_env>(ShellEnvParams {
            dir: None,
            format: ShellFormat::Json,
            versions: BTreeMap::from([("php".to_owned(), "8.4".to_owned())]),
        })
        .await
        .unwrap();
    let etc = forge_core::paths::posix_string(&forge_core::platform::long_path(&root.root.etc()));
    let vars = &env.vars;
    assert_eq!(
        vars.get("FORGEBOX_PHP_VERSION").map(String::as_str),
        Some("8.4.27"),
        "{vars:?}"
    );
    let phprc = vars.get("PHPRC").expect("PHPRC");
    assert!(
        phprc
            .replace('\\', "/")
            .eq_ignore_ascii_case(&format!("{etc}/php/8.4")),
        "{phprc} vs {etc}"
    );
    let scan = vars.get("PHP_INI_SCAN_DIR").expect("PHP_INI_SCAN_DIR");
    assert!(
        scan.replace('\\', "/")
            .eq_ignore_ascii_case(&format!("{etc}/php/8.4/conf.d")),
        "{scan}"
    );
    // The version folder first, then the shims.
    assert!(
        env.path_prepend[0]
            .to_ascii_lowercase()
            .ends_with(r"bin\php\8.4.27"),
        "{:?}",
        env.path_prepend
    );
    assert!(
        env.path_prepend
            .last()
            .unwrap()
            .to_ascii_lowercase()
            .ends_with(r"bin\shims"),
        "{:?}",
        env.path_prepend
    );

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(30)).await;
    assert!(status.success(), "{status}");
}
