//! Import of a Laragon installation (M1-E5-T5): the scan of a fixture that reproduces the
//! minimal structure (snapshot of the plan), `apply` in a temporary Forgebox root
//! (reference, copy, move, dry run, idempotence) and, `#[ignore]`d, a read-only dry run
//! against the real `C:\laragon`.
//!
//! The fixture (`tests/fixtures/laragon/`) has no executables; the test compiles
//! `tests/fixtures/fake_tool.rs` with rustc once and copies it as `php.exe`, `php-cgi.exe`
//! and `mysqld.exe` next to each `fake-version` file (first line: the version it reports,
//! second line: the executables to place).
#![cfg(windows)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use forge_core::config::ForgeConfig;
use forge_core::event::EventBus;
use forge_core::paths::{ForgeRoot, InitOptions, plain_path, posix_string};
use forge_core::store::{InstanceRepo, SiteRepo, Store, ToolVersionRepo, VersionSource};
use forge_manifest::Manifest;
use forge_manifest::schema::ToolManifest;
use forge_sites::import::{ApplyEnv, ApplyOutcome, CaseConvertFn, kind};
use forge_sites::{ImportOptions, ImportPlan, LaragonImporter, SiteRules, SiteService};
use futures::FutureExt;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Compiles the fake tool once per test binary.
fn fake_exe() -> &'static Path {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let out_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("import-fake-tool");
        std::fs::create_dir_all(&out_dir).unwrap();
        let exe = out_dir.join(format!("fake_tool_{}.exe", std::process::id()));
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let output = Command::new(rustc)
            .args(["--edition", "2021", "-O", "-o"])
            .arg(&exe)
            .arg(crate_dir().join("tests/fixtures/fake_tool.rs"))
            .output()
            .expect("running rustc for the fake tool");
        assert!(
            output.status.success(),
            "building the fake tool failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        exe
    })
}

fn manifests() -> Vec<ToolManifest> {
    let dir = crate_dir().join("../../registry/tools");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "yaml") {
            if let Manifest::Tool(tool) = forge_manifest::load_file(&path).unwrap() {
                out.push(tool);
            }
        }
    }
    out
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

/// Places the fake executables next to every `fake-version` file.
fn place_fake_tools(dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            place_fake_tools(&path);
        } else if path.file_name().is_some_and(|n| n == "fake-version") {
            let text = std::fs::read_to_string(&path).unwrap();
            for exe in text.lines().nth(1).unwrap_or("").split(',') {
                if !exe.trim().is_empty() {
                    std::fs::copy(fake_exe(), path.with_file_name(exe.trim())).unwrap();
                }
            }
        }
    }
}

struct Lab {
    _tmp: tempfile::TempDir,
    /// The copy of the fixture (the "Laragon" being imported).
    laragon: PathBuf,
    root: ForgeRoot,
    store: Store,
    sites: SiteService,
    importer: LaragonImporter,
}

impl Lab {
    fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("fb-import-")
            .tempdir()
            .unwrap();
        let base = plain_path(&std::fs::canonicalize(tmp.path()).unwrap());
        let laragon = base.join("laragon");
        copy_tree(&crate_dir().join("tests/fixtures/laragon"), &laragon);
        place_fake_tools(&laragon);
        let root = ForgeRoot::new(base.join("forgebox")).unwrap();
        root.init(InitOptions::default()).unwrap();
        let store = Store::open_in_memory().unwrap();
        let sites = SiteService::new(
            store.writer().clone(),
            store.reader().clone(),
            EventBus::new(),
            SiteRules::default(),
        );
        Self {
            _tmp: tmp,
            laragon,
            root,
            store,
            sites,
            importer: LaragonImporter::new(manifests()),
        }
    }

    async fn scan(&self) -> ImportPlan {
        self.importer.scan(&self.laragon).await.unwrap()
    }

    fn env(&self, convert_case: Option<CaseConvertFn>) -> ApplyEnv {
        ApplyEnv {
            root: self.root.clone(),
            writer: self.store.writer().clone(),
            reader: self.store.reader().clone(),
            sites: self.sites.clone(),
            config: ForgeConfig::default(),
            convert_case,
        }
    }

    async fn apply(&self, plan: &ImportPlan, options: &ImportOptions) -> ApplyOutcome {
        LaragonImporter::apply(plan, options, &self.env(None))
            .await
            .unwrap()
    }

    fn count_rows(&self) -> (usize, usize, usize) {
        let conn = self.store.conn();
        (
            ToolVersionRepo::new(conn).list(None).unwrap().len(),
            SiteRepo::new(conn).list().unwrap().len(),
            InstanceRepo::new(conn).list().unwrap().len(),
        )
    }
}

fn junction(link: &Path, target: &Path) {
    // `mklink` wants backslashes.
    let backslashes = |p: &Path| p.to_string_lossy().replace('/', "\\");
    let made = Command::new(r"C:\Windows\System32\cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(backslashes(link))
        .arg(backslashes(target))
        .output()
        .expect("cmd.exe");
    assert!(
        made.status.success(),
        "mklink: {}",
        String::from_utf8_lossy(&made.stdout)
    );
}

fn plan_json(lab: &Lab, plan: &ImportPlan) -> String {
    serde_json::to_string_pretty(plan)
        .unwrap()
        .replace(&posix_string(&lab.laragon), "<laragon>")
}

fn triples(outcome: &ApplyOutcome) -> Vec<(String, String, bool)> {
    outcome
        .report
        .items
        .iter()
        .map(|i| (i.kind.to_owned(), i.name.clone(), i.imported))
        .collect()
}

// ------------------------------------------------------------------------- scan

#[tokio::test]
async fn scan_of_the_fixture_matches_the_snapshot() {
    let lab = Lab::new();
    let plan = lab.scan().await;
    insta::assert_snapshot!("laragon_plan", plan_json(&lab, &plan));
}

#[tokio::test]
async fn scan_reads_the_moved_installation_through_the_ini_root() {
    let lab = Lab::new();
    let plan = lab.scan().await;
    // The vhost files say C:/laragon/www/...; the folder scanned is somewhere else.
    assert_eq!(plan.original_root.as_deref(), Some("C:/laragon"));
    let app1 = plan
        .sites
        .iter()
        .find(|s| s.hosts[0] == "app1.test")
        .unwrap();
    assert_eq!(app1.project_dir, format!("{}/www/app1", plan.source));
    assert_eq!(app1.root, format!("{}/www/app1/public", plan.source));
    assert_eq!(app1.driver, "laravel");
    assert!(app1.secure);
}

#[tokio::test]
async fn the_php_junction_decides_the_active_version_over_the_ini() {
    let lab = Lab::new();
    let plain = lab.scan().await;
    assert_eq!(
        plain.prefs.active_php.as_deref(),
        Some("php-8.1-Win32-vs16-x64"),
        "no junction: [php] Version"
    );
    junction(
        &lab.laragon.join("bin/php/php"),
        &lab.laragon.join("bin/php/php-8.3-Win32-vs16-x64"),
    );
    let plan = lab.scan().await;
    assert_eq!(
        plan.prefs.active_php.as_deref(),
        Some("php-8.3-Win32-vs16-x64")
    );
    assert_eq!(plan.prefs.php_series.as_deref(), Some("8.3"));
    // Sites pin it; the junction itself is not a version.
    let php_sites: Vec<_> = plan
        .sites
        .iter()
        .filter_map(|s| s.php_series.as_deref())
        .collect();
    assert_eq!(php_sites, ["8.3", "8.3"]);
    assert_eq!(
        plan.tools
            .iter()
            .filter(|t| t.tool_id.as_str() == "php")
            .count(),
        2
    );
    assert!(
        plan.skipped.iter().any(|s| s.kind == kind::TOOL_VERSION
            && s.name == "php/php"
            && s.reason.contains("php-8.3")),
        "{:?}",
        plan.skipped
    );
    // Remove the link, not its target.
    std::fs::remove_dir(lab.laragon.join("bin/php/php")).unwrap();
    assert!(
        lab.laragon
            .join("bin/php/php-8.3-Win32-vs16-x64/php.exe")
            .is_file()
    );
}

#[tokio::test]
async fn vhosts_that_share_a_root_become_one_site_with_aliases() {
    let lab = Lab::new();
    let enabled = lab.laragon.join("etc/apache2/sites-enabled");
    std::fs::write(
        enabled.join("auto.app1.local.conf"),
        "define ROOT \"C:/laragon/www/app1\"\ndefine SITE \"app1.local\"\n<VirtualHost *:80>\nDocumentRoot \"${ROOT}\"\nServerName ${SITE}\n</VirtualHost>\n",
    )
    .unwrap();
    std::fs::write(
        lab.laragon.join("etc/nginx/sites-enabled/shop.conf"),
        "server { listen 80; server_name Shop.test; root \"C:/laragon/www/app2\"; }\nserver { listen 80; server_name nowhere.test; root C:/laragon/www/missing; }\n",
    )
    .unwrap();
    let plan = lab.scan().await;
    let app1 = plan
        .sites
        .iter()
        .find(|s| s.hosts[0] == "app1.test")
        .unwrap();
    assert_eq!(app1.hosts, ["app1.test", "app1.local"]);
    assert_eq!(app1.sources.len(), 2);
    // app2 is served by the Apache file (app-two.test) and the Nginx one (shop.test).
    let app2 = plan
        .sites
        .iter()
        .find(|s| s.hosts.contains(&"shop.test".into()))
        .unwrap();
    assert_eq!(app2.hosts, ["app-two.test", "shop.test"]);
    assert!(
        app2.notes.iter().any(|n| n.contains("App_Two.test")),
        "{:?}",
        app2.notes
    );
    assert!(
        plan.skipped.iter().any(|s| s.kind == kind::SITE
            && s.name == "nowhere.test"
            && s.reason.contains("não existe")),
        "{:?}",
        plan.skipped
    );
    assert!(
        plan.skipped
            .iter()
            .any(|s| s.kind == kind::VHOST && s.name.ends_with("reverse.app1.test.conf")),
        "{:?}",
        plan.skipped
    );
}

#[tokio::test]
async fn scan_refuses_a_folder_that_is_not_laragon_and_never_writes() {
    let lab = Lab::new();
    let empty = lab.root.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let err = lab.importer.scan(&empty).await.unwrap_err();
    assert_eq!(err.code(), "invalid_input");
    let missing = lab.root.path().join("nope");
    assert_eq!(
        lab.importer.scan(&missing).await.unwrap_err().code(),
        "not_found"
    );

    let before = tree(&lab.laragon);
    lab.scan().await;
    assert_eq!(tree(&lab.laragon), before, "scan only reads");
}

fn tree(dir: &Path) -> BTreeSet<(String, u64)> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeSet<(String, u64)>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let meta = entry.metadata().unwrap();
            out.insert((
                posix_string(path.strip_prefix(base).unwrap()),
                if meta.is_file() { meta.len() } else { 0 },
            ));
            if meta.is_dir() {
                walk(&path, base, out);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(dir, dir, &mut out);
    out
}

// ------------------------------------------------------------------------ apply

#[tokio::test]
async fn apply_by_reference_registers_tools_sites_and_instances() {
    let lab = Lab::new();
    let plan = lab.scan().await;
    let before = tree(&lab.laragon);
    let outcome = lab.apply(&plan, &ImportOptions::default()).await;

    // Tool versions: registered where they are, source `import`, folder names untouched.
    let rows = ToolVersionRepo::new(lab.store.conn()).list(None).unwrap();
    assert_eq!(rows.len(), 3, "{rows:#?}");
    let php81 = rows.iter().find(|r| r.version == "8.1.22").unwrap();
    assert_eq!(php81.canonical_name, "php-8.1-Win32-vs16-x64");
    assert_eq!(php81.source, VersionSource::Import);
    assert_eq!(
        php81.path,
        format!("{}/bin/php/php-8.1-Win32-vs16-x64", plan.source)
    );
    assert_eq!(php81.variant["ts"], "nts");
    assert!(
        rows.iter()
            .any(|r| r.tool_id.as_str() == "mysql" && r.version == "8.0.30")
    );

    // Sites: driver detected, PHP = the active series, TLS kept, host normalized.
    let sites = SiteRepo::new(lab.store.conn()).list().unwrap();
    let hosts: Vec<&str> = sites.iter().map(|s| s.host.as_str()).collect();
    assert_eq!(hosts, ["app-two.test", "app1.test"]);
    let app1 = sites.iter().find(|s| s.host == "app1.test").unwrap();
    assert_eq!(app1.driver.as_deref(), Some("laravel"));
    assert_eq!(
        app1.root.as_deref(),
        Some(format!("{}/www/app1/public", plan.source).as_str())
    );
    assert_eq!(app1.php.series.as_deref(), Some("8.1"));
    assert!(app1.secure && app1.http_redirect && !app1.parked);
    let two = sites.iter().find(|s| s.host == "app-two.test").unwrap();
    assert_eq!(two.driver.as_deref(), Some("generic-php"));
    assert!(!two.secure, "no :443 block in Laragon");

    // Database: an instance over Laragon's data folder, marked as a reference.
    let instances = InstanceRepo::new(lab.store.conn()).list().unwrap();
    assert_eq!(instances.len(), 1);
    let db = &instances[0];
    assert_eq!(
        (db.tool_id.as_str(), db.name.as_str()),
        ("mysql", "mysql-8")
    );
    assert_eq!(
        db.external_data_dir(),
        Some(format!("{}/data/mysql-8", plan.source).as_str())
    );
    assert!(db.data_dir.is_some() && !db.autostart);

    // Preferences: the parked folder is added; the TLD already matches.
    assert!(outcome.config_changed);
    assert_eq!(
        outcome.config.parked_dirs,
        ["www".to_owned(), format!("{}/www", plan.source)],
        "the default `www` stays; Laragon's folder is added"
    );
    let tld = outcome
        .report
        .items
        .iter()
        .find(|i| i.kind == kind::PREF)
        .unwrap();
    assert!(tld.imported);
    assert!(!outcome.report.dry_run);
    assert_eq!(
        (outcome.tool_versions, outcome.sites, outcome.instances),
        (3, 2, 1)
    );

    // Laragon is untouched; what was skipped says why.
    assert_eq!(tree(&lab.laragon), before);
    let skipped: Vec<_> = outcome
        .report
        .skipped()
        .map(|i| (i.kind, i.name.as_str()))
        .collect();
    assert!(
        skipped
            .iter()
            .any(|(k, n)| *k == kind::TOOL && *n == "cmder"),
        "{skipped:?}"
    );
    assert!(
        skipped
            .iter()
            .any(|(k, n)| *k == kind::VHOST && n.ends_with("reverse.app1.test.conf")),
        "{skipped:?}"
    );
}

#[tokio::test]
async fn importing_twice_creates_nothing_new() {
    let lab = Lab::new();
    let plan = lab.scan().await;
    lab.apply(&plan, &ImportOptions::default()).await;
    let counts = lab.count_rows();

    let again = lab.apply(&plan, &ImportOptions::default()).await;
    assert_eq!(lab.count_rows(), counts);
    assert_eq!(
        (again.tool_versions, again.sites, again.instances),
        (0, 0, 0)
    );
    let notes: Vec<_> = again
        .report
        .items
        .iter()
        .filter(|i| matches!(i.kind, "tool_version" | "site" | "database_dir"))
        .collect();
    assert_eq!(notes.len(), 6);
    assert!(notes.iter().all(|i| !i.imported), "{notes:#?}");
}

#[tokio::test]
async fn dry_run_decides_like_the_real_import_but_writes_nothing() {
    let lab = Lab::new();
    let plan = lab.scan().await;
    let before = tree(&lab.laragon);
    let dry = lab
        .apply(
            &plan,
            &ImportOptions {
                dry_run: true,
                copy_tools: true,
                move_data: true,
                ..ImportOptions::default()
            },
        )
        .await;
    assert!(dry.report.dry_run);
    assert_eq!(lab.count_rows(), (0, 0, 0));
    assert!(!dry.config_changed, "preferences untouched in a dry run");
    assert!(!lab.root.bin().join("php").exists(), "nothing copied");
    assert!(
        !lab.root.data().join("mysql").join("mysql-8").exists(),
        "nothing moved"
    );
    assert_eq!(tree(&lab.laragon), before);
    assert_eq!(
        dry.report
            .imported()
            .filter(|i| i.kind == kind::SITE)
            .count(),
        2
    );

    // The real run with the same options reports the same decisions.
    let real = lab
        .apply(
            &plan,
            &ImportOptions {
                copy_tools: true,
                move_data: true,
                ..ImportOptions::default()
            },
        )
        .await;
    assert_eq!(triples(&dry), triples(&real));
    assert_eq!(lab.count_rows(), (3, 2, 1));
}

#[tokio::test]
async fn copy_mode_places_tools_under_bin_with_forgebox_names() {
    let lab = Lab::new();
    let plan = lab.scan().await;
    lab.apply(
        &plan,
        &ImportOptions {
            copy_tools: true,
            ..ImportOptions::default()
        },
    )
    .await;
    let rows = ToolVersionRepo::new(lab.store.conn()).list(None).unwrap();
    let php81 = rows.iter().find(|r| r.version == "8.1.22").unwrap();
    assert_eq!(php81.canonical_name, "8.1.22-nts-vs16-x64");
    let dest = lab.root.bin().join("php").join("8.1.22-nts-vs16-x64");
    assert_eq!(php81.path, posix_string(&dest));
    assert!(dest.join("php.exe").is_file() && dest.join("php-cgi.exe").is_file());
    // The mysql folder has no variants: it keeps its Laragon name.
    let mysql = rows.iter().find(|r| r.tool_id.as_str() == "mysql").unwrap();
    assert!(
        lab.root
            .bin()
            .join("mysql")
            .join(&mysql.canonical_name)
            .join("bin/mysqld.exe")
            .is_file()
    );
    // The source is intact and no staging folder is left behind.
    assert!(
        lab.laragon
            .join("bin/php/php-8.1-Win32-vs16-x64/php.exe")
            .is_file()
    );
    let leftovers: Vec<_> = std::fs::read_dir(lab.root.bin().join("php"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
}

#[tokio::test]
async fn move_mode_moves_the_data_folder_into_the_root() {
    let lab = Lab::new();
    let plan = lab.scan().await;
    lab.apply(
        &plan,
        &ImportOptions {
            move_data: true,
            ..ImportOptions::default()
        },
    )
    .await;
    let dest = lab.root.data().join("mysql").join("mysql-8");
    assert!(dest.join("auto.cnf").is_file());
    assert!(!lab.laragon.join("data/mysql-8").exists());
    let db = InstanceRepo::new(lab.store.conn())
        .list()
        .unwrap()
        .remove(0);
    assert_eq!(db.data_dir.as_deref(), Some(posix_string(&dest).as_str()));
    assert_eq!(db.external_data_dir(), None, "a moved folder is managed");
}

#[tokio::test]
async fn a_busy_destination_keeps_the_data_as_a_reference() {
    let lab = Lab::new();
    let plan = lab.scan().await;
    let dest = lab.root.data().join("mysql").join("mysql-8");
    std::fs::create_dir_all(&dest).unwrap();
    let outcome = lab
        .apply(
            &plan,
            &ImportOptions {
                move_data: true,
                ..ImportOptions::default()
            },
        )
        .await;
    assert!(lab.laragon.join("data/mysql-8/auto.cnf").is_file());
    let db = InstanceRepo::new(lab.store.conn())
        .list()
        .unwrap()
        .remove(0);
    assert!(db.external_data_dir().is_some());
    let item = outcome
        .report
        .items
        .iter()
        .find(|i| i.kind == kind::DATABASE_DIR)
        .unwrap();
    assert!(item.imported && item.note.as_deref().unwrap().contains("já existe"));
}

#[tokio::test]
async fn existing_sites_and_foreign_tlds_are_skipped_with_a_reason() {
    let lab = Lab::new();
    let mut plan = lab.scan().await;
    // A site that exists already, and one whose only host has a TLD Forgebox does not serve.
    let draft =
        forge_sites::SiteDraft::from_dir(&lab.laragon.join("www/app2"), "app-two.test", None)
            .unwrap();
    lab.sites.create(draft).await.unwrap();
    let mut other = plan.sites[0].clone();
    other.hosts = vec!["thing.dev".into()];
    plan.sites.push(other);
    let outcome = lab.apply(&plan, &ImportOptions::default()).await;
    let site = |name: &str| {
        outcome
            .report
            .items
            .iter()
            .find(|i| i.kind == kind::SITE && i.name == name)
            .unwrap()
    };
    assert!(!site("app-two.test").imported);
    assert!(
        site("app-two.test")
            .note
            .as_deref()
            .unwrap()
            .contains("já existe")
    );
    assert!(!site("thing.dev").imported);
    assert!(site("thing.dev").note.as_deref().unwrap().contains("TLD"));
    assert!(site("app1.test").imported);
}

#[tokio::test]
async fn case_conversion_is_offered_and_runs_only_when_asked() {
    let lab = Lab::new();
    let plan = lab.scan().await;
    assert!(plan.parked[0].offer_case_conversion);

    let calls: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let recorder = Arc::clone(&calls);
    let convert: CaseConvertFn = Arc::new(move |dir| {
        recorder.lock().unwrap().push(dir);
        async { Ok(()) }.boxed()
    });

    // Not asked: offered in the note, converter untouched.
    let outcome = LaragonImporter::apply(
        &plan,
        &ImportOptions::default(),
        &lab.env(Some(convert.clone())),
    )
    .await
    .unwrap();
    assert!(calls.lock().unwrap().is_empty());
    let parked = outcome
        .report
        .items
        .iter()
        .find(|i| i.kind == kind::PARKED_DIR)
        .unwrap();
    assert!(parked.note.as_deref().unwrap().contains("--convert-case"));

    // Asked: converted before the folder is parked.
    let lab = Lab::new();
    let plan = lab.scan().await;
    let outcome = LaragonImporter::apply(
        &plan,
        &ImportOptions {
            convert_case_sensitive: true,
            ..ImportOptions::default()
        },
        &lab.env(Some(convert.clone())),
    )
    .await
    .unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        [PathBuf::from(format!("{}/www", plan.source))]
    );
    assert!(
        outcome
            .report
            .items
            .iter()
            .any(|i| i.kind == kind::CASE_CONVERSION && i.imported)
    );

    // A failing conversion is a line of the report, not an error of the import.
    let failing: CaseConvertFn = Arc::new(|_| {
        async { Err(forge_core::error::ForgeError::Conflict("em uso".into())) }.boxed()
    });
    let lab = Lab::new();
    let plan = lab.scan().await;
    let outcome = LaragonImporter::apply(
        &plan,
        &ImportOptions {
            convert_case_sensitive: true,
            ..ImportOptions::default()
        },
        &lab.env(Some(failing)),
    )
    .await
    .unwrap();
    let item = outcome
        .report
        .items
        .iter()
        .find(|i| i.kind == kind::CASE_CONVERSION)
        .unwrap();
    assert!(!item.imported && item.note.as_deref().unwrap().contains("em uso"));
    assert_eq!(lab.count_rows(), (3, 2, 1));
}

// ------------------------------------------------------------------ real Laragon

/// Size and modification time of what the import must not change.
fn fingerprint(root: &Path) -> BTreeSet<(String, u64, u128)> {
    let mut out = BTreeSet::new();
    for rel in [
        "usr",
        "etc/apache2/sites-enabled",
        "etc/nginx/sites-enabled",
        "data",
        "bin/php",
        "bin/mysql",
        "bin/apache",
        "bin/nginx",
        "bin/nodejs",
        "www",
    ] {
        let Ok(entries) = std::fs::read_dir(root.join(rel)) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            let modified = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            out.insert((
                format!("{rel}/{}", entry.file_name().to_string_lossy()),
                meta.len(),
                modified,
            ));
        }
    }
    out
}

/// Dry run against the user's real `C:\laragon` (read-only: it runs the version command of
/// each tool and reads files). Run with
/// `cargo test -p forge-sites --test import_laragon -- --ignored --nocapture real_laragon`.
#[tokio::test]
#[ignore = "reads the real C:\\laragon"]
async fn real_laragon_dry_run() {
    let laragon = Path::new(r"C:\laragon");
    if !laragon.join("usr").join("laragon.ini").is_file() {
        eprintln!("skipping: C:\\laragon not found");
        return;
    }
    let before = fingerprint(laragon);
    let tmp = tempfile::Builder::new()
        .prefix("fb-import-real-")
        .tempdir()
        .unwrap();
    let root =
        ForgeRoot::new(plain_path(&std::fs::canonicalize(tmp.path()).unwrap()).join("forgebox"))
            .unwrap();
    root.init(InitOptions::default()).unwrap();
    let store = Store::open_in_memory().unwrap();
    let sites = SiteService::new(
        store.writer().clone(),
        store.reader().clone(),
        EventBus::new(),
        SiteRules::default(),
    );
    let env = ApplyEnv {
        root,
        writer: store.writer().clone(),
        reader: store.reader().clone(),
        sites,
        config: ForgeConfig::default(),
        convert_case: None,
    };

    let importer = LaragonImporter::new(manifests());
    let plan = importer.scan(laragon).await.unwrap();
    let outcome = LaragonImporter::apply(
        &plan,
        &ImportOptions {
            dry_run: true,
            ..ImportOptions::default()
        },
        &env,
    )
    .await
    .unwrap();

    println!("IMPORT REPORT (dry run) of {}", plan.source);
    println!(
        "prefs: tld={:?} active_php={:?} php_series={:?} document_roots={:?}",
        plan.prefs.tld, plan.prefs.active_php, plan.prefs.php_series, plan.prefs.document_roots
    );
    for item in &outcome.report.items {
        println!(
            "{:<10} {:<15} {} {}{}",
            if item.imported { "IMPORT" } else { "SKIP" },
            item.kind,
            item.name,
            if item.note.is_some() { "-- " } else { "" },
            item.note.as_deref().unwrap_or("")
        );
    }
    println!(
        "totals: {} would be imported, {} skipped",
        outcome.report.imported().count(),
        outcome.report.skipped().count()
    );

    assert!(outcome.report.dry_run);
    let imported = |k: &str| outcome.report.imported().filter(|i| i.kind == k).count();
    assert!(imported(kind::TOOL_VERSION) >= 3, "php versions");
    assert!(
        outcome
            .report
            .imported()
            .any(|i| i.kind == kind::DATABASE_DIR && i.name.starts_with("mysql-8")),
        "data\\mysql-8"
    );
    assert!(imported(kind::SITE) >= 1, "auto.*.conf sites");
    assert!(imported(kind::PARKED_DIR) >= 1);
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM sites", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(fingerprint(laragon), before, "C:\\laragon was not touched");
}
