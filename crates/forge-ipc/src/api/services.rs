//! `services.*`: instances and pools (docs/09 §service, docs/03 §1-§4).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::{ApiError, FileRefDto, InstanceRef, ServiceTarget, ToolSpec};
use forge_core::Timestamp;
use forge_core::ids::{InstanceId, PortReservationId, ToolId, VersionId};

/// State of an instance or pool (docs/03 §1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Crashed,
    Failed,
    /// Install/upgrade in progress.
    Maintenance,
}

/// Structured diagnosis of a `failed` instance (docs/03 §1.4), tagged by `kind`.
///
/// Published by `forge_supervisor::FailureReason` (same shape). Exit codes are the bits
/// of the Windows `DWORD` as `i32`. `kind` decides the UI action (`missing_dll` /
/// `missing_runtime` → offer the VC++ Redistributable, `port_in_use` → stop the holder or
/// change the port, `dependency_not_running` → start the dependency).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FailureReasonDto {
    /// A declared port is taken (by `pid`/`exe` when known).
    PortInUse {
        port: u16,
        pid: Option<u32>,
        exe: Option<String>,
    },
    /// A DLL (or an entry point in one) is missing: exit `0xC0000135` / `0xC0000139`.
    MissingDll { exit_code: i32 },
    /// The configuration check failed; its output (stderr, then stdout).
    ConfigInvalid { stderr: String },
    /// The VC++ Redistributable needed by `flavor` builds is missing or too old
    /// (pre-checks, docs/03 §1.5).
    MissingRuntime {
        /// `vc15`, `vs16`, `vs17`, `vs18`.
        flavor: String,
        download_url: String,
        /// Older runtime found (`14.29.30133.0`).
        installed: Option<String>,
    },
    /// A `depends_on` service is not running (pre-checks).
    DependencyNotRunning {
        /// `mysql` or `mysql/default`.
        dependency: String,
        /// `stopped`, `failed`...; `unknown` when not found.
        state: String,
    },
    /// Access denied starting or running the executable.
    Permission { message: String },
    /// Health did not pass within `start_timeout`.
    HealthTimeout {
        #[specta(type = specta_typescript::Number)]
        timeout_ms: u64,
        last_error: Option<String>,
    },
    /// The process exited before it became healthy.
    ExitedDuringStart { exit_code: i32 },
    /// The restart policy gave up after `restarts` automatic restarts in `window_ms`.
    CrashLoop {
        restarts: u32,
        #[specta(type = specta_typescript::Number)]
        window_ms: u64,
        exit_code: Option<i32>,
    },
    /// Anything else; `code` is a stable error code (`not_found`, `internal`...).
    Other { code: String, message: String },
}

/// Transport protocol of a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PortProtocol {
    Tcp,
    Udp,
}

/// Kind of owner of a reserved port (`ports.owner_kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PortOwnerKind {
    Instance,
    Pool,
    App,
    Site,
    Daemon,
}

/// State of a reserved port (`ports.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PortState {
    Reserved,
    Bound,
    Conflict,
}

/// Process outside Forgebox holding a port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PortHolderDto {
    pub pid: u32,
    /// Executable name and informative path (`mysqld.exe (C:/xampp/mysql/bin)`).
    pub exe: String,
}

/// A port reservation (docs/03 §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PortDto {
    pub id: PortReservationId,
    pub port: u16,
    pub protocol: PortProtocol,
    pub owner_kind: PortOwnerKind,
    /// ULID of the owner (instance, pool version, app, site) or `daemon`.
    pub owner_id: String,
    /// Human name of the owner (`mysql/default`, `php@8.3`).
    pub owner_name: String,
    /// Port name in the manifest (`main`, `smtp`, `ui`).
    pub name: String,
    pub state: PortState,
    pub conflict: Option<PortHolderDto>,
}

/// A service instance (`mysql 8.4 "default"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct InstanceDto {
    pub id: InstanceId,
    pub tool: ToolId,
    pub version_id: VersionId,
    pub version: String,
    pub name: String,
    pub state: ServiceState,
    pub pid: Option<u32>,
    pub ports: Vec<PortDto>,
    pub autostart: bool,
    pub started_at: Option<Timestamp>,
    pub cpu_pct: Option<f32>,
    #[specta(type = Option<specta_typescript::Number>)]
    pub rss_bytes: Option<u64>,
    pub restarts: u32,
    /// Diagnosis of `failed`/`crashed` (`port_in_use`, `config_invalid`...).
    pub last_error: Option<ApiError>,
    /// Data folder of the instance (informative path, `/` separators), when the manifest
    /// declares `service.daemon.data` or the instance references an external one. API 1.1
    /// (ADR-0041).
    #[serde(default)]
    pub data_dir: Option<String>,
    /// Size of `data_dir` on disk, from a periodic scan off the actor (M2-E2-T10); `None`
    /// until the first scan. API 1.1 (ADR-0041).
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub size_bytes: Option<u64>,
}

/// A PHP FastCGI pool (docs/03 §2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct PoolDto {
    /// `8.3`.
    pub series: String,
    /// `8.3.36`.
    pub version: String,
    pub version_id: VersionId,
    pub state: ServiceState,
    /// Upstream name used in the web server configs (`php_8_3`).
    pub upstream: String,
    pub workers: u32,
    pub busy_workers: u32,
    pub min_workers: u32,
    pub max_workers: u32,
    pub ports: Vec<u16>,
    pub sites: u32,
    pub xdebug: bool,
    #[specta(type = Option<specta_typescript::Number>)]
    pub requests_total: Option<u64>,
    #[specta(type = Option<specta_typescript::Number>)]
    pub rss_bytes: Option<u64>,
}

/// Params of `services.start|stop|restart`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ServiceTargetsParams {
    /// Targets; empty means `all`.
    #[serde(default)]
    pub targets: Vec<ServiceTarget>,
}

/// Params of `services.reload`, `services.config`, `services.config_files`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ServiceTargetParams {
    pub target: ServiceTarget,
}

/// Result of `services.status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ServiceStatusDto {
    pub instances: Vec<InstanceDto>,
    pub pools: Vec<PoolDto>,
}

/// Params of `services.create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ServiceCreateParams {
    /// `mysql@8.4`.
    pub tool: ToolSpec,
    /// Instance name (`legacy`).
    pub name: String,
    /// Main port; allocated from the tool's range when absent.
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub autostart: bool,
    /// Data folder on the daemon's machine (absolute; inside the root or a folder the user
    /// chose, never inside `bin/`); default: the manifest's `service.daemon.data.dir`. CLI
    /// `--data-dir`. API 1.1 (ADR-0041); answers `not_implemented` (M2-E2-T10) until the
    /// backend lands.
    #[serde(default)]
    pub data_dir: Option<String>,
}

/// Params of `services.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ServiceRemoveParams {
    pub instance: InstanceRef,
    #[serde(default)]
    pub delete_data: bool,
    /// Destructive: must be `true`.
    #[serde(default)]
    pub confirm: bool,
}

/// Result of `services.config` (files opened) and `services.config_files`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ConfigFilesDto {
    pub files: Vec<FileRefDto>,
}

/// Params of `services.set` (quick settings, `memory_limit=1G`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ServiceSetParams {
    pub target: ServiceTarget,
    pub settings: BTreeMap<String, String>,
}

/// Result of `services.ports`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PortMapDto {
    /// Ports reserved by Forgebox.
    pub ports: Vec<PortDto>,
    /// Listening ports held by other processes that collide with Forgebox ranges.
    pub external: Vec<ExternalPortDto>,
}

/// A port held outside Forgebox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ExternalPortDto {
    pub port: u16,
    pub protocol: PortProtocol,
    pub holder: PortHolderDto,
}

/// Params of `services.kill_port_holder`: terminates the process holding a port (the
/// "Encerrar" of a port conflict). The daemon re-checks that `pid` still holds `port` and
/// is not a Forgebox process, the daemon itself, a system/critical process or a service,
/// then publishes `port.holder_killed`. Admin, local only. API 1.1 (ADR-0040).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct KillPortHolderParams {
    pub port: u16,
    /// The process the user confirmed (from `net.port_check` or `services.ports`).
    pub pid: u32,
    /// Destructive: must be `true`.
    #[serde(default)]
    pub confirm: bool,
}

// --- API 1.1: quick settings and advanced settings (ADR-0041) --------------------------

/// Control of a quick setting: `config.quick_settings[].type` (docs/04 §1.3), a closed
/// grammar of the manifest schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum QuickSettingKind {
    Toggle,
    Select,
    Number,
    Text,
    Path,
    Port,
    /// `min`/`max` (and `step`) always set.
    Slider,
    /// Checkboxes over `options`.
    Multi,
}

/// What applying a setting takes (`apply`, docs/04 §1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SettingApply {
    /// Graceful reload (recycle of the PHP pool, `nginx -s reload`).
    Reload,
    /// Restart of the service.
    Restart,
    /// Nothing (read on the next start or per request).
    None,
}

/// Value of a quick setting, typed by its kind: `toggle` → boolean; `number`, `slider`,
/// `port` → number; `multi` → list of option texts; `select`, `text`, `path` → text.
/// `services.set` takes the text form of each (`true`/`false`, decimal numbers, `multi`
/// items joined by `,`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(untagged)]
pub enum SettingValueDto {
    Bool(bool),
    Int(#[specta(type = specta_typescript::Number)] i64),
    Float(#[specta(type = specta_typescript::Number)] f64),
    Text(String),
    List(Vec<String>),
}

/// One quick setting of a service or pool with its current value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct QuickSettingDto {
    /// Key for `services.set` (`memory_limit`).
    pub key: String,
    /// `label` of the manifest (`Memory limit`); clients fall back to `key`.
    #[serde(default)]
    pub label: Option<String>,
    pub kind: QuickSettingKind,
    /// Choices of `select`/`multi`, as text (`[128M, 256M, -1]`).
    #[serde(default)]
    pub options: Vec<String>,
    /// Bounds of `number`/`slider`.
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub step: Option<f64>,
    /// Current value (the stored one, else `default`); `None` when neither exists.
    #[serde(default)]
    pub value: Option<SettingValueDto>,
    #[serde(default)]
    pub default: Option<SettingValueDto>,
    /// `value` is the default (nothing stored for this key).
    #[serde(default)]
    pub is_default: bool,
    pub apply: SettingApply,
    /// `config.files[].id` the setting writes (the `role` of a `files` entry).
    #[serde(default)]
    pub file: Option<String>,
    /// Other keys written with the same value (`post_max_size`).
    #[serde(default)]
    pub also_set: Vec<String>,
    /// `port`: reserved in the port allocator (Xdebug 9003).
    #[serde(default)]
    pub reserve: bool,
}

/// A named port of an instance, changed with `services.set {"ports.<name>": "<port>"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ServicePortSettingDto {
    /// Name in the manifest (`main`, `smtp`, `ui`).
    pub name: String,
    /// Port reserved now; `None` before the first reservation.
    #[serde(default)]
    pub port: Option<u16>,
    /// The manifest's default.
    #[serde(default)]
    pub default: Option<u16>,
    pub protocol: PortProtocol,
    /// Range it is allocated from (`databases`).
    #[serde(default)]
    pub range: Option<String>,
}

/// When an instance is restarted after it exits (`restart_policy`, docs/04 §1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum RestartOn {
    Never,
    /// After an unexpected exit, at most `max` times in `window_secs`.
    Failure,
    /// After any exit, without a limit.
    Always,
}

/// A restart policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RestartPolicyDto {
    pub on: RestartOn,
    /// Restarts allowed inside the window (`failure`).
    pub max: u32,
    pub window_secs: u32,
}

/// Advanced settings of an instance (the "Avançado" tab, docs/07 §5.3): what
/// `services.update` changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct InstanceAdvancedDto {
    pub autostart: bool,
    /// Extra arguments, after the manifest's.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Policy in effect (the instance's, else the manifest's).
    pub restart_policy: RestartPolicyDto,
    /// The manifest's policy (the "padrão").
    pub restart_policy_default: RestartPolicyDto,
    /// Installed versions of the instance's series it can move to with
    /// `services.update.version`, newest first.
    #[serde(default)]
    pub versions: Vec<String>,
}

/// Result of `services.settings` (`forge service set <target>` without pairs): the quick
/// settings of a target with their current values, its config files and named ports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ServiceSettingsDto {
    /// The target as resolved (`mysql/default`, `php@8.3`).
    pub target: ServiceTarget,
    pub tool: ToolId,
    /// In manifest order.
    pub settings: Vec<QuickSettingDto>,
    /// Config files (`role` = `config.files[].id`), as `services.config_files`.
    #[serde(default)]
    pub files: Vec<FileRefDto>,
    /// Named ports (instances; empty for pools, whose ports come from a range).
    #[serde(default)]
    pub ports: Vec<ServicePortSettingDto>,
    /// Instances only, and only on `admin` connections (it holds the extra environment,
    /// which may carry secrets; changing it needs `admin` anyway).
    #[serde(default)]
    pub advanced: Option<InstanceAdvancedDto>,
}

/// `services.update.restart_policy`: absent fields keep their current value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RestartPolicyPatchDto {
    #[serde(default)]
    pub on: Option<RestartOn>,
    #[serde(default)]
    pub max: Option<u32>,
    #[serde(default)]
    pub window_secs: Option<u32>,
}

/// `services.update.launch`: how the process is started. A present field replaces the
/// current value (`[]`/`{}` clear it); an absent one keeps it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LaunchPatchDto {
    /// Extra arguments, after the manifest's `args`.
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Extra environment variables (over the manifest's `env`).
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
}

/// Params of `services.update` (job): the advanced settings of an instance. Absent fields
/// keep their value. The instance is re-rendered and, when running, restarted. Ports
/// change with `services.set {"ports.<name>": ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ServiceUpdateParams {
    pub instance: InstanceRef,
    #[serde(default)]
    pub autostart: Option<bool>,
    #[serde(default)]
    pub launch: Option<LaunchPatchDto>,
    #[serde(default)]
    pub restart_policy: Option<RestartPolicyPatchDto>,
    /// Another installed version of the **same series** (`8.4.13` → `8.4.14`): data dirs
    /// are not migrated, so a series change is refused (`invalid_input`).
    #[serde(default)]
    pub version: Option<String>,
}
