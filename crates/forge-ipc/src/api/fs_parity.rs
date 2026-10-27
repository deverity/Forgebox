//! `fs_parity.*`: Linux parity, case sensitivity per folder and per site, Case Mismatch
//! Finder (docs/09 §fs, docs/15).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::SiteRef;

/// Params of `fs_parity.case_status|case_enable|case_disable`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaseDirParams {
    /// Folder on the daemon's machine.
    pub dir: String,
}

/// Case sensitivity of a folder (`forge_core::fs::case::CaseStatus`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaseSensitivityDto {
    pub dir: String,
    pub supported: bool,
    /// `None` when unsupported.
    pub enabled: Option<bool>,
    /// File system of the volume (`NTFS`, `FAT32`, `ReFS`...), when known.
    pub fs_type: Option<String>,
    /// OS build number (`19045`), when known.
    pub os_build: Option<u32>,
    /// Windows 10 1803 (build 17134) or later; `None` when unknown (other OSes).
    pub win_version_ok: Option<bool>,
    /// Why it is unsupported (file system, Windows version).
    pub reason: Option<String>,
}

/// Params of `fs_parity.case_convert`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaseConvertParams {
    pub dir: String,
    /// Undo a previous conversion (back to case-insensitive).
    #[serde(default)]
    pub revert: bool,
}

/// Result of the `fs_parity.case_convert` job (`forge_core::fs::case::ConvertReport`).
/// The conversion moves every entry into a new flagged folder and back on failure; it
/// keeps no backup (docs/15 §3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaseConvertDto {
    pub dir: String,
    /// `true`: the tree is now case-sensitive; `false`: reverted to insensitive.
    pub enabled: bool,
    /// Folders, root included.
    pub directories: u32,
    pub files: u32,
    /// Junctions and symbolic links (moved, never followed).
    pub links: u32,
    #[specta(type = specta_typescript::Number)]
    pub bytes: u64,
    /// Files whose SHA-256 was compared before and after.
    pub sampled: u32,
    /// Already in the requested state: nothing moved.
    pub already: bool,
    /// An interrupted conversion was undone first.
    pub recovered: bool,
    /// `<dir>.cs-old` kept because files were written into it during the conversion.
    pub leftover: Option<String>,
    #[specta(type = specta_typescript::Number)]
    pub duration_ms: u64,
}

/// Params of `fs_parity.site_case`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SiteCaseParams {
    pub site: SiteRef,
    pub enabled: bool,
}

/// How a case mismatch is fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CaseFixStrategy {
    /// Rename the file on disk to the spelling of the reference.
    Rename,
    /// Rewrite the reference with the spelling on disk.
    Edit,
}

/// Default strategy of `forge site lint-case --fix`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CaseFixChoice {
    /// Rename when only the file name differs and nothing else uses the current name;
    /// edit otherwise (docs/15 §6).
    #[default]
    Auto,
    Rename,
    Edit,
}

/// Params of `fs_parity.lint_case`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LintCaseParams {
    pub site: SiteRef,
    /// Fix the mismatches (rename files or edit references).
    #[serde(default)]
    pub fix: bool,
    /// With `fix`: plan and preview only, write nothing.
    #[serde(default)]
    pub dry_run: bool,
    /// With `fix`: default strategy.
    #[serde(default)]
    pub strategy: CaseFixChoice,
    /// With `fix`: strategy per issue id.
    #[serde(default)]
    pub overrides: BTreeMap<String, CaseFixStrategy>,
    /// With `fix`: only these issue ids.
    pub only: Option<Vec<String>>,
}

/// Category of a Case Mismatch Finder finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CaseIssueCategory {
    /// A reference whose case differs from the file on disk (fixable).
    Mismatch,
    /// Names in one folder that differ only by case.
    CaseDuplicate,
    /// A name Windows cannot hold (`aux`, `con`, `a:b`, trailing dot); `detail` = reason.
    ReservedName,
    /// `.env` keys that differ only by case.
    EnvCaseDuplicate,
    /// A script whose shebang line ends in CRLF.
    CrlfShebang,
}

/// A planned (dry run) or applied fix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaseFixDto {
    pub strategy: CaseFixStrategy,
    /// Rename: current path; edit: the reference as written.
    pub from: String,
    /// Rename: new path; edit: the reference rewritten.
    pub to: String,
    /// Edit: the source line before and after.
    pub before: Option<String>,
    pub after: Option<String>,
    pub applied: bool,
    pub error: Option<String>,
}

/// A finding of the Case Mismatch Finder (`forge_sites::lint_case::CaseIssue`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaseIssueDto {
    /// Stable id (`<file>:<line>:<column>` for mismatches), used by `overrides`/`only`.
    pub id: String,
    pub category: CaseIssueCategory,
    /// File containing the reference, or the offending entry (relative to the project, `/`).
    pub file: String,
    /// 1-based; 0 when not about a line.
    pub line: u32,
    /// 1-based (characters); 0 when not about a line.
    pub column: u32,
    /// As written (`Views/header.php`).
    pub reference: String,
    /// As on disk (`views/Header.php`).
    pub actual: String,
    /// `php-include`, `php-path`, `php-view`, `blade-include`, `twig-include`, `psr4`,
    /// `js-import`, `html`, `asset`, `css`, `composer`, `package-json`; `fs`, `env`,
    /// `script` for the other categories.
    pub kind: String,
    /// Real file the reference reaches.
    pub target: Option<String>,
    /// Path the reference names (result of a rename).
    pub expected: Option<String>,
    /// Several entries matched ignoring case (case-sensitive folder).
    pub ambiguous: bool,
    pub detail: Option<String>,
    /// Strategies that can fix it (empty: report only).
    pub fixes: Vec<CaseFixStrategy>,
    pub fix: Option<CaseFixDto>,
    pub fixed: bool,
}

/// Result of the `fs_parity.lint_case` job (`forge_sites::lint_case::LintReport`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaseLintReportDto {
    /// Version of this report shape.
    pub v: u32,
    /// Project folder scanned.
    pub root: String,
    pub issues: Vec<CaseIssueDto>,
    pub files_scanned: u32,
    pub references_checked: u32,
    /// Fixes were planned or applied.
    pub fix: bool,
    pub dry_run: bool,
    /// Mismatches left after fixing; `None` when nothing was applied.
    pub remaining: Option<u32>,
}
