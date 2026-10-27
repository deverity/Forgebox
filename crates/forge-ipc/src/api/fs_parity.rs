//! `fs_parity.*`: Linux parity, case sensitivity per folder and per site, Case Mismatch
//! Finder (docs/09 §fs, docs/15).

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::SiteRef;

/// Params of `fs_parity.case_status|case_enable|case_disable`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaseDirParams {
    /// Folder on the daemon's machine.
    pub dir: String,
}

/// Case sensitivity of a folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaseSensitivityDto {
    pub dir: String,
    pub supported: bool,
    /// `None` when unsupported.
    pub enabled: Option<bool>,
    /// Why it is unsupported (file system, Windows version).
    pub reason: Option<String>,
}

/// Params of `fs_parity.case_convert`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaseConvertParams {
    pub dir: String,
    /// Undo a previous conversion.
    #[serde(default)]
    pub revert: bool,
}

/// Result of the `fs_parity.case_convert` job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaseConvertDto {
    pub dir: String,
    pub directories: u32,
    pub files: u32,
    /// Folder kept for `--revert`.
    pub backup: Option<String>,
}

/// Params of `fs_parity.site_case`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SiteCaseParams {
    pub site: SiteRef,
    pub enabled: bool,
}

/// Params of `fs_parity.lint_case`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LintCaseParams {
    pub site: SiteRef,
    /// Rewrite references to the on-disk spelling.
    #[serde(default)]
    pub fix: bool,
}

/// A reference whose case differs from the file on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaseIssueDto {
    /// File containing the reference (relative to the site root, `/`).
    pub file: String,
    pub line: u32,
    /// As written (`views/Header.php`).
    pub reference: String,
    /// As on disk (`Views/header.php`).
    pub actual: String,
    /// `php-include`, `psr4`, `html`, `css`, `js-import`, `composer`.
    pub kind: String,
    pub fixed: bool,
}

/// Result of the `fs_parity.lint_case` job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaseLintReportDto {
    pub issues: Vec<CaseIssueDto>,
    pub files_scanned: u32,
}
