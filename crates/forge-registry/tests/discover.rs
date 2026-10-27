//! Discovery of manually placed tools (M1-E1-T4). Uses a real executable, a tiny fake
//! built from `tests/fake_php` that prints what php.exe / nginx.exe print, so the whole
//! path is exercised: folder scan, command splitting, process spawn, regexes, variants.
#![cfg(windows)]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use forge_core::ids::ToolId;
use forge_core::paths::ForgeRoot;
use forge_core::store::{Store, ToolVersionRepo, VersionSource};
use forge_manifest::Manifest;
use forge_manifest::schema::ToolManifest;
use forge_registry::{Confidence, DiscoveredVersion, discover, import_discovered};

/// Builds `tests/fake_php` once per test binary into its own target dir.
fn fake_exe() -> &'static Path {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut hasher = DefaultHasher::new();
        manifest_dir.hash(&mut hasher);
        let target = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("fake-php-{:016x}", hasher.finish()));
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let output = Command::new(cargo)
            .arg("build")
            .arg("--quiet")
            .arg("--manifest-path")
            .arg(manifest_dir.join("tests/fake_php/Cargo.toml"))
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
        let exe = target.join("debug").join("fake_php.exe");
        assert!(exe.is_file(), "{} missing", exe.display());
        exe
    })
}

fn manifests() -> Vec<ToolManifest> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/tools");
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

struct Fixture {
    _dir: tempfile::TempDir,
    root: ForgeRoot,
    manifests: Vec<ToolManifest>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path().join("forgebox")).unwrap();
        std::fs::create_dir_all(root.bin()).unwrap();
        Self {
            _dir: dir,
            root,
            manifests: manifests(),
        }
    }

    /// `bin/<relative>`, created.
    fn dir(&self, relative: &str) -> PathBuf {
        let dir = self.root.bin().join(relative);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A copy of the fake tool as `bin/<relative>/<file>`.
    fn fake(&self, relative: &str, file: &str) -> PathBuf {
        let target = self.dir(relative).join(file);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(fake_exe(), &target).unwrap();
        target
    }

    /// A file that is not an executable (spawning it fails).
    fn garbage(&self, relative: &str, file: &str) {
        let target = self.dir(relative).join(file);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, b"this is not a program").unwrap();
    }

    async fn discover(&self) -> Vec<DiscoveredVersion> {
        discover(&self.root, &self.manifests).await
    }
}

fn variants(found: &DiscoveredVersion) -> Vec<(&str, &str)> {
    found
        .variants
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

#[tokio::test]
async fn laragon_php_folder_inside_bin_php_is_recognized_without_renaming() {
    let fx = Fixture::new();
    let name = "php/php-8.3-Win32-vs16-x64";
    fx.fake(name, "php.exe");
    fx.fake(name, "php-cgi.exe");

    let found = fx.discover().await;
    assert_eq!(found.len(), 1, "{found:#?}");
    let php = &found[0];
    assert_eq!(php.tool_id.as_str(), "php");
    assert_eq!(php.version.as_deref(), Some("8.3.21"));
    assert_eq!(
        variants(php),
        [("arch", "x64"), ("compiler", "vs16"), ("ts", "nts")]
    );
    assert_eq!(php.canonical_name.as_deref(), Some("8.3.21-nts-vs16-x64"));
    assert_eq!(
        php.path,
        fx.root.bin().join("php").join("php-8.3-Win32-vs16-x64")
    );
    assert_eq!(php.source, VersionSource::Manual);
    assert_eq!(php.confidence, Confidence::High);
    // Nothing was renamed.
    assert!(php.path.join("php.exe").is_file());
}

#[tokio::test]
async fn loose_folder_directly_in_bin_is_recognized() {
    let fx = Fixture::new();
    fx.fake("php-8.3-Win32-vs16-x64", "php.exe");
    fx.fake("php-8.3-Win32-vs16-x64", "php-cgi.exe");

    let found = fx.discover().await;
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].tool_id.as_str(), "php");
    assert_eq!(found[0].version.as_deref(), Some("8.3.21"));
    assert_eq!(found[0].path, fx.root.bin().join("php-8.3-Win32-vs16-x64"));
}

#[tokio::test]
async fn mariadb_kept_under_bin_mysql_is_not_taken_for_mysql() {
    let fx = Fixture::new();
    fx.garbage("mysql/mariadb-11.8.9-winx64/bin", "mysqld.exe");
    fx.garbage("mysql/mariadb-11.8.9-winx64/bin", "mariadbd.exe");
    fx.garbage("mysql/mysql-8.4.0-winx64/bin", "mysqld.exe");

    let mut found = fx.discover().await;
    found.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(found.len(), 2, "{found:#?}");
    assert_eq!(found[0].tool_id.as_str(), "mariadb", "{found:#?}");
    assert_eq!(found[0].version.as_deref(), Some("11.8.9"));
    assert_eq!(found[1].tool_id.as_str(), "mysql", "{found:#?}");
}

#[tokio::test]
async fn thread_safety_comes_from_the_tool_not_the_name() {
    let fx = Fixture::new();
    let name = "php/php-8.3-Win32-vs16-x64";
    fx.fake(name, "php.exe");
    fx.fake(name, "php-cgi.exe");
    std::fs::write(fx.dir(name).join("fake-ts"), b"").unwrap();

    let found = fx.discover().await;
    assert_eq!(found[0].variants["ts"], "ts");
}

#[tokio::test]
async fn forgebox_style_folder_with_plain_version_name() {
    let fx = Fixture::new();
    let name = "php/8.3.21-nts-vs17-x64";
    fx.fake(name, "php.exe");
    fx.fake(name, "php-cgi.exe");

    let found = fx.discover().await;
    assert_eq!(
        found[0].canonical_name.as_deref(),
        Some("8.3.21-nts-vs17-x64")
    );
    assert_eq!(found[0].variants["compiler"], "vs17");
}

#[tokio::test]
async fn version_printed_on_stderr_is_read() {
    let fx = Fixture::new();
    fx.fake("nginx-1.22.0", "nginx.exe");

    let found = fx.discover().await;
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].tool_id.as_str(), "nginx");
    assert_eq!(found[0].version.as_deref(), Some("1.22.0"));
    assert_eq!(found[0].canonical_name.as_deref(), Some("1.22.0"));
    assert_eq!(found[0].confidence, Confidence::High);
}

#[tokio::test]
async fn folder_name_is_the_fallback_when_the_command_fails() {
    let fx = Fixture::new();
    fx.garbage("mysql-8.0.30-winx64/bin", "mysqld.exe");
    fx.garbage("httpd-2.4.62-win64-VS17/bin", "httpd.exe");
    fx.garbage("php/php-7.3-Win32-VC15-x64", "php.exe");
    fx.garbage("php/php-7.3-Win32-VC15-x64", "php-cgi.exe");

    let found = fx.discover().await;
    let by_tool = |id: &str| found.iter().find(|f| f.tool_id.as_str() == id).unwrap();
    assert_eq!(found.len(), 3, "{found:#?}");

    let mysql = by_tool("mysql");
    assert_eq!(mysql.version.as_deref(), Some("8.0.30"));
    assert_eq!(mysql.confidence, Confidence::Medium);

    let apache = by_tool("apache");
    assert_eq!(apache.version.as_deref(), Some("2.4.62"));
    assert_eq!(apache.variants["compiler"], "vs17");
    assert_eq!(apache.confidence, Confidence::Medium);

    let php = by_tool("php");
    assert_eq!(php.version.as_deref(), Some("7.3"));
    assert_eq!(php.variants["compiler"], "vc15");
    // `ts` could not be detected, so no canonical name is made up.
    assert_eq!(php.canonical_name, None);
}

#[tokio::test]
async fn unrelated_folders_are_ignored() {
    let fx = Fixture::new();
    // `current` (junction target) and shims are never candidates.
    fx.fake("php/current", "php.exe");
    fx.fake("php/current", "php-cgi.exe");
    fx.dir("shims");
    // Missing php-cgi.exe: not a PHP install.
    fx.fake("php/php-8.2-incomplete", "php.exe");
    // Random folders and files.
    fx.garbage("tools/misc", "readme.txt");
    std::fs::write(fx.root.bin().join("php.7z"), b"x").unwrap();

    assert_eq!(fx.discover().await, []);
}

#[tokio::test]
async fn empty_or_missing_bin_finds_nothing() {
    let fx = Fixture::new();
    assert!(fx.discover().await.is_empty());
    std::fs::remove_dir_all(fx.root.bin()).unwrap();
    assert!(fx.discover().await.is_empty());
}

#[tokio::test]
async fn import_registers_manual_versions_idempotently() {
    let fx = Fixture::new();
    fx.fake("php/php-8.3-Win32-vs16-x64", "php.exe");
    fx.fake("php/php-8.3-Win32-vs16-x64", "php-cgi.exe");
    fx.fake("nginx-1.22.0", "nginx.exe");
    // Recognized but without any version (no hint in the name, command fails).
    fx.garbage("php/mystery", "php.exe");
    fx.garbage("php/mystery", "php-cgi.exe");
    let found = fx.discover().await;
    assert_eq!(found.len(), 3, "{found:#?}");

    let store = Store::open_in_memory().unwrap();
    let first = import_discovered(store.writer(), &found).await.unwrap();
    assert_eq!(first.imported.len(), 2, "{first:?}");
    assert_eq!(
        first.unknown_version,
        [fx.root.bin().join("php").join("mystery")]
    );
    assert!(first.already_registered.is_empty());

    let second = import_discovered(store.writer(), &found).await.unwrap();
    assert!(second.imported.is_empty(), "{second:?}");
    assert_eq!(second.already_registered.len(), 2);

    let php = ToolId::new("php").unwrap();
    let rows = ToolVersionRepo::new(store.conn()).list(Some(&php)).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].version, "8.3.21");
    assert_eq!(rows[0].canonical_name, "php-8.3-Win32-vs16-x64");
    assert_eq!(rows[0].source, VersionSource::Manual);
    assert_eq!(rows[0].variant["compiler"], "vs16");
    let path_ok = rows[0].path.ends_with("bin/php/php-8.3-Win32-vs16-x64");
    assert!(path_ok, "{}", rows[0].path);
    assert_eq!(
        ToolVersionRepo::new(store.conn()).list(None).unwrap().len(),
        2
    );
}

#[tokio::test]
async fn import_leaves_registry_installed_versions_alone() {
    let fx = Fixture::new();
    fx.fake("php/8.3.21-nts-vs17-x64", "php.exe");
    fx.fake("php/8.3.21-nts-vs17-x64", "php-cgi.exe");
    let found = fx.discover().await;

    let store = Store::open_in_memory().unwrap();
    // Same folder already installed from the registry.
    {
        use forge_core::store::{NewToolVersion, VersionState};
        ToolVersionRepo::new(store.conn())
            .register_if_absent(&NewToolVersion {
                tool_id: found[0].tool_id.clone(),
                tool_name: "PHP".into(),
                version: "8.3.21".into(),
                canonical_name: "8.3.21-nts-vs17-x64".into(),
                variant: found[0].variants.clone(),
                path: found[0].path.to_string_lossy().into_owned(),
                state: VersionState::Installed,
                source: VersionSource::Registry,
            })
            .unwrap()
            .unwrap();
    }
    let report = import_discovered(store.writer(), &found).await.unwrap();
    assert!(report.imported.is_empty());
    let rows = ToolVersionRepo::new(store.conn()).list(None).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, VersionSource::Registry);
}

/// Copies the real `C:\laragon\bin\php\php-8.3-Win32-vs16-x64` (read only) and discovers
/// it. Needs the old Laragon install of the dev machine, hence ignored by default:
/// `cargo test -p forge-registry --test discover -- --ignored real_laragon_php_copy_online`.
#[tokio::test]
#[ignore = "needs the old Laragon install on this machine"]
async fn real_laragon_php_copy_online() {
    let source = Path::new(r"C:\laragon\bin\php\php-8.3-Win32-vs16-x64");
    if !source.join("php.exe").is_file() {
        eprintln!("skipped: {} not found", source.display());
        return;
    }
    let fx = Fixture::new();
    let target = fx.dir("php/php-8.3-Win32-vs16-x64");
    let status = Command::new("robocopy")
        .arg(source)
        .arg(&target)
        .args(["/E", "/NFL", "/NDL", "/NJH", "/NJS", "/NP"])
        .status()
        .unwrap();
    assert!(
        status.code().is_some_and(|c| c < 8),
        "robocopy failed: {status}"
    );

    let found = fx.discover().await;
    assert_eq!(found.len(), 1, "{found:#?}");
    let php = &found[0];
    eprintln!("{php:#?}");
    assert_eq!(php.tool_id.as_str(), "php");
    assert!(php.version.as_deref().unwrap().starts_with("8.3."));
    assert_eq!(php.variants["compiler"], "vs16");
    assert_eq!(php.variants["ts"], "ts");
    assert_eq!(php.confidence, Confidence::High);
    assert_eq!(php.path, target);
}
