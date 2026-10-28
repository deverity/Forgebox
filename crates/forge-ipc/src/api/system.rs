//! `system.*`: handshake, liveness, import, bundles, backups, configuration and
//! self-update (docs/09 §registry/sistema).
//!
//! `forge completions` and `forge mcp serve` run entirely in the client.

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::{ApiVersion, InstanceRef, Role, ToolSpec};
use crate::transport::TransportKind;
use forge_core::Timestamp;
use forge_core::ids::SessionId;

/// Who is connecting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ClientInfoDto {
    /// `forge`, `Forgebox.exe`, `forge-mcp`.
    pub name: String,
    pub version: String,
}

/// Params of `system.hello`: the first request of every connection (docs/11 §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct HelloParams {
    /// Content of `state/ipc.token`.
    pub token: String,
    pub client: ClientInfoDto,
    pub api_version: ApiVersion,
}

/// Result of `system.hello`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct HelloDto {
    pub session_id: SessionId,
    pub api_version: ApiVersion,
    pub daemon_version: String,
    /// `windows`, `linux`...
    pub platform: String,
    pub role: Role,
    pub transport: TransportKind,
    /// The connection is local: `local_only` methods are allowed.
    pub local: bool,
    /// Resume point for `events.subscribe`.
    #[specta(type = specta_typescript::Number)]
    pub last_event_seq: u64,
    pub server_time: Timestamp,
    /// Methods of the contract this daemon answers with `not_implemented` (API 1.1): the
    /// GUI disables or labels the matching controls without trying them. Missing from
    /// 1.0 daemons (empty).
    #[serde(default)]
    pub not_implemented: Vec<String>,
}

/// Result of `system.ping` (latency indicator, docs/13 §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PongDto {
    pub server_time: Timestamp,
}

/// Params of `system.import_laragon`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ImportLaragonParams {
    /// Laragon folder on the daemon's machine (`C:/laragon`); only read.
    pub path: String,
    /// Decide and report, write nothing.
    #[serde(default)]
    pub dry_run: bool,
    /// Copy the tool folders into `bin/<tool>/` instead of registering them where they
    /// are.
    #[serde(default)]
    pub copy_tools: bool,
    /// Move the `data/` folders into the root (same volume) instead of referencing them.
    #[serde(default)]
    pub move_data: bool,
    /// Convert the parked folders to case-sensitive (docs/15 §3.3) after importing.
    #[serde(default)]
    pub convert_case_sensitive: bool,
}

/// One imported item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ImportItemDto {
    /// `tool_version`, `site`, `database_dir`, `vhost`, `parked_dir`, `pref`,
    /// `case_conversion`, `tool`.
    pub kind: String,
    pub name: String,
    /// Imported; in a dry run, would be imported.
    pub imported: bool,
    /// Why it was not imported, or a remark about what was.
    pub note: Option<String>,
}

/// Result of the `system.import_laragon` job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ImportReportDto {
    /// Nothing was written: `imported` means "would be imported".
    #[serde(default)]
    pub dry_run: bool,
    pub items: Vec<ImportItemDto>,
}

/// Params of `system.bundle_export`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct BundleExportParams {
    pub specs: Vec<ToolSpec>,
    /// Output on the daemon's machine (default `backups/bundles/<name>.fbx`).
    #[serde(default)]
    pub output: Option<String>,
}

/// Params of `system.bundle_import`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct BundleImportParams {
    pub file: String,
}

/// A file produced by a job (bundle, backup).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ArtifactDto {
    pub path: String,
    #[specta(type = specta_typescript::Number)]
    pub size_bytes: u64,
    /// SHA-256 of the file (hex).
    pub sha256: String,
}

/// Params of `system.backup_now`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct BackupNowParams {
    /// One instance or every database instance.
    #[serde(default)]
    pub instance: Option<InstanceRef>,
}

/// Result of the `system.backup_now` job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct BackupReportDto {
    pub artifacts: Vec<ArtifactDto>,
}

/// Backup frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum BackupFrequency {
    Off,
    Daily,
    Weekly,
}

/// Params and result of `system.backup_schedule`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct BackupScheduleDto {
    pub frequency: BackupFrequency,
    /// Local time `HH:MM` on the daemon's machine.
    #[serde(default)]
    pub at: Option<String>,
}

/// Params of `system.config_get`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ConfigGetParams {
    /// Dotted key of `forgebox.toml` (`prefs.default_server`).
    pub key: String,
}

/// Params of `system.config_set`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ConfigSetParams {
    pub key: String,
    #[specta(type = specta_typescript::Unknown)]
    pub value: serde_json::Value,
}

/// Result of `system.config_get|config_set`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ConfigValueDto {
    pub key: String,
    #[specta(type = specta_typescript::Unknown)]
    pub value: serde_json::Value,
    /// The value is the built-in default (not set in `forgebox.toml`).
    pub is_default: bool,
}

/// Params of `system.self_update`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SelfUpdateParams {
    /// Only check.
    #[serde(default)]
    pub check: bool,
    /// `stable` (default) or `beta`.
    #[serde(default)]
    pub channel: Option<String>,
}

/// Result of the `system.self_update` job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SelfUpdateDto {
    pub current: String,
    pub latest: Option<String>,
    pub updated: bool,
    /// The daemon restarts to finish the update.
    pub restart_required: bool,
}
