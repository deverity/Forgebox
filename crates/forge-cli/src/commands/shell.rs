//! `forge shell`, `forge env`, `forge exec`, `forge path` (docs/09 §shell / env).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::process::{Command, Stdio};

use forge_ipc::api::{
    NoParams, ShellCommandDto, ShellEnvDto, ShellEnvParams, ShellFormat, ShellOpenParams,
    UserPathDto,
};

use crate::cli::{EnvArgs, ExecArgs, OptDirArg, PathCmd};
use crate::context::Context;
use crate::error::{CliError, CliResult};
use crate::messages::Msg;
use crate::output::{GREEN, YELLOW, paint};

pub async fn shell(ctx: &Context, args: OptDirArg) -> CliResult<()> {
    let dir = args
        .dir
        .as_deref()
        .map(crate::parse::absolute)
        .transpose()?;
    let ack = ctx
        .call::<forge_ipc::api::methods::shell_open>(ShellOpenParams { dir })
        .await?;
    ctx.out.emit(&ack, |_| {});
    Ok(())
}

/// `--powershell` (default), `--cmd`, `--bash`; `--json` asks for the data only.
fn format_of(ctx: &Context, args: &EnvArgs) -> ShellFormat {
    if ctx.out.json {
        ShellFormat::Json
    } else if args.cmd {
        ShellFormat::Cmd
    } else if args.bash {
        ShellFormat::Bash
    } else {
        ShellFormat::Powershell
    }
}

pub async fn env(ctx: &Context, args: EnvArgs) -> CliResult<()> {
    let dir = ctx.dir_or_cwd(args.dir.as_deref())?;
    let env: ShellEnvDto = ctx
        .call::<forge_ipc::api::methods::shell_env>(ShellEnvParams {
            dir: Some(dir),
            format: format_of(ctx, &args),
            versions: BTreeMap::new(),
        })
        .await?;
    ctx.out.emit(&env, |out| match &env.script {
        Some(script) => out.raw(script),
        None => {
            for entry in &env.path_prepend {
                out.line(entry);
            }
        }
    });
    Ok(())
}

/// The `PATH` for a child: `prepend` entries first, then the current one.
pub fn child_path(prepend: &[String], current: Option<OsString>) -> OsString {
    let mut entries: Vec<std::path::PathBuf> =
        prepend.iter().map(std::path::PathBuf::from).collect();
    if let Some(current) = current {
        entries.extend(std::env::split_paths(&current));
    }
    std::env::join_paths(entries).unwrap_or_default()
}

/// The program `forge exec` starts: a bare name (`composer`, `php`) is looked up in the
/// child's `PATH` with the extensions of `PATHEXT`, as a shell would, so `.cmd`/`.bat`
/// shims are found too (the standard library only tries `.exe`); Rust then starts batch
/// files through `cmd.exe` with its argument escaping. A name with a folder or an
/// extension, or one not found, is left as is.
pub fn resolve_program(
    program: &str,
    path: &std::ffi::OsStr,
    pathext: Option<OsString>,
) -> std::path::PathBuf {
    let as_is = std::path::PathBuf::from(program);
    if program.contains(['\\', '/', ':']) || as_is.extension().is_some() {
        return as_is;
    }
    let pathext = pathext
        .and_then(|p| p.into_string().ok())
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_owned());
    let extensions: Vec<&str> = pathext
        .split(';')
        .map(str::trim)
        .filter(|e| e.starts_with('.'))
        .collect();
    for dir in std::env::split_paths(path) {
        for ext in &extensions {
            let candidate = dir.join(format!("{program}{}", ext.to_ascii_lowercase()));
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    as_is
}

/// Runs the command with the project's versions (and `--php`) in front of `PATH`, without
/// changing `current`; the exit code is the child's.
pub async fn exec(ctx: &Context, args: ExecArgs) -> CliResult<()> {
    let mut versions = BTreeMap::new();
    if let Some(php) = &args.php {
        versions.insert("php".to_owned(), php.clone());
    }
    let env: ShellEnvDto = ctx
        .call::<forge_ipc::api::methods::shell_env>(ShellEnvParams {
            dir: Some(ctx.cwd()?),
            format: ShellFormat::Json,
            versions,
        })
        .await?;
    let (program, rest) = args
        .command
        .split_first()
        .ok_or_else(|| CliError::usage(ctx.out.t(Msg::ExecNeedsCommand)))?;
    let path = child_path(&env.path_prepend, std::env::var_os("PATH"));
    let status = tokio::task::spawn_blocking({
        let (program, rest) = (program.clone(), rest.to_vec());
        let vars = env.vars.clone();
        move || {
            let exe = resolve_program(&program, &path, std::env::var_os("PATHEXT"));
            Command::new(&exe)
                .args(&rest)
                .env("PATH", path)
                .envs(vars)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .map_err(|e| (program, e))
        }
    })
    .await
    .map_err(|e| CliError::new("internal", e.to_string()))?
    .map_err(|(program, e)| {
        let code = if e.kind() == std::io::ErrorKind::NotFound {
            "not_found"
        } else {
            "io"
        };
        CliError::new(code, ctx.out.tf(Msg::ExecFailed, &[&program, &e]))
    })?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(CliError::exit_silently(code)),
        None => Err(CliError::exit_silently(1)),
    }
}

/// Runs a command the daemon prepared (`db.shell`) attached to this console: same stdin,
/// stdout and stderr, its environment additions on top of ours; exits with its code.
pub(crate) async fn run_attached(ctx: &Context, command: &ShellCommandDto) -> CliResult<()> {
    let status = tokio::task::spawn_blocking({
        let command = command.clone();
        move || {
            let mut child = Command::new(&command.program);
            child
                .args(&command.args)
                .envs(&command.env)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            if let Some(cwd) = &command.cwd {
                child.current_dir(cwd);
            }
            child.status().map_err(|e| (command.program, e))
        }
    })
    .await
    .map_err(|e| CliError::new("internal", e.to_string()))?
    .map_err(|(program, e)| {
        let code = if e.kind() == std::io::ErrorKind::NotFound {
            "not_found"
        } else {
            "io"
        };
        CliError::new(code, ctx.out.tf(Msg::ExecFailed, &[&program, &e]))
    })?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(CliError::exit_silently(code)),
        None => Err(CliError::exit_silently(1)),
    }
}

fn path_lines(ctx: &Context, path: &UserPathDto) {
    let out = &ctx.out;
    out.line(paint(crate::output::BOLD, out.t(Msg::PathForgeboxEntries)));
    if path.forgebox_entries.is_empty() {
        out.line(format!("  {}", out.t(Msg::PathNone)));
    }
    for entry in &path.forgebox_entries {
        out.line(format!("  {} {entry}", paint(GREEN, "+")));
    }
    for entry in &path.missing {
        out.line(format!(
            "  {} {entry}  ({})",
            paint(YELLOW, "-"),
            out.t(Msg::PathMissing)
        ));
    }
}

pub async fn path(ctx: &Context, command: PathCmd) -> CliResult<()> {
    let path = match command {
        PathCmd::Show => {
            ctx.call::<forge_ipc::api::methods::shell_path_show>(NoParams {})
                .await?
        }
        PathCmd::Add => {
            ctx.call::<forge_ipc::api::methods::shell_path_add>(NoParams {})
                .await?
        }
        PathCmd::Remove => {
            ctx.call::<forge_ipc::api::methods::shell_path_remove>(NoParams {})
                .await?
        }
    };
    ctx.out.emit(&path, |_| path_lines(ctx, &path));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_finds_batch_shims_like_a_shell() {
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(second.join("composer.cmd"), "@echo off").unwrap();
        std::fs::write(second.join("php.exe"), "").unwrap();
        std::fs::write(first.join("php.cmd"), "@echo off").unwrap();
        let path = std::env::join_paths([&first, &second]).unwrap();
        let ext = Some(OsString::from(".COM;.EXE;.BAT;.CMD"));
        assert_eq!(
            resolve_program("composer", &path, ext.clone()),
            second.join("composer.cmd")
        );
        assert_eq!(
            resolve_program("php", &path, ext.clone()),
            first.join("php.cmd"),
            "PATH order first, then PATHEXT order"
        );
        assert_eq!(
            resolve_program("nope", &path, ext.clone()),
            std::path::PathBuf::from("nope")
        );
        assert_eq!(
            resolve_program("php.exe", &path, ext),
            std::path::PathBuf::from("php.exe"),
            "an explicit extension is left to the standard library"
        );
    }

    #[test]
    fn child_path_puts_the_versions_first() {
        let joined = child_path(
            &["C:\\fb\\bin\\php\\8.2".to_owned()],
            Some(std::env::join_paths(["C:\\Windows"]).unwrap()),
        );
        let entries: Vec<_> = std::env::split_paths(&joined).collect();
        assert_eq!(
            entries[0],
            std::path::PathBuf::from("C:\\fb\\bin\\php\\8.2")
        );
        assert_eq!(entries[1], std::path::PathBuf::from("C:\\Windows"));
    }
}
