//! `php.*`: installed versions, pools, extensions, Xdebug and `php.ini` files. "PHP" is the
//! tool whose manifest declares `service.kind: pool` (no tool id is hardcoded); its
//! debugger is the Extension whose `toggle` declares `modes` ([`crate::php_ext`]).
//!
//! `php.ext_enable|disable` and `php.xdebug` (jobs, M2-E2-T9) check what they can before
//! the job (the DLL exists, the client port is free or already ours: `not_found`,
//! `port_in_use`, `conflict`), then: install a missing Xdebug (`php.xdebug`, through
//! [`install::install_extension`], same job, its progress) → reserve the client port →
//! store the choices under `Daemon::settings_turn` → render through the pool render queue
//! ([`crate::pools::PoolManager::render_config`]) → recycle the running pools of the
//! series when something changed. Run twice, the second run changes nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::ids::VersionId;
use forge_core::jobs::{JobContext, JobProgress};
use forge_core::model::port::{PortProtocol, ReservationOwner};
use forge_core::store::{ToolSeriesSettingsRepo, ToolVersionRow};
use forge_ipc::api::methods::{
    php_ext_disable, php_ext_enable, php_ext_list, php_ini, php_ini_open, php_list,
    php_pool_status, php_xdebug,
};
use forge_ipc::api::{
    FileRefDto, JobDto, NoParams, PhpExtDto, PhpExtListDto, PhpExtParams, PhpIniDto,
    PhpPoolStatusParams, PhpVersionDto, PhpVersionListDto, PhpVersionParams, PhpXdebugParams,
    PoolDto, PoolListDto, XdebugMode,
};
use forge_ipc::server::{Ctx, Router};

use super::convert;
use super::route;
use crate::catalog::{self, ExtensionEntry, ToolEntry};
use crate::daemon::Daemon;
use crate::install;
use crate::php_ext::{self, DebuggerState};

/// Wait for another settings change to store and render.
const TURN_TIMEOUT: Duration = Duration::from_secs(30);
/// Ask timeout of a pool recycle (the rolling replacement goes on after the answer).
const POOL_RELOAD_TIMEOUT: Duration = Duration::from_secs(10);
/// Budget of the OS port table read of the client port pre-check.
const PORT_TABLE_BUDGET: Duration = Duration::from_millis(900);

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, php_list, list);
    route!(router, daemon, php_pool_status, pool_status);
    route!(router, daemon, php_ext_list, ext_list);
    route!(router, daemon, php_ext_enable, ext_enable);
    route!(router, daemon, php_ext_disable, ext_disable);
    route!(router, daemon, php_xdebug, xdebug);
    route!(router, daemon, php_ini, ini);
    route!(router, daemon, php_ini_open, ini);
}

fn pool_tool(daemon: &Daemon) -> Result<ToolEntry> {
    catalog::pool_tools(&daemon.registry)
        .into_iter()
        .next()
        .ok_or_else(|| ForgeError::NotFound("no FastCGI runtime (PHP) in the catalog".into()))
}

async fn rows(daemon: &Daemon, tool: &ToolEntry) -> Result<Vec<ToolVersionRow>> {
    catalog::installed(
        &daemon.reader,
        &tool.manifest.metadata.id,
        Some(&tool.manifest),
    )
    .await
}

async fn pick(daemon: &Daemon, version: &str) -> Result<(ToolEntry, ToolVersionRow)> {
    let tool = pool_tool(daemon)?;
    let rows = rows(daemon, &tool).await?;
    let row = catalog::pick(&tool.manifest, &rows, version)
        .cloned()
        .ok_or_else(|| ForgeError::NotFound(format!("PHP {version} is not installed")))?;
    Ok((tool, row))
}

/// The debugger state of every installed version of the pool tool (empty without one):
/// `PhpVersionDto.xdebug` and `PoolDto.xdebug`.
pub(crate) async fn debugger_states(daemon: &Daemon) -> BTreeMap<VersionId, DebuggerState> {
    let Ok(tool) = pool_tool(daemon) else {
        return BTreeMap::new();
    };
    let Ok(rows) = rows(daemon, &tool).await else {
        return BTreeMap::new();
    };
    php_ext::debugger_states(&daemon.reader, &daemon.registry, &tool.manifest, &rows)
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(%err, "xdebug state not read");
            BTreeMap::new()
        })
}

fn xdebug_on(states: &BTreeMap<VersionId, DebuggerState>, version: VersionId) -> bool {
    states.get(&version).is_some_and(|s| s.enabled)
}

async fn list(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<PhpVersionListDto> {
    let Ok(tool) = pool_tool(&daemon) else {
        return Ok(PhpVersionListDto {
            versions: Vec::new(),
        });
    };
    let rows = rows(&daemon, &tool).await?;
    let current = daemon
        .layout
        .current_version(&tool.manifest.metadata.id)
        .await
        .ok()
        .flatten()
        .map(|c| c.canonical_name);
    let sites = daemon.sites.list().await?;
    let plan = daemon.web.php_plan(&sites).await.unwrap_or_default();
    let statuses = daemon.pools.statuses().await;
    let states = debugger_states(&daemon).await;
    let versions = rows
        .iter()
        .map(|row| {
            let count = plan.values().filter(|p| p.version.id == row.id).count();
            let count = u32::try_from(count).unwrap_or(u32::MAX);
            PhpVersionDto {
                version_id: row.id,
                version: row.version.clone(),
                series: catalog::series_of(&tool.manifest, &row.version),
                thread_safety: row.variant.get("ts").cloned().unwrap_or_default(),
                compiler: row.variant.get("compiler").cloned(),
                arch: row
                    .variant
                    .get("arch")
                    .cloned()
                    .unwrap_or_else(|| "x64".into()),
                is_current_cli: current.as_deref() == Some(row.canonical_name.as_str()),
                pool: statuses
                    .iter()
                    .find(|s| s.version_id == row.id)
                    .map(|s| convert::pool(s, count, xdebug_on(&states, row.id))),
                sites: count,
                xdebug: convert::xdebug_mode(states.get(&row.id)),
            }
        })
        .collect();
    Ok(PhpVersionListDto { versions })
}

async fn pool_status(
    daemon: Daemon,
    params: PhpPoolStatusParams,
    _ctx: Ctx,
) -> Result<PoolListDto> {
    let sites = daemon.sites.list().await?;
    let plan = daemon.web.php_plan(&sites).await.unwrap_or_default();
    let filter = match &params.version {
        Some(v) => Some(pick(&daemon, v).await?.1.id),
        None => None,
    };
    let states = debugger_states(&daemon).await;
    let pools = daemon
        .pools
        .statuses()
        .await
        .iter()
        .filter(|s| filter.is_none_or(|id| id == s.version_id))
        .map(|s| {
            let count = plan
                .values()
                .filter(|p| p.version.id == s.version_id)
                .count();
            convert::pool(
                s,
                u32::try_from(count).unwrap_or(u32::MAX),
                xdebug_on(&states, s.version_id),
            )
        })
        .collect();
    Ok(PoolListDto { pools })
}

/// Stored settings of the pool tool's series (`ext_<name>` choices among them).
async fn series_stored(
    daemon: &Daemon,
    tool: &ToolEntry,
    series: &str,
) -> Result<BTreeMap<String, String>> {
    php_ext::stored(&daemon.reader, &tool.manifest.metadata.id, series).await
}

/// Zend extensions the manifest loads for `row` (`opcache` before 8.5).
fn zend_of(tool: &ToolEntry, row: &ToolVersionRow) -> BTreeSet<String> {
    let Some(config) = tool.manifest.spec.config.extensions.as_ref() else {
        return BTreeSet::new();
    };
    let scheme = tool.manifest.spec.versioning.scheme;
    forge_manifest::version::Version::parse(scheme, &row.version)
        .map(|v| config.zend_extensions_for(scheme, &v).into_iter().collect())
        .unwrap_or_default()
}

/// The real state of the extensions of `row`: the files of its extension folder
/// (`config.extensions.pattern`) with `defaults_on` and the user's choices of the series,
/// plus the registry extensions installed into it (`bundled: false`, off by default).
async fn ext_list_of(
    daemon: &Daemon,
    tool: &ToolEntry,
    row: &ToolVersionRow,
) -> Result<PhpExtListDto> {
    let Some(config) = tool.manifest.spec.config.extensions.clone() else {
        return Ok(PhpExtListDto {
            version: row.version.clone(),
            extensions: Vec::new(),
        });
    };
    let series = catalog::series_of(&tool.manifest, &row.version);
    let stored = series_stored(daemon, tool, &series).await?;
    let choices = php_ext::choices(&stored);
    let linked = php_ext::linked(&daemon.reader, &daemon.registry, &tool.manifest, row.id).await?;
    let dir = config.dir_in(Path::new(&row.path));
    let scan = config.clone();
    let names = tokio::task::spawn_blocking(move || {
        let mut names = BTreeSet::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let file = entry.file_name().to_string_lossy().into_owned();
                if let Some(name) = scan.name_of(&file) {
                    names.insert(name);
                }
            }
        }
        names
    })
    .await
    .unwrap_or_default();
    let zend = zend_of(tool, row);
    let registry: BTreeSet<String> = linked.iter().map(|l| l.name.clone()).collect();
    let mut model = forge_render::PhpModel {
        series,
        extensions: config.defaults_on.clone(),
        zend_extensions: zend.iter().cloned().collect(),
        settings: BTreeMap::new(),
        xdebug: None,
    };
    php_ext::apply_choices(&choices, &registry, &mut model);
    let mut extensions: Vec<PhpExtDto> = names
        .into_iter()
        .filter(|name| !registry.contains(name))
        .map(|name| PhpExtDto {
            enabled: model.extensions.contains(&name) || model.zend_extensions.contains(&name),
            zend: zend.contains(&name),
            bundled: true,
            name,
        })
        .collect();
    extensions.extend(linked.iter().map(|l| PhpExtDto {
        name: l.name.clone(),
        enabled: choices.get(&l.name) == Some(&true),
        bundled: false,
        zend: php_ext::is_zend(&l.entry.manifest),
    }));
    extensions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(PhpExtListDto {
        version: row.version.clone(),
        extensions,
    })
}

async fn ext_list(daemon: Daemon, params: PhpVersionParams, _ctx: Ctx) -> Result<PhpExtListDto> {
    let (tool, row) = pick(&daemon, &params.version).await?;
    ext_list_of(&daemon, &tool, &row).await
}

/// The installed versions of `series` of the pool tool.
fn series_versions(tool: &ToolEntry, rows: &[ToolVersionRow], series: &str) -> Vec<ToolVersionRow> {
    rows.iter()
        .filter(|r| catalog::series_of(&tool.manifest, &r.version) == series)
        .cloned()
        .collect()
}

async fn settings_turn(d: &Daemon) -> Result<tokio::sync::OwnedSemaphorePermit> {
    tokio::time::timeout(TURN_TIMEOUT, Arc::clone(&d.settings_turn).acquire_owned())
        .await
        .map_err(|_| ForgeError::Timeout {
            operation: "waiting for another settings change".into(),
            after: TURN_TIMEOUT,
        })?
        .map_err(|_| ForgeError::Internal("settings turn closed".into()))
}

/// Applies `changes` to the stored settings of `tool`@`series`; whether anything changed.
async fn store(
    d: &Daemon,
    tool: &forge_core::ids::ToolId,
    series: &str,
    changes: BTreeMap<String, Option<String>>,
) -> Result<bool> {
    let before = php_ext::stored(&d.reader, tool, series).await?;
    let changes: BTreeMap<String, Option<String>> = changes
        .into_iter()
        .filter(|(key, value)| before.get(key) != value.as_ref())
        .collect();
    if changes.is_empty() {
        return Ok(false);
    }
    let (id, s) = (tool.clone(), series.to_owned());
    d.writer
        .execute(move |conn| ToolSeriesSettingsRepo::new(conn).apply(&id, &s, &changes))
        .await?;
    Ok(true)
}

/// Renders the series files with the version whose pool serves (its folder is in
/// `extension_dir`), else `row`, through the render queue; recycles the running pools of
/// the series when `changed` or a file was written. The settings `turn` is released once
/// the files are written (the recycle runs outside it, like `services.set`). Returns
/// whether the pools were recycled.
async fn render_and_recycle(
    d: &Daemon,
    job: &JobContext,
    turn: tokio::sync::OwnedSemaphorePermit,
    versions: &[ToolVersionRow],
    row: &ToolVersionRow,
    changed: bool,
) -> Result<bool> {
    let running = super::settings::running_pools(d, versions).await;
    let version = crate::pools::render_version(&running, row.id);
    let applied = d.pools.render_config(version).await;
    drop(turn);
    let applied = applied?;
    if !(changed || applied.changed()) || running.is_empty() {
        return Ok(false);
    }
    for handle in &running {
        job.progress(
            JobProgress::new()
                .pct(90.0)
                .message(format!("recycling pool {}", handle.upstream_name())),
        );
        let replaced = handle.reload(POOL_RELOAD_TIMEOUT).await?;
        tracing::info!(pool = %handle.upstream_name(), version = %handle.spec().version, replaced, "pool recycled for an extension change");
    }
    Ok(true)
}

/// A registry extension or a bundled one, as `php.ext_enable|disable` sees it.
struct Target {
    name: String,
    /// On when nothing is stored.
    default_on: bool,
}

async fn ext_target(
    daemon: &Daemon,
    tool: &ToolEntry,
    row: &ToolVersionRow,
    raw: &str,
) -> Result<Target> {
    let config = tool
        .manifest
        .spec
        .config
        .extensions
        .clone()
        .ok_or_else(|| {
            ForgeError::InvalidInput(format!(
                "{} declares no config.extensions",
                tool.manifest.metadata.id
            ))
        })?;
    let name = php_ext::parse_name(raw)?;
    let linked = php_ext::linked(&daemon.reader, &daemon.registry, &tool.manifest, row.id).await?;
    if let Some(l) = linked.iter().find(|l| l.name == name) {
        if !php_ext::is_debugger(&l.entry.manifest) {
            return Err(ForgeError::InvalidInput(format!(
                "{name}: Forgebox does not write the toggle of the registry extension {} yet",
                l.entry.manifest.metadata.id
            )));
        }
        return Ok(Target {
            name,
            default_on: false,
        });
    }
    let file = config.file_of(Path::new(&row.path), &name);
    let present = tokio::fs::try_exists(&file).await.unwrap_or(false)
        && tokio::fs::metadata(&file).await.is_ok_and(|m| m.is_file());
    if !present {
        return Err(ForgeError::NotFound(format!(
            "extension {name}: {} does not exist (not shipped with PHP {}, or built in)",
            forge_core::paths::posix_string(&file),
            row.version
        )));
    }
    let default_on = config.defaults_on.contains(&name) || zend_of(tool, row).contains(&name);
    Ok(Target { name, default_on })
}

async fn ext_toggle(
    daemon: Daemon,
    params: PhpExtParams,
    ctx: Ctx,
    enable: bool,
) -> Result<JobDto> {
    let (tool, row) = pick(&daemon, &params.version).await?;
    let target = ext_target(&daemon, &tool, &row, &params.extension).await?;
    let series = catalog::series_of(&tool.manifest, &row.version);
    let versions = series_versions(&tool, &rows(&daemon, &tool).await?, &series);
    let verb = if enable { "enable" } else { "disable" };
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("php.ext_{verb} {} {}", row.version, target.name),
        move |job| async move {
            let turn = settings_turn(&d).await?;
            let key = php_ext::choice_key(&target.name);
            let change = php_ext::change_for(target.default_on, enable);
            let changed = store(
                &d,
                &tool.manifest.metadata.id,
                &series,
                BTreeMap::from([(key, change)]),
            )
            .await?;
            let recycled = render_and_recycle(&d, &job, turn, &versions, &row, changed).await?;
            tracing::info!(php = %row.version, extension = %target.name, enable, changed, recycled, "php extension toggled");
            ext_list_of(&d, &tool, &row).await
        },
    )
}

async fn ext_enable(daemon: Daemon, params: PhpExtParams, ctx: Ctx) -> Result<JobDto> {
    ext_toggle(daemon, params, ctx, true).await
}

async fn ext_disable(daemon: Daemon, params: PhpExtParams, ctx: Ctx) -> Result<JobDto> {
    ext_toggle(daemon, params, ctx, false).await
}

/// Refuses a client port another owner reserved (`conflict`) or a process listens on
/// (`port_in_use` with `pid`/`exe`), unless `owner` already has it (then an IDE listening
/// on it is the expected state). The job's `reserve` checks again atomically.
async fn check_client_port(daemon: &Daemon, port: u16, owner: &ReservationOwner) -> Result<()> {
    if let Some(reserved) = daemon.ports.reservation_of(PortProtocol::Tcp, port).await? {
        if reserved.owner == *owner {
            return Ok(());
        }
        let names = super::services::instance_names(daemon);
        return Err(ForgeError::Conflict(format!(
            "port {port} is reserved for {}",
            super::services::owner_name(&names, &reserved.owner)
        )));
    }
    let table = Arc::clone(daemon.platform.port_table());
    let listeners = match forge_core::runtime::spawn_blocking_timeout(
        "reading the port table",
        PORT_TABLE_BUDGET,
        move || table.listeners(),
    )
    .await
    {
        Ok(listeners) => listeners,
        Err(err) => {
            tracing::warn!(%err, "port table not read; the reservation decides");
            return Ok(());
        }
    };
    if let Some(holder) = listeners.iter().find(|l| l.port == port) {
        let exe = holder
            .exe_path
            .as_deref()
            .map(forge_core::paths::posix_string);
        return Err(ForgeError::PortInUse {
            port,
            owner: Some(match &exe {
                Some(exe) => format!("{exe} (PID {})", holder.pid),
                None => format!("PID {}", holder.pid),
            }),
            pid: Some(holder.pid),
            exe,
        });
    }
    Ok(())
}

/// The pool of `row` (its actor's status, or a stopped one from its spec).
async fn pool_of(
    d: &Daemon,
    tool: &ToolEntry,
    row: &ToolVersionRow,
    xdebug: bool,
) -> Result<PoolDto> {
    let sites = d.sites.list().await.unwrap_or_default();
    let plan = d.web.php_plan(&sites).await.unwrap_or_default();
    let count = plan.values().filter(|p| p.version.id == row.id).count();
    let count = u32::try_from(count).unwrap_or(u32::MAX);
    if let Some(handle) = d.pools.get(row.id) {
        if let Ok(status) = handle.status(crate::pools::POOL_QUICK).await {
            return Ok(convert::pool(&status, count, xdebug));
        }
    }
    let spec = d.pools.spec(&tool.manifest, row)?;
    Ok(convert::stopped_pool(&spec, count, xdebug))
}

/// Whether `ext` is installed into `row` (its row and its file).
async fn installed_into(
    d: &Daemon,
    tool: &ToolEntry,
    ext: &ExtensionEntry,
    row: &ToolVersionRow,
) -> Result<bool> {
    let linked = php_ext::linked(&d.reader, &d.registry, &tool.manifest, row.id).await?;
    let Some(found) = linked
        .iter()
        .find(|l| l.entry.manifest.metadata.id == ext.manifest.metadata.id)
    else {
        return Ok(false);
    };
    Ok(tokio::fs::try_exists(PathBuf::from(&found.row.path))
        .await
        .unwrap_or(false))
}

/// `php.xdebug` (job): see the module docs. `off` turns the extension off and keeps its
/// port reserved; `on`/`trigger` store the manifest preset of that mode.
async fn xdebug(daemon: Daemon, params: PhpXdebugParams, ctx: Ctx) -> Result<JobDto> {
    let (tool, row) = pick(&daemon, &params.version).await?;
    let tool_id = tool.manifest.metadata.id.clone();
    let ext = php_ext::debugger(&daemon.registry, &tool_id).ok_or_else(|| {
        ForgeError::NotFound(format!(
            "no debugger extension (toggle.modes) for {tool_id} in the loaded registries"
        ))
    })?;
    let ext_id = ext.manifest.metadata.id.clone();
    let series = catalog::series_of(&tool.manifest, &row.version);
    let mode = match params.mode {
        XdebugMode::Off => None,
        XdebugMode::On => Some("on"),
        XdebugMode::Trigger => Some("trigger"),
    };
    let mut changes = match mode {
        Some(mode) => php_ext::mode_changes(&ext.manifest, mode)?,
        None => BTreeMap::new(),
    };
    if let Some(port) = params.port {
        let setting = php_ext::reserved_port_setting(&ext.manifest).ok_or_else(|| {
            ForgeError::InvalidInput(format!("{ext_id} has no client port setting"))
        })?;
        if port == 0 {
            return Err(ForgeError::InvalidInput("port: 0 is not a port".into()));
        }
        changes.insert(setting.key.clone(), Some(port.to_string()));
    }
    let owner = php_ext::client_port_owner(&tool_id, &series, &ext_id)?;
    // The port the enabled extension will use: the one asked for, else the stored one,
    // else the manifest default.
    let reserve = match mode {
        Some(_) => {
            let mut after = php_ext::stored(&daemon.reader, &ext_id, &series).await?;
            for (key, value) in &changes {
                match value {
                    Some(v) => after.insert(key.clone(), v.clone()),
                    None => after.remove(key),
                };
            }
            php_ext::effective_port(&ext.manifest, &after)
        }
        None => None,
    };
    if let Some(port) = reserve {
        check_client_port(&daemon, port, &owner).await?;
    }
    let versions = series_versions(&tool, &rows(&daemon, &tool).await?, &series);
    let name = php_ext::ext_name(tool.manifest.spec.config.extensions.as_ref(), &ext.manifest);
    let label = format!("php.xdebug {} {}", row.version, mode.unwrap_or("off"));
    let d = daemon.clone();
    daemon.job(&ctx, label, move |job| async move {
        // 1. The extension, installed into this version when missing (its own progress).
        if !installed_into(&d, &tool, &ext, &row).await? {
            if mode.is_none() {
                job.progress(JobProgress::new().pct(100.0).message(format!(
                    "{ext_id} is not installed into {tool_id} {}: nothing to turn off",
                    row.version
                )));
                return pool_of(&d, &tool, &row, false).await;
            }
            job.progress(JobProgress::new().pct(1.0).message(format!(
                "{ext_id} is not installed into {tool_id} {}: installing",
                row.version
            )));
            let target = format!("{tool_id}@{}", row.version);
            install::install_extension(&d, &job, &ext, None, Some(&target)).await?;
        }
        job.check_cancelled()?;
        // 2. The client port, reserved for this series (moved when it changed).
        if let Some(port) = reserve {
            d.ports.reserve(port, None, owner.clone()).await?;
        }
        // 3. Store and render, one settings change at a time.
        let turn = settings_turn(&d).await?;
        let mut changed = store(&d, &ext_id, &series, changes).await?;
        let choice = php_ext::change_for(false, mode.is_some());
        changed |= store(
            &d,
            &tool_id,
            &series,
            BTreeMap::from([(php_ext::choice_key(&name), choice)]),
        )
        .await?;
        let recycled = render_and_recycle(&d, &job, turn, &versions, &row, changed).await?;
        tracing::info!(php = %row.version, mode = mode.unwrap_or("off"), port = ?reserve, changed, recycled, "xdebug set");
        pool_of(&d, &tool, &row, mode.is_some()).await
    })
}

/// `php.ini` (the user's), then the managed `conf.d/*.ini` overlays.
async fn ini(daemon: Daemon, params: PhpVersionParams, _ctx: Ctx) -> Result<PhpIniDto> {
    let (tool, row) = pick(&daemon, &params.version).await?;
    let series = catalog::series_of(&tool.manifest, &row.version);
    let dir = daemon
        .root
        .etc()
        .join(tool.manifest.metadata.id.as_str())
        .join(&series);
    let root = daemon.root.path().to_path_buf();
    let files = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let rel = |p: &std::path::Path| {
            p.strip_prefix(&root)
                .ok()
                .map(forge_core::paths::posix_string)
        };
        let ini = dir.join("php.ini");
        out.push(FileRefDto {
            role: "php.ini".into(),
            path: forge_core::paths::posix_string(&ini),
            fs_path: rel(&ini),
            managed: false,
        });
        if let Ok(entries) = std::fs::read_dir(dir.join("conf.d")) {
            let mut overlays: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "ini"))
                .collect();
            overlays.sort();
            for path in overlays {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.push(FileRefDto {
                    role: format!("conf.d/{name}"),
                    path: forge_core::paths::posix_string(&path),
                    fs_path: rel(&path),
                    managed: true,
                });
            }
        }
        out
    })
    .await
    .unwrap_or_default();
    Ok(PhpIniDto {
        version: row.version,
        files,
    })
}
