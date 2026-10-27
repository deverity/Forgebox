//! `tools.install` through the whole pipeline and a service derived from its manifest:
//! `mailpit@1.32.0` is resolved by the manifest's provider (GitHub releases, with the
//! curated sha256 of `registry/tools/mailpit.yaml`), downloaded, verified, extracted and
//! registered; the daemon creates its `default` instance (ports from the `mail` range,
//! data dir, HTTP health). The instance is started through the API, the daemon is
//! restarted and brings it back by itself (`autostart: true`), and every stop leaves no
//! `mailpit.exe` behind.
//!
//! Network: GitHub API + a 10 MB download.
//! `cargo test -p forge-daemon --test mailpit_service_online -- --ignored --nocapture`.

mod common;

use std::time::{Duration, Instant};

use forge_ipc::Client;
use forge_ipc::api::methods::{jobs_get, services_start, services_status, tools_install};
use forge_ipc::api::{
    JobIdParams, JobState, NoParams, ServiceState, ServiceTarget, ServiceTargetsParams,
    ToolInstallParams, ToolInstallResultDto, ToolSpec,
};

use common::*;

async fn wait_job(
    client: &Client,
    id: forge_core::ids::JobId,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        let job = client
            .call::<jobs_get>(JobIdParams { job_id: id })
            .await
            .unwrap();
        match job.state {
            JobState::Done => return job.result.unwrap_or_default(),
            JobState::Failed | JobState::Cancelled => {
                panic!("job {id} ended {:?}: {:?}", job.state, job.error)
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "job {id} still {:?}", job.state);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn mailpit_running(client: &Client, timeout: Duration) -> forge_ipc::api::InstanceDto {
    let deadline = Instant::now() + timeout;
    loop {
        let status = client.call::<services_status>(NoParams {}).await.unwrap();
        if let Some(instance) = status
            .instances
            .into_iter()
            .find(|i| i.tool.as_str() == "mailpit" && i.state == ServiceState::Running)
        {
            return instance;
        }
        assert!(Instant::now() < deadline, "mailpit not running");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads mailpit 1.32.0 from GitHub"]
async fn installs_mailpit_and_runs_it_as_a_service_online() {
    let root = TestRoot::new();
    let before = pids_of("mailpit.exe");

    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
    let job = client
        .call::<tools_install>(ToolInstallParams {
            specs: vec![ToolSpec::from("mailpit@1.32.0")],
            variant: None,
            yes: true,
        })
        .await
        .unwrap();
    let started = Instant::now();
    let result: ToolInstallResultDto =
        serde_json::from_value(wait_job(&client, job.id, Duration::from_secs(600)).await).unwrap();
    println!(
        "installed in {:?}: {:?}",
        started.elapsed(),
        result.installed
    );
    assert_eq!(result.installed[0].version, "1.32.0");
    assert!(
        root.root
            .bin()
            .join("mailpit")
            .join("1.32.0")
            .join("mailpit.exe")
            .is_file()
    );

    // The default instance exists (created after the install) and starts on request.
    let job = client
        .call::<services_start>(ServiceTargetsParams {
            targets: vec![ServiceTarget::from("mailpit")],
        })
        .await
        .unwrap();
    wait_job(&client, job.id, Duration::from_secs(60)).await;
    let instance = mailpit_running(&client, Duration::from_secs(30)).await;
    println!(
        "mailpit/{} pid {:?} ports {:?}",
        instance.name,
        instance.pid,
        instance
            .ports
            .iter()
            .map(|p| (p.name.clone(), p.port))
            .collect::<Vec<_>>()
    );
    assert_eq!(instance.name, "default");
    assert!(instance.autostart);
    assert!(
        root.root.data().join("mailpit").join("default").is_dir(),
        "data dir created"
    );
    let http = instance
        .ports
        .iter()
        .find(|p| p.name == "http")
        .unwrap()
        .port;
    let body = reqwest_get(http).await;
    assert!(body.contains("Version"), "{body}");
    let first_pid = instance.pid.unwrap();

    let (exit, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    client.close();
    assert!(exit.success(), "{}", daemon.output());
    assert!(
        !pids_of("mailpit.exe").contains(&first_pid),
        "mailpit left running"
    );

    // Next boot: autostart brings it back.
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, ready) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    let instance = mailpit_running(&client, Duration::from_secs(5)).await;
    println!(
        "after restart: ready in {ready:?}, mailpit pid {:?}",
        instance.pid
    );
    let second_pid = instance.pid.unwrap();
    assert_ne!(second_pid, first_pid);
    let (exit, took) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    client.close();
    assert!(exit.success());
    println!("stopped with mailpit in {took:?}");
    let left: Vec<u32> = pids_of("mailpit.exe")
        .into_iter()
        .filter(|p| !before.contains(p))
        .collect();
    assert!(left.is_empty(), "mailpit left running: {left:?}");
    assert!(children_of(&[daemon.pid()]).is_empty());
}

/// `GET /api/v1/info` with a plain TCP client (no HTTP crate in the dev-dependencies).
async fn reqwest_get(port: u16) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    stream
        .write_all(b"GET /api/v1/info HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    out
}
