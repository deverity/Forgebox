//! Security extras (M2-E3-T11, ADR-0045) against a daemon booted **in this process**, so
//! the test can replace the certificate store (`MemoryTrustStore`: no OS store is ever
//! written) and the UAC (`FakeElevation`):
//!
//! * `tls.ca_uninstall`: `current_user` straight to the store, `local_machine` through the
//!   helper (`trust.remove_machine_root`), `tls.ca_untrusted` per root removed,
//!   idempotent;
//! * `services.kill_port_holder`: ends a real listener child; refuses without `confirm`,
//!   a pid that does not hold the port, the daemon itself, an operator and a remote
//!   client; publishes the audit event `port.holder_killed`;
//! * `sites.env`: `.env` + `.env.local` of a site linked from outside `www/`, secrets
//!   masked by default, `reveal` for an admin only, nothing but the site's two files
//!   reachable.
#![cfg(windows)]

mod common;

use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use clap::Parser;
use forge_core::config::ForgeConfig;
use forge_core::helper::{FakeElevation, HelperOp};
use forge_core::paths::{ForgeRoot, InitOptions};
use forge_core::platform::{self, CertThumbprint, MemoryTrustStore, TrustScope, TrustStore};
use forge_daemon::args::Args;
use forge_daemon::boot::{self, BootOptions, Booted};
use forge_daemon::instance_lock::InstanceLock;
use forge_ipc::api::methods::{
    jobs_get, services_kill_port_holder, sites_add, sites_env, tls_ca_install, tls_ca_uninstall,
};
use forge_ipc::api::{
    CaInstallParams, CaStatusDto, CaUninstallParams, EventEnvelope, EventFilter, JobIdParams,
    JobState, KillPortHolderParams, PortHolderKilledPayload, Role, SiteAddParams, SiteDto,
    SiteEnvDto, SiteEnvParams, SiteRef, TlsCaUntrustedPayload, TrustScopeDto,
};
use forge_ipc::auth::{Authenticator, IpcToken};
use forge_ipc::client::Subscription;
use forge_ipc::transport::{ConnectionInfo, InMemoryTransport, Transport, local_transport};
use forge_ipc::{Client, Credentials, Server, ServerHandle};
use futures::StreamExt;

use common::*;

const CLIENT_NAME: &str = "security-test";

/// A daemon running inside the test process, with fake UAC and trust store.
struct Running {
    booted: Booted,
    lock: InstanceLock,
    client: Client,
    elevation: Arc<FakeElevation>,
    trust: Arc<MemoryTrustStore>,
}

impl Running {
    async fn start(test_root: &TestRoot) -> Self {
        let root = ForgeRoot::new(platform::long_path(test_root.path())).unwrap();
        let lock = InstanceLock::acquire(&root).expect("instance lock");
        let config = ForgeConfig::load_or_default(&root.forgebox_toml()).unwrap();
        let report = root
            .init(InitOptions {
                case_sensitive: config.case_sensitive,
            })
            .unwrap();
        let args = Args::parse_from([
            "forgeboxd",
            "--root",
            &root.path().display().to_string(),
            "--registry",
            &repo_registry().display().to_string(),
            "--shutdown-timeout",
            "20",
        ]);
        let elevation = Arc::new(FakeElevation::accepting());
        let slot: Arc<OnceLock<Arc<MemoryTrustStore>>> = Arc::new(OnceLock::new());
        let made = Arc::clone(&slot);
        let boot_options = BootOptions {
            platform: Some(platform::current().with_elevation(elevation.clone())),
            trust_store: Some(Arc::new(move |helper| {
                let store = Arc::new(MemoryTrustStore::new(Some(helper)));
                let _ = made.set(Arc::clone(&store));
                store as Arc<dyn TrustStore>
            })),
            ..BootOptions::default()
        };
        let booted = boot::boot_with(args, root.clone(), &lock, config, report, boot_options)
            .await
            .expect("boot");
        let credentials = Credentials::local(&root, CLIENT_NAME, "0").unwrap();
        let client = Client::connect(local_transport(&root), credentials, options())
            .await
            .expect("client connects");
        let trust = Arc::clone(slot.get().expect("the daemon built its trust store"));
        Self {
            booted,
            lock,
            client,
            elevation,
            trust,
        }
    }

    /// A client of the daemon's router behind a server that grants `role`, on a local or
    /// remote in-memory transport.
    async fn client_as(&self, role: Role, remote: bool) -> (Client, ServerHandle) {
        let transport = Arc::new(if remote {
            InMemoryTransport::remote()
        } else {
            InMemoryTransport::new()
        });
        let server = Server::builder(Arc::new(FixedRole(role)))
            .router(forge_daemon::api::router(&self.booted.daemon))
            .build();
        let (handle, _task) = server.bind_and_spawn(transport.as_ref()).await.unwrap();
        let client = Client::new(
            transport as Arc<dyn Transport>,
            Credentials::new(IpcToken::generate().unwrap(), CLIENT_NAME, "0"),
            options(),
        );
        (client, handle)
    }

    async fn stop(self) {
        self.client.close();
        drop(self.client);
        self.booted
            .daemon
            .shutdown
            .request(forge_daemon::daemon::ShutdownRequest {
                reason: "user".into(),
                restart: false,
                keep_services: false,
            });
        forge_daemon::shutdown::run(self.booted, self.lock).await;
    }
}

/// Grants a fixed role to every connection.
struct FixedRole(Role);

impl Authenticator for FixedRole {
    fn authenticate(
        &self,
        _: &forge_ipc::api::HelloParams,
        _: &ConnectionInfo,
    ) -> forge_core::error::Result<Role> {
        Ok(self.0)
    }
}

async fn wait_job<T: serde::de::DeserializeOwned>(
    client: &Client,
    id: forge_core::ids::JobId,
) -> Result<T, String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let job = client
            .call::<jobs_get>(JobIdParams { job_id: id })
            .await
            .unwrap();
        match job.state {
            JobState::Done => {
                return Ok(serde_json::from_value(job.result.unwrap_or_default()).unwrap());
            }
            JobState::Failed | JobState::Cancelled => {
                return Err(job.error.map(|e| e.code).unwrap_or_default());
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "job {id} still {:?}", job.state);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn install(client: &Client, scope: TrustScopeDto) -> CaStatusDto {
    let job = client
        .call::<tls_ca_install>(CaInstallParams { scope })
        .await
        .unwrap();
    wait_job(client, job.id).await.unwrap()
}

async fn uninstall(client: &Client, scope: TrustScopeDto) -> Result<CaStatusDto, String> {
    let job = client
        .call::<tls_ca_uninstall>(CaUninstallParams { scope })
        .await
        .unwrap();
    wait_job(client, job.id).await
}

async fn next_event(sub: &mut Subscription<EventEnvelope>) -> EventEnvelope {
    tokio::time::timeout(Duration::from_secs(10), sub.next())
        .await
        .expect("event in time")
        .expect("stream open")
}

// ------------------------------------------------------------------ tls.ca_uninstall

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ca_uninstall_removes_from_each_store_once() {
    let root = TestRoot::new();
    let running = Running::start(&root).await;
    let client = &running.client;

    // Nothing trusted yet: uninstalling asks nobody and changes nothing.
    let status = uninstall(client, TrustScopeDto::CurrentUser).await.unwrap();
    assert!(!status.trusted_current_user);
    assert!(running.trust.calls().is_empty());

    let status = install(client, TrustScopeDto::CurrentUser).await;
    let thumbprint: CertThumbprint = status.thumbprint_sha1.clone().unwrap().parse().unwrap();
    assert!(status.trusted_current_user);
    let status = install(client, TrustScopeDto::LocalMachine).await;
    assert!(status.trusted_local_machine);
    assert!(running.trust.contains(thumbprint, TrustScope::LocalMachine));
    assert_eq!(
        running.elevation.ops(),
        vec![HelperOp::TrustAddMachineRoot {
            thumbprint: thumbprint.to_hex()
        }]
    );

    let mut events = client
        .subscribe_events(EventFilter::kinds(["tls.ca_untrusted"]), Some(0))
        .await
        .unwrap();

    // current_user: straight to the store (the Windows dialog), the machine untouched.
    let status = uninstall(client, TrustScopeDto::CurrentUser).await.unwrap();
    assert!(!status.trusted_current_user && status.trusted_local_machine);
    assert!(!running.trust.contains(thumbprint, TrustScope::CurrentUser));
    let event: TlsCaUntrustedPayload =
        serde_json::from_value(next_event(&mut events).await.event.payload).unwrap();
    assert_eq!(
        event,
        TlsCaUntrustedPayload {
            v: 1,
            scope: TrustScopeDto::CurrentUser,
            thumbprint_sha1: thumbprint.to_hex(),
        }
    );
    // Idempotent: the same status, no store call, no event.
    let calls = running.trust.calls().len();
    let again = uninstall(client, TrustScopeDto::CurrentUser).await.unwrap();
    assert_eq!(again, status);
    assert_eq!(running.trust.calls().len(), calls);

    // local_machine: through forgebox-helper (the fake UAC records the operation).
    let status = uninstall(client, TrustScopeDto::LocalMachine)
        .await
        .unwrap();
    assert!(!status.trusted_local_machine);
    assert_eq!(
        running.elevation.ops().last(),
        Some(&HelperOp::TrustRemoveMachineRoot {
            thumbprint: thumbprint.to_hex()
        })
    );
    let event: TlsCaUntrustedPayload =
        serde_json::from_value(next_event(&mut events).await.event.payload).unwrap();
    assert_eq!(event.scope, TrustScopeDto::LocalMachine);
    assert_eq!(event.thumbprint_sha1, thumbprint.to_hex());
    uninstall(client, TrustScopeDto::LocalMachine)
        .await
        .unwrap();
    assert_eq!(running.elevation.ops().len(), 2, "absent: no second prompt");
    assert!(
        tokio::time::timeout(Duration::from_millis(300), events.next())
            .await
            .is_err(),
        "one event per root removed"
    );
    // The CA itself stays (issuing goes on).
    assert!(
        running
            .booted
            .daemon
            .root
            .certs_ca()
            .join("forgebox-ca.crt")
            .is_file()
    );

    // Operators may not.
    let (operator, handle) = running.client_as(Role::Operator, false).await;
    let err = operator
        .call::<tls_ca_uninstall>(CaUninstallParams {
            scope: TrustScopeDto::CurrentUser,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "forbidden");
    operator.close();
    handle.shutdown();
    running.stop().await;
}

// ------------------------------------------------------------ services.kill_port_holder

/// The `port_listener` fixture running, and the port it holds.
fn listener() -> (Child, u16) {
    static EXE: OnceLock<std::path::PathBuf> = OnceLock::new();
    let exe = EXE.get_or_init(|| compile_fixture("port_listener"));
    let mut child = Command::new(exe)
        .stdout(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port = line
        .trim()
        .strip_prefix("listening ")
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("unexpected fixture output {line:?}"));
    (child, port)
}

fn kill(port: u16, pid: u32, confirm: bool) -> KillPortHolderParams {
    KillPortHolderParams { port, pid, confirm }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kill_port_holder_ends_only_a_checked_listener() {
    let root = TestRoot::new();
    let running = Running::start(&root).await;
    let client = &running.client;
    let (mut child, port) = listener();
    let pid = child.id();
    let mut events = client
        .subscribe_events(EventFilter::kinds(["port.holder_killed"]), Some(0))
        .await
        .unwrap();

    let code = |r: Result<forge_ipc::api::Ack, forge_ipc::client::ClientError>| {
        r.map(|_| "ok".to_owned())
            .unwrap_or_else(|e| e.code().to_owned())
    };
    // Without confirm.
    let r = client
        .call::<services_kill_port_holder>(kill(port, pid, false))
        .await;
    assert_eq!(code(r), "invalid_input");
    // A pid that does not hold this port (the port of nobody).
    let r = client
        .call::<services_kill_port_holder>(kill(free_port(), pid, true))
        .await;
    assert_eq!(code(r), "conflict");
    // The daemon itself (it runs in this test process).
    let r = client
        .call::<services_kill_port_holder>(kill(port, std::process::id(), true))
        .await;
    assert_eq!(code(r), "forbidden");
    // An operator, and a remote admin.
    for (role, remote) in [(Role::Operator, false), (Role::Admin, true)] {
        let (other, handle) = running.client_as(role, remote).await;
        let r = other
            .call::<services_kill_port_holder>(kill(port, pid, true))
            .await;
        assert_eq!(code(r), "forbidden", "{role} remote={remote}");
        other.close();
        handle.shutdown();
    }
    assert!(
        child.try_wait().unwrap().is_none(),
        "refused: still running"
    );

    // The real thing.
    let started = Instant::now();
    client
        .call::<services_kill_port_holder>(kill(port, pid, true))
        .await
        .unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(1));
    assert!(started.elapsed() < Duration::from_secs(6));
    let envelope = next_event(&mut events).await;
    let event: PortHolderKilledPayload = serde_json::from_value(envelope.event.payload).unwrap();
    assert_eq!((event.v, event.port, event.pid), (1, port, pid));
    assert_eq!(event.client.as_deref(), Some(CLIENT_NAME));
    let exe = event.exe.unwrap();
    assert!(
        Path::new(&exe)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("port_listener"),
        "{exe}"
    );
    // Gone: the pid no longer exists (or no longer holds the port).
    let r = client
        .call::<services_kill_port_holder>(kill(port, pid, true))
        .await;
    let after = code(r);
    assert!(
        matches!(after.as_str(), "not_found" | "conflict"),
        "after the kill: {after}"
    );
    running.stop().await;
}

// ------------------------------------------------------------------------ sites.env

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn site_env_masks_secrets_and_reveals_only_to_admins() {
    let root = TestRoot::new();
    // A linked site outside `www/`: a Laravel-like project served from `public/`.
    let outside = tempfile::Builder::new().prefix("fbe-").tempdir().unwrap();
    let project = outside.path().join("loja");
    std::fs::create_dir_all(project.join("public")).unwrap();
    std::fs::write(project.join("composer.json"), "{}").unwrap();
    std::fs::write(project.join("public/index.php"), "<?php echo 1;").unwrap();
    std::fs::write(
        project.join(".env"),
        "APP_NAME=Loja\nAPP_KEY=base64:c2VjcmV0\nDB_PASSWORD=\"s3cr3t\"\n\
         DATABASE_URL=mysql://root:pw@127.0.0.1:3306/loja\nDB_HOST=127.0.0.1\n",
    )
    .unwrap();
    std::fs::write(project.join(".env.local"), "APP_DEBUG=true\n").unwrap();
    std::fs::write(project.join(".env.production"), "DB_PASSWORD=prod\n").unwrap();
    let running = Running::start(&root).await;
    let client = &running.client;
    let job = client
        .call::<sites_add>(SiteAddParams {
            dir: project.join("public").display().to_string(),
            host: Some("loja.test".into()),
            php: None,
            server: None,
            secure: Some(false),
            driver: None,
        })
        .await
        .unwrap();
    let site: SiteDto = wait_job(client, job.id).await.unwrap();

    let env = |reveal| SiteEnvParams {
        site: SiteRef::from("loja.test"),
        reveal,
    };
    let masked: SiteEnvDto = client.call::<sites_env>(env(false)).await.unwrap();
    assert_eq!(
        (masked.site_id, masked.host.as_str()),
        (site.id, "loja.test")
    );
    assert!(!masked.revealed);
    let names: Vec<_> = masked.files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, [".env", ".env.local"], "nothing else is read");
    let var = |dto: &SiteEnvDto, key: &str| {
        dto.files[0]
            .vars
            .iter()
            .find(|v| v.key == key)
            .cloned()
            .unwrap_or_else(|| panic!("{key}"))
    };
    for key in ["APP_KEY", "DB_PASSWORD", "DATABASE_URL"] {
        let v = var(&masked, key);
        assert!(v.secret && v.value.is_none(), "{key} masked: {v:?}");
    }
    assert_eq!(var(&masked, "APP_NAME").value.as_deref(), Some("Loja"));
    assert_eq!(var(&masked, "DB_HOST").line, 5);
    assert!(
        masked.files[0].path.ends_with("loja/.env"),
        "{}",
        masked.files[0].path
    );
    // Masked values never leave the daemon.
    let wire = serde_json::to_string(&masked).unwrap();
    assert!(!wire.contains("s3cr3t") && !wire.contains("c2VjcmV0") && !wire.contains(":pw@"));

    let revealed: SiteEnvDto = client.call::<sites_env>(env(true)).await.unwrap();
    assert!(revealed.revealed);
    assert_eq!(
        var(&revealed, "DB_PASSWORD").value.as_deref(),
        Some("s3cr3t")
    );
    assert_eq!(
        var(&revealed, "APP_KEY").value.as_deref(),
        Some("base64:c2VjcmV0")
    );
    assert!(var(&revealed, "APP_KEY").secret);

    // An operator sees the masked view; reveal is forbidden. A remote operator likewise.
    for remote in [false, true] {
        let (operator, handle) = running.client_as(Role::Operator, remote).await;
        let seen: SiteEnvDto = operator.call::<sites_env>(env(false)).await.unwrap();
        assert_eq!(seen, masked);
        let err = operator.call::<sites_env>(env(true)).await.unwrap_err();
        assert_eq!(err.code(), "forbidden");
        operator.close();
        handle.shutdown();
    }

    // Only sites: a path, a file or a traversal is not a site.
    for reference in [
        project.join(".env").display().to_string(),
        "../../Windows/win.ini".to_owned(),
        r"C:\Windows\win.ini".to_owned(),
        "nosuch.test".to_owned(),
    ] {
        let err = client
            .call::<sites_env>(SiteEnvParams {
                site: SiteRef::from(reference.as_str()),
                reveal: true,
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err.code(), "not_found" | "invalid_input"),
            "{reference}: {err}"
        );
    }
    running.stop().await;
}
