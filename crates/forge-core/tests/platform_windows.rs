//! Real Windows behaviour through the public API: `ForgeRoot::init()` with the NTFS
//! case-sensitive flag and `bin/<tool>/current` junction switching.
//!
//! When `%TEMP%` is not on a volume that supports the flag the case-sensitivity
//! assertions are skipped with a message, unless `FORGEBOX_REQUIRE_CASE_SENSITIVE=1`
//! (set it on NTFS dev machines and CI to turn the skip into a failure).
#![cfg(windows)]

use std::fs;
use std::path::Path;

use forge_core::ids::ToolId;
use forge_core::paths::{CASE_SENSITIVE_DIRS, ForgeRoot, InitOptions};
use forge_core::platform::{self, CaseSensitivitySupport};

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn cs_supported(dir: &Path) -> bool {
    match platform::current().case_sensitive_fs().support(dir) {
        CaseSensitivitySupport::Supported => true,
        CaseSensitivitySupport::Unsupported(reason) => {
            if std::env::var("FORGEBOX_REQUIRE_CASE_SENSITIVE").as_deref() == Ok("1") {
                panic!("case sensitivity required but unsupported: {reason}");
            }
            eprintln!("SKIPPED case-sensitivity assertions: {reason}");
            false
        }
    }
}

#[test]
fn init_creates_layout_with_case_sensitive_dirs() {
    let base = tempfile::tempdir().unwrap();
    let root = ForgeRoot::new(base.path().join("Forgebox")).unwrap();
    let report = root.init(InitOptions::default()).unwrap();

    for dir in root.layout_dirs() {
        assert!(dir.is_dir(), "{}", dir.display());
    }
    assert!(report.created.contains(&root.www()));

    if !cs_supported(root.path()) {
        assert!(report.case_sensitive_unavailable.is_some());
        return;
    }
    assert_eq!(report.case_sensitive_unavailable, None, "{report:?}");
    assert!(report.case_sensitive_failed.is_empty(), "{report:?}");

    let cs = platform::current();
    let cs = cs.case_sensitive_fs();
    for name in CASE_SENSITIVE_DIRS {
        let dir = root.path().join(name);
        assert!(cs.get(&dir).unwrap(), "{name} must be case-sensitive");
        assert!(report.case_sensitive_applied.contains(&dir), "{name}");
    }
    for dir in [root.tmp_downloads(), root.tmp_stage(), root.etc_sites()] {
        assert!(cs.get(&dir).unwrap(), "{} inherits", dir.display());
    }
    for dir in [
        root.bin(),
        root.state(),
        root.app(),
        root.certs(),
        root.manifests_local(),
    ] {
        assert!(
            !cs.get(&dir).unwrap(),
            "{} stays insensitive",
            dir.display()
        );
    }

    // Linux parity inside www/: Index.php and index.php are different files, and a new
    // project folder inherits the flag.
    let project = root.www().join("loja");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("A.txt"), "A").unwrap();
    fs::write(project.join("a.txt"), "a").unwrap();
    assert_eq!(entries(&project), vec!["A.txt", "a.txt"]);
    // bin/ keeps Windows semantics.
    fs::write(root.bin().join("B.txt"), "B").unwrap();
    assert!(root.bin().join("b.txt").exists());

    // Idempotent: nothing new is created, flags are kept.
    let again = root.init(InitOptions::default()).unwrap();
    assert!(again.created.is_empty(), "{again:?}");
    assert!(again.case_sensitive_applied.contains(&root.www()));
}

#[test]
fn init_without_case_sensitivity_leaves_dirs_insensitive() {
    let base = tempfile::tempdir().unwrap();
    let root = ForgeRoot::new(base.path()).unwrap();
    let report = root
        .init(InitOptions {
            case_sensitive: false,
        })
        .unwrap();
    assert!(report.case_sensitive_applied.is_empty());
    if cs_supported(root.path()) {
        let platform = platform::current();
        assert!(!platform.case_sensitive_fs().get(&root.www()).unwrap());
    }
}

#[test]
fn current_junction_switches_between_versions() {
    let base = tempfile::tempdir().unwrap();
    let root = ForgeRoot::new(base.path()).unwrap();
    root.init(InitOptions {
        case_sensitive: false,
    })
    .unwrap();
    let php = ToolId::new("php").unwrap();
    let v83 = root.tool_version_dir(&php, "8.3.37-nts-vs17-x64").unwrap();
    let v84 = root.tool_version_dir(&php, "8.4.14-nts-vs17-x64").unwrap();
    for (dir, file) in [(&v83, "php83.exe"), (&v84, "php84.exe")] {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(file), "").unwrap();
    }

    let links = platform::current();
    let links = links.link_fs();
    let current = root.tool_current_link(&php);
    links.create_junction(&current, &v83).unwrap();
    assert_eq!(entries(&current), vec!["php83.exe"]);

    links.replace_junction(&current, &v84).unwrap();
    assert_eq!(entries(&current), vec!["php84.exe"]);
    assert_eq!(
        fs::canonicalize(links.read_junction(&current).unwrap()).unwrap(),
        fs::canonicalize(&v84).unwrap()
    );
    assert_eq!(entries(&root.tool_dir(&php)), {
        let mut v = vec![
            "8.3.37-nts-vs17-x64".to_owned(),
            "8.4.14-nts-vs17-x64".to_owned(),
            "current".to_owned(),
        ];
        v.sort();
        v
    });

    links.remove_junction(&current).unwrap();
    assert!(!current.exists());
    assert_eq!(entries(&v83), vec!["php83.exe"]);
    assert_eq!(entries(&v84), vec!["php84.exe"]);
}
