//! `shell.*`: shells and environment with the Forgebox PATH (docs/09 §shell).
//!
//! `shell.open`, `shell.open_editor` and `shell.env` depend on the caller's machine
//! (terminal or editor window, paths for the caller's shell), so they are `local_only`.
//! `forge exec` is `shell.env` with `versions` overrides, then the CLI runs the command
//! itself. The integrated terminal of the GUI is not here: its sessions live in the daemon
//! (`terminal.*`, ADR-0040).

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

/// Params of `shell.open_editor`: opens a file in the configured editor (`[editor]
/// command` with `{path}`, `{line}`, `{column}`, or an autodetected VS Code / PhpStorm /
/// Notepad) at a position. Local only; the path must resolve inside the root, a parked
/// folder or a site root (M2-E3-T9). API 1.1 (ADR-0040).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ShellOpenEditorParams {
    /// File or folder on the daemon's machine (the same machine: the method is local only).
    pub path: String,
    /// 1-based line.
    #[serde(default)]
    pub line: Option<u32>,
    /// 1-based column (used only with `line`).
    #[serde(default)]
    pub column: Option<u32>,
}

/// A program to run, with its arguments, working folder and environment additions:
/// `db.shell` (the instance's CLI client) answers one. The caller runs it attached to a
/// console (`forge db shell`) or in an integrated terminal (`terminal.open {command}`).
/// `program` is an absolute path on the daemon's machine. API 1.1 (ADR-0040).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ShellCommandDto {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Working folder; the caller's when `None`.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Variables set on top of the caller's environment (`PGPASSWORD`, `MYSQL_PWD`...).
    /// May contain credentials: never log them.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
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
