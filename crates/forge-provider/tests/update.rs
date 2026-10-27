//! Update engine (M1-E2-T5), end to end in a temporary `ForgeRoot` on NTFS with a fake
//! release source, a fake fetcher (archives zipped by the tests) and a fake
//! `PoolSwitcher` that records its calls. Acceptance scenario of docs/10: install
//! 8.3.36 → check finds 8.3.37 (auto-patch) → apply → rollback.
#![cfg(windows)]

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use forge_core::Timestamp;
use forge_core::actor::ActorSupervisor;
use forge_core::config::{MeteredPolicy, QuietHours, UpdatesConfig};
use forge_core::error::{ForgeError, Result};
use forge_core::event::{Event, EventBus, EventKind};
use forge_core::ids::{JobId, ToolId, VersionId};
use forge_core::jobs::{JobInfo, JobRegistry, JobState};
use forge_core::paths::{ForgeRoot, InitOptions};
use forge_core::platform::{self, MemoryUserEnv};
use forge_core::store::{Store, ToolVersionRepo, UpdateHistoryRepo, UpdateStatus, UpdateTrigger};
use forge_ipc::api::events::ToolVersionChangePayload;
use forge_manifest::Manifest;
use forge_manifest::schema::ToolManifest;
use forge_manifest::schema::tool::UpdatePolicy;
use forge_provider::download::Priority;
use forge_provider::layout::{
    CurrentPolicy, InstallOptions, InstallRequest, Layout, RenderRequest, TemplateRenderer,
};
use forge_provider::provider::Release;
use forge_provider::update::{
    ApplyRequest, ArchiveFetcher, AvailableUpdate, DeferReason, FetchArchive, PoolSwitch,
    PoolSwitcher, ReleaseSource, RollbackReport, RollbackRequest, StaticCatalog,
    ToolUpdateEventPayload, UpdateConfig, UpdateDeps, UpdateEngine, UpdateKind, UpdateReport,
    fixed_metered,
};
use futures::future::BoxFuture;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

// ---------------------------------------------------------------- fixtures

fn php_manifest(category: &str, policy: &str, retention: u32) -> Arc<ToolManifest> {
    let text = format!(
        r#"
apiVersion: forgebox.dev/v1
kind: Tool
metadata: {{ id: php, name: PHP, category: {category} }}
spec:
  platforms: [windows-x64]
  versioning:
    scheme: semver
    series_key: "{{major}}.{{minor}}"
    variants:
      - {{ key: ts, values: [nts, ts], default: nts }}
      - {{ key: arch, values: [x64], default: x64 }}
  source:
    providers:
      - type: php-windows
  install:
    archive: zip
    strip_root: auto
    post_install:
      - copy: {{ from: "php.ini-development", to: "{{{{ paths.etc }}}}/php/{{{{ series }}}}/php.ini", if_missing: true }}
      - render: {{ template: "php/php.ini.j2", to: "{{{{ paths.etc }}}}/php/{{{{ series }}}}/conf.d/10-forgebox.ini" }}
  service:
    kind: pool
    pool:
      exe: php-cgi.exe
      args: ["-b", "127.0.0.1:{{{{ port }}}}"]
  update:
    policy: {policy}
    retention: {retention}
    check_interval: 6h
    migrate:
      - copy_dir: {{ from: "ext/", to: "ext/", only_missing: true }}
"#
    );
    match forge_manifest::load_str(&text).unwrap_or_else(|e| panic!("{e}")) {
        Manifest::Tool(tool) => {
            let issues = forge_manifest::validate(&Manifest::Tool(tool.clone()));
            // The linter refuses `auto*` for databases; the engine caps them anyway
            // (`databases_are_never_updated_automatically`).
            let expected = usize::from(category == "database" && policy.starts_with("auto"));
            assert_eq!(issues.len(), expected, "{issues:#?}");
            Arc::new(tool)
        }
        _ => panic!("not a tool"),
    }
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `php-<version>/{php.exe, php.ini-development, ext/php_curl.dll}`.
fn php_zip(dir: &Path, version: &str) -> PathBuf {
    let path = dir.join(format!("php-{version}-nts-Win32-x64.zip"));
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    let top = format!("php-{version}");
    for (name, body) in [
        ("php.exe", format!("fake php {version}")),
        ("php.ini-development", format!("; php.ini of {version}\n")),
        ("ext/php_curl.dll", format!("curl built for {version}")),
    ] {
        zip.start_file(format!("{top}/{name}"), options).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    path
}

fn release(version: &str, archive: &Path) -> Release {
    let bytes = std::fs::read(archive).unwrap();
    Release {
        version: version.to_owned(),
        url: format!("https://downloads.example.invalid/php-{version}.zip"),
        sha256: Some(sha256_hex(&bytes)),
        sha512: None,
        size: Some(bytes.len() as u64),
        variants: BTreeMap::from([
            ("ts".to_owned(), "nts".to_owned()),
            ("arch".to_owned(), "x64".to_owned()),
        ]),
        published_at: None,
        signature: None,
        file_name: archive.file_name().unwrap().to_string_lossy().into_owned(),
        checksum_url: None,
        tags: Vec::new(),
    }
}

/// Writes `extension_dir = <paths.version>/ext` into the destination.
#[derive(Default)]
struct FakeRenderer {
    calls: Mutex<Vec<RenderRequest>>,
}

impl TemplateRenderer for FakeRenderer {
    fn render(&self, request: &RenderRequest) -> Result<()> {
        std::fs::write(
            &request.dest,
            format!("extension_dir = \"{}/ext\"\n", request.vars.paths.version),
        )
        .unwrap();
        self.calls.lock().unwrap().push(request.clone());
        Ok(())
    }
}

/// The provider: whatever the test put in `releases`.
#[derive(Default)]
struct FakeReleases {
    releases: Mutex<Vec<Release>>,
    calls: Mutex<u32>,
}

impl ReleaseSource for FakeReleases {
    fn releases<'a>(&'a self, _manifest: &'a ToolManifest) -> BoxFuture<'a, Result<Vec<Release>>> {
        *self.calls.lock().unwrap() += 1;
        let releases = self.releases.lock().unwrap().clone();
        Box::pin(async move { Ok(releases) })
    }
}

/// "Downloads" by handing out archives built by the test; can be held on a gate.
#[derive(Default)]
struct FakeFetcher {
    archives: Mutex<HashMap<String, PathBuf>>,
    fetched: Mutex<Vec<(String, Priority)>>,
    gate: Mutex<Option<Arc<Semaphore>>>,
}

impl ArchiveFetcher for FakeFetcher {
    fn fetch<'a>(&'a self, req: FetchArchive<'a>) -> BoxFuture<'a, Result<PathBuf>> {
        Box::pin(async move {
            let gate = self.gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                let _permit = gate.acquire().await.unwrap();
            }
            self.fetched
                .lock()
                .unwrap()
                .push((req.release.version.clone(), req.priority));
            self.archives
                .lock()
                .unwrap()
                .get(&req.release.version)
                .cloned()
                .ok_or_else(|| ForgeError::NotFound(req.release.url.clone()))
        })
    }
}

/// Records every switch; fails on demand.
#[derive(Default)]
struct FakePools {
    calls: Mutex<Vec<PoolSwitch>>,
    fail: Mutex<bool>,
    serving: Mutex<Vec<VersionId>>,
}

impl PoolSwitcher for FakePools {
    fn switch<'a>(&'a self, switch: &'a PoolSwitch) -> BoxFuture<'a, Result<()>> {
        self.calls.lock().unwrap().push(switch.clone());
        let fail = *self.fail.lock().unwrap();
        Box::pin(async move {
            if fail {
                Err(ForgeError::Internal("warmup of the new pool failed".into()))
            } else {
                Ok(())
            }
        })
    }

    fn serving<'a>(&'a self, _tool: &'a ToolId, version: VersionId) -> BoxFuture<'a, Result<bool>> {
        let serving = self.serving.lock().unwrap().contains(&version);
        Box::pin(async move { Ok(serving) })
    }
}

struct Env {
    _dir: tempfile::TempDir,
    work: PathBuf,
    root: ForgeRoot,
    store: Store,
    bus: EventBus,
    jobs: JobRegistry,
    renderer: Arc<FakeRenderer>,
    layout: Layout,
    releases: Arc<FakeReleases>,
    fetcher: Arc<FakeFetcher>,
    pools: Arc<FakePools>,
    engine: UpdateEngine,
    manifest: Arc<ToolManifest>,
    _supervisor: ActorSupervisor,
}

impl Env {
    fn new(manifest: Arc<ToolManifest>) -> Self {
        Self::with(manifest, UpdateConfig::default(), false)
    }

    fn with(manifest: Arc<ToolManifest>, mut config: UpdateConfig, metered: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path().join("Forgebox")).unwrap();
        root.init(InitOptions::default()).unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let store = Store::open(&root.db()).unwrap();
        let bus = EventBus::new();
        let jobs = JobRegistry::new(bus.clone());
        let renderer = Arc::new(FakeRenderer::default());
        let user_env = Arc::new(MemoryUserEnv::with_path(r"C:\Windows\system32"));
        let platform = platform::current().with_user_env(user_env);
        let layout = Layout::new(
            root.clone(),
            platform,
            store.writer().clone(),
            store.reader().clone(),
            jobs.clone(),
            renderer.clone(),
        );
        let releases = Arc::new(FakeReleases::default());
        let fetcher = Arc::new(FakeFetcher::default());
        let pools = Arc::new(FakePools::default());
        let supervisor = ActorSupervisor::new(bus.clone());
        config.time_zone = jiff::tz::TimeZone::UTC;
        let engine = UpdateEngine::spawn(
            &supervisor,
            UpdateDeps {
                layout: layout.clone(),
                reader: store.reader().clone(),
                writer: store.writer().clone(),
                catalog: Arc::new(StaticCatalog::new().with(Arc::clone(&manifest), true)),
                releases: releases.clone(),
                fetcher: fetcher.clone(),
                pools: pools.clone(),
                metered: fixed_metered(metered),
            },
            config,
        );
        Self {
            _dir: dir,
            work,
            root,
            store,
            bus,
            jobs,
            renderer,
            layout,
            releases,
            fetcher,
            pools,
            engine,
            manifest,
            _supervisor: supervisor,
        }
    }

    fn php() -> ToolId {
        ToolId::new("php").unwrap()
    }

    async fn wait(&self, id: JobId) -> JobInfo {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let info = self.jobs.get(id).expect("job known");
            if info.state != JobState::Running {
                return info;
            }
            assert!(Instant::now() < deadline, "job {id} did not finish");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Installs `version` directly through the layout (what the user did before).
    async fn install(&self, version: &str) -> VersionId {
        let zip = php_zip(&self.work, version);
        let id = self
            .layout
            .install_from_archive(InstallRequest {
                manifest: Arc::clone(&self.manifest),
                release: release(version, &zip),
                archive: zip,
                options: InstallOptions {
                    current: CurrentPolicy::Always,
                    ..InstallOptions::default()
                },
            })
            .unwrap();
        let info = self.wait(id).await;
        assert_eq!(info.state, JobState::Done, "{:?}", info.error);
        let report: forge_provider::layout::InstallReport =
            serde_json::from_value(info.result.unwrap()).unwrap();
        report.version_id
    }

    /// Publishes `version` upstream (release + archive for the fetcher).
    fn publish(&self, version: &str) {
        let zip = php_zip(&self.work, version);
        self.releases
            .releases
            .lock()
            .unwrap()
            .push(release(version, &zip));
        self.fetcher
            .archives
            .lock()
            .unwrap()
            .insert(version.to_owned(), zip);
    }

    fn version_dir(&self, folder: &str) -> PathBuf {
        self.root.bin().join("php").join(folder)
    }

    fn current(&self) -> Option<String> {
        let link = self.root.tool_current_link(&Self::php());
        platform::current()
            .link_fs()
            .read_junction(&link)
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
    }

    fn conf_d(&self) -> String {
        std::fs::read_to_string(self.root.etc().join("php/8.3/conf.d/10-forgebox.ini")).unwrap()
    }

    fn folders(&self) -> Vec<String> {
        let mut rows: Vec<String> = ToolVersionRepo::new(self.store.conn())
            .list(Some(&Self::php()))
            .unwrap()
            .into_iter()
            .map(|r| r.canonical_name)
            .collect();
        rows.sort();
        rows
    }

    async fn check_one(&self) -> AvailableUpdate {
        let mut found = self.engine.check(Some(&Self::php())).await.unwrap();
        assert_eq!(found.len(), 1, "{found:#?}");
        found.remove(0)
    }

    async fn apply(&self, req: ApplyRequest) -> JobInfo {
        let id = self.engine.apply(req).await.unwrap();
        self.wait(id).await
    }
}

fn events_of(bus: &EventBus, kind: EventKind) -> Vec<Event> {
    bus.history_since(0, usize::MAX)
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

fn done<T: serde::de::DeserializeOwned>(info: &JobInfo) -> T {
    assert_eq!(info.state, JobState::Done, "{:?}", info.error);
    serde_json::from_value(info.result.clone().expect("job result")).unwrap()
}

/// `HH:MM` of `now + minutes` in UTC.
fn hhmm(now: Timestamp, minutes: i64) -> String {
    let t = now
        .checked_add(jiff::SignedDuration::from_secs(minutes * 60))
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::UTC);
    format!("{:02}:{:02}", t.hour(), t.minute())
}

// ---------------------------------------------------------------- tests

/// Acceptance (docs/10 M1-E2-T5): 8.3.36 → 8.3.37 → rollback with a fake pool.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn update_then_rollback_with_fake_pools() {
    let env = Env::new(php_manifest("runtime", "auto-patch", 1));
    let v36 = env.install("8.3.36").await;
    assert_eq!(env.current().as_deref(), Some("8.3.36-nts-x64"));
    // An extension the user added to 8.3.36.
    std::fs::write(
        env.version_dir("8.3.36-nts-x64").join("ext/php_xdebug.dll"),
        "xdebug for 8.3",
    )
    .unwrap();
    env.publish("8.3.36");
    env.publish("8.3.37");
    env.publish("8.4.14");

    // Check: auto-patch finds 8.3.37 (not 8.4).
    let update = env.check_one().await;
    assert_eq!(
        (update.from.as_str(), update.to.as_str()),
        ("8.3.36", "8.3.37")
    );
    assert_eq!(update.series, "8.3");
    assert_eq!(update.kind, UpdateKind::Patch);
    assert_eq!(update.policy, UpdatePolicy::AutoPatch);
    assert!(update.automatic && !update.installed && !update.pinned);
    assert_eq!(update.from_id, v36);

    // Apply.
    let report: UpdateReport = done(&env.apply(ApplyRequest::auto(update)).await);
    assert!(report.installed && report.pools_switched && report.current_moved);
    assert!(report.removed.is_empty(), "retention 1 keeps 8.3.36");
    assert_eq!(report.migrated, ["ext/php_xdebug.dll"]);
    let v37 = report.to_id;
    {
        let calls = env.pools.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            (calls[0].series.as_str(), calls[0].from, calls[0].to),
            ("8.3", v36, v37)
        );
        assert_eq!(
            (calls[0].from_version.as_str(), calls[0].to_version.as_str()),
            ("8.3.36", "8.3.37")
        );
    }
    assert_eq!(
        env.fetcher.fetched.lock().unwrap().as_slice(),
        [("8.3.37".to_owned(), Priority::AutoUpdate)]
    );
    assert_eq!(env.current().as_deref(), Some("8.3.37-nts-x64"));
    let new_ext = env.version_dir("8.3.37-nts-x64").join("ext");
    assert_eq!(
        std::fs::read_to_string(new_ext.join("php_xdebug.dll")).unwrap(),
        "xdebug for 8.3"
    );
    assert_eq!(
        std::fs::read_to_string(new_ext.join("php_curl.dll")).unwrap(),
        "curl built for 8.3.37",
        "only_missing keeps the new build's own files"
    );
    assert!(
        env.conf_d().contains("8.3.37-nts-x64/ext"),
        "{}",
        env.conf_d()
    );
    assert!(
        env.version_dir("8.3.36-nts-x64").is_dir(),
        "8.3.36 retained"
    );
    assert_eq!(env.folders(), ["8.3.36-nts-x64", "8.3.37-nts-x64"]);
    let updated = events_of(&env.bus, EventKind::ToolUpdated);
    assert_eq!(updated.len(), 1);
    let payload: ToolUpdateEventPayload =
        serde_json::from_value(updated[0].payload.clone()).unwrap();
    assert_eq!(
        (payload.from.as_str(), payload.to.as_str()),
        ("8.3.36", "8.3.37")
    );
    // Contract with forge-ipc.
    let ipc: ToolVersionChangePayload = serde_json::from_value(updated[0].payload.clone()).unwrap();
    assert_eq!((ipc.version_id, ipc.series.as_deref()), (v37, Some("8.3")));
    assert_eq!(ipc.update_id, Some(report.update_id));

    // Nothing more to update.
    assert!(
        env.engine
            .check(Some(&Env::php()))
            .await
            .unwrap()
            .is_empty()
    );

    // Rollback.
    let job = env
        .engine
        .rollback(RollbackRequest::new(Env::php(), "8.3"))
        .await
        .unwrap();
    let back: RollbackReport = done(&env.wait(job).await);
    assert_eq!((back.from.as_str(), back.to.as_str()), ("8.3.37", "8.3.36"));
    assert!(back.pools_switched && back.current_moved);
    assert_eq!(back.rollback_of, report.update_id);
    {
        let calls = env.pools.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!((calls[1].from, calls[1].to), (v37, v36), "inverse switch");
    }
    assert_eq!(env.current().as_deref(), Some("8.3.36-nts-x64"));
    assert!(
        env.conf_d().contains("8.3.36-nts-x64/ext"),
        "{}",
        env.conf_d()
    );
    assert!(env.version_dir("8.3.37-nts-x64").is_dir(), "8.3.37 is kept");

    let history = UpdateHistoryRepo::new(env.store.conn())
        .list(Some(&Env::php()), 10)
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].triggered_by, UpdateTrigger::Rollback);
    assert_eq!(history[0].status, UpdateStatus::Completed);
    assert_eq!(history[0].rollback_of, Some(history[1].id));
    assert_eq!(
        (history[0].from_version_id, history[0].to_version_id),
        (Some(v37), Some(v36))
    );
    assert_eq!(history[1].triggered_by, UpdateTrigger::Auto);
    assert_eq!(history[1].status, UpdateStatus::RolledBack);
    assert_eq!(history[1].policy.as_deref(), Some("auto-patch"));
    assert_eq!(history[1].series.as_deref(), Some("8.3"));
    let rollback_events = events_of(&env.bus, EventKind::ToolRollback);
    assert_eq!(rollback_events.len(), 1);
    let ipc: ToolVersionChangePayload =
        serde_json::from_value(rollback_events[0].payload.clone()).unwrap();
    assert_eq!(
        (ipc.version_id, ipc.rollback_of),
        (v36, Some(report.update_id))
    );

    // The series is pinned: 8.3.37 is offered again, installed, but not automatically.
    let again = env.check_one().await;
    assert_eq!(again.to, "8.3.37");
    assert!(again.installed && again.pinned && !again.automatic);
    let run = env
        .engine
        .run_scheduled(&Env::php(), Timestamp::now())
        .await
        .unwrap();
    assert!(run.started.is_empty() && run.deferred.is_none());

    // Nothing left to roll back in 8.3.
    let job = env
        .engine
        .rollback(RollbackRequest::new(Env::php(), "8.3"))
        .await
        .unwrap();
    let info = env.wait(job).await;
    assert_eq!(info.state, JobState::Failed);
    assert_eq!(info.error.unwrap().code, "not_found");

    // The user re-applies by hand: the installed 8.3.37 is reused and the pin goes away.
    let report: UpdateReport = done(&env.apply(ApplyRequest::user(again)).await);
    assert!(!report.installed && report.pools_switched);
    assert_eq!(
        env.fetcher.fetched.lock().unwrap().len(),
        1,
        "no new download"
    );
    assert_eq!(env.current().as_deref(), Some("8.3.37-nts-x64"));
    assert!(
        env.engine
            .check(Some(&Env::php()))
            .await
            .unwrap()
            .is_empty()
    );
    // Every re-render targeted the series' conf.d.
    assert!(
        env.renderer
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c.dest.ends_with("php/8.3/conf.d/10-forgebox.ini"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retention_one_with_three_versions_removes_the_oldest() {
    let env = Env::new(php_manifest("runtime", "auto-patch", 1));
    let v35 = env.install("8.3.35").await;
    env.install("8.3.36").await;
    env.publish("8.3.37");
    let update = env.check_one().await;
    assert_eq!(update.from, "8.3.36");
    let report: UpdateReport = done(&env.apply(ApplyRequest::auto(update)).await);
    assert_eq!(report.removed, ["8.3.35-nts-x64"]);
    assert_eq!(env.folders(), ["8.3.36-nts-x64", "8.3.37-nts-x64"]);
    assert!(!env.version_dir("8.3.35-nts-x64").exists());
    let leftovers: Vec<String> = std::fs::read_dir(env.root.bin().join("php"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    // The history keeps the text of a removed version.
    let history = env.engine.history(Some(&Env::php()), 10).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].from_version.as_deref(), Some("8.3.36"));
    let ipc: ToolVersionChangePayload = serde_json::from_value(
        events_of(&env.bus, EventKind::ToolUpdated)[0]
            .payload
            .clone(),
    )
    .unwrap();
    assert_eq!(ipc.removed, ["8.3.35-nts-x64"]);
    assert!(
        ToolVersionRepo::new(env.store.conn())
            .get(v35)
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retention_keeps_a_version_a_pool_still_serves() {
    let env = Env::new(php_manifest("runtime", "auto-patch", 0));
    let v36 = env.install("8.3.36").await;
    env.publish("8.3.37");
    env.pools.serving.lock().unwrap().push(v36);
    let report: UpdateReport = done(&env.apply(ApplyRequest::auto(env.check_one().await)).await);
    assert!(report.removed.is_empty());
    assert!(
        report.warnings.iter().any(|w| w.contains("8.3.36-nts-x64")),
        "{:?}",
        report.warnings
    );
    // Once nothing serves it, retention 0 removes the replaced version after the switch.
    env.pools.serving.lock().unwrap().clear();
    env.publish("8.3.38");
    let report: UpdateReport = done(&env.apply(ApplyRequest::auto(env.check_one().await)).await);
    assert_eq!(
        report.removed,
        ["8.3.37-nts-x64", "8.3.36-nts-x64"],
        "newest first"
    );
    assert_eq!(env.folders(), ["8.3.38-nts-x64"]);
}

/// A cross-series update (auto-minor 8.3 -> 8.4, auto 8.3 -> 9.0) supersedes the old
/// line: the retained 8.3.36 is not offered again and the scheduler does not re-apply it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_series_update_is_not_offered_again() {
    for (manifest_policy, override_policy, target, kind) in [
        ("auto-minor", None, "8.4.14", UpdateKind::Minor),
        ("auto-patch", Some("auto"), "9.0.1", UpdateKind::Major),
    ] {
        let env = Env::new(php_manifest("runtime", manifest_policy, 1));
        let v36 = env.install("8.3.36").await;
        if let Some(policy) = override_policy {
            ToolVersionRepo::new(env.store.conn())
                .set_update_policy(&Env::php(), Some(policy))
                .unwrap();
        }
        env.publish("8.3.36");
        env.publish("8.4.14");
        env.publish("9.0.1");
        let update = env.check_one().await;
        assert_eq!(
            (
                update.series.as_str(),
                update.from.as_str(),
                update.to.as_str()
            ),
            ("8.3", "8.3.36", target)
        );
        assert_eq!(update.kind, kind);
        assert!(update.automatic, "{update:#?}");

        // The scheduler applies it once.
        let run = env
            .engine
            .run_scheduled(&Env::php(), Timestamp::now())
            .await
            .unwrap();
        assert_eq!(run.started.len(), 1, "{run:#?}");
        let report: UpdateReport = done(&env.wait(run.started[0]).await);
        assert_eq!((report.from_id, report.to.as_str()), (v36, target));
        assert!(report.removed.is_empty(), "retention 1 keeps 8.3.36");
        let to_folder = format!("{target}-nts-x64");
        assert_eq!(
            env.folders(),
            ["8.3.36-nts-x64".to_owned(), to_folder.clone()]
        );

        // The 8.3 line is superseded: no phantom update, nothing re-applied.
        let found = env.engine.check(Some(&Env::php())).await.unwrap();
        assert!(found.is_empty(), "{found:#?}");
        for _ in 0..2 {
            let run = env
                .engine
                .run_scheduled(&Env::php(), Timestamp::now())
                .await
                .unwrap();
            assert!(run.updates.is_empty() && run.started.is_empty(), "{run:#?}");
        }
        assert_eq!(env.pools.calls.lock().unwrap().len(), 1);
        assert_eq!(events_of(&env.bus, EventKind::ToolUpdated).len(), 1);
        assert_eq!(
            env.engine
                .history(Some(&Env::php()), 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(env.fetcher.fetched.lock().unwrap().len(), 1);

        // Rolling it back makes the 8.3 line active and pinned again.
        let job = env
            .engine
            .rollback(RollbackRequest::new(Env::php(), "8.3"))
            .await
            .unwrap();
        let back: RollbackReport = done(&env.wait(job).await);
        assert_eq!((back.to_id, back.to.as_str()), (v36, "8.3.36"));
        let again = env.check_one().await;
        assert_eq!((again.from.as_str(), again.to.as_str()), ("8.3.36", target));
        assert!(again.pinned && again.installed && !again.automatic);
        let run = env
            .engine
            .run_scheduled(&Env::php(), Timestamp::now())
            .await
            .unwrap();
        assert!(run.started.is_empty(), "{run:#?}");
        assert_eq!(env.pools.calls.lock().unwrap().len(), 2);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn policy_notify_reports_but_never_installs() {
    let env = Env::new(php_manifest("runtime", "notify", 1));
    env.install("8.3.36").await;
    env.publish("8.3.37");
    let update = env.check_one().await;
    assert_eq!(update.policy, UpdatePolicy::Notify);
    assert!(!update.automatic);
    let run = env
        .engine
        .run_scheduled(&Env::php(), Timestamp::now())
        .await
        .unwrap();
    assert_eq!(run.updates.len(), 1);
    assert!(run.started.is_empty());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(env.fetcher.fetched.lock().unwrap().is_empty());
    assert!(env.pools.calls.lock().unwrap().is_empty());
    assert_eq!(env.folders(), ["8.3.36-nts-x64"]);
    // The scheduler recorded the check: the next one is an interval later.
    let next = env
        .engine
        .next_check_at(&Env::php(), Timestamp::now())
        .await
        .unwrap();
    let at = next.at.unwrap();
    let ahead = at.duration_since(Timestamp::now()).as_secs();
    assert!((5 * 3600..=7 * 3600).contains(&ahead), "{ahead}");
    // `manual` (user override in tools.update_policy) never checks in the background.
    ToolVersionRepo::new(env.store.conn())
        .set_update_policy(&Env::php(), Some("manual"))
        .unwrap();
    let next = env
        .engine
        .next_check_at(&Env::php(), Timestamp::now())
        .await
        .unwrap();
    assert_eq!(next.at, None);
    let calls = *env.releases.calls.lock().unwrap();
    let run = env
        .engine
        .run_scheduled(&Env::php(), Timestamp::now())
        .await
        .unwrap();
    assert!(run.updates.is_empty());
    assert_eq!(
        *env.releases.calls.lock().unwrap(),
        calls,
        "manual: no resolve"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn databases_are_never_updated_automatically() {
    // Even a manifest that skipped the linter (`policy: auto` is a lint error for a
    // database) is capped at notify.
    let env = Env::new(php_manifest("database", "auto", 1));
    env.install("8.3.36").await;
    env.publish("8.3.37");
    env.publish("9.0.1");
    let update = env.check_one().await;
    assert_eq!(
        update.policy,
        UpdatePolicy::Notify,
        "database capped at notify"
    );
    assert_eq!(update.to, "8.3.37", "notify reports the patch");
    assert!(!update.automatic);
    let run = env
        .engine
        .run_scheduled(&Env::php(), Timestamp::now())
        .await
        .unwrap();
    assert!(run.started.is_empty());
    // The user override cannot lift it either.
    ToolVersionRepo::new(env.store.conn())
        .set_update_policy(&Env::php(), Some("auto-patch"))
        .unwrap();
    assert!(!env.check_one().await.automatic);
    assert!(env.fetcher.fetched.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quiet_hours_and_metered_connections_postpone() {
    let now = Timestamp::now();
    let mut config = UpdateConfig::new(UpdatesConfig {
        metered: MeteredPolicy::Pause,
        quiet_hours: Some(QuietHours {
            start: hhmm(now, -60),
            end: hhmm(now, 60),
        }),
    });
    config.first_check_delay = Duration::ZERO;
    let env = Env::with(php_manifest("runtime", "auto-patch", 1), config, false);
    env.install("8.3.36").await;
    env.publish("8.3.37");
    let run = env.engine.run_scheduled(&Env::php(), now).await.unwrap();
    assert_eq!(run.deferred, Some(DeferReason::QuietHours));
    assert!(run.updates.is_empty() && run.started.is_empty());
    assert_eq!(*env.releases.calls.lock().unwrap(), 0, "nothing resolved");
    let next = env.engine.next_check_at(&Env::php(), now).await.unwrap();
    assert_eq!(next.deferred, Some(DeferReason::QuietHours));
    let at = next.at.unwrap().to_zoned(jiff::tz::TimeZone::UTC);
    assert_eq!(
        format!("{:02}:{:02}", at.hour(), at.minute()),
        hhmm(now, 60)
    );

    // Metered (no quiet hours): deferred too; with `metered = "allow"` it runs.
    let env = Env::with(
        php_manifest("runtime", "auto-patch", 1),
        UpdateConfig::new(UpdatesConfig::default()),
        true,
    );
    env.install("8.3.36").await;
    env.publish("8.3.37");
    let run = env
        .engine
        .run_scheduled(&Env::php(), Timestamp::now())
        .await
        .unwrap();
    assert_eq!(run.deferred, Some(DeferReason::Metered));
    let env = Env::with(
        php_manifest("runtime", "auto-patch", 1),
        UpdateConfig::new(UpdatesConfig {
            metered: MeteredPolicy::Allow,
            quiet_hours: None,
        }),
        true,
    );
    env.install("8.3.36").await;
    env.publish("8.3.37");
    let run = env
        .engine
        .run_scheduled(&Env::php(), Timestamp::now())
        .await
        .unwrap();
    assert_eq!(run.deferred, None);
    assert_eq!(run.started.len(), 1);
    let report: UpdateReport = done(&env.wait(run.started[0]).await);
    assert_eq!(report.to, "8.3.37");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_switch_keeps_the_old_version_serving() {
    let env = Env::new(php_manifest("runtime", "auto-patch", 1));
    let v36 = env.install("8.3.36").await;
    env.publish("8.3.37");
    *env.pools.fail.lock().unwrap() = true;
    let info = env.apply(ApplyRequest::auto(env.check_one().await)).await;
    assert_eq!(info.state, JobState::Failed);
    assert_eq!(env.current().as_deref(), Some("8.3.36-nts-x64"));
    assert!(
        env.conf_d().contains("8.3.36-nts-x64/ext"),
        "{}",
        env.conf_d()
    );
    let history = env.engine.history(Some(&Env::php()), 10).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].status, UpdateStatus::Failed);
    assert!(history[0].error.as_deref().unwrap().contains("warmup"));
    assert!(events_of(&env.bus, EventKind::ToolUpdated).is_empty());
    // The version the job installed is gone; a later attempt downloads it again.
    assert_eq!(env.folders(), ["8.3.36-nts-x64"]);
    assert!(!env.version_dir("8.3.37-nts-x64").exists());
    *env.pools.fail.lock().unwrap() = false;
    let update = env.check_one().await;
    assert_eq!(update.from_id, v36);
    assert!(!update.installed);
    let report: UpdateReport = done(&env.apply(ApplyRequest::user(update)).await);
    assert!(report.installed);
    assert_eq!(env.fetcher.fetched.lock().unwrap().len(), 2);
    assert_eq!(env.current().as_deref(), Some("8.3.37-nts-x64"));
    // No completed update to undo for another series.
    let job = env
        .engine
        .rollback(RollbackRequest::new(Env::php(), "8.4"))
        .await
        .unwrap();
    assert_eq!(env.wait(job).await.state, JobState::Failed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_operation_per_series_at_a_time() {
    let env = Env::new(php_manifest("runtime", "auto-patch", 1));
    env.install("8.3.36").await;
    env.publish("8.3.37");
    let gate = Arc::new(Semaphore::new(0));
    *env.fetcher.gate.lock().unwrap() = Some(Arc::clone(&gate));
    let update = env.check_one().await;
    let first = env
        .engine
        .apply(ApplyRequest::auto(update.clone()))
        .await
        .unwrap();
    let second = env
        .engine
        .apply(ApplyRequest::user(update.clone()))
        .await
        .unwrap();
    assert_eq!(first, second, "same target: the running job");
    let err = env
        .engine
        .rollback(RollbackRequest::new(Env::php(), "8.3"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "conflict");
    let status = env.engine.status().await.unwrap();
    assert_eq!(status.in_flight.len(), 1);
    gate.add_permits(10);
    let _report: UpdateReport = done(&env.wait(first).await);
    // The slot is released once the job ended.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !env.engine.status().await.unwrap().in_flight.is_empty() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // A blocked update is refused without a job.
    let mut blocked = update;
    blocked.blocked = Some("other compiler".into());
    assert_eq!(
        env.engine
            .apply(ApplyRequest::user(blocked))
            .await
            .unwrap_err()
            .code(),
        "conflict"
    );
}
