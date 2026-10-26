//! `tools.*`: catalog, install/remove/update/rollback, policies, active CLI version
//! (docs/09 §tool, docs/05). Downloads always happen on the daemon's machine.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::ToolSpec;
use forge_core::Timestamp;
use forge_core::ids::{SiteId, ToolId, VersionId};

/// Update policy of a tool (docs/05 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "kebab-case")]
pub enum UpdatePolicy {
    Manual,
    Notify,
    AutoPatch,
    AutoMinor,
    Auto,
}

/// State of an installed version (`tool_versions.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ToolVersionState {
    Downloading,
    Installed,
    Broken,
    Removing,
}

/// How a version got into `bin/` (`tool_versions.source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ToolVersionSource {
    Registry,
    Manual,
    Import,
}

/// An installed version of a tool: `bin/<tool>/<canonical_name>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ToolVersionDto {
    pub id: VersionId,
    pub tool: ToolId,
    /// `8.3.36`.
    pub version: String,
    /// Folder name (`8.3.36-nts-vs17-x64`).
    pub canonical_name: String,
    /// Build variant (`{ts: "nts", vc: "vs17", arch: "x64"}`).
    pub variant: BTreeMap<String, String>,
    pub state: ToolVersionState,
    pub source: ToolVersionSource,
    /// Informative absolute path on the daemon's machine.
    pub path: String,
    #[specta(type = Option<specta_typescript::Number>)]
    pub size_bytes: Option<u64>,
    pub installed_at: Option<Timestamp>,
    /// Target of the `current` junction (active CLI version).
    pub is_current: bool,
}

/// A tool known locally (installed or in the catalog).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ToolDto {
    pub id: ToolId,
    pub name: String,
    pub category: Option<String>,
    pub description: Option<String>,
    pub installed: Vec<ToolVersionDto>,
    /// Version behind `current` (CLI).
    pub current: Option<String>,
    pub policy: UpdatePolicy,
    /// Latest version known to the registry.
    pub latest: Option<String>,
    /// Set when an installed series has a newer patch/minor.
    pub update_available: Option<String>,
    /// The manifest declares a service (has instances).
    pub has_service: bool,
}

/// A catalog entry (`forge search`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CatalogEntryDto {
    pub id: ToolId,
    pub name: String,
    pub category: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    /// Registry the manifest comes from.
    pub registry: String,
    /// Versions offered for this platform, newest first.
    pub versions: Vec<String>,
    pub installed: bool,
}

/// Params of `tools.search`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolSearchParams {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub category: Option<String>,
}

/// Result of `tools.search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CatalogSearchDto {
    pub entries: Vec<CatalogEntryDto>,
}

/// Params of `tools.list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolListParams {
    /// Only tools with at least one installed version.
    #[serde(default)]
    pub installed: bool,
    /// Only tools with an update available.
    #[serde(default)]
    pub updates: bool,
}

/// Result of `tools.list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ToolListDto {
    pub tools: Vec<ToolDto>,
}

/// Params of `tools.info`, `tools.discover`-like single tool methods.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolParams {
    pub tool: ToolId,
}

/// A version offered by the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct AvailableVersionDto {
    pub version: String,
    /// Variants (`nts-vs17-x64`, `ts-vs17-x64`).
    pub variants: Vec<String>,
    /// `stable`, `lts`, `beta`...
    pub channel: Option<String>,
}

/// A site that depends on a tool version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SiteUsageDto {
    pub site_id: SiteId,
    pub host: String,
    pub version: String,
}

/// Result of `tools.info`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ToolInfoDto {
    pub tool: ToolDto,
    pub available: Vec<AvailableVersionDto>,
    pub used_by: Vec<SiteUsageDto>,
}

/// Params of `tools.install`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolInstallParams {
    /// `php@8.3` (series → latest patch), `php@8.3.36` (exact), `nginx`.
    pub specs: Vec<ToolSpec>,
    /// Build variant (`ts`, `nts`).
    #[serde(default)]
    pub variant: Option<String>,
    /// Accept dependent installs without asking (CLI `--yes`).
    #[serde(default)]
    pub yes: bool,
}

/// Result of the `tools.install` job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ToolInstallResultDto {
    pub installed: Vec<ToolVersionDto>,
}

/// Params of `tools.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolRemoveParams {
    pub spec: ToolSpec,
    #[serde(default)]
    pub keep_config: bool,
    /// Destructive: must be `true` (CLI `--yes`, MCP `confirm`).
    #[serde(default)]
    pub confirm: bool,
}

/// Params of `tools.update` and `tools.check_updates`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolUpdateParams {
    /// One tool, or all when `None`.
    #[serde(default)]
    pub tool: Option<ToolId>,
}

/// One available or applied update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolUpdateDto {
    pub tool: ToolId,
    pub from: String,
    pub to: String,
    /// Allowed by the tool's policy without confirmation.
    pub automatic: bool,
}

/// Result of the `tools.update` and `tools.check_updates` jobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolUpdatesDto {
    pub updates: Vec<ToolUpdateDto>,
}

/// Params of `tools.rollback`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolRollbackParams {
    /// Series to roll back (`php@8.3`).
    pub spec: ToolSpec,
}

/// Params of `tools.policy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolPolicyParams {
    pub tool: ToolId,
    pub policy: UpdatePolicy,
}

/// Params of `tools.add_local` (path on the caller's machine: local only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolAddLocalParams {
    /// Zip or folder; its `forgebox.yaml` is used, or the layout is discovered.
    pub source: String,
}

/// Result of the `tools.discover` job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ToolDiscoverDto {
    /// Versions registered from loose folders in `bin/`.
    pub registered: Vec<ToolVersionDto>,
    /// Folders that could not be identified (informative paths).
    pub unknown: Vec<String>,
}

/// Params of `tools.use`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolUseParams {
    pub tool: ToolId,
    /// Series or exact version (`8.4`, `22`, `3.13.1`).
    pub version: String,
}

/// Params of `tools.which`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolWhichParams {
    pub tool: ToolId,
    /// Project folder on the daemon's machine whose `.php-version`/`forgebox.yaml`
    /// applies (CLI: current directory when local).
    #[serde(default)]
    pub project_dir: Option<String>,
}

/// Result of `tools.which`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct WhichDto {
    pub tool: ToolId,
    /// Behind `current`.
    pub global: Option<ToolVersionDto>,
    /// Selected by the project, if any.
    pub project: Option<ToolVersionDto>,
    /// File that selected the project version (`.php-version`, `forgebox.yaml`).
    pub project_source: Option<String>,
    /// Executable that runs (informative path).
    pub executable: Option<String>,
}
