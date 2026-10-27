//! `db.*`: databases of database instances (docs/09 §db).

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::{EnvVarsDto, InstanceRef};
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

/// Result of `db.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DatabaseListDto {
    pub databases: Vec<DatabaseDto>,
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

/// Params of `db.shell` (local only: opens a terminal).
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

/// Params of `db.open` (local only: launches a GUI client).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DbOpenParams {
    pub instance: InstanceRef,
    /// Client tool (`heidisql`, `dbeaver`, `adminer`, `phpmyadmin`).
    pub with: ToolId,
    #[serde(default)]
    pub database: Option<String>,
}
