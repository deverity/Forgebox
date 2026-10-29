//! `db.*`: databases of database instances (docs/09 §db).
//!
//! Which instances are databases, and what they can do, comes from the optional
//! `spec.database` block of their manifest (a protocol family such as `mysql` or
//! `postgresql`, the client runner and the SQL templates; ADR-0040): the daemon reports it
//! in [`DbInstanceDto`], so clients never guess from the tool id.

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::{EnvVarsDto, InstanceRef};
use super::services::ServiceState;
use forge_core::ids::{InstanceId, ToolId};

/// A database inside an instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DatabaseDto {
    pub instance_id: InstanceId,
    /// `mysql/default`.
    pub instance: String,
    pub name: String,
    pub charset: Option<String>,
    pub collation: Option<String>,
    #[specta(type = Option<specta_typescript::Number>)]
    pub size_bytes: Option<u64>,
    pub tables: Option<u32>,
}

/// Params of `db.list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DbListParams {
    /// One instance, or every database instance.
    #[serde(default)]
    pub instance: Option<InstanceRef>,
}

/// What a database instance supports (from its manifest's `spec.database`, ADR-0040).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DbCapabilitiesDto {
    #[serde(default)]
    pub create: bool,
    #[serde(default)]
    pub drop: bool,
    #[serde(default)]
    pub export: bool,
    #[serde(default)]
    pub import: bool,
    /// `db.shell` returns a command for the instance's CLI client.
    #[serde(default)]
    pub shell: bool,
    /// `db.url` renders a DSN and `.env` variables.
    #[serde(default)]
    pub url: bool,
    /// GUI clients `db.open` can launch (M3-E1; empty until then).
    #[serde(default)]
    pub open_with: Vec<ToolId>,
}

/// A database instance (an instance whose manifest declares `spec.database`). Added in
/// API 1.1 (ADR-0040).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DbInstanceDto {
    pub instance_id: InstanceId,
    /// `mysql/default`.
    pub instance: String,
    pub tool: ToolId,
    /// Exact version (`8.4.13`).
    pub version: String,
    /// Protocol family of the manifest (`mysql`, `postgresql`). A string, not an enum:
    /// families come from registry data, and a newer daemon may report one an older client
    /// does not know.
    pub protocol: String,
    pub state: ServiceState,
    /// Main port, when one is reserved.
    pub port: Option<u16>,
    /// Size of the data folder; `None` until the daemon has measured it (cached walk).
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub data_size_bytes: Option<u64>,
    #[serde(default)]
    pub capabilities: DbCapabilitiesDto,
}

/// Result of `db.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DatabaseListDto {
    pub databases: Vec<DatabaseDto>,
    /// Every database instance (also the stopped ones, whose databases are not listed), so
    /// the GUI never guesses which instances are databases. Added in API 1.1; empty from
    /// 1.0 daemons.
    #[serde(default)]
    pub instances: Vec<DbInstanceDto>,
}

/// Params of `db.create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DbCreateParams {
    pub name: String,
    /// Default: the only/default database instance.
    #[serde(default)]
    pub instance: Option<InstanceRef>,
    /// Default `utf8mb4` (MySQL/MariaDB), `UTF8` (PostgreSQL).
    #[serde(default)]
    pub charset: Option<String>,
}

/// Params of `db.drop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DbDropParams {
    pub name: String,
    #[serde(default)]
    pub instance: Option<InstanceRef>,
    /// Destructive: must be `true`.
    #[serde(default)]
    pub confirm: bool,
}

/// Params of `db.export`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DbExportParams {
    pub name: String,
    #[serde(default)]
    pub instance: Option<InstanceRef>,
    /// Output file on the daemon's machine; default `backups/<instance>/<db>-<ts>.sql.zst`.
    #[serde(default)]
    pub output: Option<String>,
}

/// Result of the `db.export` job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DbExportDto {
    pub path: String,
    #[specta(type = specta_typescript::Number)]
    pub size_bytes: u64,
}

/// Params of `db.import`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DbImportParams {
    pub name: String,
    #[serde(default)]
    pub instance: Option<InstanceRef>,
    /// `.sql`, `.sql.gz` or `.sql.zst` on the daemon's machine.
    pub file: String,
}

/// Params of `db.shell` (local only: the answer is a command line for the caller's
/// machine). The result is a [`super::shell::ShellCommandDto`] (API 1.1, ADR-0040; it was
/// `Ack` in 1.0, when the method was never implemented): the CLI runs it attached to the
/// console, the GUI opens it in an integrated terminal (`terminal.open {command}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DbShellParams {
    pub instance: InstanceRef,
    #[serde(default)]
    pub database: Option<String>,
}

/// Params of `db.url`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DbUrlParams {
    pub instance: InstanceRef,
    pub database: String,
}

/// Result of `db.url`: DSN plus `.env` variables (contains credentials).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DbUrlDto {
    pub url: String,
    pub env: EnvVarsDto,
}

/// Params of `db.open` (local only: launches a GUI client; M3-E1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DbOpenParams {
    pub instance: InstanceRef,
    /// Client tool (`heidisql`, `dbeaver`, `adminer`, `phpmyadmin`).
    pub with: ToolId,
    #[serde(default)]
    pub database: Option<String>,
}
