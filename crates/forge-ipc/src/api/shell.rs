//! `shell.*`: shells and environment with the Forgebox PATH (docs/09 §shell).
//!
//! `shell.open` and `shell.env` depend on the caller's machine (terminal window, paths
//! for the caller's shell), so they are `local_only`. `forge exec` is `shell.env` with
//! `versions` overrides, then the CLI runs the command itself.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use specta::Type;

/// Shell syntax for `shell.env`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ShellFormat {
    #[default]
    Powershell,
    Cmd,
    Bash,
    /// Only `vars`/`path_prepend`, no script.
    Json,
}

/// Params of `shell.open` (`forge shell`; opens a terminal: local only).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ShellOpenParams {
    /// Project folder (versions from `forgebox.yaml`/`.php-version`).
    #[serde(default)]
    pub dir: Option<String>,
}

/// Params of `shell.env` (`forge env`, `forge exec`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ShellEnvParams {
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub format: ShellFormat,
    /// Tool → version overrides (`{"php": "8.2"}` for `forge exec --php 8.2`).
    #[serde(default)]
    pub versions: BTreeMap<String, String>,
}

/// Result of `shell.env`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ShellEnvDto {
    /// Folders to put in front of `PATH`, in order.
    pub path_prepend: Vec<String>,
    pub vars: BTreeMap<String, String>,
    /// Script for `Invoke-Expression`/`eval` in the requested format.
    pub script: Option<String>,
}

/// Result of `shell.path_show|path_add|path_remove` (user `PATH`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct UserPathDto {
    pub entries: Vec<String>,
    /// Entries owned by Forgebox (`bin/<tool>/current`, `bin/shims`).
    pub forgebox_entries: Vec<String>,
    /// Forgebox entries that should be there and are not.
    pub missing: Vec<String>,
}
