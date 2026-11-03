//! `terminal.*`: sessions hosted by the daemon (ADR-0040, [`crate::terminal`]). Every
//! method needs `admin` (checked by the IPC server from the method table before the
//! handler runs: a shell is full control of the machine); none is `local_only`.
//!
//! `terminal.open` resolves what to run: the working folder (`dir`, the site's project
//! folder with `site`, the root otherwise), the environment ([`super::shell::forgebox_env`]
//! with the site's PHP or the given `versions`, on top of the daemon's own environment,
//! plus `FORGEBOX_TERMINAL=1`) and the program (`command`, or the shell from `shell` /
//! `terminal.integrated_shell`, `pwsh` → `powershell` → `cmd`).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::paths::plain_path;
use forge_core::platform::PtyCommand;
use forge_ipc::api::methods::{
    terminal_close, terminal_input, terminal_list, terminal_open, terminal_resize,
    terminal_subscribe,
};
use forge_ipc::api::{
    Ack, NoParams, SubscriptionDto, TerminalDto, TerminalIdParams, TerminalInputParams,
    TerminalListDto, TerminalOpenParams, TerminalResizeParams, TerminalSubscribeParams,
};
use forge_ipc::server::{Ctx, Router};

use super::route;
use super::shell::{ForgeboxEnv, forgebox_env};
use crate::daemon::Daemon;
use crate::terminal::{
    CLOSE_ASK, Lookup, RESIZE_ASK, STREAM_QUEUE, TerminalSpec, find_shell, resolve_command,
    session_env, shell_args,
};

/// Answer deadline of `terminal.open` (site lookup, environment, Job, ConPTY, process).
const OPEN_TIMEOUT: Duration = Duration::from_secs(20);

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, terminal_open, open);
    route!(router, daemon, terminal_list, list);
    route!(router, daemon, terminal_input, input);
    route!(router, daemon, terminal_resize, resize);
    route!(router, daemon, terminal_close, close);
    route!(router, daemon, terminal_subscribe, subscribe);
    router
        .timeout::<terminal_open>(OPEN_TIMEOUT)
        .timeout::<terminal_resize>(RESIZE_ASK + Duration::from_secs(1))
        .timeout::<terminal_close>(CLOSE_ASK + Duration::from_secs(1));
}

/// Native form of a stored path (`C:/www/loja` → `C:\www\loja` on Windows), without the
/// `\\?\` prefix (a shell would show it in its prompt; `cmd` refuses it as a cwd).
fn native(path: &str) -> PathBuf {
    let path = if cfg!(windows) {
        PathBuf::from(path.replace('/', "\\"))
    } else {
        PathBuf::from(path)
    };
    plain_path(&path)
}

/// The site's project folder and its PHP (`tool id → exact version`).
async fn site_context(
    daemon: &Daemon,
    site: &forge_ipc::api::SiteRef,
) -> Result<(
    forge_core::store::Site,
    Option<PathBuf>,
    Option<(String, String)>,
)> {
    let site = super::sites::find(daemon, site).await?;
    let dir = match site.root.clone() {
        Some(root) => {
            let root = native(&root);
            Some(
                forge_core::runtime::spawn_blocking_timeout(
                    "finding the project folder",
                    Duration::from_secs(5),
                    move || Ok(forge_sites::lint_case::project_dir_for_root(&root)),
                )
                .await?,
            )
        }
        None => None,
    };
    let php = daemon
        .web
        .php_plan(std::slice::from_ref(&site))
        .await
        .unwrap_or_default()
        .remove(&site.id)
        .map(|plan| (plan.version.tool_id.to_string(), plan.version.version));
    Ok((site, dir, php))
}

async fn open(daemon: Daemon, params: TerminalOpenParams, _ctx: Ctx) -> Result<TerminalDto> {
    if daemon.shutdown.is_requested() {
        return Err(ForgeError::Conflict("the daemon is shutting down".into()));
    }
    // Validates the size before anything is looked up.
    forge_core::platform::PtySize::new(params.cols, params.rows)?;
    let config = daemon.config();
    let mut versions = BTreeMap::new();
    let (site, site_dir) = match &params.site {
        Some(reference) => {
            let (site, dir, php) = site_context(&daemon, reference).await?;
            if let Some((tool, version)) = php {
                versions.insert(tool, version);
            }
            (Some(site), dir)
        }
        None => (None, None),
    };
    versions.extend(params.versions.clone());
    let cwd = match &params.dir {
        Some(dir) => {
            let dir = native(dir);
            if !dir.is_absolute() {
                return Err(ForgeError::InvalidInput(format!(
                    "dir {} must be an absolute path",
                    dir.display()
                )));
            }
            dir
        }
        None => site_dir.unwrap_or_else(|| plain_path(daemon.root.path())),
    };
    let ForgeboxEnv {
        mut path_prepend,
        vars,
    } = forgebox_env(&daemon, Some(&cwd.to_string_lossy()), &versions).await?;
    // `forge` next to the daemon (the installed or portable pair) is on the PATH too.
    let forge_exe = std::env::current_exe().ok().and_then(|exe| {
        let name = if cfg!(windows) { "forge.exe" } else { "forge" };
        exe.parent().map(|dir| dir.join(name))
    });
    let system32 = daemon
        .job_tree
        .processes()
        .system_path()
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32"));
    let configured = config.terminal.integrated_shell.clone();
    let max_sessions = config.terminal.max_sessions;
    let TerminalOpenParams {
        shell,
        command,
        env: extra,
        cols,
        rows,
        title,
        ..
    } = params;
    let site_id = site.as_ref().map(|s| s.id);
    let site_host = site.as_ref().map(|s| s.host.clone());
    let base_env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let (spec, cwd) = forge_core::runtime::spawn_blocking_timeout(
        "resolving the terminal program",
        Duration::from_secs(5),
        move || {
            let forge = forge_exe.filter(|p| p.is_file());
            if let Some(dir) = forge.as_ref().and_then(|f| f.parent()) {
                path_prepend.push(dir.display().to_string());
            }
            let (env, path) = session_env(base_env.clone(), &path_prepend, &vars, &extra);
            let pathext = env
                .iter()
                .rev()
                .find(|(k, _)| {
                    k.to_str()
                        .is_some_and(|k| k.eq_ignore_ascii_case("PATHEXT"))
                })
                .map(|(_, v)| v.clone());
            let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
            let comspec = std::env::var_os("ComSpec").map(PathBuf::from);
            let is_file = |p: &Path| p.is_file();
            let lookup = Lookup {
                system32: &system32,
                path: &path,
                pathext: pathext.as_deref(),
                program_files: program_files.as_deref(),
                comspec: comspec.as_deref(),
                is_file: &is_file,
            };
            let (exe, args, shown) = if command.is_empty() {
                let (name, exe) = find_shell(shell.as_deref(), configured.as_deref(), &lookup)?;
                (exe, shell_args(name, forge.as_deref()), name.to_owned())
            } else {
                let (exe, args) = resolve_command(&command, &cwd, &lookup)?;
                let shown = Path::new(&command[0])
                    .file_name()
                    .map_or_else(|| command[0].clone(), |n| n.to_string_lossy().into_owned());
                (exe, args, shown)
            };
            let title = title
                .or(site_host)
                .unwrap_or_else(|| shown.trim_end_matches(".exe").to_owned());
            let spec = TerminalSpec {
                title,
                shell: shown,
                site_id,
                command: PtyCommand {
                    exe,
                    args,
                    cwd: cwd.clone(),
                    env,
                    cols,
                    rows,
                },
            };
            Ok((spec, cwd))
        },
    )
    .await?;
    tracing::debug!(cwd = %cwd.display(), exe = %spec.command.exe.display(), "opening a terminal");
    daemon.terminals.open(spec, max_sessions).await
}

async fn list(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<TerminalListDto> {
    Ok(TerminalListDto {
        terminals: daemon.terminals.list(),
    })
}

async fn input(daemon: Daemon, params: TerminalInputParams, _ctx: Ctx) -> Result<Ack> {
    daemon
        .terminals
        .input(
            params.id,
            params.writer,
            params.input_seq.get(),
            params.data,
        )
        .await?;
    Ok(Ack::ok())
}

async fn resize(daemon: Daemon, params: TerminalResizeParams, _ctx: Ctx) -> Result<Ack> {
    daemon
        .terminals
        .resize(params.id, params.cols, params.rows)
        .await?;
    Ok(Ack::ok())
}

async fn close(daemon: Daemon, params: TerminalIdParams, _ctx: Ctx) -> Result<Ack> {
    daemon.terminals.close(params.id).await?;
    Ok(Ack::ok())
}

async fn subscribe(
    daemon: Daemon,
    params: TerminalSubscribeParams,
    ctx: Ctx,
) -> Result<SubscriptionDto> {
    let mut id = None;
    let last_seq = daemon
        .terminals
        .subscribe(params.id, params.resume_from, || {
            let sender = ctx.open_stream(Some(STREAM_QUEUE))?;
            id = Some(sender.id());
            Ok(sender)
        })?;
    Ok(SubscriptionDto {
        subscription_id: id
            .ok_or_else(|| ForgeError::Internal("terminal stream not opened".into()))?,
        stream: "terminal".into(),
        last_seq,
    })
}
