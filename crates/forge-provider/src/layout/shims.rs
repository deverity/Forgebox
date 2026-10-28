//! `bin/shims/<name>.cmd` (docs/03 §5.2, docs/06 §2.3, ADR-0005, ADR-0025).
//!
//! A shim runs the tool's command with the version the *project* asks for, without
//! talking to the daemon:
//!
//! 1. From the current directory up to the drive root, the first directory holding one of
//!    the manifest's `install.version_files` (`.php-version`, `.nvmrc`, `.node-version`),
//!    or a `forgebox.yaml` that names a version for the tool (`install.project_key` such
//!    as `spec.site.php`, or a `spec.runtimes[]` item with `tool: <id>`), wins. Version
//!    files are read by `cmd.exe` itself (first word of the first non-comment line);
//!    `forgebox.yaml` is read by the generated `forgebox-project.ps1` (Windows PowerShell,
//!    only started when such a file is found).
//! 2. The selection (`8.3`, `8.3.37`, `v22`, a folder name) is looked up in a table
//!    written into the shim when it is generated: every alias of every installed version
//!    (folder name, version, `major`, `major.minor`, with or without a leading `v`) maps
//!    to the preferred build ([`forge_manifest::version::cmp_builds`]: newest, then variant
//!    preference). A selection that is not installed is an error (exit 1), never a silent
//!    fallback.
//! 3. Without a selection, `bin/<tool>/current` (or `bin/<tool>` for `layout: single`).
//!
//! The command itself is `<name>.exe|.com|.cmd|.bat` found in the version folder or its
//! `install.path` subfolders when the shim is generated, or the shim's explicit `file`;
//! with `interpreter`, the shim calls `bin/shims/<interpreter>.cmd <file> <args>` (the
//! interpreter then applies its own project version: `composer` → `php`).
//!
//! Shims are regenerated, idempotently (a file is only written when its content
//! changes), whenever versions of the tool are installed; shims of the tool that the
//! manifest no longer declares are removed. Every path inside a shim is relative to the
//! shim (`%~dp0..\php\8.3.37-nts-vs17-x64`), so a moved or portable root keeps working.
//!
//! Safety: values read from project files never reach the command parser: they are only
//! expanded with delayed expansion inside comparisons and messages. Folder names that
//! `cmd.exe` would interpret (`%`, `!`, `"`, `^`, `&`, `|`, `<`, `>`) are left out of the
//! table with a warning. The `.cmd` files use CRLF (batch labels and `goto` misbehave
//! with LF-only files) and ASCII only (the console code page is not UTF-8); this is the
//! one exception to the LF rule of generated files (docs/15 §7).
//!
//! Environment (M1 gate): `FORGEBOX_<TOOL>_VERSION` (`FORGEBOX_PHP_VERSION=8.3`), when
//! set, selects the version before any project file (what `forge exec --php 8.3` sets,
//! so `composer` → `php.cmd` runs the same PHP). Before running a version the shim calls
//! its `forgebox-env.cmd` ([`ENV_SCRIPT`]), written in each version folder from the
//! manifest's `config.env` (`PHPRC`, `PHP_INI_SCAN_DIR`): without it `php` would start
//! without the Forgebox `php.ini` and its extensions. The script finds the root from its
//! own location (`%~dp0..\..\..`), so it also works through the `current` junction and in
//! a moved portable root.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use forge_core::error::{IoResultExt, Result};
use forge_core::paths::{ForgeRoot, write_atomic};
use forge_core::store::{ToolVersionRow, VersionState};
use forge_manifest::schema::ToolManifest;
use forge_manifest::schema::tool::{Layout, Shim};
use forge_manifest::version::{Build, Version, cmp_builds};
use serde::{Deserialize, Serialize};

use super::vars::TemplateVars;
use crate::messages;

/// Line every generated shim carries, followed by the tool id.
pub const SHIM_MARKER: &str = "rem forgebox-shim tool=";
/// Per-version environment script called by the shims (`config.env` of the manifest).
pub const ENV_SCRIPT: &str = "forgebox-env.cmd";
/// First line after `@echo off` of every [`ENV_SCRIPT`].
const ENV_MARKER: &str = "rem forgebox-env tool=";

/// Environment variable that selects the version of `tool` for the shims and
/// `forge exec` (`FORGEBOX_PHP_VERSION`, `FORGEBOX_NODEJS_VERSION`).
pub fn version_override_var(tool: &str) -> String {
    format!(
        "FORGEBOX_{}_VERSION",
        tool.to_ascii_uppercase().replace(['-', '.'], "_")
    )
}
/// The `forgebox.yaml` reader shared by the shims, in `bin/shims/`.
pub const PROJECT_HELPER: &str = "forgebox-project.ps1";
/// Extensions tried, in order, for a shim without `file`.
pub const SHIM_EXTENSIONS: [&str; 4] = ["exe", "com", "cmd", "bat"];

/// An installed version a shim can select.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimVersion {
    pub version: String,
    pub canonical_name: String,
    pub variant: BTreeMap<String, String>,
    /// Absolute folder.
    pub path: PathBuf,
}

impl ShimVersion {
    /// Installed rows of the store (other states are skipped).
    pub fn from_rows(rows: &[ToolVersionRow]) -> Vec<Self> {
        rows.iter()
            .filter(|r| r.state == VersionState::Installed)
            .map(|r| Self {
                version: r.version.clone(),
                canonical_name: r.canonical_name.clone(),
                variant: r.variant.clone(),
                path: PathBuf::from(&r.path),
            })
            .collect()
    }
}

/// What [`ensure`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShimReport {
    /// Shim names written (new or changed).
    pub written: Vec<String>,
    /// Shim names already up to date.
    pub unchanged: Vec<String>,
    /// Stale shims of the tool that were deleted.
    pub removed: Vec<String>,
    /// Shims or versions left out, with the reason.
    pub warnings: Vec<String>,
}

/// Characters `cmd.exe` would interpret inside the generated script.
fn is_batch_safe(text: &str) -> bool {
    !text.is_empty()
        && !text
            .chars()
            .any(|c| c.is_control() || matches!(c, '%' | '!' | '"' | '^' | '&' | '|' | '<' | '>'))
}

/// Path of `dir` relative to `bin/shims/` (`..\php\8.3.37`), when it is under `bin/`.
fn relative_to_shims(root: &ForgeRoot, dir: &Path) -> Option<String> {
    let rest = dir.strip_prefix(root.bin()).ok()?;
    let mut out = String::from("..");
    for part in rest.components() {
        match part {
            std::path::Component::Normal(name) => {
                out.push('\\');
                out.push_str(name.to_str()?);
            }
            _ => return None,
        }
    }
    (out.len() > 2).then_some(out)
}

/// Aliases (lowercase) → index of the preferred version (module docs, step 2).
pub(crate) fn alias_table(
    manifest: &ToolManifest,
    versions: &[ShimVersion],
) -> BTreeMap<String, usize> {
    let scheme = manifest.spec.versioning.scheme;
    let variants = &manifest.spec.versioning.variants;
    let parsed: Vec<Option<Version>> = versions
        .iter()
        .map(|v| Version::parse(scheme, &v.version).ok())
        .collect();
    let mut candidates: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, v) in versions.iter().enumerate() {
        let mut aliases = BTreeSet::new();
        aliases.insert(v.canonical_name.to_lowercase());
        aliases.insert(v.version.to_lowercase());
        let base = v
            .version
            .split('+')
            .next()
            .unwrap_or(&v.version)
            .to_lowercase();
        aliases.insert(base.clone());
        if parsed[index].is_some() {
            let mut prefix = String::new();
            for (i, part) in base.split('.').enumerate() {
                if i > 0 {
                    prefix.push('.');
                }
                prefix.push_str(part);
                aliases.insert(prefix.clone());
            }
        }
        for alias in aliases {
            if alias.starts_with(|c: char| c.is_ascii_digit()) {
                candidates
                    .entry(format!("v{alias}"))
                    .or_default()
                    .push(index);
            }
            candidates.entry(alias).or_default().push(index);
        }
    }
    candidates
        .into_iter()
        .map(|(alias, mut indices)| {
            indices.sort_by(|&a, &b| match (&parsed[a], &parsed[b]) {
                (Some(va), Some(vb)) => cmp_builds(
                    &Build {
                        version: va,
                        variants: &versions[a].variant,
                    },
                    &Build {
                        version: vb,
                        variants: &versions[b].variant,
                    },
                    variants,
                ),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => versions[a].canonical_name.cmp(&versions[b].canonical_name),
            });
            (alias, indices[0])
        })
        .collect()
}

/// How the shim starts its target.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Launch {
    /// An executable (`.exe`, `.com`, anything else).
    Direct,
    /// A batch file (`.cmd`, `.bat`): `call`.
    Call,
    /// Through another shim (`bin/shims/<interpreter>.cmd <file>`).
    Interpreter(String),
}

/// Everything rendered into one shim.
#[derive(Debug, Clone)]
struct ShimScript<'a> {
    tool: &'a str,
    name: &'a str,
    /// Relative to the version folder, `\` separators.
    file: String,
    launch: Launch,
    version_files: &'a [String],
    project_key: &'a str,
    /// Alias → folder relative to `bin/shims`.
    table: Vec<(String, String)>,
    /// `..\<tool>\current` (or `..\<tool>` for `layout: single`).
    fallback: String,
    /// Folder names, for the "not installed" message.
    installed: String,
}

/// The `.cmd` text (CRLF, ASCII).
fn render_cmd(s: &ShimScript<'_>) -> String {
    let mut l: Vec<String> = Vec::new();
    let tool = s.tool;
    l.push("@echo off".into());
    l.push(format!(
        "rem Forgebox shim \"{}\" of {tool} (docs/06 2.3). Generated by Forgebox: do not edit, it is",
        s.name
    ));
    l.push(format!(
        "rem rewritten whenever versions of {tool} are installed. Project files: {} forgebox.yaml.",
        s.version_files.join(" ")
    ));
    l.push(format!("{SHIM_MARKER}{tool}"));
    l.push("setlocal DisableDelayedExpansion".into());
    l.push("set \"FB_SHIMS=%~dp0\"".into());
    l.push("set \"FB_SEL=\"".into());
    l.push("set \"FB_SRC=\"".into());
    l.push("set \"FB_D=%CD%\"".into());
    let override_var = version_override_var(tool);
    l.push(format!("if defined {override_var} goto fb_env"));
    l.push(":fb_walk".into());
    for file in s.version_files {
        l.push(format!("set \"FB_SRC=%FB_D%\\{file}\""));
        l.push("if exist \"%FB_SRC%\" if not exist \"%FB_SRC%\\\" call :fb_read".into());
        l.push("if defined FB_SEL goto fb_select".into());
    }
    l.push("set \"FB_SRC=%FB_D%\\forgebox.yaml\"".into());
    l.push("if exist \"%FB_SRC%\" if not exist \"%FB_SRC%\\\" call :fb_project".into());
    l.push("if defined FB_SEL goto fb_select".into());
    l.push("for %%P in (\"%FB_D%\\..\") do set \"FB_UP=%%~fP\"".into());
    l.push("if \"%FB_UP%\"==\"%FB_D%\" goto fb_current".into());
    l.push("set \"FB_D=%FB_UP%\"".into());
    l.push("goto fb_walk".into());
    l.push(String::new());
    // The override is only expanded with delayed expansion (like project files).
    l.push(":fb_env".into());
    l.push("setlocal EnableDelayedExpansion".into());
    l.push(format!(
        "for /f \"delims=\" %%V in (\"!{override_var}!\") do endlocal & set \"FB_SEL=%%V\""
    ));
    l.push(format!("set \"FB_SRC={override_var}\""));
    l.push("goto fb_select".into());
    l.push(String::new());
    l.push(":fb_current".into());
    l.push(format!("set \"FB_DIR={}\"", s.fallback));
    l.push(format!(
        "if exist \"%FB_SHIMS%{}\\\" goto fb_run",
        s.fallback
    ));
    l.push(format!(">&2 echo {}", messages::shim_no_current(tool)));
    l.push("exit /b 1".into());
    l.push(String::new());
    l.push(":fb_select".into());
    l.push("setlocal EnableDelayedExpansion".into());
    l.push("set \"FB_DIR=\"".into());
    for (alias, dir) in &s.table {
        l.push(format!(
            "if not defined FB_DIR if /i \"!FB_SEL!\"==\"{alias}\" set \"FB_DIR={dir}\""
        ));
    }
    l.push("if not defined FB_DIR goto fb_unknown".into());
    l.push("for /f \"delims=\" %%D in (\"!FB_DIR!\") do endlocal & set \"FB_DIR=%%D\"".into());
    l.push("goto fb_run".into());
    l.push(String::new());
    l.push(":fb_unknown".into());
    for line in messages::shim_not_installed(tool, &s.installed) {
        l.push(format!(">&2 echo {line}"));
    }
    l.push("exit /b 1".into());
    l.push(String::new());
    l.push(":fb_run".into());
    l.push(format!("set \"FB_EXE=%FB_SHIMS%%FB_DIR%\\{}\"", s.file));
    l.push("if not exist \"%FB_EXE%\" goto fb_missing".into());
    l.push("for %%E in (\"%FB_EXE%\") do set \"FB_EXE=%%~fE\"".into());
    l.push(format!(
        "if exist \"%FB_SHIMS%%FB_DIR%\\{ENV_SCRIPT}\" call \"%FB_SHIMS%%FB_DIR%\\{ENV_SCRIPT}\""
    ));
    match &s.launch {
        Launch::Direct => l.push("\"%FB_EXE%\" %*".into()),
        Launch::Call => l.push("call \"%FB_EXE%\" %*".into()),
        Launch::Interpreter(shim) => {
            l.push(format!("call \"%FB_SHIMS%{shim}.cmd\" \"%FB_EXE%\" %*"));
        }
    }
    l.push("exit /b %ERRORLEVEL%".into());
    l.push(String::new());
    l.push(":fb_missing".into());
    l.push(format!(">&2 echo {}", messages::shim_missing_file()));
    l.push("exit /b 1".into());
    l.push(String::new());
    l.push(":fb_read".into());
    l.push("for /f \"usebackq eol=# tokens=1\" %%L in (\"%FB_SRC%\") do (".into());
    l.push("  set \"FB_SEL=%%L\"".into());
    l.push("  goto :eof".into());
    l.push(")".into());
    l.push("goto :eof".into());
    l.push(String::new());
    l.push(":fb_project".into());
    l.push(format!(
        "for /f \"usebackq delims=\" %%V in (`%SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe \
         -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"%FB_SHIMS%{PROJECT_HELPER}\" \
         \"%FB_SRC%\" \"{tool}\" \"{}\"`) do set \"FB_SEL=%%V\"",
        s.project_key
    ));
    l.push("goto :eof".into());
    let mut text = l.join("\r\n");
    text.push_str("\r\n");
    text
}

/// Reader of `forgebox.yaml` (kind: Project) used by the shims: prints the version the
/// project selects for a tool. A small indentation-based reader for the shapes of
/// docs/04 §4 (block mappings, `spec.runtimes` as flow `{ tool, version }` items or block
/// items); anything it does not understand selects nothing. ASCII (PowerShell 5.1 reads
/// BOM-less scripts in the ANSI code page).
pub const PROJECT_HELPER_SCRIPT: &str = r#"# Forgebox: prints the version that a project forgebox.yaml (kind: Project) selects for a
# tool. Generated by Forgebox in bin/shims; do not edit. Used by the .cmd shims (docs/06 2.3).
# Usage: forgebox-project.ps1 <forgebox.yaml> <tool id> [<dotted key such as spec.site.php>]
param([string]$File, [string]$Tool, [string]$Key)
$ErrorActionPreference = 'Stop'
try { $lines = [System.IO.File]::ReadAllLines($File) } catch { exit 0 }

function Get-Scalar([string]$text) {
    $t = $text.Trim()
    if ($t.Length -ge 2 -and ($t[0] -eq '"' -or $t[0] -eq "'")) {
        $end = $t.IndexOf($t[0], 1)
        if ($end -gt 0) { return $t.Substring(1, $end - 1) }
    }
    $hash = $t.IndexOf(' #')
    if ($hash -ge 0) { $t = $t.Substring(0, $hash) }
    return $t.Trim()
}

function Get-Flow([string]$text) {
    $map = @{}
    $t = $text.Trim()
    if (-not ($t.StartsWith('{') -and $t.EndsWith('}'))) { return $map }
    foreach ($pair in $t.Substring(1, $t.Length - 2).Split(',')) {
        $i = $pair.IndexOf(':')
        if ($i -gt 0) { $map[$pair.Substring(0, $i).Trim()] = Get-Scalar $pair.Substring($i + 1) }
    }
    return $map
}

$script:fromRuntimes = $null
function Use-Runtime($item) {
    if ($null -eq $script:fromRuntimes -and $item['tool'] -eq $Tool) {
        if ($item['version']) { $script:fromRuntimes = $item['version'] }
        elseif ($item['series']) { $script:fromRuntimes = $item['series'] }
    }
}

$stack = New-Object System.Collections.Generic.List[object]
$fromKey = $null
$runtime = $null
$runtimeIndent = -1
foreach ($raw in $lines) {
    $text = $raw.Trim()
    if ($text.Length -eq 0 -or $text.StartsWith('#') -or $text -eq '---') { continue }
    $indent = $raw.Length - $raw.TrimStart().Length
    $isItem = $text -eq '-' -or $text.StartsWith('- ')
    if ($null -ne $runtime -and $indent -le $runtimeIndent) { Use-Runtime $runtime; $runtime = $null }
    while ($stack.Count -gt 0 -and ($stack[$stack.Count - 1].Indent -gt $indent -or
            (-not $isItem -and $stack[$stack.Count - 1].Indent -eq $indent))) {
        $stack.RemoveAt($stack.Count - 1)
    }
    $parent = ($stack | ForEach-Object { $_.Name }) -join '.'
    if ($isItem) {
        $item = $text.Substring(1).Trim()
        if ($parent -eq 'spec.runtimes') {
            if ($item.StartsWith('{')) { Use-Runtime (Get-Flow $item) }
            else {
                $runtime = @{}
                $runtimeIndent = $indent
                if ($item -match '^([A-Za-z0-9_]+):\s*(.*)$') { $runtime[$Matches[1]] = Get-Scalar $Matches[2] }
            }
        }
        continue
    }
    if ($text -notmatch '^([A-Za-z0-9_]+):\s*(.*)$') { continue }
    $name = $Matches[1]
    $value = $Matches[2].Trim()
    if ($null -ne $runtime) { $runtime[$name] = Get-Scalar $value; continue }
    $path = if ($parent) { "$parent.$name" } else { $name }
    if ($value.Length -eq 0 -or $value.StartsWith('#')) {
        $stack.Add([pscustomobject]@{ Indent = $indent; Name = $name })
    } elseif ($value.StartsWith('{')) {
        $flow = Get-Flow $value
        foreach ($k in $flow.Keys) { if ($Key -and "$path.$k" -eq $Key) { $fromKey = $flow[$k] } }
    } elseif ($Key -and $path -eq $Key) {
        $fromKey = Get-Scalar $value
    }
}
if ($null -ne $runtime) { Use-Runtime $runtime }
if ($fromKey) { Write-Output $fromKey } elseif ($script:fromRuntimes) { Write-Output $script:fromRuntimes }
"#;

/// The project helper with CRLF line endings, as written to disk.
fn project_helper_text() -> String {
    PROJECT_HELPER_SCRIPT
        .replace("\r\n", "\n")
        .replace('\n', "\r\n")
}

/// Writes `text` to `path` unless it already holds exactly that. Returns whether it wrote.
fn write_if_changed(path: &Path, text: &str) -> Result<bool> {
    match std::fs::read(path) {
        Ok(existing) if existing == text.as_bytes() => Ok(false),
        _ => {
            write_atomic(path, text.as_bytes())?;
            Ok(true)
        }
    }
}

/// Tool id in the marker line of an existing shim.
fn owner_of(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(SHIM_MARKER))
        .map(|tool| tool.trim().to_owned())
}

/// Finds the shim's target in the version folders `dirs` (first match wins).
fn resolve_file(manifest: &ToolManifest, shim: &Shim, dirs: &[PathBuf]) -> Option<String> {
    if let Some(file) = shim.file() {
        let rel = file.replace('/', "\\");
        return dirs.iter().any(|d| d.join(&rel).is_file()).then_some(rel);
    }
    let subs: Vec<&str> = if manifest.spec.install.path.is_empty() {
        vec!["."]
    } else {
        manifest
            .spec
            .install
            .path
            .iter()
            .map(String::as_str)
            .collect()
    };
    for dir in dirs {
        for sub in &subs {
            let sub = sub.trim_matches('/');
            for ext in SHIM_EXTENSIONS {
                let file = format!("{}.{ext}", shim.name());
                let rel = if sub.is_empty() || sub == "." {
                    file
                } else {
                    format!("{}\\{file}", sub.replace('/', "\\"))
                };
                if dir.join(&rel).is_file() {
                    return Some(rel);
                }
            }
        }
    }
    None
}

/// The [`ENV_SCRIPT`] of version `v` (CRLF, ASCII): `config.env` substituted for that
/// version, values under the root written relative to the script (`%~dp0..\..\..\etc\...`).
/// `None` when the manifest declares no environment or `v` is not under `bin/`. Values
/// `cmd.exe` would interpret are left out with a warning.
fn render_env_script(
    root: &ForgeRoot,
    manifest: &ToolManifest,
    v: &ShimVersion,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let env = &manifest.spec.config.env;
    if env.is_empty() || !v.path.starts_with(root.bin()) {
        return None;
    }
    let tool = manifest.metadata.id.as_str();
    let depth = v.path.strip_prefix(root.path()).ok()?.components().count();
    let up = format!("%~dp0{}", "..\\".repeat(depth));
    let root_posix = root.to_posix_string();
    let vars = TemplateVars::of_version(
        root,
        manifest,
        &v.version,
        &v.canonical_name,
        &v.variant,
        &v.path,
    );
    let mut l: Vec<String> = vec![
        "@echo off".into(),
        format!("{ENV_MARKER}{tool}"),
        format!(
            "rem Environment of {tool} {} (config.env), set by the Forgebox shims before they run it.",
            v.canonical_name
        ),
        "rem Generated by Forgebox: do not edit, it is rewritten with the shims.".into(),
    ];
    for (name, value) in env {
        let value = match vars.substitute(value) {
            Ok(value) => value,
            Err(err) => {
                warnings.push(format!(
                    "{tool}: config.env {name} left out of {ENV_SCRIPT}: {err}"
                ));
                continue;
            }
        };
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || !is_batch_safe(&value)
            || !value.is_ascii()
        {
            warnings.push(format!(
                "{tool}: config.env {name} left out of {ENV_SCRIPT} (characters cmd.exe interprets)"
            ));
            continue;
        }
        let value = match value.strip_prefix(&root_posix) {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                let rest = rest.trim_start_matches('/').replace('/', "\\");
                format!("{up}{rest}").trim_end_matches('\\').to_owned()
            }
            _ => value,
        };
        l.push(format!("set \"{name}={value}\""));
    }
    let mut text = l.join("\r\n");
    text.push_str("\r\n");
    Some(text)
}

/// Generates (or refreshes) the shims of `manifest` for its installed `versions` and
/// removes the tool's stale ones (module docs). Blocking.
pub fn ensure(
    root: &ForgeRoot,
    manifest: &ToolManifest,
    versions: &[ShimVersion],
) -> Result<ShimReport> {
    let tool = manifest.metadata.id.as_str();
    let install = &manifest.spec.install;
    let shims_dir = root.bin_shims();
    std::fs::create_dir_all(&shims_dir).at("creating", &shims_dir)?;
    let mut report = ShimReport::default();

    let mut usable: Vec<(ShimVersion, String)> = Vec::new();
    for v in versions {
        match relative_to_shims(root, &v.path) {
            Some(rel) if is_batch_safe(&rel) && is_batch_safe(&v.canonical_name) => {
                usable.push((v.clone(), rel));
            }
            _ => report.warnings.push(format!(
                "{tool} {}: folder {} cannot be selected by the shims (outside bin/ or with \
                 characters cmd.exe interprets)",
                v.canonical_name,
                v.path.display()
            )),
        }
    }

    // The environment script of each version the shims can run.
    if !install.shims.is_empty() {
        for (v, _) in &usable {
            let path = v.path.join(ENV_SCRIPT);
            match render_env_script(root, manifest, v, &mut report.warnings) {
                Some(text) => {
                    write_if_changed(&path, &text)?;
                }
                None => {
                    let ours = std::fs::read_to_string(&path)
                        .is_ok_and(|t| t.lines().any(|l| l.starts_with(ENV_MARKER)));
                    if ours {
                        std::fs::remove_file(&path).at("removing", &path)?;
                    }
                }
            }
        }
    }

    let mut declared = BTreeSet::new();
    if !usable.is_empty() && !install.shims.is_empty() {
        write_if_changed(&shims_dir.join(PROJECT_HELPER), &project_helper_text())?;
        let only: Vec<ShimVersion> = usable.iter().map(|(v, _)| v.clone()).collect();
        let table: Vec<(String, String)> = alias_table(manifest, &only)
            .into_iter()
            .filter(|(alias, _)| is_batch_safe(alias))
            .map(|(alias, index)| (alias, usable[index].1.clone()))
            .collect();
        let fallback = match install.layout {
            Layout::Versioned => format!("..\\{tool}\\current"),
            Layout::Single => format!("..\\{tool}"),
        };
        let mut dirs = vec![root.tool_current_link(&manifest.metadata.id)];
        dirs.extend(usable.iter().map(|(v, _)| v.path.clone()));
        let installed = usable
            .iter()
            .map(|(v, _)| v.canonical_name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let project_key = install.project_key.as_deref().unwrap_or("");
        for shim in &install.shims {
            let name = shim.name();
            let path = shims_dir.join(format!("{name}.cmd"));
            if let Some(owner) = owner_of(&path).filter(|o| o != tool) {
                report.warnings.push(format!(
                    "shim {name} belongs to {owner}; not replaced by {tool}"
                ));
                continue;
            }
            if path.exists() && owner_of(&path).is_none() {
                report.warnings.push(format!(
                    "{} was not written by Forgebox; left alone",
                    path.display()
                ));
                continue;
            }
            let Some(file) = resolve_file(manifest, shim, &dirs) else {
                report.warnings.push(format!(
                    "shim {name}: no {name}.exe/.com/.cmd/.bat (or `file`) in the installed \
                     versions of {tool}"
                ));
                continue;
            };
            let launch = match shim.interpreter() {
                Some(interpreter) => Launch::Interpreter(interpreter.to_owned()),
                None if file.to_ascii_lowercase().ends_with(".cmd")
                    || file.to_ascii_lowercase().ends_with(".bat") =>
                {
                    Launch::Call
                }
                None => Launch::Direct,
            };
            if !is_batch_safe(&file) {
                report
                    .warnings
                    .push(format!("shim {name}: file {file:?} has unsafe characters"));
                continue;
            }
            let text = render_cmd(&ShimScript {
                tool,
                name,
                file,
                launch,
                version_files: &install.version_files,
                project_key,
                table: table.clone(),
                fallback: fallback.clone(),
                installed: installed.clone(),
            });
            declared.insert(name.to_owned());
            if write_if_changed(&path, &text)? {
                report.written.push(name.to_owned());
            } else {
                report.unchanged.push(name.to_owned());
            }
        }
    }

    // Shims of this tool that are no longer generated.
    for entry in std::fs::read_dir(&shims_dir).at("listing", &shims_dir)? {
        let entry = entry.at("listing", &shims_dir)?;
        let path = entry.path();
        let Some(stem) = path
            .file_stem()
            .filter(|_| {
                path.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("cmd"))
            })
            .and_then(|s| s.to_str())
        else {
            continue;
        };
        if declared.contains(stem) || owner_of(&path).as_deref() != Some(tool) {
            continue;
        }
        std::fs::remove_file(&path).at("removing", &path)?;
        report.removed.push(stem.to_owned());
    }
    tracing::debug!(tool, ?report, "shims ensured");
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_safety() {
        assert!(is_batch_safe(r"..\php\8.3.37-nts-vs17-x64"));
        assert!(is_batch_safe("php-8.3 (old)"));
        for bad in [
            "a%b", "a!b", "a\"b", "a^b", "a&b", "a|b", "a<b", "a>b", "", "a\nb",
        ] {
            assert!(!is_batch_safe(bad), "{bad:?}");
        }
    }

    #[test]
    fn relative_paths_from_the_shims_folder() {
        let root = ForgeRoot::new(r"C:\fb").unwrap();
        assert_eq!(
            relative_to_shims(&root, Path::new("C:/fb/bin/php/8.3.37")).as_deref(),
            Some(r"..\php\8.3.37")
        );
        assert_eq!(
            relative_to_shims(&root, Path::new(r"C:\fb\bin\php-8.1-Win32")).as_deref(),
            Some(r"..\php-8.1-Win32")
        );
        assert_eq!(relative_to_shims(&root, Path::new(r"D:\other\php")), None);
        assert_eq!(relative_to_shims(&root, &root.bin()), None);
    }

    #[test]
    fn rendered_shims_are_crlf_ascii_and_data_driven() {
        let files = vec![".nvmrc".to_owned(), ".node-version".to_owned()];
        let script = |launch: Launch| {
            render_cmd(&ShimScript {
                tool: "nodejs",
                name: "npm",
                file: "npm.cmd".into(),
                launch,
                version_files: &files,
                project_key: "",
                table: vec![("22".into(), r"..\nodejs\22.12.0".into())],
                fallback: r"..\nodejs\current".into(),
                installed: "22.12.0".into(),
            })
        };
        let text = script(Launch::Call);
        assert!(text.is_ascii());
        assert!(text.ends_with("\r\n") && !text.replace("\r\n", "").contains('\n'));
        assert!(text.contains(&format!("{SHIM_MARKER}nodejs\r\n")));
        for file in &files {
            assert!(
                text.contains(&format!(r#"set "FB_SRC=%FB_D%\{file}""#)),
                "{file}"
            );
        }
        assert!(text.contains(
            r#"if not defined FB_DIR if /i "!FB_SEL!"=="22" set "FB_DIR=..\nodejs\22.12.0""#
        ));
        assert!(text.contains(r#"set "FB_DIR=..\nodejs\current""#));
        assert!(text.contains("\r\ncall \"%FB_EXE%\" %*\r\n"));
        assert!(
            text.contains(r#""nodejs" ""`)"#),
            "empty project key is passed"
        );
        let direct = script(Launch::Direct);
        assert!(direct.contains("\r\n\"%FB_EXE%\" %*\r\n"));
        let interpreted = script(Launch::Interpreter("php".into()));
        assert!(interpreted.contains(r#"call "%FB_SHIMS%php.cmd" "%FB_EXE%" %*"#));
        // Values read from project files are only expanded with delayed expansion.
        assert!(!text.contains("%FB_SEL%"));
        // The version override and the per-version environment.
        assert_eq!(version_override_var("nodejs"), "FORGEBOX_NODEJS_VERSION");
        assert_eq!(version_override_var("my-tool"), "FORGEBOX_MY_TOOL_VERSION");
        assert!(text.contains("if defined FORGEBOX_NODEJS_VERSION goto fb_env\r\n"));
        assert!(text.contains(r#"("!FORGEBOX_NODEJS_VERSION!")"#));
        assert!(!text.contains("%FORGEBOX_NODEJS_VERSION%"));
        assert!(text.contains(
            r#"if exist "%FB_SHIMS%%FB_DIR%\forgebox-env.cmd" call "%FB_SHIMS%%FB_DIR%\forgebox-env.cmd""#
        ));
    }

    #[test]
    fn helper_script_is_ascii() {
        assert!(PROJECT_HELPER_SCRIPT.is_ascii());
        assert!(project_helper_text().contains("\r\n"));
    }
}
