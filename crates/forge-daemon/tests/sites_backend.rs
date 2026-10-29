//! Sites backend gaps (M2-E2-T6) against the real `forgeboxd` on a temporary root, with
//! fake PHP versions (`tests/fixtures/fake_php.rs`: `php.exe` + a FastCGI `php-cgi.exe`):
//!
//! * `sites.php` to a version that is not installed: `not_found` with
//!   `version_not_installed` details, the site unchanged;
//! * a swap between two installed versions: one `site.php_changed {from, to}` between
//!   `job.started` and `job.done`;
//! * a pool that cannot start fails the job and the site keeps its PHP, while another
//!   site's broken pool does not fail the job of a healthy switch;
//! * `sites.driver` set then auto; `sites.info` checks; `tls.list`;
//! * `sites.ps` `instance_id` and `site_id` on the `service.*` events of app processes.
#![cfg(windows)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use forge_ipc::Client;
use forge_ipc::api::methods::{
    events_subscribe, jobs_get, sites_add, sites_driver, sites_info, sites_list, sites_php,
    sites_ps, sites_up, tls_ca_status, tls_issue, tls_list,
};
use forge_ipc::api::{
    CheckStatus, EventFilter, EventsSubscribeParams, JobDto, JobIdParams, JobState, NoParams,
    ProjectParams, SiteAddParams, SiteDriverParams, SiteDto, SiteInfoDto, SitePhpParams,
    SitePsParams, SiteRef, TlsHostParams,
};
use forge_ipc::client::SubscriptionItem;
use serde_json::Value;

use common::*;

fn fake_php() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| compile_fixture("fake_php")).clone()
}

/// `bin/php/<version>/` with the fake `php.exe` and `php-cgi.exe`; `fails` makes its
/// FastCGI worker exit at once.
fn install_php(root: &TestRoot, version: &str, fails: bool) {
    let dir = root.root.bin().join("php").join(version);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fake_php(), dir.join("php.exe")).unwrap();
    std::fs::copy(fake_php(), dir.join("php-cgi.exe")).unwrap();
    if fails {
        std::fs::write(dir.join("cgi-fails"), "x").unwrap();
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A Laravel-looking project (`artisan`, `composer.json`, `public/index.php`).
fn laravel_project(root: &TestRoot, name: &str) -> PathBuf {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../forge-sites/tests/fixtures/projects/laravel");
    let dir = root.path().join("projects").join(name);
    copy_tree(&fixture, &dir);
    forge_core::platform::long_path(&dir)
}

/// Root with `parked_dirs = []` (the projects are added by hand) and the fake PHPs.
fn root_with(php: &[(&str, bool)]) -> TestRoot {
    let root = TestRoot::new();
    for (version, fails) in php {
        install_php(&root, version, *fails);
    }
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    std::fs::write(
        root.root.forgebox_toml(),
        format!("parked_dirs = []\n{toml}"),
    )
    .unwrap();
    root
}

/// Waits for the job to end (any way) and returns its final snapshot.
async fn finish(client: &Client, job: JobDto) -> JobDto {
    let deadline = Instant::now() + Duration::from_secs(120);
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

fn site_of(job: &JobDto) -> SiteDto {
    assert_eq!(job.state, JobState::Done, "{:?}", job.error);
    serde_json::from_value(job.result.clone().expect("job result")).unwrap()
}

/// Events collected in the background: `(seq, kind, payload)`.
#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<(u64, String, Value)>>>);

impl Events {
    async fn collect(client: &Client, kinds: &[&str]) -> Self {
        let events = Self::default();
        let mut sub = client
            .subscribe::<events_subscribe>(EventsSubscribeParams {
                filter: EventFilter::kinds(kinds.iter().copied()),
                resume_from: None,
            })
            .await
            .unwrap();
        let sink = events.clone();
        tokio::spawn(async move {
            while let Some(item) = sub.next_item().await {
                if let SubscriptionItem::Item(envelope) = item {
                    sink.0.lock().unwrap().push((
                        envelope.event.seq,
                        envelope.event.kind,
                        envelope.event.payload,
                    ));
                }
            }
        });
        events
    }

    fn all(&self) -> Vec<(u64, String, Value)> {
        self.0.lock().unwrap().clone()
    }

    /// Waits until `wanted` holds for the events collected so far.
    async fn wait(&self, what: &str, wanted: impl Fn(&[(u64, String, Value)]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !wanted(&self.all()) {
            assert!(
                Instant::now() < deadline,
                "{what}: {:#?}",
                self.all().iter().map(|e| (&e.1, &e.2)).collect::<Vec<_>>()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Seq of the first event of `kind` for which `pick` holds.
    fn seq_of(&self, kind: &str, pick: impl Fn(&Value) -> bool) -> Option<u64> {
        self.all()
            .iter()
            .find(|(_, k, p)| k == kind && pick(p))
            .map(|e| e.0)
    }

    fn count(&self, kind: &str) -> usize {
        self.all().iter().filter(|(_, k, _)| k == kind).count()
    }
}

async fn sites(client: &Client) -> Vec<SiteDto> {
    client.call::<sites_list>(NoParams {}).await.unwrap().sites
}

async fn site(client: &Client, host: &str) -> SiteDto {
    sites(client)
        .await
        .into_iter()
        .find(|s| s.host == host)
        .unwrap_or_else(|| panic!("{host} not listed"))
}

async fn add_site(client: &Client, dir: &Path, host: &str, php: &str) -> SiteDto {
    let job = client
        .call::<sites_add>(SiteAddParams {
            dir: forge_core::paths::posix_string(dir),
            host: Some(host.into()),
            php: Some(php.into()),
            server: None,
            secure: Some(false),
            driver: None,
        })
        .await
        .unwrap();
    site_of(&finish(client, job).await)
}

fn resolved(site: &SiteDto) -> Option<String> {
    site.php.as_ref().and_then(|p| p.resolved.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn php_switch_is_validated_announced_and_scoped_to_its_site() {
    let root = root_with(&[("8.3.36", false), ("8.4.27", false), ("8.2.30", true)]);
    let app = laravel_project(&root, "app");
    let other = laravel_project(&root, "other");
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    let events = Events::collect(&client, &["job.*", "site.php_changed"]).await;

    let added = add_site(&client, &app, "app.test", "8.3").await;
    assert_eq!(resolved(&added).as_deref(), Some("8.3.36"));
    // A neighbour whose PHP can never start (8.2's worker exits at once).
    let neighbour = add_site(&client, &other, "other.test", "8.2").await;
    assert_eq!(resolved(&neighbour).as_deref(), Some("8.2.30"));

    // 1. A version that is not installed: not_found with the installed list; no change.
    let err = client
        .call::<sites_php>(SitePhpParams {
            site: SiteRef::from("app.test".to_owned()),
            php: "9.9".into(),
        })
        .await
        .expect_err("9.9 is not installed");
    assert_eq!(err.code(), "not_found");
    let api = err.api().expect("api error");
    assert_eq!(api.reason(), Some("version_not_installed"), "{api:?}");
    let args = &api.details.as_ref().unwrap()["args"];
    assert_eq!(args["tool"], "php");
    assert_eq!(args["requested"], "9.9");
    assert_eq!(args["fix"], "forge tool install php@9.9");
    let mut installed: Vec<String> = args["installed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    installed.sort();
    assert_eq!(installed, ["8.2.30", "8.3.36", "8.4.27"]);
    assert_eq!(
        resolved(&site(&client, "app.test").await).as_deref(),
        Some("8.3.36")
    );
    assert_eq!(events.count("site.php_changed"), 0);

    // 2. Between two installed versions: one site.php_changed, between started and done.
    // (other.test's pool is broken the whole time and must not fail this job.)
    let job = client
        .call::<sites_php>(SitePhpParams {
            site: SiteRef::from("app.test".to_owned()),
            php: "8.4".into(),
        })
        .await
        .unwrap();
    let id = serde_json::json!(job.id);
    let done = finish(&client, job).await;
    let switched = site_of(&done);
    assert_eq!(resolved(&switched).as_deref(), Some("8.4.27"));
    assert_eq!(
        switched.php.as_ref().unwrap().series.as_deref(),
        Some("8.4")
    );
    events
        .wait("job.done and site.php_changed", |all| {
            all.iter()
                .any(|(_, k, p)| k == "job.done" && p["job_id"] == id)
                && all.iter().any(|(_, k, _)| k == "site.php_changed")
        })
        .await;
    assert_eq!(events.count("site.php_changed"), 1);
    let started = events
        .seq_of("job.started", |p| p["job_id"] == id)
        .expect("job.started");
    let changed = events.seq_of("site.php_changed", |_| true).unwrap();
    let ended = events
        .seq_of("job.done", |p| p["job_id"] == id)
        .expect("job.done");
    assert!(
        started < changed && changed < ended,
        "{started} {changed} {ended}"
    );
    let payload = &events
        .all()
        .into_iter()
        .find(|(_, k, _)| k == "site.php_changed")
        .unwrap()
        .2;
    assert_eq!(payload["from"], "8.3.36");
    assert_eq!(payload["to"], "8.4.27");
    assert_eq!(payload["host"], "app.test");
    assert_eq!(payload["site_id"], serde_json::json!(switched.id));

    // The same version again changes nothing and announces nothing.
    let job = client
        .call::<sites_php>(SitePhpParams {
            site: SiteRef::from("app.test".to_owned()),
            php: "8.4.27".into(),
        })
        .await
        .unwrap();
    site_of(&finish(&client, job).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(events.count("site.php_changed"), 1);

    // 3. A pool that cannot start fails the job; the site keeps its PHP.
    let job = client
        .call::<sites_php>(SitePhpParams {
            site: SiteRef::from("app.test".to_owned()),
            php: "8.2".into(),
        })
        .await
        .unwrap();
    let failed_id = serde_json::json!(job.id);
    let failed = finish(&client, job).await;
    assert_eq!(failed.state, JobState::Failed, "{failed:?}");
    let error = failed.error.expect("job error");
    println!("pool failure reported as {}: {}", error.code, error.message);
    assert!(!error.message.is_empty());
    let kept = site(&client, "app.test").await;
    assert_eq!(resolved(&kept).as_deref(), Some("8.4.27"));
    assert_eq!(kept.php.as_ref().unwrap().series.as_deref(), Some("8.4"));
    events
        .wait("job.failed", |all| {
            all.iter()
                .any(|(_, k, p)| k == "job.failed" && p["job_id"] == failed_id)
        })
        .await;
    assert_eq!(
        events.count("site.php_changed"),
        1,
        "no event for a failed swap"
    );

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}\n{}", daemon.output());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn driver_override_info_checks_and_certificates() {
    let root = root_with(&[("8.4.27", false)]);
    let app = laravel_project(&root, "shop");
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;

    // Laravel is detected; its root is public/.
    let added = add_site(&client, &app, "shop.test", "8.4").await;
    assert_eq!(added.driver.as_deref(), Some("laravel"));
    assert!(added.root.as_deref().unwrap().ends_with("/shop/public"));

    // Override: the driver and the document root change, then auto restores them.
    let job = client
        .call::<sites_driver>(SiteDriverParams {
            site: SiteRef::from("shop.test".to_owned()),
            driver: Some("wordpress".into()),
        })
        .await
        .unwrap();
    let forced = site_of(&finish(&client, job).await);
    assert_eq!(forced.driver.as_deref(), Some("wordpress"));
    assert!(
        forced.root.as_deref().unwrap().ends_with("/shop"),
        "{:?}",
        forced.root
    );
    assert_eq!(
        site(&client, "shop.test").await.driver.as_deref(),
        Some("wordpress")
    );
    let job = client
        .call::<sites_driver>(SiteDriverParams {
            site: SiteRef::from("shop.test".to_owned()),
            driver: None,
        })
        .await
        .unwrap();
    let auto = site_of(&finish(&client, job).await);
    assert_eq!(auto.driver.as_deref(), Some("laravel"));
    assert!(auto.root.as_deref().unwrap().ends_with("/shop/public"));
    // An unknown driver is refused before any job and nothing changes.
    let err = client
        .call::<sites_driver>(SiteDriverParams {
            site: SiteRef::from("shop.test".to_owned()),
            driver: Some("nope".into()),
        })
        .await
        .expect_err("unknown driver");
    assert_eq!(err.code(), "invalid_input");
    assert_eq!(
        site(&client, "shop.test").await.driver.as_deref(),
        Some("laravel")
    );

    // sites.info: the four checks.
    let info: SiteInfoDto = client
        .call::<sites_info>(forge_ipc::api::SiteParams {
            site: SiteRef::from("shop.test".to_owned()),
        })
        .await
        .unwrap();
    let ids: Vec<&str> = info.checks.iter().map(|c| c.id.as_str()).collect();
    for wanted in [
        "site.dns",
        "site.certificate",
        "site.php_pool",
        "site.root_access",
    ] {
        assert!(ids.contains(&wanted), "{wanted} in {ids:?}");
    }
    let check = |id: &str| info.checks.iter().find(|c| c.id == id).unwrap();
    // Readable; the case flag is not set on a temp folder (Warn when the config wants it).
    let access = check("site.root_access");
    assert!(
        matches!(access.status, CheckStatus::Ok | CheckStatus::Warn),
        "{access:?}"
    );
    assert!(
        access.message.contains("readable, case-"),
        "{}",
        access.message
    );
    assert_eq!(
        check("site.php_pool").status,
        CheckStatus::Ok,
        "{:?}",
        info.checks
    );
    assert!(check("site.php_pool").message.contains("8.4.27"));
    // The site is HTTP only: no certificate needed.
    assert_eq!(check("site.certificate").status, CheckStatus::Ok);

    // A site with a certificate: days left (a fresh leaf lasts well over 30 days).
    client.call::<tls_ca_status>(NoParams {}).await.unwrap();
    let issued = client.call::<tls_issue>(TlsHostParams {
        host: "shop.test".into(),
    });
    let job = issued.await.unwrap();
    let job = finish(&client, job).await;
    assert_eq!(job.state, JobState::Done, "{:?}", job.error);
    let certs = client
        .call::<tls_list>(NoParams {})
        .await
        .unwrap()
        .certificates;
    assert_eq!(certs.len(), 1, "{certs:?}");
    let cert = &certs[0];
    assert_eq!(cert.host, "shop.test");
    assert_eq!(cert.site_id, Some(added.id));
    let days = (cert.not_after.as_second() - jiff::Timestamp::now().as_second()) / 86_400;
    assert!(days > 30, "{days} days left");
    assert_eq!(
        cert.trusted,
        Some(false),
        "the test root's CA is not in any store"
    );
    assert!(cert.ca_thumbprint_sha256.is_some());
    // Secure site (turned on after the issue): the check reports the expiry.
    let job = client
        .call::<forge_ipc::api::methods::sites_secure>(forge_ipc::api::SiteSecureParams {
            site: SiteRef::from("shop.test".to_owned()),
            secure: true,
        })
        .await
        .unwrap();
    site_of(&finish(&client, job).await);
    let info = client
        .call::<sites_info>(forge_ipc::api::SiteParams {
            site: SiteRef::from("shop.test".to_owned()),
        })
        .await
        .unwrap();
    let cert_check = info
        .checks
        .iter()
        .find(|c| c.id == "site.certificate")
        .unwrap();
    assert_eq!(cert_check.status, CheckStatus::Ok, "{cert_check:?}");
    assert!(
        cert_check.message.contains("days left"),
        "{}",
        cert_check.message
    );

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}\n{}", daemon.output());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn app_processes_report_instance_id_and_site_id() {
    let root = root_with(&[("8.4.27", false)]);
    let dir = root.path().join("projects").join("queue");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("forgebox.yaml"),
        "apiVersion: forgebox.dev/v1\nkind: Project\nmetadata: { name: queue }\nspec:\n  site: { host: queue.test, secure: false }\n  processes:\n    worker: { cmd: 'ping -n 300 127.0.0.1', autostart: true }\n",
    )
    .unwrap();
    let dir = forge_core::platform::long_path(&dir);
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    let events = Events::collect(&client, &["service.*"]).await;

    let job = client
        .call::<sites_up>(ProjectParams {
            dir: forge_core::paths::posix_string(&dir),
        })
        .await
        .unwrap();
    let done = finish(&client, job).await;
    assert_eq!(done.state, JobState::Done, "{:?}", done.error);

    let site_id = site(&client, "queue.test").await.id;
    let ps = client
        .call::<sites_ps>(SitePsParams {
            site: Some(SiteRef::from("queue.test".to_owned())),
        })
        .await
        .unwrap();
    assert_eq!(ps.processes.len(), 1, "{ps:?}");
    let process = &ps.processes[0];
    assert_eq!(process.site_id, site_id);
    let instance_id = process.instance_id.expect("instance_id");

    events
        .wait("service.running of the worker", |all| {
            all.iter().any(|(_, k, p)| {
                k == "service.running" && p["instance_id"] == serde_json::json!(instance_id)
            })
        })
        .await;
    let service_events = events.all();
    assert!(!service_events.is_empty());
    for (_, kind, payload) in service_events
        .iter()
        .filter(|(_, _, p)| p["instance_id"] == serde_json::json!(instance_id))
    {
        assert_eq!(
            payload["site_id"],
            serde_json::json!(site_id),
            "{kind}: {payload}"
        );
    }
    // The stream can be narrowed by the site.
    let mut by_site = client
        .subscribe::<events_subscribe>(EventsSubscribeParams {
            filter: EventFilter {
                kinds: vec!["service.*".into()],
                site_id: Some(site_id),
                ..EventFilter::default()
            },
            resume_from: Some(0),
        })
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), by_site.next_item())
        .await
        .expect("an event of the site")
        .expect("open");
    match first {
        SubscriptionItem::Item(envelope) => {
            assert_eq!(
                envelope.event.payload["site_id"],
                serde_json::json!(site_id)
            );
        }
        other => panic!("{other:?}"),
    }

    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(60)).await;
    assert!(status.success(), "{status}\n{}", daemon.output());
}
