//! `forge shell`, `forge env`, `forge exec`, `forge path` (docs/09 §shell / env).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::process::{Command, Stdio};

use forge_ipc::api::{
    NoParams, ShellEnvDto, ShellEnvParams, ShellFormat, ShellOpenParams, UserPathDto,
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
            Command::new(&program)
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
