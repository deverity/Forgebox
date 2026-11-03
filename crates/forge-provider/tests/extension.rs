//! `kind: Extension` installs (M2-E2-T8, ADR-0043), end to end in a temporary `ForgeRoot`:
//! a fixture extension with two builds (PHP 8.4 nts/vs17 and 8.3 nts/vs16), releases
//! resolved by the real `http` provider from the manifest (no network: curated URLs and
//! hashes), fake PHP versions registered in `tool_versions` and the layout engine with a
//! renderer that records its calls.
//!
//! * every compatible installed version gets the DLL of its own build, an incompatible one
//!   is skipped; `--for` installs into that version only;
//! * a wrong sha256 leaves `bin/` (and `etc/`) untouched, also over an installed DLL;
//! * an incompatible explicit target is `not_found` naming the compatible series;
//! * removing for one target removes that DLL only, and the series overlay only when no
//!   other installation shares it;
//! * removing the target version drops the linked rows; `carry_extensions` re-links the
//!   files `update.migrate` copied into a new version.
#![cfg(windows)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use forge_core::error::Result;
use forge_core::event::{EventBus, EventKind};
use forge_core::ids::{JobId, ToolId, VersionId};
use forge_core::jobs::{JobInfo, JobRegistry, JobState};
use forge_core::paths::{ForgeRoot, InitOptions};
use forge_core::platform::{self, MemoryUserEnv};
use forge_core::store::{
    NewToolVersion, Store, ToolVersionRepo, ToolVersionRow, VersionSource, VersionState,
};
use forge_manifest::Manifest;
use forge_manifest::schema::{ExtensionManifest, ToolManifest};
use forge_provider::layout::extension::plan;
use forge_provider::layout::{
    ExtensionInstallRequest, ExtensionLink, ExtensionReport, ExtensionTarget, InstallOptions,
    Layout, RemovedExtension, RenderRequest, TemplateRenderer,
};
use forge_provider::provider::{ProviderConfig, ProviderRegistry, Release, ResolveRequest};
use sha2::{Digest, Sha256};

const DLL_84: &[u8] = b"fakext 1.2.0 for php 8.4 nts vs17";
const DLL_83: &[u8] = b"fakext 1.2.0 for php 8.3 nts vs16";

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The fixture extension: two curated builds told apart by `php_series`, like xdebug.
fn fakext() -> Arc<ExtensionManifest> {
    let text = format!(
        r#"
apiVersion: forgebox.dev/v1
kind: Extension
metadata: {{ id: fakext, name: Fake extension, category: devtool }}
spec:
  extends: php
  compat:
    - {{ tool: "~8.4", variant: {{ ts: nts, compiler: vs17 }}, version: "1.*" }}
    - {{ tool: "~8.3", variant: {{ ts: nts, compiler: vs16 }}, version: "1.*" }}
  source:
    providers:
      - type: http
        url: "https://ext.example.invalid/php_fakext-{{version}}-{{php_series}}-{{compiler}}.dll"
    releases:
      - version: "1.2.0"
        url: https://ext.example.invalid/php_fakext-1.2.0-8.4-nts-vs17.dll
        sha256: "{sha84}"
        size: {size84}
        variants: {{ php_series: "8.4", ts: nts, compiler: vs17 }}
      - version: "1.2.0"
        url: https://ext.example.invalid/php_fakext-1.2.0-8.3-nts-vs16.dll
        sha256: "{sha83}"
        size: {size83}
        variants: {{ php_series: "8.3", ts: nts, compiler: vs16 }}
  install:
    archive: file
    target: "{{{{ paths.version }}}}/ext/php_fakext.dll"
    post_install:
      - render: {{ template: "php/fakext.ini.j2", to: "{{{{ paths.etc }}}}/php/{{{{ series }}}}/conf.d/20-fakext.ini" }}
  toggle: {{ enable: "zend_extension=php_fakext.dll", disable: ";zend_extension=php_fakext.dll" }}
"#,
        sha84 = sha256_hex(DLL_84),
        size84 = DLL_84.len(),
        sha83 = sha256_hex(DLL_83),
        size83 = DLL_83.len(),
    );
    match forge_manifest::load_str(&text).unwrap_or_else(|e| panic!("{e}")) {
        Manifest::Extension(ext) => {
            let issues = forge_manifest::validate(&Manifest::Extension(ext.clone()));
            assert!(issues.is_empty(), "{issues:#?}");
            Arc::new(ext)
        }
        _ => panic!("not an extension"),
    }
}

fn php_manifest() -> Arc<ToolManifest> {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/tools/php.yaml"),
    )
    .unwrap();
    match forge_manifest::load_str(&text).unwrap() {
        Manifest::Tool(t) => Arc::new(t),
        _ => unreachable!(),
    }
}

/// Writes `; <tool> <version> <paths.version>` and records the call.
#[derive(Default)]
struct FakeRenderer {
    calls: Mutex<Vec<RenderRequest>>,
}

impl TemplateRenderer for FakeRenderer {
    fn render(&self, request: &RenderRequest) -> Result<()> {
        std::fs::write(
            &request.dest,
            format!(
                ";zend_extension=php_fakext.dll ; {} {} {}\n",
                request.tool, request.vars.version, request.vars.paths.version
            ),
        )
        .unwrap();
        self.calls.lock().unwrap().push(request.clone());
        Ok(())
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
    providers: ProviderRegistry,
    ext: Arc<ExtensionManifest>,
    php: Arc<ToolManifest>,
}

impl Env {
    fn new() -> Self {
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
        let layout = Layout::new(
            root.clone(),
            platform::current().with_user_env(user_env),
            store.writer().clone(),
            store.reader().clone(),
            jobs.clone(),
            renderer.clone(),
        );
        let mut config = ProviderConfig::new(dir.path().join("cache"));
        config.offline = true;
        let providers = ProviderRegistry::new(config).unwrap();
        Self {
            _dir: dir,
            work,
            root,
            store,
            bus,
            jobs,
            renderer,
            layout,
            providers,
            ext: fakext(),
            php: php_manifest(),
        }
    }

    /// A fake PHP version: `bin/php/<version>-nts-<compiler>-x64/` with `php.exe`, `ext/`.
    async fn php_version(&self, version: &str, compiler: &str) -> ToolVersionRow {
        let name = format!("{version}-nts-{compiler}-x64");
        let dir = self.root.bin().join("php").join(&name);
        std::fs::create_dir_all(dir.join("ext")).unwrap();
        std::fs::write(dir.join("php.exe"), version).unwrap();
        std::fs::write(dir.join("ext/php_curl.dll"), "curl").unwrap();
        let new = NewToolVersion {
            tool_id: ToolId::new("php").unwrap(),
            tool_name: "PHP".into(),
            version: version.into(),
            canonical_name: name.clone(),
            variant: BTreeMap::from([
                ("ts".to_owned(), "nts".to_owned()),
                ("compiler".to_owned(), compiler.to_owned()),
                ("arch".to_owned(), "x64".to_owned()),
            ]),
            path: forge_core::paths::posix_string(&dir),
            state: VersionState::Installed,
            source: VersionSource::Registry,
        };
        let (id, _) = self
            .store
            .writer()
            .execute(move |conn| ToolVersionRepo::new(conn).record_install(&new, None, None))
            .await
            .unwrap();
        self.row(id)
    }

    fn row(&self, id: VersionId) -> ToolVersionRow {
        ToolVersionRepo::new(self.store.conn())
            .get(id)
            .unwrap()
            .expect("row")
    }

    fn rows(&self, tool: &str) -> Vec<ToolVersionRow> {
        ToolVersionRepo::new(self.store.conn())
            .list(Some(&ToolId::new(tool).unwrap()))
            .unwrap()
    }

    /// Releases of the extension through the real `http` provider (offline: curated).
    async fn releases(&self) -> Vec<Release> {
        let view = self.ext.release_view();
        let variants = BTreeMap::new();
        self.providers
            .resolve(ResolveRequest {
                tool: &view,
                constraint: None,
                variants: &variants,
                channel: None,
            })
            .await
            .unwrap()
    }

    /// The "downloaded" file of `release` (its bytes come from the URL's build).
    fn archive(&self, release: &Release) -> PathBuf {
        let body = if release.url.contains("-8.4-") {
            DLL_84
        } else {
            DLL_83
        };
        let path = self.work.join(&release.file_name);
        std::fs::write(&path, body).unwrap();
        path
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

    /// Installs `release` into `target` and waits for the job.
    async fn install(&self, target: &ToolVersionRow, release: Release) -> JobInfo {
        let archive = self.archive(&release);
        let id = self
            .layout
            .install_extension(ExtensionInstallRequest {
                manifest: Arc::clone(&self.ext),
                target: ExtensionTarget {
                    manifest: Arc::clone(&self.php),
                    row: target.clone(),
                },
                release,
                archive,
                options: InstallOptions::default(),
            })
            .unwrap();
        self.wait(id).await
    }

    /// Plans for `targets` and installs every item.
    async fn install_all(
        &self,
        targets: &[ToolVersionRow],
        explicit: bool,
    ) -> Vec<ExtensionReport> {
        let releases = self.releases().await;
        let planned = plan(
            &self.ext,
            &self.php,
            targets,
            explicit,
            &releases,
            &self.rows("fakext"),
        )
        .unwrap();
        let mut out = Vec::new();
        for item in planned.items {
            let info = self.install(&item.target, item.release).await;
            out.push(done(&info));
        }
        out
    }

    fn dll(&self, target: &ToolVersionRow) -> PathBuf {
        Path::new(&target.path).join("ext/php_fakext.dll")
    }

    fn overlay(&self, series: &str) -> PathBuf {
        self.root
            .etc()
            .join(format!("php/{series}/conf.d/20-fakext.ini"))
    }

    async fn remove(&self, id: VersionId) -> RemovedExtension {
        self.layout
            .remove_extension(Arc::clone(&self.ext), Arc::clone(&self.php), id)
            .await
            .unwrap()
    }

    fn ext_row_for(&self, target: &ToolVersionRow) -> Option<ToolVersionRow> {
        self.rows("fakext")
            .into_iter()
            .find(|r| ExtensionLink::of(r).is_some_and(|l| l.id == target.id))
    }
}

fn done<T: serde::de::DeserializeOwned>(info: &JobInfo) -> T {
    assert_eq!(info.state, JobState::Done, "{:?}", info.error);
    serde_json::from_value(info.result.clone().expect("job result")).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn installs_the_matching_build_into_every_compatible_version() {
    let env = Env::new();
    let php84 = env.php_version("8.4.27", "vs17").await;
    let php83 = env.php_version("8.3.36", "vs16").await;
    let php80 = env.php_version("8.0.30", "vs16").await;
    let targets = [php84.clone(), php83.clone(), php80.clone()];

    let releases = env.releases().await;
    assert_eq!(releases.len(), 2, "{releases:#?}");
    let planned = plan(&env.ext, &env.php, &targets, false, &releases, &[]).unwrap();
    assert_eq!(planned.items.len(), 2);
    assert_eq!(planned.skipped.len(), 1);
    assert_eq!(planned.skipped[0].0, php80.canonical_name);

    let reports = env.install_all(&targets, false).await;
    assert_eq!(reports.len(), 2);
    assert_eq!(std::fs::read(env.dll(&php84)).unwrap(), DLL_84);
    assert_eq!(std::fs::read(env.dll(&php83)).unwrap(), DLL_83);
    assert!(!env.dll(&php80).exists());

    // Rows: one per target, linked through the variant map, path = the DLL.
    let row84 = env.ext_row_for(&php84).expect("row for 8.4");
    assert_eq!(row84.canonical_name, "php-8.4.27-nts-vs17-x64");
    assert_eq!(row84.version, "1.2.0");
    assert_eq!(row84.variant["php_series"], "8.4");
    assert_eq!(row84.variant["target_tool"], "php");
    assert_eq!(row84.variant["target_name"], php84.canonical_name);
    assert_eq!(
        row84.path.to_lowercase(),
        forge_core::paths::posix_string(&env.dll(&php84)).to_lowercase()
    );
    assert!(env.ext_row_for(&php83).is_some());
    assert_eq!(env.rows("fakext").len(), 2);

    // post_install rendered once per target with the target's variables (series overlay).
    let calls = env.renderer.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|c| c.tool.as_str() == "php"));
    assert!(env.overlay("8.4").is_file());
    assert!(env.overlay("8.3").is_file());
    let overlay = std::fs::read_to_string(env.overlay("8.4")).unwrap();
    assert!(
        overlay.starts_with(";zend_extension"),
        "disabled: {overlay}"
    );
    assert!(overlay.contains("8.4.27-nts-vs17-x64"), "{overlay}");
    assert!(!env.overlay("8.0").exists());

    // Events and staging.
    let completed = env
        .bus
        .history_since(0, usize::MAX)
        .into_iter()
        .filter(|e| e.kind == EventKind::ToolInstallCompleted)
        .count();
    assert_eq!(completed, 2);
    let stage = env.root.tmp_stage();
    assert!(
        std::fs::read_dir(&stage).map_or(true, |mut d| d.next().is_none()),
        "staging left behind"
    );

    // Planning again: both up to date.
    let installed = env.rows("fakext");
    let again = plan(&env.ext, &env.php, &targets, false, &releases, &installed).unwrap();
    assert!(again.items.iter().all(|i| i.up_to_date()));

    // Reinstalling updates the row in place.
    let item = again.items[0].clone();
    let report: ExtensionReport = done(&env.install(&item.target, item.release).await);
    assert!(report.reinstalled);
    assert_eq!(env.rows("fakext").len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_explicit_target_installs_only_there() {
    let env = Env::new();
    let php84 = env.php_version("8.4.27", "vs17").await;
    let php83 = env.php_version("8.3.36", "vs16").await;
    let reports = env.install_all(std::slice::from_ref(&php84), true).await;
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].target.id, php84.id);
    assert_eq!(std::fs::read(env.dll(&php84)).unwrap(), DLL_84);
    assert!(!env.dll(&php83).exists());
    assert!(!env.overlay("8.3").exists());
    assert_eq!(env.rows("fakext").len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_sha256_leaves_bin_untouched() {
    let env = Env::new();
    let php84 = env.php_version("8.4.27", "vs17").await;
    let releases = env.releases().await;
    let planned = plan(
        &env.ext,
        &env.php,
        std::slice::from_ref(&php84),
        true,
        &releases,
        &[],
    )
    .unwrap();
    let mut release = planned.items[0].release.clone();
    release.sha256 = Some(sha256_hex(b"something else"));
    let info = env.install(&php84, release.clone()).await;
    assert_eq!(info.state, JobState::Failed);
    assert_eq!(info.error.unwrap().code, "verification_failed");
    assert!(!env.dll(&php84).exists());
    assert!(!env.overlay("8.4").exists());
    assert!(env.rows("fakext").is_empty());
    assert!(env.renderer.calls.lock().unwrap().is_empty());
    let failed = env
        .bus
        .history_since(0, usize::MAX)
        .into_iter()
        .filter(|e| e.kind == EventKind::ToolInstallFailed)
        .count();
    assert_eq!(failed, 1);

    // Over an installed DLL: the old file and row stay as they were.
    let ok: ExtensionReport = done(&env.install(&php84, planned.items[0].release.clone()).await);
    let before = env.row(ok.version_id);
    let info = env.install(&php84, release).await;
    assert_eq!(info.error.unwrap().code, "verification_failed");
    assert_eq!(std::fs::read(env.dll(&php84)).unwrap(), DLL_84);
    assert_eq!(env.row(ok.version_id), before);
    let ext_dir = Path::new(&php84.path).join("ext");
    let mut names: Vec<String> = std::fs::read_dir(ext_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["php_curl.dll", "php_fakext.dll"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_compatible_build_is_not_found_with_the_compatible_series() {
    let env = Env::new();
    let php80 = env.php_version("8.0.30", "vs16").await;
    let odd84 = env.php_version("8.4.1", "vs16").await;
    let releases = env.releases().await;
    for explicit in [true, false] {
        let err = plan(
            &env.ext,
            &env.php,
            std::slice::from_ref(&php80),
            explicit,
            &releases,
            &[],
        )
        .unwrap_err();
        assert_eq!(err.code(), "not_found");
        let message = err.to_string();
        assert!(message.contains("8.4 (vs17, nts)"), "{message}");
        assert!(message.contains("8.3 (vs16, nts)"), "{message}");
    }
    // The same series with another compiler does not fit either.
    let err = plan(&env.ext, &env.php, &[odd84], true, &releases, &[]).unwrap_err();
    assert!(err.to_string().contains("8.4.1"), "{err}");
    // No target installed at all.
    let err = plan(&env.ext, &env.php, &[], false, &releases, &[]).unwrap_err();
    assert_eq!(err.code(), "not_found");
    assert!(err.to_string().contains("forge tool install php"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_for_one_target_keeps_the_others_and_shared_overlays() {
    let env = Env::new();
    let php84 = env.php_version("8.4.27", "vs17").await;
    let php84_old = env.php_version("8.4.26", "vs17").await;
    let php83 = env.php_version("8.3.36", "vs16").await;
    env.install_all(&[php84.clone(), php84_old.clone(), php83.clone()], false)
        .await;
    assert_eq!(env.rows("fakext").len(), 3);

    // 8.4.27: its DLL goes; 8.4.26 renders the same series overlay, so it stays.
    let row = env.ext_row_for(&php84).unwrap();
    let removed = env.remove(row.id).await;
    assert!(!env.dll(&php84).exists());
    assert!(removed.removed_files.is_empty());
    assert_eq!(removed.shared_files, [env.overlay("8.4")]);
    assert!(env.overlay("8.4").is_file());
    assert_eq!(std::fs::read(env.dll(&php84_old)).unwrap(), DLL_84);
    assert_eq!(std::fs::read(env.dll(&php83)).unwrap(), DLL_83);
    assert!(env.ext_row_for(&php84).is_none());
    assert_eq!(env.rows("fakext").len(), 2);
    // The rest of the target version is untouched.
    assert!(Path::new(&php84.path).join("ext/php_curl.dll").is_file());

    // The last 8.4 installation takes the overlay with it.
    let row = env.ext_row_for(&php84_old).unwrap();
    let removed = env.remove(row.id).await;
    assert_eq!(removed.removed_files, [env.overlay("8.4")]);
    assert!(!env.overlay("8.4").exists());
    assert!(env.overlay("8.3").is_file());
    assert_eq!(env.rows("fakext").len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn target_versions_going_away_or_updated_keep_the_rows_consistent() {
    let env = Env::new();
    let php83 = env.php_version("8.3.36", "vs16").await;
    let php84 = env.php_version("8.4.27", "vs17").await;
    env.install_all(&[php83.clone(), php84.clone()], false)
        .await;

    // update.migrate copied ext/ into 8.4.28: the installation follows it.
    let php84_new = env.php_version("8.4.28", "vs17").await;
    std::fs::copy(env.dll(&php84), env.dll(&php84_new)).unwrap();
    let carried = env
        .layout
        .carry_extensions(&php84, &php84_new)
        .await
        .unwrap();
    assert_eq!(carried.len(), 1);
    let row = env.ext_row_for(&php84_new).expect("carried row");
    assert_eq!(row.canonical_name, "php-8.4.28-nts-vs17-x64");
    assert_eq!(row.version, "1.2.0");
    assert_eq!(row.variant["php_series"], "8.4");
    // Nothing to carry when the file did not come along.
    let php83_new = env.php_version("8.3.37", "vs16").await;
    assert!(
        env.layout
            .carry_extensions(&php83, &php83_new)
            .await
            .unwrap()
            .is_empty()
    );

    // Removing a target version drops the rows linked to it (the DLL went with it).
    let removed = env
        .layout
        .remove_version(Arc::clone(&env.php), php84.id)
        .await
        .unwrap();
    assert_eq!(removed.extensions.len(), 1);
    assert!(env.ext_row_for(&php84).is_none());
    assert!(env.ext_row_for(&php84_new).is_some());
    assert!(env.ext_row_for(&php83).is_some());
}
