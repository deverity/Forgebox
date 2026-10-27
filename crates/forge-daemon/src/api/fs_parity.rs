//! `fs_parity.*`: case sensitivity per folder and per site, conversion jobs and the Case
//! Mismatch Finder (docs/15), over `forge_core::fs::case` and `forge_sites::CaseParity`.
//! Folders must be inside the root or a parked folder.

use std::path::{Path, PathBuf};

use forge_core::error::{ForgeError, Result};
use forge_core::fs::case::{CaseConversions, CaseStatus};
use forge_ipc::api::methods::{
    fs_parity_case_convert, fs_parity_case_disable, fs_parity_case_enable, fs_parity_case_status,
    fs_parity_lint_case, fs_parity_site_case,
};
use forge_ipc::api::{
    CaseConvertParams, CaseDirParams, CaseFixChoice, CaseSensitivityDto, JobDto, LintCaseParams,
    SiteCaseParams,
};
use forge_ipc::server::{Ctx, Router};
use forge_sites::CaseParity;

use super::route;
use crate::daemon::Daemon;

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, fs_parity_case_status, case_status);
    route!(router, daemon, fs_parity_case_enable, case_enable);
    route!(router, daemon, fs_parity_case_disable, case_disable);
    route!(router, daemon, fs_parity_case_convert, case_convert);
    route!(router, daemon, fs_parity_site_case, site_case);
    route!(router, daemon, fs_parity_lint_case, lint_case);
}

fn conversions(daemon: &Daemon) -> CaseConversions {
    daemon.case.clone()
}

/// Absolute folder inside the root or a parked folder (`forbidden` otherwise).
fn allowed_dir(daemon: &Daemon, dir: &str) -> Result<PathBuf> {
    let path = PathBuf::from(dir);
    let path = if path.is_absolute() {
        path
    } else {
        daemon.root.path().join(path)
    };
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(ForgeError::InvalidInput(format!(
            "{dir}: `..` is not allowed"
        )));
    }
    let config = daemon.config();
    let parked: Vec<PathBuf> = config
        .parked_dirs
        .iter()
        .map(|d| {
            let p = Path::new(d);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                daemon.root.path().join(p)
            }
        })
        .collect();
    let inside_root = path.starts_with(daemon.root.path())
        && !path.starts_with(daemon.root.bin())
        && !path.starts_with(daemon.root.state());
    if inside_root || parked.iter().any(|p| path.starts_with(p)) {
        Ok(path)
    } else {
        Err(ForgeError::Forbidden(format!(
            "{dir} is outside the root and the parked folders"
        )))
    }
}

fn dto(dir: &Path, status: CaseStatus) -> CaseSensitivityDto {
    let value = serde_json::to_value(&status).unwrap_or_default();
    let get_bool = |k: &str| value.get(k).and_then(serde_json::Value::as_bool);
    CaseSensitivityDto {
        dir: forge_core::paths::posix_string(dir),
        supported: get_bool("supported").unwrap_or(false),
        enabled: get_bool("enabled"),
        fs_type: value
            .get("fs_type")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        os_build: value
            .get("os_build")
            .and_then(serde_json::Value::as_u64)
            .and_then(|b| u32::try_from(b).ok()),
        win_version_ok: get_bool("win_version_ok"),
        reason: value
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
    }
}

async fn case_status(
    daemon: Daemon,
    params: CaseDirParams,
    _ctx: Ctx,
) -> Result<CaseSensitivityDto> {
    let dir = allowed_dir(&daemon, &params.dir)?;
    let status = conversions(&daemon).status(dir.clone()).await?;
    Ok(dto(&dir, status))
}

async fn set(daemon: Daemon, params: CaseDirParams, enable: bool) -> Result<CaseSensitivityDto> {
    let dir = allowed_dir(&daemon, &params.dir)?;
    let status = conversions(&daemon).set_flag(dir.clone(), enable).await?;
    Ok(dto(&dir, status))
}

async fn case_enable(
    daemon: Daemon,
    params: CaseDirParams,
    _ctx: Ctx,
) -> Result<CaseSensitivityDto> {
    set(daemon, params, true).await
}

async fn case_disable(
    daemon: Daemon,
    params: CaseDirParams,
    _ctx: Ctx,
) -> Result<CaseSensitivityDto> {
    set(daemon, params, false).await
}

fn job_dto(daemon: &Daemon, id: forge_core::ids::JobId) -> Result<JobDto> {
    daemon
        .jobs
        .get(id)
        .map(JobDto::from)
        .ok_or_else(|| ForgeError::Internal(format!("job {id} vanished")))
}

async fn case_convert(daemon: Daemon, params: CaseConvertParams, ctx: Ctx) -> Result<JobDto> {
    let dir = allowed_dir(&daemon, &params.dir)?;
    let id = conversions(&daemon).convert(dir, !params.revert, Some(ctx.request_id()));
    job_dto(&daemon, id)
}

fn parity(daemon: &Daemon) -> CaseParity {
    CaseParity::new(daemon.sites.clone(), conversions(daemon))
}

async fn site_case(daemon: Daemon, params: SiteCaseParams, ctx: Ctx) -> Result<JobDto> {
    let site = super::sites::find(&daemon, &params.site).await?;
    let id = parity(&daemon).set_site_case(site.id, params.enabled, Some(ctx.request_id()));
    job_dto(&daemon, id)
}

async fn lint_case(daemon: Daemon, params: LintCaseParams, ctx: Ctx) -> Result<JobDto> {
    let site = super::sites::find(&daemon, &params.site).await?;
    let fix = params.fix.then(|| forge_sites::lint_case::FixOptions {
        dry_run: params.dry_run,
        strategy: match params.strategy {
            CaseFixChoice::Auto => forge_sites::lint_case::StrategyChoice::Auto,
            CaseFixChoice::Rename => forge_sites::lint_case::StrategyChoice::Rename,
            CaseFixChoice::Edit => forge_sites::lint_case::StrategyChoice::Edit,
        },
        overrides: params
            .overrides
            .iter()
            .map(|(id, s)| {
                let strategy = match s {
                    forge_ipc::api::CaseFixStrategy::Rename => {
                        forge_sites::lint_case::FixStrategy::Rename
                    }
                    forge_ipc::api::CaseFixStrategy::Edit => {
                        forge_sites::lint_case::FixStrategy::Edit
                    }
                };
                (id.clone(), strategy)
            })
            .collect(),
        only: params.only.clone().map(|ids| ids.into_iter().collect()),
    });
    let id = parity(&daemon).lint_site(site.id, fix, Some(ctx.request_id()));
    job_dto(&daemon, id)
}
