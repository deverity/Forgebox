//! Linux parity of a project folder besides the file system flag (docs/15 §7), for
//! `forge new` and `forge site add` (M3/E5): Git must not hide case-only renames
//! (`core.ignorecase=false`) nor rewrite line endings (`core.autocrlf=false`), the
//! repository keeps LF (`.gitattributes`: `* text=auto eol=lf`) and editors write LF and
//! UTF-8 (`.editorconfig`).
//!
//! The user's files are never rewritten: `.gitattributes` and `.editorconfig` are
//! **created** when missing (LF, UTF-8 without BOM); when they exist without the LF
//! rule the result is [`ParityOutcome::Suggested`] with the line to add, because
//! appending at the end would override the user's more specific rules (in both formats
//! later lines win). Git settings are written with the `git` of the machine (or the one
//! given) through [`forge_core::platform::run_probe`] (own Job Object, timeout); in a
//! folder that is not a repository they are skipped unless
//! [`GitParityOptions::init_repo`].
//!
//! Blocking (files and child processes): async callers use `spawn_blocking`.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::paths::posix_string;
use forge_core::platform::{ProbeCommand, run_probe};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::messages;

/// Content of a `.gitattributes` created by Forgebox.
pub const GITATTRIBUTES: &str = "# Linux parity (Forgebox, docs/15 §7): LF in the repository and in the working tree.\n\
* text=auto eol=lf\n";

/// Content of an `.editorconfig` created by Forgebox.
pub const EDITORCONFIG: &str = "# Linux parity (Forgebox, docs/15 §7).\n\
root = true\n\
\n\
[*]\n\
charset = utf-8\n\
end_of_line = lf\n\
insert_final_newline = true\n";

/// Git settings applied, with their values.
pub const GIT_SETTINGS: [(&str, &str); 2] =
    [("core.ignorecase", "false"), ("core.autocrlf", "false")];

/// What happened to one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParityOutcome {
    /// File written by Forgebox.
    Created,
    /// Git setting changed.
    Set,
    /// Already as wanted.
    Unchanged,
    /// The user's file lacks the rule; `detail` says what to add.
    Suggested,
    /// Not applicable (no Git, not a repository).
    Skipped,
    Failed,
}

/// One item of [`GitParityReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParityItem {
    /// `.gitattributes`, `.editorconfig`, `core.ignorecase`, `core.autocrlf`, `git init`.
    pub name: String,
    pub outcome: ParityOutcome,
    /// pt-BR note (suggestion, why skipped) or the error.
    pub detail: Option<String>,
}

/// Result of [`git_parity_init`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitParityReport {
    pub dir: String,
    pub items: Vec<ParityItem>,
}

impl GitParityReport {
    /// Outcome of the item `name`.
    pub fn outcome(&self, name: &str) -> Option<ParityOutcome> {
        self.items
            .iter()
            .find(|i| i.name == name)
            .map(|i| i.outcome)
    }
}

/// Options of [`git_parity_init`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitParityOptions {
    /// `git` executable; `None` searches `PATH` (`bin/git/current` of the Forgebox root
    /// once the git tool is installed should be passed here by the daemon).
    pub git: Option<PathBuf>,
    /// Run `git init` when the folder is not a repository (`forge new`).
    pub init_repo: bool,
    /// Budget of each git invocation.
    pub timeout: Duration,
}

impl Default for GitParityOptions {
    fn default() -> Self {
        Self {
            git: None,
            init_repo: false,
            timeout: Duration::from_secs(10),
        }
    }
}

fn item(name: &str, outcome: ParityOutcome, detail: Option<String>) -> ParityItem {
    ParityItem {
        name: name.to_owned(),
        outcome,
        detail,
    }
}

static GITATTRIBUTES_LF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*\*\s+.*\beol=lf\b").expect("gitattributes rule"));
static EDITORCONFIG_LF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?mi)^\s*end_of_line\s*=\s*lf\s*$").expect("editorconfig rule"));

/// Creates `name` with `content` when missing; otherwise checks it has `rule`.
fn ensure_file(dir: &Path, name: &str, content: &str, rule: &Regex, hint: &str) -> ParityItem {
    let path = dir.join(name);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            if rule.is_match(&text) {
                item(name, ParityOutcome::Unchanged, None)
            } else {
                item(
                    name,
                    ParityOutcome::Suggested,
                    Some(messages::parity_suggestion(name, hint).1),
                )
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match forge_render::managed::write_atomic(&path, content) {
                Ok(_) => item(name, ParityOutcome::Created, None),
                Err(err) => item(name, ParityOutcome::Failed, Some(err.to_string())),
            }
        }
        Err(e) => item(
            name,
            ParityOutcome::Failed,
            Some(ForgeError::io_at("reading", &path, e).to_string()),
        ),
    }
}

/// `git` on `PATH`.
fn find_git() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "git.exe" } else { "git" };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(exe))
        .find(|p| p.is_file())
}

fn git(exe: &Path, dir: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    let cmd = ProbeCommand {
        exe: exe.to_path_buf(),
        args: args.iter().map(Into::into).collect(),
        cwd: dir.to_path_buf(),
    };
    let out = run_probe(&cmd, timeout)?;
    match out.exit_code {
        Some(0) => Ok(out.stdout.trim().to_owned()),
        code => Err(ForgeError::io(
            format!("git {} exited with {code:?}", args.join(" ")),
            std::io::Error::other(out.combined().trim().to_owned()),
        )),
    }
}

fn is_repo(dir: &Path) -> bool {
    // `.git` is a folder, or a file in worktrees and submodules.
    dir.join(".git").exists()
}

/// Applies the Linux parity settings to the project at `dir` (see the module docs).
/// `NotFound`/`InvalidInput` only for a bad `dir`; every other problem is reported per
/// item.
pub fn git_parity_init(dir: &Path, opts: &GitParityOptions) -> Result<GitParityReport> {
    let meta = std::fs::metadata(dir).at("inspecting", dir)?;
    if !meta.is_dir() {
        return Err(ForgeError::InvalidInput(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    let mut items = vec![
        ensure_file(
            dir,
            ".gitattributes",
            GITATTRIBUTES,
            &GITATTRIBUTES_LF,
            "* text=auto eol=lf",
        ),
        ensure_file(
            dir,
            ".editorconfig",
            EDITORCONFIG,
            &EDITORCONFIG_LF,
            "[*] end_of_line = lf",
        ),
    ];
    let skip_all = |items: &mut Vec<ParityItem>, why: &str| {
        for (key, _) in GIT_SETTINGS {
            items.push(item(key, ParityOutcome::Skipped, Some(why.to_owned())));
        }
    };
    let Some(exe) = opts.git.clone().or_else(find_git) else {
        skip_all(&mut items, messages::parity_git_missing().1);
        return Ok(report(dir, items));
    };
    if !is_repo(dir) {
        if !opts.init_repo {
            skip_all(&mut items, messages::parity_not_a_repo().1);
            return Ok(report(dir, items));
        }
        match git(&exe, dir, &["init", "--quiet"], opts.timeout) {
            Ok(_) => items.push(item("git init", ParityOutcome::Created, None)),
            Err(err) => {
                items.push(item(
                    "git init",
                    ParityOutcome::Failed,
                    Some(err.to_string()),
                ));
                skip_all(&mut items, "git init failed");
                return Ok(report(dir, items));
            }
        }
    }
    for (key, value) in GIT_SETTINGS {
        let current = git(
            &exe,
            dir,
            &["config", "--local", "--get", key],
            opts.timeout,
        )
        .ok();
        if current.as_deref() == Some(value) {
            items.push(item(key, ParityOutcome::Unchanged, None));
            continue;
        }
        match git(&exe, dir, &["config", "--local", key, value], opts.timeout) {
            Ok(_) => items.push(item(key, ParityOutcome::Set, None)),
            Err(err) => items.push(item(key, ParityOutcome::Failed, Some(err.to_string()))),
        }
    }
    Ok(report(dir, items))
}

fn report(dir: &Path, items: Vec<ParityItem>) -> GitParityReport {
    GitParityReport {
        dir: posix_string(dir),
        items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_files_are_lf_utf8_without_bom() {
        for content in [GITATTRIBUTES, EDITORCONFIG] {
            assert!(!content.contains('\r'));
            assert!(!content.starts_with('\u{feff}'));
            assert!(content.ends_with('\n'));
        }
        assert!(GITATTRIBUTES_LF.is_match(GITATTRIBUTES));
        assert!(EDITORCONFIG_LF.is_match(EDITORCONFIG));
    }

    #[test]
    fn existing_files_are_never_rewritten() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join(".gitattributes"), "*.bat eol=crlf\r\n").unwrap();
        std::fs::write(
            t.path().join(".editorconfig"),
            "root = true\n[*]\nEnd_Of_Line = LF\n",
        )
        .unwrap();
        let opts = GitParityOptions {
            git: Some(t.path().join("no-git.exe")),
            ..GitParityOptions::default()
        };
        let r = git_parity_init(t.path(), &opts).unwrap();
        assert_eq!(r.outcome(".gitattributes"), Some(ParityOutcome::Suggested));
        assert_eq!(r.outcome(".editorconfig"), Some(ParityOutcome::Unchanged));
        assert_eq!(
            std::fs::read_to_string(t.path().join(".gitattributes")).unwrap(),
            "*.bat eol=crlf\r\n"
        );
        // not a repository: git settings skipped
        assert_eq!(r.outcome("core.ignorecase"), Some(ParityOutcome::Skipped));
    }
}
