//! `tools.*`: catalog, install/remove/update/rollback, policies, active CLI version
//! (docs/09 §tool, docs/05). Downloads always happen on the daemon's machine.
//!
//! API 1.1 (ADR-0041) adds the catalog metadata of the core screens (kind, summary, color,
//! icon, platforms, size estimate, variant labels, stack contents), `tools.releases` and
//! `tools.plan` (jobs: provider releases and the install plan, nothing downloaded),
//! `tools.history` (update history), `tools.icon` (sanitized SVG) and the extension
//! `target` of `tools.install`/`tools.remove`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::ToolSpec;
use forge_core::Timestamp;
use forge_core::ids::{JobId, SiteId, ToolId, UpdateId, VersionId};

fn default_true() -> bool {
    true
}

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

/// What a catalog entry is: the `kind` of its manifest (docs/04: `Tool`, `Extension`,
/// `Stack`). API 1.1 (ADR-0041); entries of 1.0 daemons are tools.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CatalogKind {
    #[default]
    Tool,
    /// Installed inside a version of another tool (`extends`, docs/04 §2).
    Extension,
    /// Preset of tools, instances and a site template (docs/04 §3).
    Stack,
}

/// A catalog entry (`forge search`).
///
/// The fields after `installed` are API 1.1 (ADR-0041, filled by M2-E2-T4); a 1.0 daemon
/// sends none of them and they read as their defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CatalogEntryDto {
    pub id: ToolId,
    pub name: String,
    pub category: Option<String>,
    /// The one-line `metadata.summary` (API 1.0 name; the same text as `summary`).
    pub description: Option<String>,
    pub tags: Vec<String>,
    /// Registry the manifest comes from.
    pub registry: String,
    /// Versions offered for this platform, newest first.
    pub versions: Vec<String>,
    pub installed: bool,
    #[serde(default)]
    pub kind: CatalogKind,
    /// `metadata.summary`: the line under the name on a card.
    #[serde(default)]
    pub summary: Option<String>,
    /// `ui.color` (`#777BB4`).
    #[serde(default)]
    pub color: Option<String>,
    /// SHA-256 (hex) of the sanitized SVG served by `tools.icon`; `None` without an icon.
    /// Clients cache icons by it.
    #[serde(default)]
    pub icon_sha256: Option<String>,
    /// `spec.platforms` (`windows-x64`...).
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Runs natively on the daemon's platform (the "nativo Windows" badge); `false` for a
    /// tool that would need WSL (v2). `true` when a 1.0 daemon does not say.
    #[serde(default = "default_true")]
    pub native: bool,
    /// The manifest comes from the official, signed registry (`false` for third-party
    /// registries, and when a 1.0 daemon does not say).
    #[serde(default)]
    pub official: bool,
    /// Newest stable version known to the registry.
    #[serde(default)]
    pub latest: Option<String>,
    /// Newer version for an installed series, when one is known.
    #[serde(default)]
    pub update_available: Option<String>,
    /// `install.size_estimate_mb` in bytes (an estimate: real sizes come from
    /// `tools.releases`/`tools.plan`). Stacks: the sum of their tools.
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub size_estimate_bytes: Option<u64>,
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

/// A build dimension of a tool (`versioning.variants[]`, docs/04 §1.6), for the variant
/// picker of the catalog. API 1.1 (ADR-0041).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct VariantDefDto {
    /// `ts`, `compiler`, `arch`.
    pub key: String,
    /// Values in order of preference (`[nts, ts]`).
    pub values: Vec<String>,
    /// Preferred value (`nts`).
    #[serde(default)]
    pub default: Option<String>,
    /// Human labels per value (`{nts: "Non Thread Safe"}`), as the manifest writes them
    /// (not translated; clients may have their own).
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// The provider decides the value (`derive: from_provider`): shown, not chosen.
    #[serde(default)]
    pub derived: bool,
}

/// A tool of a stack (`spec.tools[]`, docs/04 §3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct StackToolDto {
    pub id: ToolId,
    /// Series (`8.4`); `None` = latest stable.
    #[serde(default)]
    pub series: Option<String>,
    /// Exact version, when the stack pins one.
    #[serde(default)]
    pub version: Option<String>,
    /// Instance created for a service (`default`).
    #[serde(default)]
    pub instance: Option<String>,
    /// Extensions installed into this tool (`[xdebug]`).
    #[serde(default)]
    pub extensions: Vec<ToolId>,
}

/// What a stack installs (`tools.info` of a stack). API 1.1 (ADR-0041).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct StackDto {
    pub tools: Vec<StackToolDto>,
}

/// Result of `tools.info` (a tool, an extension or a stack of the catalog).
///
/// The fields after `used_by` are API 1.1 (ADR-0041, filled by M2-E2-T4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ToolInfoDto {
    pub tool: ToolDto,
    pub available: Vec<AvailableVersionDto>,
    pub used_by: Vec<SiteUsageDto>,
    #[serde(default)]
    pub kind: CatalogKind,
    /// `metadata.description`: Markdown, untrusted (clients render it without raw HTML).
    #[serde(default)]
    pub description_md: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub docs: Option<String>,
    /// Upstream changelog / release notes.
    #[serde(default)]
    pub changelog_url: Option<String>,
    /// SPDX id (`PHP-3.01`).
    #[serde(default)]
    pub license: Option<String>,
    /// Build dimensions, in folder-name order.
    #[serde(default)]
    pub variants: Vec<VariantDefDto>,
    /// `spec.suggests`: offered after installing.
    #[serde(default)]
    pub suggests: Vec<ToolId>,
    /// Extensions: the tool they install into (`php`).
    #[serde(default)]
    pub extends: Option<ToolId>,
    /// Stacks: what they install.
    #[serde(default)]
    pub stack: Option<StackDto>,
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
    /// Extensions (`xdebug`): the installed version of the extended tool to install into
    /// (`php@8.3`, `php@8.3.36`); `None` = every compatible installed version. CLI
    /// `--for`. API 1.1 (ADR-0041); answers `not_implemented` (M2-E2-T8) until the
    /// extension install lands.
    #[serde(default)]
    pub target: Option<ToolSpec>,
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
    /// Extensions: remove only from this version of the extended tool (`php@8.4`); `None`
    /// = from every version that has it. CLI `--for`. API 1.1 (ADR-0041); answers
    /// `not_implemented` (M2-E2-T8) until the extension install lands.
    #[serde(default)]
    pub target: Option<ToolSpec>,
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

// --- API 1.1: releases, install plan, history, icons (ADR-0041) -------------------------

/// Params of `tools.releases` (job: asks the providers; nothing is downloaded).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolReleasesParams {
    pub tool: ToolId,
    /// Only this channel (`stable`, `preview`: `versioning.channels`); `None` = every
    /// channel the manifest declares, each release naming its own.
    #[serde(default)]
    pub channel: Option<String>,
}

/// One build of a release: a combination of variant values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ReleaseVariantDto {
    /// `{ts: "nts", compiler: "vs17", arch: "x64"}`.
    pub variant: BTreeMap<String, String>,
    /// Folder the build installs to (`8.3.37-nts-vs17-x64`, docs/08 §5).
    pub canonical_name: String,
    /// Download size reported by the provider.
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub size_bytes: Option<u64>,
    /// This exact build is in `bin/`.
    #[serde(default)]
    pub installed: bool,
}

/// A version published by the providers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolReleaseDto {
    /// `8.3.37`.
    pub version: String,
    /// `8.3` (`versioning.series_key`).
    pub series: String,
    /// Channel of the version (`stable`, `preview`).
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub published_at: Option<Timestamp>,
    /// Size of the preferred build (the first of `variants`).
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub size_bytes: Option<u64>,
    /// Some build of this version is installed.
    #[serde(default)]
    pub installed: bool,
    /// Builds offered for this platform, preferred first (docs/04 §1.6).
    #[serde(default)]
    pub variants: Vec<ReleaseVariantDto>,
}

/// Result of the `tools.releases` job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolReleasesDto {
    pub tool: ToolId,
    /// Newest first (by the tool's version scheme); clients group them by `series`.
    pub releases: Vec<ToolReleaseDto>,
    /// The providers could not be reached (offline, timeout): this is the cached list.
    #[serde(default)]
    pub stale: bool,
    /// When the list was fetched from the providers.
    #[serde(default)]
    pub fetched_at: Option<Timestamp>,
}

/// Params of `tools.plan` (job): what `tools.install` would do, without doing it.
///
/// `specs`, or `stack` (a stack id: its tools and their extensions). With a stack, `specs`
/// replace the stack's entry of the same tool (`php@8.3` instead of the stack's 8.4) and
/// add the others, so a client can plan again after the user adjusts versions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolPlanParams {
    #[serde(default)]
    pub specs: Vec<ToolSpec>,
    #[serde(default)]
    pub stack: Option<String>,
    /// Build variant, as in `tools.install`.
    #[serde(default)]
    pub variant: Option<String>,
    /// Extension target, as in `tools.install`.
    #[serde(default)]
    pub target: Option<ToolSpec>,
}

/// Why an item is in the plan: a closed set of the resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ToolPlanReason {
    /// Asked for in `specs`.
    Requested,
    /// `spec.depends_on` of another item.
    Dependency,
    /// A tool of the planned stack.
    Stack,
    /// An extension of the stack or of `specs`, installed into `target`.
    Extension,
}

/// One version the install would download (or finds already installed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolPlanItemDto {
    pub tool: ToolId,
    #[serde(default)]
    pub kind: CatalogKind,
    /// What the item resolves (`php@8.4`): what to pass to `tools.install`.
    pub spec: ToolSpec,
    /// Exact version chosen (`8.4.14`).
    pub version: String,
    /// Build chosen (`{ts: "nts", ...}`).
    #[serde(default)]
    pub variant: BTreeMap<String, String>,
    /// Folder it installs to, when known (`8.4.14-nts-vs17-x64`).
    #[serde(default)]
    pub canonical_name: Option<String>,
    /// Download size: the provider's, else the manifest estimate (`size_estimated`).
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub size_estimated: bool,
    /// Already in `bin/`: nothing to download.
    #[serde(default)]
    pub already_installed: bool,
    pub reason: ToolPlanReason,
    /// Extensions: the version of the extended tool it goes into (`php@8.4`).
    #[serde(default)]
    pub target: Option<ToolSpec>,
}

/// Result of the `tools.plan` job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolPlanDto {
    /// In install order (dependencies first).
    pub items: Vec<ToolPlanItemDto>,
    /// Sum of `size_bytes` of the items to download, estimates included (show "≈" when
    /// any of them has `size_estimated`).
    #[specta(type = specta_typescript::Number)]
    pub total_bytes: u64,
    /// Items to download without any size (left out of `total_bytes`).
    #[serde(default)]
    pub unknown_size: u32,
}

/// Params of `tools.history`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolHistoryParams {
    pub tool: ToolId,
    /// Only this series (`8.3`).
    #[serde(default)]
    pub series: Option<String>,
    /// Newest first; default 50, at most 500.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Who started an update (`update_history.triggered_by`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum UpdateTriggerDto {
    /// The scheduler, under an `auto-*` policy.
    Auto,
    /// `forge tool update`, the GUI.
    User,
    /// A rollback row.
    Rollback,
}

/// State of a history row (`update_history.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatusDto {
    Started,
    Completed,
    Failed,
    /// A completed update that a later rollback undid.
    RolledBack,
}

/// One update or rollback of a tool (`update_history`, docs/05 §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct UpdateHistoryEntryDto {
    pub id: UpdateId,
    /// Series of the replaced version (`8.3`).
    #[serde(default)]
    pub series: Option<String>,
    /// Version before (`8.3.36`), as text: it outlives the removal of the version.
    #[serde(default)]
    pub from: Option<String>,
    pub to: String,
    /// `None` once the version was removed.
    #[serde(default)]
    pub from_version_id: Option<VersionId>,
    #[serde(default)]
    pub to_version_id: Option<VersionId>,
    /// Policy in effect.
    #[serde(default)]
    pub policy: Option<UpdatePolicy>,
    pub trigger: UpdateTriggerDto,
    pub status: UpdateStatusDto,
    /// Why it failed (English, like logs).
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub job_id: Option<JobId>,
    /// Rollback rows: the update undone.
    #[serde(default)]
    pub rollback_of: Option<UpdateId>,
    pub started_at: Timestamp,
    #[serde(default)]
    pub finished_at: Option<Timestamp>,
}

/// Result of `tools.history`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct UpdateHistoryDto {
    pub tool: ToolId,
    /// Newest first.
    pub entries: Vec<UpdateHistoryEntryDto>,
}

/// Result of `tools.icon` (params: `ToolParams` with any catalog id): the icon of a
/// catalog entry from the daemon's verified registry cache, sanitized (at most 64 KiB; no
/// scripts, `foreignObject`, event attributes or references other than `#id` and `data:`).
/// Clients show it only as an `<img>` data URI, never as inline SVG. `not_found` when the
/// entry has no icon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct IconDto {
    pub tool: ToolId,
    /// The SVG document (UTF-8 text).
    pub svg: String,
    /// SHA-256 (hex) of `svg`: the `CatalogEntryDto.icon_sha256` of the entry.
    pub sha256: String,
}
