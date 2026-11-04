//! `shell.*`: the user `PATH` entries of Forgebox (`forge path show|add|remove`) and the
//! environment of a project (`forge env`, `forge exec`; [`forgebox_env`] is also the
//! environment of the integrated terminal, `terminal.open`). `shell.open` and
//! `shell.open_editor` (M2-E3-T9) start the user's external terminal and editor through the
//! platform's [`ShellOpener`]: detached, so they outlive the daemon.
//!
//! [`ShellOpener`]: forge_core::platform::ShellOpener

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::paths::plain_path;
use forge_core::platform::{
    EditorTarget, LaunchSpec, ProgramLookup, normalize_path_entry, resolve_editor, resolve_terminal,
};
use forge_ipc::api::methods::{
    shell_env, shell_open, shell_open_editor, shell_path_add, shell_path_remove, shell_path_show,
};
use forge_ipc::api::{
    Ack, NoParams, ShellEnvDto, ShellEnvParams, ShellFormat, ShellOpenEditorParams,
    ShellOpenParams, UserPathDto,
};
use forge_ipc::server::{Ctx, Router};
use forge_manifest::schema::tool::ToolManifest;

use super::route;
use crate::catalog;
use crate::daemon::Daemon;

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, shell_path_show, path_show);
    route!(router, daemon, shell_path_add, path_add);
    route!(router, daemon, shell_path_remove, path_remove);
    route!(router, daemon, shell_env, env);
    route!(router, daemon, shell_open, open);
    route!(router, daemon, shell_open_editor, open_editor);
    router
        .timeout::<shell_open>(LAUNCH_BUDGET)
        .timeout::<shell_open_editor>(LAUNCH_BUDGET);
}

/// Answer deadline of the launchers (environment, allowed locations, program lookup, start).
const LAUNCH_BUDGET: Duration = Duration::from_secs(10);
/// Budget of the blocking part of a launcher.
const BLOCKING_BUDGET: Duration = Duration::from_secs(5);

/// Manifests of the installed tools.
async fn installed_manifests(daemon: &Daemon) -> Result<Vec<Arc<ToolManifest>>> {
    let installed: std::collections::BTreeSet<_> = catalog::all_installed(&daemon.reader)
        .await?
        .into_iter()
        .map(|r| r.tool_id)
        .collect();
    Ok(catalog::tools(&daemon.registry)
        .into_iter()
        .filter(|t| installed.contains(&t.manifest.metadata.id))
        .map(|t| t.manifest)
        .collect())
}

pub(super) async fn snapshot(daemon: &Daemon) -> Result<UserPathDto> {
    let manifests = installed_manifests(daemon).await?;
    let config = daemon.config();
    let refs: Vec<&ToolManifest> = manifests.iter().map(AsRef::as_ref).collect();
    let wanted = forge_provider::layout::path::wanted_entries(&daemon.root, &refs, &config.path);
    let env = Arc::clone(daemon.platform.user_env());
    let entries = forge_core::runtime::spawn_blocking_timeout(
        "reading the user PATH",
        Duration::from_secs(5),
        move || env.path_entries(),
    )
    .await?;
    let bin = normalize_path_entry(&daemon.root.bin().to_string_lossy(), &|_| None);
    let forgebox_entries: Vec<String> = entries
        .iter()
        .filter(|e| normalize_path_entry(e, &|_| None).starts_with(&bin))
        .cloned()
        .collect();
    let present: Vec<String> = entries
        .iter()
        .map(|e| normalize_path_entry(e, &|_| None))
        .collect();
    let missing = wanted
        .into_iter()
        .filter(|w| !present.contains(&normalize_path_entry(w, &|_| None)))
        .collect();
    Ok(UserPathDto {
        entries,
        forgebox_entries,
        missing,
    })
}

async fn path_show(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<UserPathDto> {
    snapshot(&daemon).await
}

async fn path_add(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<UserPathDto> {
    if daemon.root.is_portable() {
        return Err(ForgeError::Conflict(
            "portable mode never changes the user PATH (docs/08 §2.2)".into(),
        ));
    }
    let manifests = installed_manifests(&daemon).await?;
    daemon
        .layout
        .ensure_path(manifests, daemon.config().path.clone())
        .await?;
    snapshot(&daemon).await
}

async fn path_remove(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<UserPathDto> {
    daemon.layout.remove_path_entries().await?;
    snapshot(&daemon).await
}

fn quote_ps(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The Forgebox part of a child's environment: folders for the front of `PATH` and
/// variables to set (see [`forgebox_env`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ForgeboxEnv {
    pub path_prepend: Vec<String>,
    pub vars: BTreeMap<String, String>,
}

/// Environment for a project, shared by `shell.env` (`forge env`, `forge exec`) and the
/// integrated terminal (`terminal.open`): each installed tool with a `current` link at
/// the version the project selects (`install.version_files` up from `dir`), an override
/// in `versions`, or `current`. `PATH` gets the version folders first and then
/// `bin/shims` (commands that only exist as shims, such as `composer`); `vars` carries
/// the tool's `config.env` for that version (`PHPRC`, `PHP_INI_SCAN_DIR`), so a CLI run
/// sees the same configuration as the services.
pub(crate) async fn forgebox_env(
    daemon: &Daemon,
    dir: Option<&str>,
    versions: &BTreeMap<String, String>,
) -> Result<ForgeboxEnv> {
    let manifests = installed_manifests(daemon).await?;
    let mut path_prepend = Vec::new();
    let mut vars = BTreeMap::new();
    for manifest in manifests {
        let install = &manifest.spec.install;
        if !install.current_link {
            continue;
        }
        let id = manifest.metadata.id.clone();
        let rows = catalog::installed(&daemon.reader, &id, Some(&manifest)).await?;
        let explicit = versions.get(id.as_str()).cloned();
        let mut wanted = explicit.clone();
        if wanted.is_none() {
            if let Some(dir) = dir.map(str::to_owned) {
                let files = install.version_files.clone();
                wanted = tokio::task::spawn_blocking(move || {
                    let mut current = Some(PathBuf::from(dir));
                    while let Some(d) = current {
                        for name in &files {
                            if let Ok(text) = std::fs::read_to_string(d.join(name)) {
                                let v = text.trim().trim_start_matches('v').to_owned();
                                if !v.is_empty() {
                                    return Some(v);
                                }
                            }
                        }
                        current = d.parent().map(PathBuf::from);
                    }
                    None
                })
                .await
                .ok()
                .flatten();
            }
        }
        let picked = wanted
            .as_deref()
            .and_then(|w| catalog::pick(&manifest, &rows, w));
        let (base, row) = match picked {
            Some(row) => {
                if explicit.is_some() {
                    // `forge exec --php 8.3`: the shims (`composer` → `php.cmd`) run the
                    // same version as the PATH.
                    vars.insert(
                        forge_provider::layout::shims::version_override_var(id.as_str()),
                        row.version.clone(),
                    );
                }
                (PathBuf::from(&row.path), Some(row.clone()))
            }
            None => {
                let current = daemon.layout.current_version(&id).await.ok().flatten();
                let row = current.and_then(|c| {
                    rows.iter()
                        .find(|r| r.canonical_name == c.canonical_name)
                        .cloned()
                });
                (daemon.root.tool_current_link(&id), row)
            }
        };
        for sub in &install.path {
            let sub = sub.trim_matches('/');
            let dir = if sub.is_empty() || sub == "." {
                base.clone()
            } else {
                base.join(sub.replace('/', "\\"))
            };
            path_prepend.push(dir.to_string_lossy().replace('/', "\\"));
        }
        if let Some(row) = row {
            let template = daemon.layout.vars_for(&manifest, &row);
            for (name, value) in &manifest.spec.config.env {
                match template.substitute(value) {
                    Ok(value) => {
                        vars.insert(name.clone(), value);
                    }
                    Err(err) => {
                        tracing::warn!(tool = %id, var = %name, %err, "config.env left out of the shell environment");
                    }
                }
            }
        }
    }
    path_prepend.push(daemon.root.bin_shims().to_string_lossy().replace('/', "\\"));
    Ok(ForgeboxEnv { path_prepend, vars })
}

/// `shell.env`: [`forgebox_env`] plus the script in the requested format.
async fn env(daemon: Daemon, params: ShellEnvParams, _ctx: Ctx) -> Result<ShellEnvDto> {
    let ForgeboxEnv { path_prepend, vars } =
        forgebox_env(&daemon, params.dir.as_deref(), &params.versions).await?;
    let script = match params.format {
        ShellFormat::Json => None,
        ShellFormat::Powershell => {
            let mut lines = vec![format!(
                "$env:PATH = {} + ';' + $env:PATH",
                quote_ps(&path_prepend.join(";"))
            )];
            for (name, value) in &vars {
                lines.push(format!("$env:{name} = {}", quote_ps(value)));
            }
            Some(lines.join("\n"))
        }
        ShellFormat::Cmd => {
            let mut lines = vec![format!("set \"PATH={};%PATH%\"", path_prepend.join(";"))];
            for (name, value) in &vars {
                lines.push(format!("set \"{name}={value}\""));
            }
            Some(lines.join("\r\n"))
        }
        ShellFormat::Bash => {
            let mut lines = vec![format!(
                "export PATH=\"{}:$PATH\"",
                path_prepend
                    .iter()
                    .map(|p| p.replace('\\', "/"))
                    .collect::<Vec<_>>()
                    .join(":")
            )];
            for (name, value) in &vars {
                lines.push(format!("export {name}='{}'", value.replace('\'', "'\\''")));
            }
            Some(lines.join("\n"))
        }
    };
    Ok(ShellEnvDto {
        path_prepend,
        vars,
        script,
    })
}

/// Where programs are looked up: the daemon's own `PATH`, `System32`, `%LOCALAPPDATA%`
/// and `%ProgramFiles%`. Blocking (file checks).
fn with_lookup<T>(system32: &Path, f: impl FnOnce(&ProgramLookup<'_>) -> T) -> T {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
    // `wt.exe` is an app execution alias, a reparse point `is_file` may not follow.
    let is_file = |p: &Path| p.is_file() || std::fs::symlink_metadata(p).is_ok_and(|m| !m.is_dir());
    f(&ProgramLookup {
        path: &path,
        system32,
        local_app_data: local.as_deref(),
        program_files: program_files.as_deref(),
        is_file: &is_file,
    })
}

fn system32(daemon: &Daemon) -> PathBuf {
    daemon
        .job_tree
        .processes()
        .system_path()
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32"))
}

/// Opens an external terminal in `dir` (an existing folder) with the Forgebox environment:
/// the tools' folders first on `PATH` (the versions the project selects, `versions` on
/// top) plus their `config.env`, as in the integrated terminal. The terminal is detached:
/// closing the daemon leaves it open.
pub(super) async fn launch_terminal(
    daemon: &Daemon,
    dir: PathBuf,
    versions: BTreeMap<String, String>,
) -> Result<()> {
    let dir = plain_path(&dir);
    if !dir.is_absolute() {
        return Err(ForgeError::InvalidInput(format!(
            "dir {} must be an absolute path",
            dir.display()
        )));
    }
    let check = dir.clone();
    forge_core::runtime::spawn_blocking_timeout(
        "checking the folder",
        BLOCKING_BUDGET,
        move || {
            if check.is_dir() {
                Ok(())
            } else {
                Err(ForgeError::NotFound(format!("folder {}", check.display())))
            }
        },
    )
    .await?;
    let ForgeboxEnv {
        mut path_prepend,
        vars,
    } = forgebox_env(daemon, Some(&dir.to_string_lossy()), &versions).await?;
    // `forge` next to the daemon (the installed or portable pair) is on the PATH too.
    let forge_dir = std::env::current_exe().ok().and_then(|exe| {
        let name = if cfg!(windows) { "forge.exe" } else { "forge" };
        let parent = exe.parent()?;
        parent.join(name).is_file().then(|| parent.to_path_buf())
    });
    if let Some(forge_dir) = forge_dir {
        path_prepend.push(forge_dir.display().to_string());
    }
    let configured = daemon.config().terminal.command.clone();
    let system32 = system32(daemon);
    let opener = Arc::clone(daemon.platform.shell_opener());
    let base_env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let (exe, pid) = forge_core::runtime::spawn_blocking_timeout(
        "starting the terminal",
        BLOCKING_BUDGET,
        move || {
            let resolved = with_lookup(&system32, |lookup| {
                resolve_terminal(configured.as_deref(), &dir, lookup)
            })?;
            let (env, _path) =
                crate::terminal::session_env(base_env, &path_prepend, &vars, &BTreeMap::new());
            let pid = opener.launch(&LaunchSpec {
                exe: resolved.exe.clone(),
                args: resolved.args,
                cwd: dir,
                env: Some(env),
                new_console: resolved.new_console,
            })?;
            Ok((resolved.exe, pid))
        },
    )
    .await?;
    tracing::debug!(exe = %exe.display(), pid, "opened an external terminal");
    Ok(())
}

/// Opens `path` in the editor at an optional position. The path must exist and lie inside
/// the root, a parked folder or a site (its root or its project folder); otherwise
/// `forbidden`: the editor would otherwise open any file of the machine for a client.
pub(super) async fn launch_editor(
    daemon: &Daemon,
    path: &Path,
    line: Option<u32>,
    column: Option<u32>,
) -> Result<()> {
    if !path.is_absolute() {
        return Err(ForgeError::InvalidInput(format!(
            "path {} must be absolute",
            path.display()
        )));
    }
    let mut allowed = vec![daemon.root.path().to_path_buf()];
    allowed.extend(
        daemon
            .config()
            .parked_dirs
            .iter()
            .map(|d| super::sites::parked_path(daemon, d)),
    );
    let site_roots: Vec<PathBuf> = daemon
        .sites
        .list()
        .await?
        .into_iter()
        .filter_map(|s| s.root)
        .map(|r| super::terminal::native(&r))
        .collect();
    let configured = daemon.config().editor.command.clone();
    let system32 = system32(daemon);
    let opener = Arc::clone(daemon.platform.shell_opener());
    let path = path.to_path_buf();
    let (exe, pid) = forge_core::runtime::spawn_blocking_timeout(
        "starting the editor",
        BLOCKING_BUDGET,
        move || {
            let canonical = std::fs::canonicalize(&path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ForgeError::NotFound(format!("path {}", path.display()))
                } else {
                    ForgeError::io_at("opening", &path, e)
                }
            })?;
            for root in site_roots {
                allowed.push(forge_sites::lint_case::project_dir_for_root(&root));
                allowed.push(root);
            }
            let inside = allowed
                .iter()
                .filter_map(|root| std::fs::canonicalize(root).ok())
                .any(|root| canonical.starts_with(root));
            if !inside {
                return Err(ForgeError::Forbidden(format!(
                    "{} is outside the Forgebox root, the parked folders and the sites",
                    path.display()
                )));
            }
            let canonical = plain_path(&canonical);
            let cwd = if canonical.is_dir() {
                canonical.clone()
            } else {
                canonical
                    .parent()
                    .map_or_else(|| canonical.clone(), Path::to_path_buf)
            };
            let resolved = with_lookup(&system32, |lookup| {
                resolve_editor(
                    configured.as_deref(),
                    EditorTarget {
                        path: &canonical,
                        line,
                        column,
                    },
                    lookup,
                )
            })?;
            let pid = opener.launch(&LaunchSpec {
                exe: resolved.exe.clone(),
                args: resolved.args,
                cwd,
                env: None,
                new_console: false,
            })?;
            Ok((resolved.exe, pid))
        },
    )
    .await?;
    tracing::debug!(exe = %exe.display(), pid, "opened the editor");
    Ok(())
}

/// `shell.open` (local only): an external terminal in `dir` (default: the root).
async fn open(daemon: Daemon, params: ShellOpenParams, _ctx: Ctx) -> Result<Ack> {
    let dir = match params.dir.as_deref().filter(|d| !d.trim().is_empty()) {
        Some(dir) => super::terminal::native(dir),
        None => plain_path(daemon.root.path()),
    };
    launch_terminal(&daemon, dir, BTreeMap::new()).await?;
    Ok(Ack::ok())
}

/// `shell.open_editor` (local only): `path` in the editor at `line:column`.
async fn open_editor(daemon: Daemon, params: ShellOpenEditorParams, _ctx: Ctx) -> Result<Ack> {
    let path = super::terminal::native(&params.path);
    launch_editor(&daemon, &path, params.line, params.column).await?;
    Ok(Ack::ok())
}
