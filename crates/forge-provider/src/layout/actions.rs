//! `post_install` actions and the `install.verify` command (docs/04 §1, docs/11 §2),
//! executed on the staged copy of a version before it is moved into `bin/`.
//!
//! * `copy` — `from` is relative to the version folder (or `{{ paths.<dir> }}...`); `to`
//!   must stay inside the root (or the staged version). `if_missing` keeps user edits.
//! * `render` — handed to the injected [`TemplateRenderer`] (the daemon plugs
//!   `forge-render` in; this crate does not depend on it). The renderer gets the *final*
//!   variables (`paths.version` = `bin/<tool>/<canonical_name>`), because rendered
//!   configuration outlives the staging folder.
//! * `mkdir` — creates a folder inside the root.
//! * `run` — only an executable of the version itself: a relative path resolved in the
//!   staged folder (never through `PATH`) or `{{ paths.version }}/...`, without `..`,
//!   variables or environment references in the program. Arguments, `cwd` and `env` are
//!   substituted with the *physical* variables (the staged folder is where the files are
//!   while the command runs). The process runs through [`run_probe_with_env`]: own Job
//!   Object, explicit environment, no window, timeout (default [`DEFAULT_RUN_TIMEOUT`]).
//!   Only allowed when the caller says the manifest may run commands
//!   (`InstallOptions::allow_run`: official registry or the user's own manifest).
//!
//! Everything here is blocking: the layout engine calls it from `spawn_blocking`.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::ids::ToolId;
use forge_core::paths::ForgeRoot;
use forge_core::platform::{ProbeCommand, run_probe_with_env};
use forge_manifest::schema::common::RunAction;
use forge_manifest::schema::tool::{PostInstallAction, Verify};
use regex::Regex;

use super::vars::TemplateVars;

/// Limit of a `run` action without `timeout`.
pub const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(600);
/// Limit of the `install.verify` command.
pub const VERIFY_TIMEOUT: Duration = Duration::from_secs(60);

/// What a `render` action asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderRequest {
    pub tool: ToolId,
    /// Template path with `/` (`php/php.ini.j2`), from `forge-render/templates/` or the
    /// registry's `templates/`.
    pub template: String,
    /// Absolute destination, already checked to be inside the root or the staged version.
    pub dest: PathBuf,
    /// Final variables: `paths.version` is `bin/<tool>/<canonical_name>`.
    pub vars: TemplateVars,
    /// Where the version's files are right now: the staged copy during an install (they
    /// move to `paths.version` afterwards), the version folder on a re-render. For checks
    /// on the shipped files (which extensions exist).
    pub files_dir: PathBuf,
}

/// Renders `post_install[].render` templates. Implemented by the daemon on top of
/// `forge-render` (managed blocks, LF, UTF-8 without BOM). Blocking: called from
/// `spawn_blocking`.
pub trait TemplateRenderer: Send + Sync {
    fn render(&self, request: &RenderRequest) -> Result<()>;
}

/// [`TemplateRenderer`] for setups without templates: every `render` action fails.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTemplates;

impl TemplateRenderer for NoTemplates {
    fn render(&self, request: &RenderRequest) -> Result<()> {
        Err(ForgeError::NotImplemented(format!(
            "no template renderer configured for {}",
            request.template
        )))
    }
}

/// Inputs shared by the actions of one installation.
pub(crate) struct ActionContext<'a> {
    pub root: &'a ForgeRoot,
    pub tool: &'a ToolId,
    /// The version's files right now (`tmp/stage/<id>/<canonical_name>`).
    pub staged_dir: &'a Path,
    /// `paths.version` = `staged_dir`.
    pub physical: &'a TemplateVars,
    /// `paths.version` = final folder.
    pub final_vars: &'a TemplateVars,
    pub renderer: &'a dyn TemplateRenderer,
    pub allow_run: bool,
    pub cancelled: &'a dyn Fn() -> bool,
}

/// Runs `actions` in order. Errors name the failing action (`post_install[2] (copy)`).
pub(crate) fn run_actions(ctx: &ActionContext<'_>, actions: &[PostInstallAction]) -> Result<()> {
    for (index, action) in actions.iter().enumerate() {
        if (ctx.cancelled)() {
            return Err(ForgeError::Cancelled("post_install".into()));
        }
        let (kind, result) = match action {
            PostInstallAction::Copy(copy) => ("copy", copy_action(ctx, copy)),
            PostInstallAction::Render(render) => ("render", render_action(ctx, render)),
            PostInstallAction::Mkdir(dir) => ("mkdir", mkdir_action(ctx, dir)),
            PostInstallAction::Run(run) => ("run", run_action(ctx, run)),
        };
        result.map_err(|e| annotate(e, &format!("post_install[{index}] ({kind})")))?;
        tracing::debug!(tool = %ctx.tool, index, kind, "post_install action done");
    }
    Ok(())
}

/// Runs only the `render` actions of `actions`, in order, for a version that is already
/// in place (`staged_dir` = its folder, both variable sets equal): the update engine
/// re-renders configuration shared by a series (`etc/php/8.3/conf.d/`) for the version
/// that is about to serve it (docs/05 §4). Returns the rendered files.
pub(crate) fn run_renders(
    ctx: &ActionContext<'_>,
    actions: &[PostInstallAction],
) -> Result<Vec<PathBuf>> {
    let mut rendered = Vec::new();
    for (index, action) in actions.iter().enumerate() {
        let PostInstallAction::Render(render) = action else {
            continue;
        };
        if (ctx.cancelled)() {
            return Err(ForgeError::Cancelled("re-render".into()));
        }
        let dest = target(ctx, &render.to)
            .map_err(|e| annotate(e, &format!("post_install[{index}] (render)")))?;
        render_action(ctx, render)
            .map_err(|e| annotate(e, &format!("post_install[{index}] (render)")))?;
        rendered.push(dest);
    }
    Ok(rendered)
}

/// Prefixes the message of `err` with `what`, keeping its code.
pub(crate) fn annotate(err: ForgeError, what: &str) -> ForgeError {
    match err {
        ForgeError::NotFound(m) => ForgeError::NotFound(format!("{what}: {m}")),
        ForgeError::Conflict(m) => ForgeError::Conflict(format!("{what}: {m}")),
        ForgeError::VerificationFailed(m) => ForgeError::VerificationFailed(format!("{what}: {m}")),
        ForgeError::Io { context, source } => ForgeError::Io {
            context: format!("{what}: {context}"),
            source,
        },
        ForgeError::InvalidInput(m) => ForgeError::InvalidInput(format!("{what}: {m}")),
        ForgeError::Forbidden(m) => ForgeError::Forbidden(format!("{what}: {m}")),
        ForgeError::Internal(m) => ForgeError::Internal(format!("{what}: {m}")),
        ForgeError::NotImplemented(m) => ForgeError::NotImplemented(format!("{what}: {m}")),
        ForgeError::Timeout { operation, after } => ForgeError::Timeout {
            operation: format!("{what}: {operation}"),
            after,
        },
        other => other,
    }
}

/// Lexically normalized absolute path (`.` dropped); `None` if `..` appears.
fn clean(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => return None,
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

fn same_or_inside(path: &Path, base: &Path) -> bool {
    let lower = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    let (p, b) = (lower(path), lower(base));
    p == b || p.starts_with(&format!("{}\\", b.trim_end_matches('\\')))
}

/// A write target: substituted with the physical variables, absolute, without `..`,
/// inside the root or the staged version.
fn target(ctx: &ActionContext<'_>, text: &str) -> Result<PathBuf> {
    let substituted = ctx.physical.substitute(text)?;
    let path = PathBuf::from(&substituted);
    let cleaned = clean(&path).filter(|p| p.is_absolute()).ok_or_else(|| {
        ForgeError::InvalidInput(format!("{text:?} must be an absolute path without `..`"))
    })?;
    if same_or_inside(&cleaned, ctx.root.path()) || same_or_inside(&cleaned, ctx.staged_dir) {
        Ok(cleaned)
    } else {
        Err(ForgeError::Forbidden(format!(
            "{text:?} resolves to {} outside the Forgebox root",
            cleaned.display()
        )))
    }
}

/// A source: relative to the staged version, or a [`target`]-like absolute path.
fn source(ctx: &ActionContext<'_>, text: &str) -> Result<PathBuf> {
    if text.trim_start().starts_with("{{") {
        return target(ctx, text);
    }
    relative_in(ctx.staged_dir, text).ok_or_else(|| {
        ForgeError::InvalidInput(format!(
            "{text:?} must be a relative path inside the version"
        ))
    })
}

/// `base` joined with the relative `rel` (`/` or `\`), refusing absolute paths, drives,
/// `..` and environment references.
pub(crate) fn relative_in(base: &Path, rel: &str) -> Option<PathBuf> {
    if rel.contains(['%', '$']) || rel.contains("{{") {
        return None;
    }
    let path = Path::new(rel);
    if path.is_absolute() || rel.starts_with(['/', '\\']) {
        return None;
    }
    let mut out = base.to_path_buf();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

fn create_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).at("creating", parent)?;
    }
    Ok(())
}

fn copy_action(
    ctx: &ActionContext<'_>,
    copy: &forge_manifest::schema::tool::CopyAction,
) -> Result<()> {
    let from = source(ctx, &copy.from)?;
    let to = target(ctx, &copy.to)?;
    if copy.if_missing && std::fs::symlink_metadata(&to).is_ok() {
        tracing::debug!(to = %to.display(), "post_install copy skipped: destination exists");
        return Ok(());
    }
    if !from.is_file() {
        return Err(ForgeError::NotFound(format!(
            "{} (copy source {:?})",
            from.display(),
            copy.from
        )));
    }
    create_parent(&to)?;
    std::fs::copy(&from, &to).at("copying", &from)?;
    Ok(())
}

fn render_action(
    ctx: &ActionContext<'_>,
    render: &forge_manifest::schema::tool::RenderAction,
) -> Result<()> {
    let dest = target(ctx, &render.to)?;
    create_parent(&dest)?;
    ctx.renderer.render(&RenderRequest {
        tool: ctx.tool.clone(),
        template: render.template.clone(),
        dest,
        vars: ctx.final_vars.clone(),
        files_dir: ctx.staged_dir.to_path_buf(),
    })
}

fn mkdir_action(ctx: &ActionContext<'_>, dir: &str) -> Result<()> {
    let dir = target(ctx, dir)?;
    std::fs::create_dir_all(&dir).at("creating", &dir)
}

/// Splits a manifest command line: whitespace separates words, double quotes group them
/// (`\"` is a quote inside them), `{{ ... }}` is never split.
pub(crate) fn split_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut in_quotes = false;
    let mut rest = command;
    while let Some(c) = rest.chars().next() {
        if rest.starts_with("{{") {
            let end = rest.find("}}").map_or(rest.len(), |e| e + 2);
            current.push_str(&rest[..end]);
            in_word = true;
            rest = &rest[end..];
            continue;
        }
        rest = &rest[c.len_utf8()..];
        match c {
            '"' => {
                in_quotes = !in_quotes;
                in_word = true;
            }
            '\\' if in_quotes && rest.starts_with('"') => {
                current.push('"');
                rest = &rest[1..];
            }
            c if c.is_whitespace() && !in_quotes => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            c => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(current);
    }
    words
}

/// The executable of a `run`/`verify` command: inside `version_dir`, existing.
fn version_executable(version_dir: &Path, program: &str) -> Result<PathBuf> {
    let outside = || {
        ForgeError::Forbidden(format!(
            "`{program}` is not an executable of the installed version (post_install.run and \
             install.verify only run files inside {{{{ paths.version }}}}, never through PATH)"
        ))
    };
    let exe = if program.trim_start().starts_with("{{") {
        let (dir, rest) = forge_manifest::template::root_dir_prefix(program).ok_or_else(outside)?;
        if dir != "version" || rest.contains("{{") {
            return Err(outside());
        }
        relative_in(version_dir, rest.trim_start_matches(['/', '\\'])).ok_or_else(outside)?
    } else {
        relative_in(version_dir, program).ok_or_else(outside)?
    };
    if !exe.is_file() {
        return Err(ForgeError::NotFound(format!(
            "{} (program of the command)",
            exe.display()
        )));
    }
    Ok(exe)
}

fn run_action(ctx: &ActionContext<'_>, run: &RunAction) -> Result<()> {
    if !ctx.allow_run {
        return Err(ForgeError::Forbidden(
            "post_install.run is only allowed for manifests of the official registry or the \
             user's own manifests"
                .into(),
        ));
    }
    let mut words = split_words(&run.command).into_iter();
    let program = words
        .next()
        .ok_or_else(|| ForgeError::InvalidInput("empty command".into()))?;
    let exe = version_executable(ctx.staged_dir, &program)?;
    let args = words
        .map(|w| ctx.physical.substitute(&w).map(OsString::from))
        .collect::<Result<Vec<_>>>()?;
    let cwd = match &run.cwd {
        Some(cwd) => target(ctx, cwd)?,
        None => ctx.staged_dir.to_path_buf(),
    };
    let env = run
        .env
        .iter()
        .map(|(k, v)| {
            Ok((
                OsString::from(k),
                OsString::from(ctx.physical.substitute(v)?),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let timeout = run.timeout.map_or(DEFAULT_RUN_TIMEOUT, |t| t.0);
    let cmd = ProbeCommand { exe, args, cwd };
    tracing::info!(tool = %ctx.tool, command = %run.command, "post_install run");
    let output = run_probe_with_env(&cmd, &env, timeout)?;
    if output.exit_code != Some(0) {
        return Err(ForgeError::VerificationFailed(format!(
            "`{}` exited with {:?}: {}",
            run.command,
            output.exit_code,
            tail(&output.combined())
        )));
    }
    Ok(())
}

/// Last lines of a command's output, for diagnostics.
fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(10);
    lines[start..].join(" | ")
}

/// `install.verify`: runs the command inside the staged version and, with a regex,
/// checks that group 1 is the version being installed (`8.3.36` for `8.3.36+1`, `8.5.0`
/// for `8.5.0-RC1`).
pub(crate) fn run_verify(
    staged_dir: &Path,
    physical: &TemplateVars,
    verify: &Verify,
) -> Result<()> {
    let mut words = split_words(&verify.command).into_iter();
    let program = words
        .next()
        .ok_or_else(|| ForgeError::InvalidInput("install.verify.command is empty".into()))?;
    let exe = version_executable(staged_dir, &program)?;
    let args = words
        .map(|w| physical.substitute(&w).map(OsString::from))
        .collect::<Result<Vec<_>>>()?;
    let cmd = ProbeCommand {
        exe,
        args,
        cwd: staged_dir.to_path_buf(),
    };
    let output = run_probe_with_env(&cmd, &[], VERIFY_TIMEOUT)?;
    if output.exit_code != Some(0) {
        return Err(ForgeError::VerificationFailed(format!(
            "install.verify `{}` exited with {:?} (exit code 0xC0000135 means a missing DLL, \
             e.g. the Visual C++ runtime): {}",
            verify.command,
            output
                .exit_code
                .map(|c| format!("{c} / 0x{:08X}", c as u32)),
            tail(&output.combined())
        )));
    }
    let Some(pattern) = &verify.regex else {
        return Ok(());
    };
    let re = Regex::new(&format!("(?m){pattern}"))
        .map_err(|e| ForgeError::InvalidInput(format!("install.verify.regex: {e}")))?;
    let combined = output.combined();
    let found = re
        .captures(&combined)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_owned()))
        .ok_or_else(|| {
            ForgeError::VerificationFailed(format!(
                "install.verify: `{}` printed nothing matching {pattern:?}: {}",
                verify.command,
                tail(&combined)
            ))
        })?;
    if !version_matches(&physical.version, &found) {
        return Err(ForgeError::VerificationFailed(format!(
            "install.verify: expected version {}, `{}` reports {found}",
            physical.version, verify.command
        )));
    }
    Ok(())
}

/// Whether the version printed by a tool (`reported`) is the release `expected`: equal
/// once build metadata (`+1`) is dropped, or a prefix followed by a non-digit (`8.5.0`
/// for `8.5.0-RC1`, `8.5.0RC1`).
pub(crate) fn version_matches(expected: &str, reported: &str) -> bool {
    let base = expected.split('+').next().unwrap_or(expected);
    if reported.is_empty() {
        return false;
    }
    base == reported
        || base
            .strip_prefix(reported)
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_words_keeps_quotes_and_templates() {
        assert_eq!(
            split_words(r#"bin/mysqld.exe --initialize-insecure "--basedir={{ paths.version }}""#),
            [
                "bin/mysqld.exe",
                "--initialize-insecure",
                "--basedir={{ paths.version }}"
            ]
        );
        assert_eq!(
            split_words(r#""bin/my tool.exe" -r "echo \"x\";" {{ paths.version }}/a"#),
            [
                "bin/my tool.exe",
                "-r",
                "echo \"x\";",
                "{{ paths.version }}/a"
            ]
        );
        assert!(split_words("   ").is_empty());
    }

    #[test]
    fn versions_match_like_tools_print_them() {
        assert!(version_matches("8.3.36", "8.3.36"));
        assert!(version_matches("8.3.36+1", "8.3.36"));
        assert!(version_matches("8.5.0-RC1", "8.5.0"));
        assert!(!version_matches("8.3.36", "8.3.3"));
        assert!(!version_matches("8.3.36", "8.3.21"));
        assert!(!version_matches("8.3.36", ""));
    }

    #[test]
    fn executables_must_live_in_the_version() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::write(dir.path().join("bin").join("tool.exe"), b"x").unwrap();
        let ok = version_executable(dir.path(), "bin/tool.exe").unwrap();
        assert!(ok.ends_with("tool.exe"));
        let ok = version_executable(dir.path(), "{{ paths.version }}/bin/tool.exe").unwrap();
        assert!(ok.ends_with("tool.exe"));
        for bad in [
            "../tool.exe",
            r"C:\Windows\System32\cmd.exe",
            "%COMSPEC%",
            "{{ paths.etc }}/x.exe",
            "cmd.exe",
        ] {
            let err = version_executable(dir.path(), bad).unwrap_err();
            assert!(
                matches!(err.code(), "forbidden" | "not_found"),
                "{bad}: {err}"
            );
        }
    }
}
