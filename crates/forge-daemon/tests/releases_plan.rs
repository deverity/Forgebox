//! `tools.releases` and `tools.plan` (M2-E2-T5, docs/05 §1, docs/03 §5.1) against a daemon
//! booted **in this process** with a fake release resolver (canned releases, no network):
//!
//! * `tools.plan {stack: laravel}` on the repository's registry: one item per tool (plus
//!   the stack's Xdebug into the planned PHP), dependencies first, `total_bytes` = the sizes
//!   of what is not installed (release size, else the manifest estimate, flagged);
//! * `tools.releases`: grouped by series with variants, sizes, channel and the installed
//!   flag; the cached list comes back with `stale`;
//! * install and plan share the resolver: the planned spec installs the planned version
//!   and variant, and a plan after it says `already_installed`; a plan writes nothing to
//!   `bin/`;
//! * `jobs.cancel` ends a listing stuck behind a slow provider in under a second while
//!   `services.status` keeps answering in under 50 ms.
#![cfg(windows)]

mod common;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use clap::Parser;
use forge_core::config::ForgeConfig;
use forge_core::ids::ToolId;
use forge_core::paths::{ForgeRoot, InitOptions};
use forge_core::platform;
use forge_core::store::{
    NewToolVersion, Store, ToolVersionRepo, ToolVersionRow, VersionSource, VersionState,
};
use forge_daemon::args::Args;
use forge_daemon::boot::{self, BootOptions, Booted};
use forge_daemon::install::testing::{FakeResolver, release};
use forge_daemon::instance_lock::InstanceLock;
use forge_ipc::api::methods::{
    jobs_cancel, jobs_get, services_status, tools_install, tools_plan, tools_releases,
};
use forge_ipc::api::{
    CatalogKind, JobDto, JobIdParams, JobState, NoParams, ToolInstallParams, ToolInstallResultDto,
    ToolPlanDto, ToolPlanParams, ToolPlanReason, ToolReleasesDto, ToolReleasesParams, ToolSpec,
};
use forge_ipc::transport::local_transport;
use forge_ipc::{Client, ClientError, Credentials};
use sha2::{Digest, Sha256};

use common::*;

const MB: u64 = 1024 * 1024;

struct Running {
    booted: Booted,
    lock: InstanceLock,
    client: Client,
    root: ForgeRoot,
}

impl Running {
    async fn start(root: &TestRoot, resolver: Arc<FakeResolver>) -> Self {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let root = ForgeRoot::new(platform::long_path(root.path())).unwrap();
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
        let options = BootOptions {
            release_resolver: Some(resolver),
            ..BootOptions::default()
        };
        let booted = boot::boot_with(args, root.clone(), &lock, config, report, options)
            .await
            .expect("boot");
        let credentials =
            Credentials::local(&root, "releases-plan-test", env!("CARGO_PKG_VERSION")).unwrap();
        let client = Client::connect(local_transport(&root), credentials, options_for_client())
            .await
            .expect("client connects");
        Self {
            booted,
            lock,
            client,
            root,
        }
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

fn options_for_client() -> forge_ipc::ClientOptions {
    common::options()
}

/// Waits for the job and returns its final snapshot (any end state).
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
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn plan(client: &Client, params: ToolPlanParams) -> ToolPlanDto {
    let job = client.call::<tools_plan>(params).await.unwrap();
    let done = finish(client, job, Duration::from_secs(30)).await;
    assert_eq!(done.state, JobState::Done, "{:?}", done.error);
    serde_json::from_value(done.result.unwrap()).unwrap()
}

async fn plan_error(client: &Client, params: ToolPlanParams) -> String {
    match client.call::<tools_plan>(params).await {
        Err(ClientError::Api(api)) => api.code,
        Ok(job) => {
            let done = finish(client, job, Duration::from_secs(30)).await;
            assert_eq!(done.state, JobState::Failed);
            done.error.unwrap().code
        }
        Err(other) => panic!("{other:?}"),
    }
}

async fn releases(client: &Client, tool: &str, channel: Option<&str>) -> ToolReleasesDto {
    let job = client
        .call::<tools_releases>(ToolReleasesParams {
            tool: ToolId::new(tool).unwrap(),
            channel: channel.map(str::to_owned),
        })
        .await
        .unwrap();
    let done = finish(client, job, Duration::from_secs(30)).await;
    assert_eq!(done.state, JobState::Done, "{:?}", done.error);
    serde_json::from_value(done.result.unwrap()).unwrap()
}

fn specs(list: &[&str]) -> Vec<ToolSpec> {
    list.iter().map(|s| ToolSpec::from(*s)).collect()
}

/// Canned releases for the tools of the Laravel stack. Sizes: PHP, MySQL, Mailpit and Node
/// have one; Composer, Nginx and Memurai do not (the manifest estimate stands in).
fn laravel_releases(fake: &FakeResolver) {
    fake.set(
        "php",
        vec![
            release(
                "8.4.27",
                &[("ts", "ts"), ("compiler", "vs17"), ("arch", "x64")],
                Some(33 * MB),
            ),
            release(
                "8.4.27",
                &[("ts", "nts"), ("compiler", "vs17"), ("arch", "x64")],
                Some(31 * MB),
            ),
            release(
                "8.4.26",
                &[("ts", "nts"), ("compiler", "vs17"), ("arch", "x64")],
                Some(30 * MB),
            ),
            release(
                "8.3.36",
                &[("ts", "nts"), ("compiler", "vs16"), ("arch", "x64")],
                Some(28 * MB),
            ),
        ],
    );
    fake.set("composer", vec![release("2.10.3", &[], None)]);
    fake.set("nginx", vec![release("1.30.6", &[], None)]);
    fake.set(
        "mysql",
        vec![
            release("8.4.13", &[], Some(200 * MB)),
            release("8.0.40", &[], Some(180 * MB)),
        ],
    );
    fake.set("memurai", vec![release("4.1.6", &[], None)]);
    fake.set("mailpit", vec![release("1.32.0", &[], Some(10_632_291))]);
    fake.set(
        "nodejs",
        vec![
            release("22.23.4", &[], Some(35_604_229)),
            release("24.1.0", &[], Some(40 * MB)),
        ],
    );
    fake.set(
        "xdebug",
        vec![
            release(
                "3.5.3",
                &[("php_series", "8.4"), ("ts", "nts"), ("compiler", "vs17")],
                Some(331_776),
            ),
            release(
                "3.5.3",
                &[("php_series", "8.3"), ("ts", "nts"), ("compiler", "vs16")],
                Some(330_240),
            ),
        ],
    );
}

fn fake_php() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| compile_fixture("fake_php")).clone()
}

/// `bin/php/<folder>/` with the fake `php.exe`, registered like an installed version.
fn seed_php(root: &TestRoot, store: &Store, folder: &str) -> ToolVersionRow {
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

fn root_with_php(folders: &[&str]) -> TestRoot {
    let root = TestRoot::new();
    if !folders.is_empty() {
        std::fs::create_dir_all(root.root.db().parent().unwrap()).unwrap();
        let store = Store::open(&root.root.db()).unwrap();
        for folder in folders {
            seed_php(&root, &store, folder);
        }
    }
    root
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_laravel_stack_plans_one_item_per_tool_with_summed_sizes() {
    let root = TestRoot::new();
    let fake = FakeResolver::new();
    laravel_releases(&fake);
    let running = Running::start(&root, fake.clone()).await;

    let plan = plan(
        &running.client,
        ToolPlanParams {
            stack: Some("laravel".into()),
            ..ToolPlanParams::default()
        },
    )
    .await;

    // One item per tool of the stack, in dependency order (Composer needs PHP), and the
    // Xdebug of the stack right after the PHP it goes into.
    let order: Vec<&str> = plan.items.iter().map(|i| i.tool.as_str()).collect();
    assert_eq!(
        order,
        [
            "php", "xdebug", "composer", "nginx", "mysql", "memurai", "mailpit", "nodejs"
        ]
    );
    let by = |tool: &str| plan.items.iter().find(|i| i.tool.as_str() == tool).unwrap();
    // Series → newest patch, default variant (NTS before TS), canonical folder name.
    let php = by("php");
    assert_eq!(php.version, "8.4.27");
    assert_eq!(php.variant["ts"], "nts");
    assert_eq!(php.canonical_name.as_deref(), Some("8.4.27-nts-vs17-x64"));
    assert_eq!(php.size_bytes, Some(31 * MB));
    assert!(!php.size_estimated && !php.already_installed);
    assert_eq!(php.reason, ToolPlanReason::Stack);
    assert_eq!(by("mysql").version, "8.4.13");
    assert_eq!(by("nodejs").version, "22.23.4");
    // No size from the provider: the manifest estimate, flagged.
    let composer = by("composer");
    assert_eq!(
        (composer.size_bytes, composer.size_estimated),
        (Some(4 * MB), true)
    );
    assert_eq!(by("nginx").size_bytes, Some(6 * MB));
    assert_eq!(by("memurai").size_bytes, Some(25 * MB));
    // The extension goes into the PHP of the same plan.
    let xdebug = by("xdebug");
    assert_eq!(xdebug.kind, CatalogKind::Extension);
    assert_eq!(xdebug.reason, ToolPlanReason::Extension);
    assert_eq!(
        xdebug.target.as_ref().map(|t| t.as_str()),
        Some("php@8.4.27")
    );
    assert_eq!(xdebug.size_bytes, Some(331_776));

    let expected =
        31 * MB + 331_776 + 4 * MB + 6 * MB + 200 * MB + 25 * MB + 10_632_291 + 35_604_229;
    assert_eq!(plan.total_bytes, expected);
    assert_eq!(plan.unknown_size, 0);
    // Nothing was written.
    assert!(!running.root.bin().join("php").exists());
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn specs_replace_stack_entries_and_installed_versions_cost_nothing() {
    let root = root_with_php(&["8.3.36-nts-vs16-x64"]);
    let fake = FakeResolver::new();
    laravel_releases(&fake);
    let running = Running::start(&root, fake.clone()).await;

    // `php@8.3` instead of the stack's 8.4: already installed, so it adds no bytes, and
    // the stack's Xdebug is planned into that PHP.
    let planned = plan(
        &running.client,
        ToolPlanParams {
            stack: Some("laravel".into()),
            specs: specs(&["php@8.3"]),
            ..ToolPlanParams::default()
        },
    )
    .await;
    let php = planned
        .items
        .iter()
        .find(|i| i.tool.as_str() == "php")
        .unwrap();
    assert_eq!(php.version, "8.3.36");
    assert!(php.already_installed);
    assert_eq!(php.spec.as_str(), "php@8.3");
    let downloads: u64 = planned
        .items
        .iter()
        .filter(|i| !i.already_installed)
        .filter_map(|i| i.size_bytes)
        .sum();
    assert_eq!(planned.total_bytes, downloads);
    assert!(planned.total_bytes < 31 * MB + 300 * MB);

    // A spec the stack does not have is added.
    let planned = plan(
        &running.client,
        ToolPlanParams {
            stack: Some("laravel".into()),
            specs: specs(&["mailpit"]),
            ..ToolPlanParams::default()
        },
    )
    .await;
    assert_eq!(
        planned
            .items
            .iter()
            .filter(|i| i.tool.as_str() == "mailpit")
            .count(),
        1
    );
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dependencies_come_first_and_only_when_missing() {
    // No PHP: Composer pulls it in.
    let root = TestRoot::new();
    let fake = FakeResolver::new();
    laravel_releases(&fake);
    let running = Running::start(&root, fake.clone()).await;
    let planned = plan(
        &running.client,
        ToolPlanParams {
            specs: specs(&["composer"]),
            ..ToolPlanParams::default()
        },
    )
    .await;
    let seen: Vec<(&str, ToolPlanReason)> = planned
        .items
        .iter()
        .map(|i| (i.tool.as_str(), i.reason))
        .collect();
    assert_eq!(
        seen,
        [
            ("php", ToolPlanReason::Dependency),
            ("composer", ToolPlanReason::Requested)
        ]
    );
    // The variant is for what was asked, not for the dependency.
    assert_eq!(planned.items[0].variant["ts"], "nts");
    running.stop().await;

    // With PHP installed Composer is alone.
    let root = root_with_php(&["8.4.27-nts-vs17-x64"]);
    let running = Running::start(&root, fake.clone()).await;
    let planned = plan(
        &running.client,
        ToolPlanParams {
            specs: specs(&["composer"]),
            ..ToolPlanParams::default()
        },
    )
    .await;
    assert_eq!(planned.items.len(), 1);
    // An extension of the installed PHP: one item per compatible installed version.
    let planned = plan(
        &running.client,
        ToolPlanParams {
            specs: specs(&["xdebug"]),
            target: Some(ToolSpec::from("php@8.4")),
            ..ToolPlanParams::default()
        },
    )
    .await;
    assert_eq!(planned.items.len(), 1);
    assert_eq!(planned.items[0].kind, CatalogKind::Extension);
    assert_eq!(
        planned.items[0].target.as_ref().map(|t| t.as_str()),
        Some("php@8.4.27")
    );
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plan_errors_are_stable_codes() {
    let root = TestRoot::new();
    let fake = FakeResolver::new();
    laravel_releases(&fake);
    let running = Running::start(&root, fake.clone()).await;
    let client = &running.client;
    let none = ToolPlanParams::default();
    assert_eq!(plan_error(client, none).await, "invalid_input");
    assert_eq!(
        plan_error(
            client,
            ToolPlanParams {
                stack: Some("nope".into()),
                ..ToolPlanParams::default()
            }
        )
        .await,
        "not_found"
    );
    assert_eq!(
        plan_error(
            client,
            ToolPlanParams {
                specs: specs(&["nonexistent"]),
                ..ToolPlanParams::default()
            }
        )
        .await,
        "not_found"
    );
    // A tool with no release for this platform.
    assert_eq!(
        plan_error(
            client,
            ToolPlanParams {
                specs: specs(&["php@7.0"]),
                ..ToolPlanParams::default()
            }
        )
        .await,
        "not_found"
    );
    // An extension with no PHP anywhere.
    assert_eq!(
        plan_error(
            client,
            ToolPlanParams {
                specs: specs(&["xdebug"]),
                ..ToolPlanParams::default()
            }
        )
        .await,
        "not_found"
    );
    // --variant is not for extensions; --for is not for tools.
    assert_eq!(
        plan_error(
            client,
            ToolPlanParams {
                specs: specs(&["xdebug"]),
                variant: Some("nts".into()),
                ..ToolPlanParams::default()
            }
        )
        .await,
        "invalid_input"
    );
    assert_eq!(
        plan_error(
            client,
            ToolPlanParams {
                specs: specs(&["nginx"]),
                target: Some(ToolSpec::from("php@8.4")),
                ..ToolPlanParams::default()
            }
        )
        .await,
        "invalid_input"
    );
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn releases_group_by_series_with_variants_sizes_channel_and_installed() {
    let root = root_with_php(&["8.3.36-nts-vs16-x64"]);
    let fake = FakeResolver::new();
    laravel_releases(&fake);
    // A preview build, only listed with the preview channel.
    let mut php = vec![
        release(
            "8.4.27",
            &[("ts", "nts"), ("compiler", "vs17"), ("arch", "x64")],
            Some(31 * MB),
        ),
        release(
            "8.4.27",
            &[("ts", "ts"), ("compiler", "vs17"), ("arch", "x64")],
            Some(33 * MB),
        ),
        release(
            "8.3.36",
            &[("ts", "nts"), ("compiler", "vs16"), ("arch", "x64")],
            Some(28 * MB),
        ),
        release(
            "8.3.36",
            &[("ts", "ts"), ("compiler", "vs16"), ("arch", "x64")],
            Some(29 * MB),
        ),
        release(
            "8.3.35",
            &[("ts", "nts"), ("compiler", "vs16"), ("arch", "x64")],
            None,
        ),
    ];
    php.push(release(
        "8.6.0-RC1",
        &[("ts", "nts"), ("compiler", "vs17"), ("arch", "x64")],
        None,
    ));
    fake.set("php", php);
    let running = Running::start(&root, fake.clone()).await;

    let found = releases(&running.client, "php", None).await;
    assert!(!found.stale);
    assert!(found.fetched_at.is_some());
    let versions: Vec<&str> = found.releases.iter().map(|r| r.version.as_str()).collect();
    assert_eq!(versions, ["8.6.0-RC1", "8.4.27", "8.3.36", "8.3.35"]);
    let r84 = &found.releases[1];
    assert_eq!(r84.series, "8.4");
    assert_eq!(r84.channel.as_deref(), Some("stable"));
    assert_eq!(r84.size_bytes, Some(31 * MB), "the preferred build's size");
    assert!(!r84.installed);
    // Preferred build first, every variant listed with its canonical folder name.
    let names: Vec<&str> = r84
        .variants
        .iter()
        .map(|v| v.canonical_name.as_str())
        .collect();
    assert_eq!(names, ["8.4.27-nts-vs17-x64", "8.4.27-ts-vs17-x64"]);
    assert_eq!(r84.variants[1].size_bytes, Some(33 * MB));
    // Installed: the NTS build of 8.3.36 only.
    let r83 = &found.releases[2];
    assert_eq!(r83.series, "8.3");
    assert!(r83.installed);
    assert_eq!(
        r83.variants.iter().map(|v| v.installed).collect::<Vec<_>>(),
        [true, false]
    );
    assert!(!found.releases[3].installed);
    // The pre-release belongs to the preview channel.
    assert_eq!(found.releases[0].channel.as_deref(), Some("preview"));

    // One channel only.
    let stable = releases(&running.client, "php", Some("stable")).await;
    assert_eq!(stable.releases.len(), 3);
    let preview = releases(&running.client, "php", Some("preview")).await;
    assert_eq!(preview.releases.len(), 1);

    // Extensions list like tools.
    let xdebug = releases(&running.client, "xdebug", None).await;
    assert_eq!(xdebug.releases.len(), 1);
    assert_eq!(xdebug.releases[0].version, "3.5.3");
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_answer_is_flagged_with_its_age() {
    let root = TestRoot::new();
    let fake = FakeResolver::new();
    laravel_releases(&fake);
    let then: jiff::Timestamp = "2026-10-01T10:00:00Z".parse().unwrap();
    fake.set_stale(true, Some(then));
    let running = Running::start(&root, fake.clone()).await;
    let found = releases(&running.client, "nginx", None).await;
    assert!(found.stale);
    assert_eq!(found.fetched_at, Some(then));
    assert_eq!(found.releases.len(), 1);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_a_slow_listing_is_immediate_and_status_stays_fast() {
    let root = TestRoot::new();
    let fake = FakeResolver::new();
    laravel_releases(&fake);
    fake.set_delay(Duration::from_secs(60));
    let running = Running::start(&root, fake.clone()).await;
    let client = &running.client;

    let listing = client
        .call::<tools_releases>(ToolReleasesParams {
            tool: ToolId::new("php").unwrap(),
            channel: None,
        })
        .await
        .unwrap();
    let planning = client
        .call::<tools_plan>(ToolPlanParams {
            stack: Some("laravel".into()),
            ..ToolPlanParams::default()
        })
        .await
        .unwrap();
    // The provider is asked, and keeps the jobs waiting.
    let deadline = Instant::now() + Duration::from_secs(10);
    while fake.calls().len() < 2 {
        assert!(Instant::now() < deadline, "the resolver was not asked");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for _ in 0..10 {
        let started = Instant::now();
        client.call::<services_status>(NoParams {}).await.unwrap();
        let took = started.elapsed();
        assert!(took < Duration::from_millis(50), "services.status {took:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let started = Instant::now();
    for job in [&listing, &planning] {
        client
            .call::<jobs_cancel>(JobIdParams { job_id: job.id })
            .await
            .unwrap();
    }
    for job in [listing, planning] {
        let done = finish(client, job, Duration::from_secs(5)).await;
        assert_eq!(done.state, JobState::Cancelled, "{:?}", done.error);
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "cancel took {:?}",
        started.elapsed()
    );
    running.stop().await;
}

// ------------------------------------------------------------------ install == plan

/// A zip with the fake `mailpit.exe` at its top, and its sha256.
fn mailpit_zip() -> (Vec<u8>, String) {
    let exe = std::fs::read(compile_fixture("fake_mailpit")).unwrap();
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("mailpit.exe", options).unwrap();
        zip.write_all(&exe).unwrap();
        zip.finish().unwrap();
    }
    let bytes = buf.into_inner();
    let sha = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    (bytes, sha)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_gets_what_the_plan_promised_and_the_plan_writes_nothing() {
    let (zip_bytes, sha256) = mailpit_zip();
    let body = Arc::new(zip_bytes);
    let app = axum::Router::new().route(
        "/mailpit.zip",
        axum::routing::get({
            let body = Arc::clone(&body);
            move || {
                let body = Arc::clone(&body);
                async move { body.as_ref().clone() }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let root = TestRoot::new();
    let fake = FakeResolver::new();
    let mut build = release("1.32.0", &[], Some(body.len() as u64));
    build.url = format!("http://127.0.0.1:{port}/mailpit.zip");
    build.sha256 = Some(sha256);
    build.file_name = "mailpit-windows-amd64.zip".into();
    // A newer release that does not exist for this platform's build is simply not offered:
    // the plan and the install both take the first of the same list.
    fake.set("mailpit", vec![build]);
    let running = Running::start(&root, fake.clone()).await;
    let client = &running.client;
    let bin = running.root.bin().join("mailpit");

    // Dry run: the plan names the build and writes nothing under bin/.
    let before = plan(
        client,
        ToolPlanParams {
            specs: specs(&["mailpit"]),
            ..ToolPlanParams::default()
        },
    )
    .await;
    assert_eq!(before.items.len(), 1);
    let planned = &before.items[0];
    assert!(!planned.already_installed);
    assert_eq!(planned.size_bytes, Some(body.len() as u64));
    assert!(!bin.exists(), "a plan must not create bin/mailpit");

    // Install exactly the planned spec: same version, same folder.
    let job = client
        .call::<tools_install>(ToolInstallParams {
            specs: vec![planned.spec.clone()],
            variant: None,
            yes: true,
            target: None,
        })
        .await
        .unwrap();
    let done = finish(client, job, Duration::from_secs(60)).await;
    assert_eq!(done.state, JobState::Done, "{:?}", done.error);
    let result: ToolInstallResultDto = serde_json::from_value(done.result.unwrap()).unwrap();
    assert_eq!(result.installed.len(), 1);
    let installed = &result.installed[0];
    assert_eq!(installed.version, planned.version);
    assert_eq!(
        Some(installed.canonical_name.as_str()),
        planned.canonical_name.as_deref()
    );
    assert!(
        bin.join(&installed.canonical_name)
            .join("mailpit.exe")
            .is_file()
    );

    // Planning again: nothing left to download.
    let after = plan(
        client,
        ToolPlanParams {
            specs: specs(&["mailpit"]),
            ..ToolPlanParams::default()
        },
    )
    .await;
    assert!(after.items[0].already_installed);
    assert_eq!((after.total_bytes, after.unknown_size), (0, 0));

    // And the listing marks it.
    let found = releases(client, "mailpit", None).await;
    assert!(found.releases[0].installed);
    running.stop().await;
    server.abort();
}
