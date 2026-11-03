//! `tools.*`: catalog, installation, removal, updates, active CLI version.
//!
//! `tools.install`/`tools.remove`/`tools.info` also take extensions (`kind: Extension`,
//! ADR-0043): installed into versions of the tool they extend (`target`, CLI `--for`).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::ids::ToolId;
use forge_core::jobs::JobState;
use forge_core::store::{SiteRepo, ToolVersionRepo, ToolVersionRow};
use forge_ipc::api::methods::{
    tools_check_updates, tools_discover, tools_info, tools_install, tools_list, tools_policy,
    tools_remove, tools_rollback, tools_search, tools_update, tools_use, tools_which,
};
use forge_ipc::api::{
    Ack, AvailableVersionDto, CatalogEntryDto, CatalogKind, CatalogSearchDto, JobDto, NoParams,
    SiteUsageDto, ToolDiscoverDto, ToolDto, ToolInfoDto, ToolInstallParams, ToolInstallResultDto,
    ToolListDto, ToolListParams, ToolParams, ToolPolicyParams, ToolRemoveParams,
    ToolRollbackParams, ToolSearchParams, ToolUpdateDto, ToolUpdateParams, ToolUpdatesDto,
    ToolUseParams, ToolVersionDto, ToolWhichParams, UpdatePolicy, WhichDto,
};
use forge_ipc::server::{Ctx, Router};
use forge_manifest::schema::tool::ServiceKind;
use forge_provider::layout::ExtensionLink;
use forge_provider::update::{ApplyRequest, AvailableUpdate, RollbackRequest};

use super::convert;
use super::route;
use crate::catalog::{self, ExtensionEntry, Installable, ToolEntry};
use crate::daemon::Daemon;
use crate::install;

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, tools_search, search);
    route!(router, daemon, tools_list, list);
    route!(router, daemon, tools_info, info);
    route!(router, daemon, tools_install, install_tools);
    route!(router, daemon, tools_remove, remove);
    route!(router, daemon, tools_update, update);
    route!(router, daemon, tools_check_updates, check_updates);
    route!(router, daemon, tools_rollback, rollback);
    route!(router, daemon, tools_policy, policy);
    route!(router, daemon, tools_discover, discover);
    route!(router, daemon, tools_use, use_version);
    route!(router, daemon, tools_which, which);
}

async fn search(daemon: Daemon, params: ToolSearchParams, _ctx: Ctx) -> Result<CatalogSearchDto> {
    let category = params
        .category
        .as_deref()
        .and_then(forge_registry::search::parse_category);
    let installed: BTreeSet<ToolId> = catalog::all_installed(&daemon.reader)
        .await?
        .into_iter()
        .map(|r| r.tool_id)
        .collect();
    let entries = daemon
        .registry
        .search(&params.text, category)
        .into_iter()
        .map(|hit| {
            let entry = hit.entry;
            // API 1.1 metadata (ADR-0041): what the index entry already says is filled here;
            // color, icon, platforms, sizes and updates come with M2-E2-T4.
            CatalogEntryDto {
                id: entry.id.clone(),
                name: entry.name().to_owned(),
                category: entry
                    .category()
                    .map(|c| format!("{c:?}").to_ascii_lowercase()),
                description: entry.summary().map(str::to_owned),
                tags: entry.tags().to_vec(),
                registry: entry.registry.clone(),
                versions: entry.latest.clone().into_iter().collect(),
                installed: installed.contains(&entry.id),
                kind: convert::catalog_kind(entry.kind()),
                summary: entry.summary().map(str::to_owned),
                color: None,
                icon_sha256: None,
                platforms: Vec::new(),
                native: true,
                official: entry.official,
                latest: entry.latest.clone(),
                update_available: None,
                size_estimate_bytes: None,
            }
        })
        .collect();
    Ok(CatalogSearchDto { entries })
}

async fn policy_of(daemon: &Daemon, tool: &ToolEntry) -> UpdatePolicy {
    let id = tool.manifest.metadata.id.clone();
    let stored = daemon
        .reader
        .query(move |conn| ToolVersionRepo::new(conn).update_policy(&id))
        .await
        .ok()
        .flatten();
    stored
        .as_deref()
        .and_then(convert::update_policy)
        .or_else(|| {
            let text = serde_json::to_value(tool.manifest.spec.update.policy)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            convert::update_policy(&text)
        })
        .unwrap_or(UpdatePolicy::Notify)
}

async fn tool_dto(
    daemon: &Daemon,
    tool: &ToolEntry,
    rows: Vec<ToolVersionRow>,
    updates: &[AvailableUpdate],
) -> ToolDto {
    let id = tool.manifest.metadata.id.clone();
    let current = daemon
        .layout
        .current_version(&id)
        .await
        .ok()
        .flatten()
        .map(|c| c.canonical_name);
    ToolDto {
        id: id.clone(),
        name: tool.manifest.metadata.name.clone(),
        category: serde_json::to_value(tool.manifest.metadata.category)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned)),
        description: tool.manifest.metadata.summary.clone(),
        installed: rows
            .iter()
            .map(|r| {
                convert::tool_version(r, current.as_deref() == Some(r.canonical_name.as_str()))
            })
            .collect(),
        current: current.clone(),
        policy: policy_of(daemon, tool).await,
        latest: tool.latest.clone(),
        update_available: updates.iter().find(|u| u.tool == id).map(|u| u.to.clone()),
        has_service: tool.manifest.spec.service.kind != ServiceKind::None,
    }
}

async fn list(daemon: Daemon, params: ToolListParams, _ctx: Ctx) -> Result<ToolListDto> {
    let installed = catalog::all_installed(&daemon.reader).await?;
    let updates = if params.updates {
        tokio::time::timeout(Duration::from_secs(1), daemon.updates.check(None))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut tools = Vec::new();
    for tool in catalog::tools(&daemon.registry) {
        let id = &tool.manifest.metadata.id;
        let mut rows: Vec<ToolVersionRow> = installed
            .iter()
            .filter(|r| &r.tool_id == id)
            .cloned()
            .collect();
        catalog::sort_newest_first(&tool.manifest, &mut rows);
        if params.installed && rows.is_empty() {
            continue;
        }
        let dto = tool_dto(&daemon, &tool, rows, &updates).await;
        if params.updates && dto.update_available.is_none() {
            continue;
        }
        tools.push(dto);
    }
    Ok(ToolListDto { tools })
}

async fn info(daemon: Daemon, params: ToolParams, _ctx: Ctx) -> Result<ToolInfoDto> {
    let tool = match catalog::require_installable(&daemon.registry, &params.tool)? {
        Installable::Tool(tool) => tool,
        Installable::Extension(ext) => return extension_info(&daemon, &ext).await,
    };
    let rows = catalog::installed(&daemon.reader, &params.tool, Some(&tool.manifest)).await?;
    let sites = daemon
        .reader
        .query(|conn| SiteRepo::new(conn).list())
        .await?;
    let plan = daemon.web.php_plan(&sites).await.unwrap_or_default();
    let used_by = sites
        .iter()
        .filter_map(|s| {
            let php = plan.get(&s.id)?;
            (php.version.tool_id == params.tool).then(|| SiteUsageDto {
                site_id: s.id,
                host: s.host.clone(),
                version: php.version.version.clone(),
            })
        })
        .collect();
    let available = tool
        .latest
        .iter()
        .map(|v| AvailableVersionDto {
            version: v.clone(),
            variants: Vec::new(),
            channel: Some("stable".into()),
        })
        .collect();
    // The API 1.1 metadata (Markdown description, links, variants, suggests, extensions
    // and stacks) is M2-E2-T4 (ADR-0041).
    Ok(ToolInfoDto {
        tool: tool_dto(&daemon, &tool, rows, &[]).await,
        available,
        used_by,
        kind: CatalogKind::Tool,
        description_md: None,
        homepage: None,
        docs: None,
        changelog_url: None,
        license: None,
        variants: Vec::new(),
        suggests: Vec::new(),
        extends: None,
        stack: None,
    })
}

/// The installed rows of extension `ext`, ordered by target (newest target build first,
/// rows whose target is gone last).
async fn extension_rows(daemon: &Daemon, ext: &ExtensionEntry) -> Result<Vec<ToolVersionRow>> {
    let manifest = &ext.manifest;
    let mut rows = catalog::installed(&daemon.reader, &manifest.metadata.id, None).await?;
    if let Some(target) = catalog::tool(&daemon.registry, &manifest.spec.extends) {
        let targets = catalog::installed(
            &daemon.reader,
            &manifest.spec.extends,
            Some(&target.manifest),
        )
        .await?;
        rows.sort_by_key(|r| {
            ExtensionLink::of(r)
                .and_then(|l| targets.iter().position(|t| t.id == l.id))
                .unwrap_or(usize::MAX)
        });
    }
    Ok(rows)
}

/// `tools.info` of an extension: `kind: extension`, `extends`, one installed row per
/// target version (its `variant` carries `target_tool`/`target_id`/`target_name`).
async fn extension_info(daemon: &Daemon, ext: &ExtensionEntry) -> Result<ToolInfoDto> {
    let manifest = &ext.manifest;
    let meta = &manifest.metadata;
    let rows = extension_rows(daemon, ext).await?;
    let available = ext
        .latest
        .iter()
        .map(|v| AvailableVersionDto {
            version: v.clone(),
            variants: Vec::new(),
            channel: Some("stable".into()),
        })
        .collect();
    Ok(ToolInfoDto {
        tool: ToolDto {
            id: meta.id.clone(),
            name: meta.name.clone(),
            category: serde_json::to_value(meta.category)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned)),
            description: meta.summary.clone(),
            installed: rows
                .iter()
                .map(|r| convert::tool_version(r, false))
                .collect(),
            current: None,
            // Extensions are not followed by the update engine (ADR-0043).
            policy: UpdatePolicy::Manual,
            latest: ext.latest.clone(),
            update_available: None,
            has_service: false,
        },
        available,
        used_by: Vec::new(),
        kind: CatalogKind::Extension,
        description_md: meta.description.clone(),
        homepage: meta.homepage.clone(),
        docs: meta.docs.clone(),
        changelog_url: None,
        license: meta.license.clone(),
        variants: Vec::new(),
        suggests: Vec::new(),
        extends: Some(manifest.spec.extends.clone()),
        stack: None,
    })
}

/// `--for` only means something for extensions.
fn target_only_for_extensions(id: &ToolId) -> ForgeError {
    ForgeError::InvalidInput(format!(
        "{id} is a tool: --for (target) only applies to extensions (kind: Extension)"
    ))
}

async fn install_tools(daemon: Daemon, params: ToolInstallParams, ctx: Ctx) -> Result<JobDto> {
    if params.specs.is_empty() {
        return Err(ForgeError::InvalidInput("nothing to install".into()));
    }
    let mut kinds = Vec::new();
    for spec in &params.specs {
        let (tool, _) = install::parse_spec(spec.as_str())?;
        let kind = catalog::require_installable(&daemon.registry, &tool)?;
        match &kind {
            Installable::Tool(_) if params.target.is_some() => {
                return Err(target_only_for_extensions(&tool));
            }
            Installable::Extension(_) if params.variant.is_some() => {
                return Err(ForgeError::InvalidInput(format!(
                    "{tool} is an extension: its build follows the target version (no --variant)"
                )));
            }
            _ => {}
        }
        kinds.push(kind);
    }
    // Targets that are already wrong fail before a job exists, unless the same request
    // installs the extended tool first (`php@8.4 xdebug`).
    let requested: BTreeSet<ToolId> = params
        .specs
        .iter()
        .filter_map(|s| install::parse_spec(s.as_str()).ok().map(|(t, _)| t))
        .collect();
    for kind in &kinds {
        let Installable::Extension(ext) = kind else {
            continue;
        };
        let extends = &ext.manifest.spec.extends;
        if requested.contains(extends) {
            continue;
        }
        let target_tool = catalog::require_tool(&daemon.registry, extends)?;
        let rows = catalog::installed(&daemon.reader, extends, Some(&target_tool.manifest)).await?;
        let target = params.target.as_ref().map(|t| t.as_str());
        install::extension_targets(&ext.manifest, &target_tool.manifest, &rows, target)?;
        if rows.is_empty() {
            return Err(ForgeError::NotFound(format!(
                "{} installs into {extends}, and no version of {extends} is installed \
                 (forge tool install {extends})",
                ext.manifest.metadata.id
            )));
        }
    }
    let name = format!(
        "tools.install {}",
        params
            .specs
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let d = daemon.clone();
    daemon.job(&ctx, name, move |job| async move {
        let mut installed = Vec::new();
        for (spec, kind) in params.specs.iter().zip(kinds) {
            job.check_cancelled()?;
            if let Installable::Extension(ext) = kind {
                let (_, version) = install::parse_spec(spec.as_str())?;
                let target = params.target.as_ref().map(|t| t.as_str());
                let rows =
                    install::install_extension(&d, &job, &ext, version.as_deref(), target).await?;
                installed.extend(rows.iter().map(|r| convert::tool_version(r, false)));
                continue;
            }
            let row =
                install::install_one(&d, &job, spec.as_str(), params.variant.as_deref()).await?;
            let current = d
                .layout
                .current_version(&row.tool_id)
                .await
                .ok()
                .flatten()
                .is_some_and(|c| c.canonical_name == row.canonical_name);
            installed.push(convert::tool_version(&row, current));
        }
        Ok(ToolInstallResultDto { installed })
    })
}

async fn remove(daemon: Daemon, params: ToolRemoveParams, ctx: Ctx) -> Result<JobDto> {
    if !params.confirm {
        return Err(ForgeError::InvalidInput(
            "removing a tool version is destructive: confirm must be true".into(),
        ));
    }
    let (tool_id, version) = install::parse_spec(params.spec.as_str())?;
    let tool = match catalog::require_installable(&daemon.registry, &tool_id)? {
        Installable::Tool(_) if params.target.is_some() => {
            return Err(target_only_for_extensions(&tool_id));
        }
        Installable::Tool(tool) => tool,
        Installable::Extension(ext) => {
            return remove_extension(daemon, ext, version, params, ctx).await;
        }
    };
    let rows = catalog::installed(&daemon.reader, &tool_id, Some(&tool.manifest)).await?;
    let targets: Vec<ToolVersionRow> = match version.as_deref() {
        Some(v) => {
            let row = catalog::pick(&tool.manifest, &rows, v)
                .cloned()
                .ok_or_else(|| ForgeError::NotFound(format!("{tool_id}@{v} is not installed")))?;
            vec![row]
        }
        None => rows.clone(),
    };
    if targets.is_empty() {
        return Err(ForgeError::NotFound(format!("{tool_id} is not installed")));
    }
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("tools.remove {}", params.spec),
        move |_job| async move {
            for row in targets {
                if let Some(pool) = d.pools.get(row.id) {
                    let _ = pool.stop(crate::pools::POOL_STOP_TIMEOUT).await;
                    d.pools.stop(row.id).await?;
                }
                d.layout
                    .remove_version(Arc::clone(&tool.manifest), row.id)
                    .await?;
            }
            Ok(Ack::ok())
        },
    )
}

/// `tools.remove` of an extension: the installations of `version` (all when `None`) in
/// the target version `params.target` names (every target when `None`), each with its
/// file and unshared rendered files (ADR-0043).
async fn remove_extension(
    daemon: Daemon,
    ext: ExtensionEntry,
    version: Option<String>,
    params: ToolRemoveParams,
    ctx: Ctx,
) -> Result<JobDto> {
    let manifest = Arc::clone(&ext.manifest);
    let id = manifest.metadata.id.clone();
    let extends = manifest.spec.extends.clone();
    let target_tool = catalog::require_tool(&daemon.registry, &extends)?;
    let target_rows =
        catalog::installed(&daemon.reader, &extends, Some(&target_tool.manifest)).await?;
    let target = params.target.as_ref().map(|t| t.as_str());
    let (targets, explicit) =
        install::extension_targets(&manifest, &target_tool.manifest, &target_rows, target)?;
    let rows: Vec<ToolVersionRow> = extension_rows(&daemon, &ext)
        .await?
        .into_iter()
        .filter(|r| {
            version
                .as_deref()
                .is_none_or(|v| r.version == v || r.version.starts_with(&format!("{v}.")))
        })
        .filter(|r| {
            !explicit || ExtensionLink::of(r).is_some_and(|l| targets.iter().any(|t| t.id == l.id))
        })
        .collect();
    if rows.is_empty() {
        let what = version.map_or_else(|| id.to_string(), |v| format!("{id}@{v}"));
        return Err(ForgeError::NotFound(match target {
            Some(t) if explicit => format!("{what} is not installed for {t}"),
            _ => format!("{what} is not installed"),
        }));
    }
    let d = daemon.clone();
    let name = match target {
        Some(t) => format!("tools.remove {} --for {t}", params.spec),
        None => format!("tools.remove {}", params.spec),
    };
    daemon.job(&ctx, name, move |_job| async move {
        for row in rows {
            d.layout
                .remove_extension(
                    Arc::clone(&manifest),
                    Arc::clone(&target_tool.manifest),
                    row.id,
                )
                .await?;
        }
        Ok(Ack::ok())
    })
}

fn update_dto(update: &AvailableUpdate) -> ToolUpdateDto {
    ToolUpdateDto {
        tool: update.tool.clone(),
        from: update.from.clone(),
        to: update.to.clone(),
        automatic: update.automatic,
    }
}

async fn check_updates(daemon: Daemon, params: ToolUpdateParams, ctx: Ctx) -> Result<JobDto> {
    let d = daemon.clone();
    daemon.job(&ctx, "tools.check_updates", move |_job| async move {
        let updates = d.updates.check(params.tool.as_ref()).await?;
        Ok(ToolUpdatesDto {
            updates: updates.iter().map(update_dto).collect(),
        })
    })
}

/// Waits for a job of another component; its failure keeps the code of the original
/// error (a `not_found` stays `not_found`, exit code 3) instead of becoming `internal`.
pub(super) async fn wait_job(
    daemon: &Daemon,
    id: forge_core::ids::JobId,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let handle = daemon
        .jobs
        .handle(id)
        .ok_or_else(|| ForgeError::Internal(format!("job {id} vanished")))?;
    let info = handle.wait(timeout).await?;
    match info.state {
        JobState::Done => Ok(info.result.unwrap_or_default()),
        JobState::Cancelled => Err(ForgeError::Cancelled(format!("job {id}"))),
        _ => Err(info.error.map_or_else(
            || ForgeError::Internal(format!("job {id} failed")),
            |e| ForgeError::from_code(&e.code, &e.message),
        )),
    }
}

async fn update(daemon: Daemon, params: ToolUpdateParams, ctx: Ctx) -> Result<JobDto> {
    let d = daemon.clone();
    daemon.job(&ctx, "tools.update", move |_job| async move {
        let updates = d.updates.check(params.tool.as_ref()).await?;
        let mut applied = Vec::new();
        for update in updates {
            let id = d.updates.apply(ApplyRequest::user(update.clone())).await?;
            wait_job(&d, id, Duration::from_secs(60 * 60)).await?;
            applied.push(update_dto(&update));
        }
        Ok(ToolUpdatesDto { updates: applied })
    })
}

async fn rollback(daemon: Daemon, params: ToolRollbackParams, ctx: Ctx) -> Result<JobDto> {
    let (tool_id, series) = install::parse_spec(params.spec.as_str())?;
    let series = series.ok_or_else(|| {
        ForgeError::InvalidInput("rollback needs a series: tool@series (php@8.3)".into())
    })?;
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("tools.rollback {}", params.spec),
        move |_job| async move {
            let id = d
                .updates
                .rollback(RollbackRequest::new(tool_id.clone(), series))
                .await?;
            let result = wait_job(&d, id, Duration::from_secs(30 * 60)).await?;
            let report: forge_provider::update::RollbackReport = serde_json::from_value(result)
                .map_err(|e| ForgeError::Internal(format!("rollback report: {e}")))?;
            let row = catalog::version(&d.reader, report.to_id).await?;
            Ok(convert::tool_version(&row, report.current_moved))
        },
    )
}

async fn policy(daemon: Daemon, params: ToolPolicyParams, _ctx: Ctx) -> Result<ToolDto> {
    let tool = catalog::require_tool(&daemon.registry, &params.tool)?;
    let id = params.tool.clone();
    let text = convert::update_policy_text(params.policy);
    let has_rows = !catalog::installed(&daemon.reader, &id, None)
        .await?
        .is_empty();
    if !has_rows {
        return Err(ForgeError::NotFound(format!(
            "{id} is not installed (policies apply to installed tools)"
        )));
    }
    let tid = id.clone();
    daemon
        .writer
        .execute(move |conn| ToolVersionRepo::new(conn).set_update_policy(&tid, Some(text)))
        .await?;
    let rows = catalog::installed(&daemon.reader, &id, Some(&tool.manifest)).await?;
    Ok(tool_dto(&daemon, &tool, rows, &[]).await)
}

async fn discover(daemon: Daemon, _params: NoParams, ctx: Ctx) -> Result<JobDto> {
    let d = daemon.clone();
    daemon.job(&ctx, "tools.discover", move |_job| async move {
        let manifests: Vec<_> = catalog::tools(&d.registry)
            .into_iter()
            .map(|t| (*t.manifest).clone())
            .collect();
        let found = forge_registry::discover(&d.root, &manifests).await;
        let report = forge_registry::import_discovered(&d.writer, &found).await?;
        let imported: BTreeSet<PathBuf> = report.imported.iter().cloned().collect();
        let rows = catalog::all_installed(&d.reader).await?;
        let registered = rows
            .iter()
            .filter(|r| {
                imported
                    .iter()
                    .any(|p| forge_core::paths::posix_string(p) == r.path.replace('\\', "/"))
            })
            .map(|r| convert::tool_version(r, false))
            .collect();
        let unknown = report
            .unknown_version
            .iter()
            .chain(&report.needs_confirmation)
            .map(|p| forge_core::paths::posix_string(p))
            .collect();
        if let Err(err) = d.services.ensure_defaults().await {
            tracing::warn!(%err, "default instances after discover");
        }
        Ok(ToolDiscoverDto {
            registered,
            unknown,
        })
    })
}

async fn use_version(daemon: Daemon, params: ToolUseParams, _ctx: Ctx) -> Result<ToolVersionDto> {
    let tool = catalog::require_tool(&daemon.registry, &params.tool)?;
    let report = daemon
        .layout
        .use_version(&tool.manifest, &params.version)
        .await?;
    let row = catalog::version(&daemon.reader, report.version_id).await?;
    Ok(convert::tool_version(&row, true))
}

/// The version a project folder selects through `install.version_files` (nearest file
/// in `dir` or above). Blocking.
fn project_version(files: &[String], dir: &std::path::Path) -> Option<(String, PathBuf)> {
    let mut current = Some(dir);
    while let Some(d) = current {
        for name in files {
            let path = d.join(name);
            if let Ok(text) = std::fs::read_to_string(&path) {
                let version = text.trim().trim_start_matches('v').to_owned();
                if !version.is_empty() {
                    return Some((version, path));
                }
            }
        }
        current = d.parent();
    }
    None
}

async fn which(daemon: Daemon, params: ToolWhichParams, _ctx: Ctx) -> Result<WhichDto> {
    let tool = catalog::require_tool(&daemon.registry, &params.tool)?;
    let rows = catalog::installed(&daemon.reader, &params.tool, Some(&tool.manifest)).await?;
    let current = daemon.layout.current_version(&params.tool).await?;
    let global = current
        .as_ref()
        .and_then(|c| rows.iter().find(|r| r.canonical_name == c.canonical_name))
        .map(|r| convert::tool_version(r, true));
    let (mut project, mut project_source) = (None, None);
    if let Some(dir) = params.project_dir.clone() {
        let files = tool.manifest.spec.install.version_files.clone();
        let found = tokio::task::spawn_blocking(move || {
            project_version(&files, std::path::Path::new(&dir))
        })
        .await
        .ok()
        .flatten();
        if let Some((version, source)) = found {
            if let Some(row) = catalog::pick(&tool.manifest, &rows, &version) {
                let is_current = current
                    .as_ref()
                    .is_some_and(|c| c.canonical_name == row.canonical_name);
                project = Some(convert::tool_version(row, is_current));
                project_source = Some(forge_core::paths::posix_string(&source));
            }
        }
    }
    let chosen = project.as_ref().or(global.as_ref());
    let executable = chosen.and_then(|v| {
        let shim = tool.manifest.spec.install.shims.first()?;
        let name = match shim {
            forge_manifest::schema::tool::Shim::Name(name) => name.clone(),
            forge_manifest::schema::tool::Shim::Detailed(spec) => spec.name.clone(),
        };
        Some(format!("{}/{}.exe", v.path, name))
    });
    Ok(WhichDto {
        tool: params.tool,
        global,
        project,
        project_source,
        executable,
    })
}
