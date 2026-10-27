//! `php.*`: versions, pools, extensions, Xdebug and php.ini (docs/09 §php, docs/03 §2).

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::FileRefDto;
use super::services::PoolDto;
use forge_core::ids::VersionId;

/// Xdebug mode of a PHP version (affects its pool).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum XdebugMode {
    Off,
    On,
    Trigger,
}

/// An installed PHP version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct PhpVersionDto {
    pub version_id: VersionId,
    /// `8.3.36`.
    pub version: String,
    /// `8.3`.
    pub series: String,
    /// `nts`/`ts`.
    pub thread_safety: String,
    /// `vs17`.
    pub compiler: Option<String>,
    pub arch: String,
    pub is_current_cli: bool,
    pub pool: Option<PoolDto>,
    pub sites: u32,
    pub xdebug: XdebugMode,
}

/// Result of `php.list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct PhpVersionListDto {
    pub versions: Vec<PhpVersionDto>,
}

/// Params of `php.pool_status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PhpPoolStatusParams {
    /// Series or exact version; all pools when absent.
    #[serde(default)]
    pub version: Option<String>,
}

/// Result of `php.pool_status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct PoolListDto {
    pub pools: Vec<PoolDto>,
}

/// Params of methods acting on one PHP version (`php.ext_list`, `php.ini`...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PhpVersionParams {
    /// Series (`8.3`) or exact version (`8.3.36`).
    pub version: String,
}

/// A PHP extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PhpExtDto {
    pub name: String,
    pub enabled: bool,
    /// Ships with PHP (`ext/php_*.dll`) vs installed from the registry (`kind:
    /// Extension`).
    pub bundled: bool,
    /// Loaded with `zend_extension`.
    pub zend: bool,
}

/// Result of `php.ext_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PhpExtListDto {
    pub version: String,
    pub extensions: Vec<PhpExtDto>,
}

/// Params of `php.ext_enable`/`php.ext_disable`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PhpExtParams {
    pub version: String,
    pub extension: String,
}

/// Params of `php.xdebug`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PhpXdebugParams {
    pub version: String,
    pub mode: XdebugMode,
    /// Client port (default 9003, reserved by the allocator).
    #[serde(default)]
    pub port: Option<u16>,
}

/// Result of `php.ini`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PhpIniDto {
    pub version: String,
    /// `php.ini` and the `conf.d/*.ini` overlays.
    pub files: Vec<FileRefDto>,
}
