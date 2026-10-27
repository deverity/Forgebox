//! Versioned layout (M1-E2-T4): install pipeline, `current` junction, shims and the user
//! `PATH`, end to end in a temporary `ForgeRoot` on NTFS. The "tool" is a tiny fake
//! `php.exe` built from `tests/fake_tool` whose version comes from a text file next to
//! it; archives are zipped by the tests (nothing binary is committed).
#![cfg(windows)]

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use forge_core::config::PathConfig;
use forge_core::error::Result;
use forge_core::event::{Event, EventBus, EventKind};
use forge_core::ids::{JobId, ToolId};
use forge_core::jobs::{JobInfo, JobRegistry, JobState};
use forge_core::paths::{ForgeRoot, InitOptions};
use forge_core::platform::{self, MemoryUserEnv, UserEnv};
use forge_core::store::{Store, ToolVersionRepo, VersionSource};
use forge_manifest::Manifest;
use forge_manifest::schema::ToolManifest;
use forge_provider::layout::{
    CurrentPolicy, InstallOptions, InstallReport, InstallRequest, Layout, RenderRequest,
    TemplateRenderer, ToolCurrentEventPayload, ToolInstallEventPayload,
};
use forge_provider::provider::Release;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------- fixtures

/// Builds `tests/fake_tool` once per test binary into its own target dir.
fn fake_exe() -> &'static Path {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut hasher = DefaultHasher::new();
        manifest_dir.hash(&mut hasher);
        let target = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("fake-tool-{:016x}", hasher.finish()));
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let output = Command::new(cargo)
            .arg("build")
            .arg("--quiet")
            .arg("--manifest-path")
            .arg(manifest_dir.join("tests/fake_tool/Cargo.toml"))
            .arg("--target-dir")
            .arg(&target)
            .env_remove("CARGO_MAKEFLAGS")
            .env_remove("MAKEFLAGS")
            .env_remove("MFLAGS")
            .output()
            .expect("running cargo to build the fake tool");
        assert!(
            output.status.success(),
            "building the fake tool failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let exe = target.join("debug").join("fake_tool.exe");
        assert!(exe.is_file(), "{} missing", exe.display());
        exe
    })
}

const PHP_MANIFEST: &str = r#"
apiVersion: forgebox.dev/v1
kind: Tool
metadata: { id: php, name: PHP, category: runtime }
spec:
  platforms: [windows-x64]
  versioning:
    scheme: semver
    series_key: "{major}.{minor}"
    variants:
      - { key: ts, values: [nts, ts], default: nts }
      - { key: arch, values: [x64], default: x64 }
  source:
    providers:
      - type: php-windows
  install:
    archive: zip
    strip_root: auto
    path: ["."]
    shims: [php]
    version_files: [".php-version"]
    project_key: spec.site.php
    verify: { command: "php.exe -v", regex: "^PHP (\\d+\\.\\d+\\.\\d+)" }
    post_install:
      - copy: { from: "php.ini-development", to: "{{ paths.etc }}/php/{{ series }}/php.ini", if_missing: true }
      - render: { template: "php/php.ini.j2", to: "{{ paths.etc }}/php/{{ series }}/conf.d/10-forgebox.ini" }
      - mkdir: "{{ paths.tmp }}/php/{{ series }}/sessions"
"#;

fn manifest(text: &str) -> Arc<ToolManifest> {
    match forge_manifest::load_str(text).unwrap_or_else(|e| panic!("{e}")) {
        Manifest::Tool(tool) => {
            let issues = forge_manifest::validate(&Manifest::Tool(tool.clone()));
            assert!(issues.is_empty(), "{issues:#?}");
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

/// A php-like zip: `php-<version>/{php.exe, fake-version.txt, php.ini-development}`.
fn php_zip(dir: &Path, version: &str) -> PathBuf {
    let path = dir.join(format!("php-{version}-nts-Win32-x64.zip"));
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    let top = format!("php-{version}");
    zip.add_directory(format!("{top}/"), options).unwrap();
    zip.start_file(format!("{top}/php.exe"), options).unwrap();
    zip.write_all(&std::fs::read(fake_exe()).unwrap()).unwrap();
    zip.start_file(format!("{top}/fake-version.txt"), options)
        .unwrap();
    zip.write_all(version.as_bytes()).unwrap();
    zip.start_file(format!("{top}/php.ini-development"), options)
        .unwrap();
    zip.write_all(format!("; php.ini of {version}\n").as_bytes())
        .unwrap();
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

/// Records render requests and writes a marker file.
#[derive(Default)]
struct FakeRenderer {
    calls: Mutex<Vec<RenderRequest>>,
}

impl TemplateRenderer for FakeRenderer {
    fn render(&self, request: &RenderRequest) -> Result<()> {
        std::fs::write(
            &request.dest,
            format!(
                "; {} rendered with extension_dir = {}/ext\n",
                request.template, request.vars.paths.version
            ),
        )
        .unwrap();
        // The shipped files are reachable while rendering (staged copy during an install),
        // even though `paths.version` already names the final folder.
        assert!(
            request.files_dir.join("php.exe").is_file(),
            "files_dir {} holds the version's files",
            request.files_dir.display()
        );
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
    user_env: Arc<MemoryUserEnv>,
    layout: Layout,
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
        let user_env = Arc::new(MemoryUserEnv::with_path(r"C:\Windows\system32;C:\Tools"));
        let platform = platform::current().with_user_env(user_env.clone());
        let layout = Layout::new(
            root.clone(),
            platform,
            store.writer().clone(),
            store.reader().clone(),
            jobs.clone(),
            renderer.clone(),
        );
        Self {
            _dir: dir,
            work,
            root,
            store,
            bus,
            jobs,
            renderer,
            user_env,
            layout,
        }
    }

    async fn wait(&self, id: JobId) -> JobInfo {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let info = self.jobs.get(id).expect("job known");
            if info.state != JobState::Running {
                return info;
            }
            assert!(Instant::now() < deadline, "job {id} did not finish");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn install(
        &self,
        manifest: &Arc<ToolManifest>,
        release: Release,
        archive: PathBuf,
        options: InstallOptions,
    ) -> JobInfo {
        let id = self
            .layout
            .install_from_archive(InstallRequest {
                manifest: Arc::clone(manifest),
                release,
                archive,
                options,
            })
            .unwrap();
        self.wait(id).await
    }

    fn rows(&self, tool: &str) -> usize {
        ToolVersionRepo::new(self.store.conn())
            .list(Some(&ToolId::new(tool).unwrap()))
            .unwrap()
            .len()
    }

    fn stage_is_empty(&self) -> bool {
        std::fs::read_dir(self.root.tmp_stage())
            .unwrap()
            .next()
            .is_none()
    }
}

fn report(info: &JobInfo) -> InstallReport {
    assert_eq!(info.state, JobState::Done, "{:?}", info.error);
    serde_json::from_value(info.result.clone().expect("job result")).unwrap()
}

fn events_of(bus: &EventBus, kind: EventKind) -> Vec<Event> {
    bus.history_since(0, usize::MAX)
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

/// Runs `bin/shims/<shim>.cmd args` from `cwd` through cmd.exe.
fn run_shim(root: &ForgeRoot, shim: &str, cwd: &Path, args: &[&str]) -> std::process::Output {
    let script = root.bin_shims().join(format!("{shim}.cmd"));
    assert!(script.is_file(), "{} missing", script.display());
    Command::new("cmd.exe")
        .arg("/d")
        .arg("/c")
        .arg(&script)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

// ---------------------------------------------------------------- tests

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_versions_current_switch_shims_and_path() {
    let env = Env::new();
    let php = manifest(PHP_MANIFEST);
    let tool = php.metadata.id.clone();

    // Install 8.3.37 (first version: becomes current) and 8.4.14 (not current).
    let zip83 = php_zip(&env.work, "8.3.37");
    let first = env
        .install(
            &php,
            release("8.3.37", &zip83),
            zip83.clone(),
            InstallOptions::default(),
        )
        .await;
    let first = report(&first);
    assert_eq!(first.canonical_name, "8.3.37-nts-x64");
    assert!(first.current, "first version becomes current");
    let zip84 = php_zip(&env.work, "8.4.14");
    let second = env
        .install(
            &php,
            release("8.4.14", &zip84),
            zip84,
            InstallOptions {
                current: CurrentPolicy::Never,
                ..InstallOptions::default()
            },
        )
        .await;
    let second = report(&second);
    assert!(!second.current);

    let dir83 = env.root.bin().join("php").join("8.3.37-nts-x64");
    let dir84 = env.root.bin().join("php").join("8.4.14-nts-x64");
    for dir in [&dir83, &dir84] {
        assert!(dir.join("php.exe").is_file(), "{}", dir.display());
    }
    assert_eq!(env.rows("php"), 2, "two tool_versions rows");
    let rows = ToolVersionRepo::new(env.store.conn())
        .list(Some(&tool))
        .unwrap();
    assert!(rows.iter().all(|r| r.source == VersionSource::Registry));
    assert!(env.stage_is_empty(), "staging removed");

    // post_install ran with the final paths.
    let ini = env.root.etc().join("php").join("8.3").join("php.ini");
    assert_eq!(
        std::fs::read_to_string(&ini).unwrap(),
        "; php.ini of 8.3.37\n"
    );
    let overlay =
        std::fs::read_to_string(env.root.etc().join("php/8.3/conf.d/10-forgebox.ini")).unwrap();
    assert!(overlay.contains("bin/php/8.3.37-nts-x64/ext"), "{overlay}");
    assert!(!overlay.contains("tmp/stage"), "{overlay}");
    assert!(env.root.tmp().join("php/8.4/sessions").is_dir());
    assert_eq!(env.renderer.calls.lock().unwrap().len(), 2);

    // Events.
    assert_eq!(events_of(&env.bus, EventKind::ToolInstallStarted).len(), 2);
    let completed = events_of(&env.bus, EventKind::ToolInstallCompleted);
    assert_eq!(completed.len(), 2);
    let payload: forge_ipc::api::ToolInstallPayload =
        serde_json::from_value(completed[0].payload.clone()).unwrap();
    assert_eq!(payload.version_id, Some(first.version_id));
    assert_eq!(payload.canonical_name.as_deref(), Some("8.3.37-nts-x64"));

    // current → 8.3 after the first install.
    let current = env.layout.current_version(&tool).await.unwrap().unwrap();
    assert_eq!(current.canonical_name, "8.3.37-nts-x64");

    // use 8.4 / 8.3 / 8.4: atomic switch, measured; the best of three under 50 ms.
    let mut best = Duration::MAX;
    for spec in ["8.4", "8.3", "8.4"] {
        let started = Instant::now();
        let used = env.layout.use_version(&php, spec).await.unwrap();
        let elapsed = started.elapsed();
        best = best.min(elapsed);
        assert!(!used.unchanged);
        assert_eq!(
            used.to,
            format!("{spec}.{}-nts-x64", if spec == "8.4" { 14 } else { 37 })
        );
        println!(
            "use php {spec}: {:.2} ms total, junction switch {} us",
            elapsed.as_secs_f64() * 1000.0,
            used.switch_us
        );
    }
    assert!(best < Duration::from_millis(50), "best use took {best:?}");
    let current = env.layout.current_version(&tool).await.unwrap().unwrap();
    assert_eq!(current.canonical_name, "8.4.14-nts-x64");
    let link = env.root.tool_current_link(&tool);
    assert_eq!(
        std::fs::read_to_string(link.join("fake-version.txt")).unwrap(),
        "8.4.14"
    );
    assert!(!link.with_file_name("current.tmp").exists());
    let again = env.layout.use_version(&php, "8.4.14").await.unwrap();
    assert!(again.unchanged && again.switch_us == 0);
    let switched = events_of(&env.bus, EventKind::ToolCurrentChanged);
    assert_eq!(switched.len(), 4, "first install + three switches");
    let last: forge_ipc::api::ToolCurrentPayload =
        serde_json::from_value(switched[3].payload.clone()).unwrap();
    assert_eq!(last.from.as_deref(), Some("8.3.37-nts-x64"));
    assert_eq!(last.to, "8.4.14-nts-x64");
    let err = env.layout.use_version(&php, "7.4").await.unwrap_err();
    assert_eq!(err.code(), "not_found");

    // Shims: .php-version selects, nothing selects current, unknown versions fail.
    let project = env.work.join("project").join("src");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(env.work.join("project").join(".php-version"), "8.3\n").unwrap();
    let out = run_shim(&env.root, "php", &project, &["-v"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stdout(&out), "PHP 8.3.37 (cli) (fake)");
    let plain = env.work.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let out = run_shim(&env.root, "php", &plain, &["-v"]);
    assert_eq!(stdout(&out), "PHP 8.4.14 (cli) (fake)", "current");
    let out = run_shim(&env.root, "php", &plain, &["-r", "echo 1;", "a b"]);
    assert_eq!(
        stdout(&out),
        "PHP 8.4.14 ARGS -r|echo 1;|a b",
        "arguments pass through"
    );

    std::fs::write(
        env.work.join("project").join(".php-version"),
        "v8.4.14 # pinned\n",
    )
    .unwrap();
    let out = run_shim(&env.root, "php", &project, &["-v"]);
    assert_eq!(stdout(&out), "PHP 8.4.14 (cli) (fake)");
    std::fs::write(
        env.work.join("project").join(".php-version"),
        "# comment\r\n8.3\r\n",
    )
    .unwrap();
    let out = run_shim(&env.root, "php", &project, &["-v"]);
    assert_eq!(stdout(&out), "PHP 8.3.37 (cli) (fake)", "CRLF and comments");
    std::fs::write(project.join(".php-version"), "").unwrap();
    let out = run_shim(&env.root, "php", &project, &["-v"]);
    assert_eq!(
        stdout(&out),
        "PHP 8.3.37 (cli) (fake)",
        "empty file: keep looking up"
    );
    std::fs::remove_file(project.join(".php-version")).unwrap();
    std::fs::write(env.work.join("project").join(".php-version"), "7.4\n").unwrap();
    let out = run_shim(&env.root, "php", &project, &["-v"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("7.4") && err.contains("nao esta instalado"),
        "{err}"
    );
    // A hostile version file is data, never a command.
    std::fs::write(
        env.work.join("project").join(".php-version"),
        "8.3\"&echo.INJECTED&\"\n",
    )
    .unwrap();
    let out = run_shim(&env.root, "php", &project, &["-v"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("INJECTED"),
        "{out:?}"
    );

    // forgebox.yaml (kind: Project): spec.site.php, read by forgebox-project.ps1.
    std::fs::remove_file(env.work.join("project").join(".php-version")).unwrap();
    std::fs::write(
        env.work.join("project").join("forgebox.yaml"),
        "apiVersion: forgebox.dev/v1\nkind: Project\nmetadata: { name: loja }\nspec:\n  site:\n    \
         host: loja.test\n    php: \"8.3\"   # site PHP\n  runtimes:\n    - { tool: nodejs, version: \"22\" }\n",
    )
    .unwrap();
    let out = run_shim(&env.root, "php", &project, &["-v"]);
    assert_eq!(stdout(&out), "PHP 8.3.37 (cli) (fake)", "{out:?}");

    // Shims are idempotent.
    let shims = env.layout.ensure_shims(Arc::clone(&php)).await.unwrap();
    assert_eq!(shims.unchanged, ["php"]);
    assert!(shims.written.is_empty() && shims.removed.is_empty());

    // PATH: bin\php\current once, however many times it is ensured.
    let prefs = PathConfig::default();
    let added = env
        .layout
        .ensure_path(vec![Arc::clone(&php)], prefs.clone())
        .await
        .unwrap();
    assert_eq!(added.added.len(), 1);
    let again = env
        .layout
        .ensure_path(vec![Arc::clone(&php)], prefs)
        .await
        .unwrap();
    assert!(again.added.is_empty());
    let entries = env.user_env.path_entries().unwrap();
    let wanted = link.to_string_lossy().replace('/', "\\");
    assert_eq!(
        entries.iter().filter(|e| **e == wanted).count(),
        1,
        "{entries:?}"
    );
    assert_eq!(env.user_env.notifications(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_sha256_leaves_bin_untouched() {
    let env = Env::new();
    let php = manifest(PHP_MANIFEST);
    let zip = php_zip(&env.work, "8.3.37");
    let mut bad = release("8.3.37", &zip);
    bad.sha256 = Some("0".repeat(64));
    let info = env.install(&php, bad, zip, InstallOptions::default()).await;
    assert_eq!(info.state, JobState::Failed);
    assert_eq!(info.error.as_ref().unwrap().code, "verification_failed");
    assert!(!env.root.bin().join("php").join("8.3.37-nts-x64").exists());
    assert!(!env.root.bin().join("php").exists(), "bin/ untouched");
    assert_eq!(env.rows("php"), 0);
    assert!(env.stage_is_empty());
    let failed = events_of(&env.bus, EventKind::ToolInstallFailed);
    assert_eq!(failed.len(), 1);
    let payload: ToolInstallEventPayload =
        serde_json::from_value(failed[0].payload.clone()).unwrap();
    assert_eq!(payload.error.unwrap().code, "verification_failed");
    let ipc: forge_ipc::api::ToolInstallPayload =
        serde_json::from_value(failed[0].payload.clone()).unwrap();
    assert_eq!(ipc.step.as_deref(), Some("verify"));

    // A release without any checksum is refused before a job exists.
    let mut unverified = release("8.3.37", &env.work.join("php-8.3.37-nts-Win32-x64.zip"));
    unverified.sha256 = None;
    let err = env
        .layout
        .install_from_archive(InstallRequest {
            manifest: Arc::clone(&php),
            release: unverified,
            archive: env.work.join("php-8.3.37-nts-Win32-x64.zip"),
            options: InstallOptions::default(),
        })
        .unwrap_err();
    assert_eq!(err.code(), "verification_failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn post_install_failures_leave_bin_untouched() {
    let env = Env::new();
    // `run` needs allow_run; a missing copy source fails the step.
    let with_run = PHP_MANIFEST.replace(
        "      - mkdir: \"{{ paths.tmp }}/php/{{ series }}/sessions\"\n",
        "      - mkdir: \"{{ paths.tmp }}/php/{{ series }}/sessions\"\n      - run: { command: \"php.exe -v\", timeout: 30s }\n",
    );
    let php = manifest(&with_run);
    let zip = php_zip(&env.work, "8.3.37");
    let info = env
        .install(
            &php,
            release("8.3.37", &zip),
            zip.clone(),
            InstallOptions::default(),
        )
        .await;
    assert_eq!(info.state, JobState::Failed);
    let error = info.error.unwrap();
    assert_eq!(error.code, "forbidden", "{}", error.message);
    assert!(
        error.message.contains("post_install[3] (run)"),
        "{}",
        error.message
    );
    assert!(!env.root.bin().join("php").exists());
    assert!(env.stage_is_empty());

    let allowed = || InstallOptions {
        allow_run: true,
        ..InstallOptions::default()
    };
    let ok = env
        .install(&php, release("8.3.37", &zip), zip.clone(), allowed())
        .await;
    report(&ok);

    let broken =
        manifest(&PHP_MANIFEST.replace("from: \"php.ini-development\"", "from: \"missing.ini\""));
    let zip2 = php_zip(&env.work, "8.4.14");
    let info = env
        .install(
            &broken,
            release("8.4.14", &zip2),
            zip2,
            InstallOptions::default(),
        )
        .await;
    assert_eq!(info.state, JobState::Failed);
    assert_eq!(info.error.as_ref().unwrap().code, "not_found");
    assert!(!env.root.bin().join("php").join("8.4.14-nts-x64").exists());
    assert_eq!(env.rows("php"), 1);

    // install.verify catches a binary that is not the announced version.
    let lying = php_zip(&env.work, "8.4.13");
    let renamed = env.work.join("php-8.4.15.zip");
    std::fs::rename(&lying, &renamed).unwrap();
    let info = env
        .install(&php, release("8.4.15", &renamed), renamed, allowed())
        .await;
    assert_eq!(info.state, JobState::Failed);
    let error = info.error.unwrap();
    assert_eq!(error.code, "verification_failed");
    assert!(error.message.contains("8.4.13"), "{}", error.message);
    assert!(!env.root.bin().join("php").join("8.4.15-nts-x64").exists());
    assert!(env.stage_is_empty());

    // Reinstalling an existing folder needs replace_existing.
    let info = env
        .install(&php, release("8.3.37", &zip), zip.clone(), allowed())
        .await;
    assert_eq!(info.error.unwrap().code, "conflict");
    let again = env
        .install(
            &php,
            release("8.3.37", &zip),
            zip,
            InstallOptions {
                replace_existing: true,
                allow_run: true,
                ..InstallOptions::default()
            },
        )
        .await;
    assert!(report(&again).reinstalled);
    assert_eq!(env.rows("php"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interpreter_shim_runs_the_file_with_the_project_php() {
    let env = Env::new();
    let php = manifest(PHP_MANIFEST);
    for version in ["8.3.37", "8.4.14"] {
        let zip = php_zip(&env.work, version);
        report(
            &env.install(&php, release(version, &zip), zip, InstallOptions::default())
                .await,
        );
    }
    let composer = manifest(
        r#"
apiVersion: forgebox.dev/v1
kind: Tool
metadata: { id: composer, name: Composer, category: devtool }
spec:
  platforms: [windows-x64]
  depends_on: [php]
  source:
    providers:
      - type: nodejs-dist
  install:
    archive: file
    strip_root: false
    path: ["."]
    shims:
      - { name: composer, file: composer.phar, interpreter: php }
"#,
    );
    let phar = env.work.join("composer.phar");
    std::fs::write(&phar, b"<?php // fake composer\n").unwrap();
    let mut rel = release("2.10.3", &phar);
    rel.variants.clear();
    let done = report(
        &env.install(&composer, rel, phar, InstallOptions::default())
            .await,
    );
    assert_eq!(done.shims.written, ["composer"]);
    assert!(
        env.root
            .bin()
            .join("composer/2.10.3/composer.phar")
            .is_file()
    );

    let project = env.work.join("legacy");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join(".php-version"), "8.3").unwrap();
    let out = run_shim(&env.root, "composer", &project, &["about"]);
    let text = stdout(&out);
    assert!(text.starts_with("PHP 8.3.37 ARGS "), "{out:?}");
    assert!(text.ends_with("composer.phar|about"), "{text}");
}

#[test]
fn project_helper_reads_keys_and_runtimes() {
    let dir = tempfile::tempdir().unwrap();
    let helper = dir.path().join("forgebox-project.ps1");
    std::fs::write(
        &helper,
        forge_provider::layout::shims::PROJECT_HELPER_SCRIPT,
    )
    .unwrap();
    let run = |yaml: &str, tool: &str, key: &str| {
        let file = dir.path().join("forgebox.yaml");
        std::fs::write(&file, yaml).unwrap();
        let out = Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&helper)
            .arg(&file)
            .args([tool, key])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        stdout(&out)
    };
    let block = "apiVersion: forgebox.dev/v1\nkind: Project\nmetadata:\n  name: loja\nspec:\n  \
                 site:\n    host: loja.test\n    php: '8.3' # pinned\n  runtimes:\n  - tool: nodejs\n    \
                 series: \"20\"\n  - tool: python\n    version: 3.13\n  processes:\n    web: { cmd: x }\n";
    assert_eq!(run(block, "php", "spec.site.php"), "8.3");
    assert_eq!(run(block, "nodejs", ""), "20");
    assert_eq!(run(block, "python", ""), "3.13");
    assert_eq!(run(block, "mysql", ""), "");
    let flow = "spec:\n  site: { host: a.test, php: \"8.2\" }\n  runtimes:\n    - { tool: nodejs, version: \"22\" }\n";
    assert_eq!(run(flow, "php", "spec.site.php"), "8.2");
    assert_eq!(run(flow, "nodejs", ""), "22");
    // The runtimes entry counts for the site tool too, after the key.
    let both = "spec:\n  runtimes:\n    - { tool: php, version: \"7.4\" }\n";
    assert_eq!(run(both, "php", "spec.site.php"), "7.4");
}

#[test]
fn payloads_match_the_ipc_contract() {
    let install = ToolInstallEventPayload {
        v: 1,
        job_id: Some(JobId::new()),
        tool: ToolId::new("php").unwrap(),
        version: "8.3.37".into(),
        version_id: None,
        progress: None,
        error: None,
        step: Some(forge_provider::layout::InstallStep::PostInstall),
        canonical_name: Some("8.3.37-nts-x64".into()),
    };
    let json = serde_json::to_value(&install).unwrap();
    let ipc: forge_ipc::api::ToolInstallPayload = serde_json::from_value(json).unwrap();
    assert_eq!(ipc.step.as_deref(), Some("post_install"));
    assert_eq!(ipc.tool.as_str(), "php");

    let current = ToolCurrentEventPayload {
        v: 1,
        tool: ToolId::new("php").unwrap(),
        from: None,
        to: "8.4.14-nts-x64".into(),
        version_id: None,
        elapsed_us: 1200,
    };
    let ipc: forge_ipc::api::ToolCurrentPayload =
        serde_json::from_value(serde_json::to_value(&current).unwrap()).unwrap();
    assert_eq!(ipc.elapsed_us, 1200);
    assert_eq!(
        forge_ipc::api::EVENT_PAYLOADS
            .iter()
            .find(|(k, _)| *k == "tool.current_changed")
            .map(|(_, p)| *p),
        Some("ToolCurrentPayload")
    );
}
