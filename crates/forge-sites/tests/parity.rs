//! Git parity of a project (docs/15 §7) with the real `git` of the machine: a new
//! repository gets `core.ignorecase=false`, `core.autocrlf=false`, `.gitattributes` and
//! `.editorconfig`; a second run changes nothing. Skipped when `git` is not on `PATH`.

use std::path::{Path, PathBuf};
use std::process::Command;

use forge_sites::parity::{EDITORCONFIG, GITATTRIBUTES, ParityOutcome};
use forge_sites::{GitParityOptions, git_parity_init};

fn git_on_path() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "git.exe" } else { "git" };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(exe))
        .find(|p| p.is_file())
}

fn config(dir: &Path, key: &str) -> String {
    let out = Command::new(git_on_path().unwrap())
        .args(["config", "--local", "--get", key])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn new_repository_gets_linux_parity_and_reruns_are_idempotent() {
    let Some(git) = git_on_path() else {
        eprintln!("SKIPPED: git not on PATH");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("novo");
    std::fs::create_dir(&dir).unwrap();
    let opts = GitParityOptions {
        git: Some(git),
        init_repo: true,
        ..GitParityOptions::default()
    };
    let report = git_parity_init(&dir, &opts).unwrap();
    assert_eq!(report.outcome("git init"), Some(ParityOutcome::Created));
    assert_eq!(
        report.outcome(".gitattributes"),
        Some(ParityOutcome::Created)
    );
    assert_eq!(
        report.outcome(".editorconfig"),
        Some(ParityOutcome::Created)
    );
    // Git for Windows sets core.ignorecase=true on init: it must be turned off.
    assert_eq!(report.outcome("core.ignorecase"), Some(ParityOutcome::Set));
    assert!(matches!(
        report.outcome("core.autocrlf"),
        Some(ParityOutcome::Set | ParityOutcome::Unchanged)
    ));
    assert_eq!(config(&dir, "core.ignorecase"), "false");
    assert_eq!(config(&dir, "core.autocrlf"), "false");
    assert_eq!(
        std::fs::read(dir.join(".gitattributes")).unwrap(),
        GITATTRIBUTES.as_bytes()
    );
    assert_eq!(
        std::fs::read(dir.join(".editorconfig")).unwrap(),
        EDITORCONFIG.as_bytes()
    );

    let again = git_parity_init(&dir, &opts).unwrap();
    assert!(
        again
            .items
            .iter()
            .all(|i| i.outcome == ParityOutcome::Unchanged),
        "{again:#?}"
    );
    assert!(!report.dir.contains('\\'));
}

#[test]
fn without_a_repository_git_settings_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let report = git_parity_init(tmp.path(), &GitParityOptions::default()).unwrap();
    assert_eq!(
        report.outcome("core.ignorecase"),
        Some(ParityOutcome::Skipped)
    );
    assert_eq!(
        report.outcome("core.autocrlf"),
        Some(ParityOutcome::Skipped)
    );
    assert!(tmp.path().join(".gitattributes").is_file());
    assert!(!tmp.path().join(".git").exists());
    let err =
        git_parity_init(&tmp.path().join("missing"), &GitParityOptions::default()).unwrap_err();
    assert_eq!(err.code(), "not_found");
}
