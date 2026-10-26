//! Lint rules for registry manifests (docs/04 §6).
//!
//! Provisional: only rule 1 (valid YAML, `apiVersion`, `kind`) is implemented.
//! The seven rules come in M1-E1-T1.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Accepted `apiVersion` value.
pub const API_VERSION: &str = "forgebox.dev/v1";
/// Accepted `kind` values.
pub const KINDS: [&str; 5] = ["Tool", "Extension", "Stack", "Project", "Registry"];

/// A rule violation found in a manifest file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintIssue {
    /// Rule number, 1..=7 as listed in docs/04 §6.
    pub rule: u8,
    pub path: PathBuf,
    pub message: String,
}

impl std::fmt::Display for LintIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: [rule {}] {}",
            self.path.display(),
            self.rule,
            self.message
        )
    }
}

/// Failure to lint at all (as opposed to issues found in the file).
#[derive(Debug, thiserror::Error)]
pub enum LintError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Deserialize)]
struct Header {
    #[serde(rename = "apiVersion")]
    api_version: Option<String>,
    kind: Option<String>,
}

/// Lints one manifest file and returns the issues found (empty = clean).
pub fn lint_file(path: &Path) -> Result<Vec<LintIssue>, LintError> {
    let text = std::fs::read_to_string(path).map_err(|source| LintError::Io {
        path: path.to_owned(),
        source,
    })?;
    let issue = |message: String| LintIssue {
        rule: 1,
        path: path.to_owned(),
        message,
    };
    let header: Header = match serde_yaml_ng::from_str(&text) {
        Ok(h) => h,
        Err(e) => return Ok(vec![issue(format!("invalid YAML: {e}"))]),
    };
    let mut issues = Vec::new();
    match header.api_version.as_deref() {
        Some(API_VERSION) => {}
        Some(other) => issues.push(issue(format!(
            "apiVersion must be `{API_VERSION}`, found `{other}`"
        ))),
        None => issues.push(issue("missing `apiVersion`".to_owned())),
    }
    match header.kind.as_deref() {
        Some(k) if KINDS.contains(&k) => {}
        Some(other) => issues.push(issue(format!(
            "kind must be one of {KINDS:?}, found `{other}`"
        ))),
        None => issues.push(issue("missing `kind`".to_owned())),
    }
    Ok(issues)
}
