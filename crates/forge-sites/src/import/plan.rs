//! The data model of an import: what was found ([`ImportPlan`]), what the user chose
//! ([`ImportOptions`]) and what happened ([`ImportReport`]).
//!
//! The plan is plain data (paths with `/`, no handles) so it can be shown, serialized and
//! compared in tests; the same plan is the input of a dry run and of the real import.

use std::collections::BTreeMap;

use forge_core::ids::ToolId;
use serde::Serialize;

/// Report item kinds (`ImportItemDto.kind`).
pub mod kind {
    /// A tool version registered from `bin/`.
    pub const TOOL_VERSION: &str = "tool_version";
    /// A site created from a virtual host.
    pub const SITE: &str = "site";
    /// A database instance over a `data/` folder.
    pub const DATABASE_DIR: &str = "database_dir";
    /// A virtual host file that gave no site.
    pub const VHOST: &str = "vhost";
    /// A folder served as parked.
    pub const PARKED_DIR: &str = "parked_dir";
    /// A preference of `usr/laragon.ini`.
    pub const PREF: &str = "pref";
    /// The case sensitivity conversion of a parked folder.
    pub const CASE_CONVERSION: &str = "case_conversion";
    /// A `bin/` folder that is not a tool version.
    pub const TOOL: &str = "tool";
}

/// Everything [`crate::import::LaragonImporter::scan`] found in a Laragon installation.
/// Nothing in it has been written anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportPlan {
    /// The folder that was scanned (`C:/laragon`).
    pub source: String,
    /// Where `laragon.ini` says the installation lives, when that is not [`Self::source`]
    /// (the folder was moved or copied): paths of the vhost files below it are read from
    /// `source` instead.
    pub original_root: Option<String>,
    pub prefs: PlannedPrefs,
    pub tools: Vec<PlannedTool>,
    pub databases: Vec<PlannedDatabase>,
    pub sites: Vec<PlannedSite>,
    pub parked: Vec<PlannedParked>,
    /// Everything that will not be imported, with the reason.
    pub skipped: Vec<Skipped>,
}

/// Preferences read from `usr/laragon.ini`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedPrefs {
    /// TLD of `HostnameFormat={name}.test`, without the dot.
    pub tld: Option<String>,
    /// `DocumentRootList` (or `[apache] DocumentRoot`, or `<source>/www`), existing
    /// folders only.
    pub document_roots: Vec<String>,
    /// Folder of the PHP version Laragon runs (the `bin/php/php` junction, else
    /// `[php] Version`).
    pub active_php: Option<String>,
    /// Series (`8.2`) of [`Self::active_php`], the PHP of the imported sites.
    pub php_series: Option<String>,
}

/// A version found in `bin/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedTool {
    pub tool_id: ToolId,
    pub tool_name: String,
    /// Printed by the tool itself (versions guessed from a folder name are skipped).
    pub version: String,
    /// Folder name in Laragon (`php-8.2-Win32-vs16-x64`).
    pub folder: String,
    /// The folder (`C:/laragon/bin/php/php-8.2-Win32-vs16-x64`).
    pub path: String,
    pub variants: BTreeMap<String, String>,
    /// What Forgebox would call the build (`8.2.12-nts-vs16-x64`); the folder is only
    /// renamed when the tool is copied.
    pub canonical_name: Option<String>,
    /// The version Laragon is configured to run.
    pub active: bool,
}

/// A folder of `data/` that becomes a database instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedDatabase {
    /// Instance name (the folder name: `mysql-8`).
    pub name: String,
    pub tool_id: ToolId,
    /// The folder (`C:/laragon/data/mysql-8`).
    pub data_dir: String,
    /// Folder of the tool version that will run it (`mysql-8.0.46-winx64`): the active
    /// one when it matches the folder's version, else the newest that does.
    pub version_folder: String,
    pub version: String,
}

/// A virtual host (or several that share a document root) that becomes a site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedSite {
    /// Normalized names, preferred first (the TLD of Laragon's `HostnameFormat`); the
    /// importer picks the first whose TLD Forgebox serves as the host, the rest become
    /// aliases.
    pub hosts: Vec<String>,
    /// Document root.
    pub root: String,
    /// Project folder the driver looked at.
    pub project_dir: String,
    pub driver: String,
    /// TLS was configured in Laragon.
    pub secure: bool,
    /// PHP series of the site (the active one of Laragon); `None` for static drivers or
    /// when it is unknown.
    pub php_series: Option<String>,
    /// Files the site was read from, relative to the Laragon folder.
    pub sources: Vec<String>,
    /// Remarks (renamed hosts, wildcard aliases left out).
    pub notes: Vec<String>,
}

/// A folder to serve as parked (`www`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedParked {
    pub dir: String,
    /// Sub-folders (projects) inside.
    pub projects: usize,
    /// The folder has content: converting it to case-sensitive is offered
    /// (docs/15 §3.1).
    pub offer_case_conversion: bool,
}

/// Something that will not be imported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    /// One of [`kind`].
    pub kind: &'static str,
    pub name: String,
    pub reason: String,
}

/// What to do with what the plan found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportOptions {
    /// Decide and report, write nothing (no copy, no move, no database row, no config).
    pub dry_run: bool,
    /// Copy tool folders into `bin/<tool>/` (Forgebox names). Default: register them
    /// where they are.
    pub copy_tools: bool,
    /// Move `data/` folders to `data/<tool>/<name>` (same volume only; otherwise they
    /// are referenced). Default: reference them where they are.
    pub move_data: bool,
    /// Convert the parked folders to case-sensitive after importing (docs/15 §3.3).
    pub convert_case_sensitive: bool,
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportItem {
    /// One of [`kind`].
    pub kind: &'static str,
    pub name: String,
    /// Imported (in a dry run: would be imported).
    pub imported: bool,
    /// Why it was not imported, or a remark about what was.
    pub note: Option<String>,
}

/// What an import did (or, with `dry_run`, would do).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    pub dry_run: bool,
    pub items: Vec<ReportItem>,
}

impl ImportReport {
    /// Items that were imported (or would be).
    pub fn imported(&self) -> impl Iterator<Item = &ReportItem> {
        self.items.iter().filter(|i| i.imported)
    }

    /// Items that were not.
    pub fn skipped(&self) -> impl Iterator<Item = &ReportItem> {
        self.items.iter().filter(|i| !i.imported)
    }

    pub(crate) fn push(
        &mut self,
        kind: &'static str,
        name: impl Into<String>,
        imported: bool,
        note: Option<String>,
    ) {
        self.items.push(ReportItem {
            kind,
            name: name.into(),
            imported,
            note,
        });
    }
}
