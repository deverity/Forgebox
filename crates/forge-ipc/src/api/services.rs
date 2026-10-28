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
