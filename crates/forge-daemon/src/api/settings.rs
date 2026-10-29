//! `services.settings` and `services.set` (docs/04 §1.3, ADR-0041 §6, ADR-0042): the quick
//! settings of an instance (`mysql/default`) or of a PHP series (`php@8.4`), and the
//! reserved keys `ports.<name>` that move a named port of an instance ("Trocar porta").
//!
//! `services.set` checks everything before it writes anything: every key and value
//! against the manifest (`invalid_input` naming the key), the file paths, the managed
//! blocks it is going to rewrite (a block edited by hand → `conflict`, the file untouched)
//! and the ports (`port_in_use` with the holder's `pid`/`exe`, `conflict` when another
//! owner has it reserved). The job then moves the reservations
//! (`PortAllocator::reserve`), stores the values (`instances.settings` or
//! `tool_series_settings`), renders the files (`config.rendered`) and applies them: the
//! manifest's `apply` decides — `reload` recycles the PHP pool or runs the service's
//! reload, `restart` goes through the supervisor (validated first), a moved port rebuilds
//! the instance on its new spec. Store and render take `Daemon::settings_turn`; the
//! restart runs outside it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::ids::{InstanceId, ToolId};
use forge_core::model::port::PortProtocol as StoredProtocol;
use forge_core::store::{ToolSeriesSettingsRepo, ToolVersionRow};
use forge_fcgi::PoolHandle;
use forge_ipc::api::methods::{services_set, services_settings};
use forge_ipc::api::{
    FileRefDto, JobDto, PortProtocol, ServicePortSettingDto, ServiceSetParams, ServiceSettingsDto,
    ServiceStatusDto, ServiceTarget, ServiceTargetParams,
};
use forge_ipc::server::{Ctx, Router};
use forge_manifest::schema::common::Apply;
use forge_manifest::schema::tool::{ServiceKind, ToolManifest};
use tokio::sync::OwnedSemaphorePermit;

use super::route;
use crate::catalog::{self, ToolEntry};
use crate::daemon::Daemon;
use crate::install;
use crate::pools::POOL_QUICK;
use crate::services::{Entry, instance_owner};
use crate::settings::{self, Changes};
use crate::vars::RuntimeVars;

/// Wait for another `services.set` to store and render.
const TURN_TIMEOUT: Duration = Duration::from_secs(30);
/// Budget of the OS port table read of the pre-check.
const PORT_TABLE_BUDGET: Duration = Duration::from_millis(900);
/// Budget of the managed-block check of the files about to be written.
const DRIFT_BUDGET: Duration = Duration::from_secs(5);
/// Ask timeout of a pool recycle (the rolling replacement goes on after the answer).
const POOL_RELOAD_TIMEOUT: Duration = Duration::from_secs(10);

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, services_settings, read);
    route!(router, daemon, services_set, set);
}

/// What a settings target designates.
#[derive(Clone)]
enum Scope {
    /// An instance (`mysql/default`).
    Instance(Box<Entry>),
    /// The installed versions of one series of a pool tool (`php@8.4`), newest first.
    Series {
        tool: ToolEntry,
        series: String,
        versions: Vec<ToolVersionRow>,
    },
}

impl Scope {
    fn label(&self) -> String {
        match self {
            Self::Instance(entry) => entry.label(),
            Self::Series { tool, series, .. } => format!("{}@{series}", tool.manifest.metadata.id),
        }
    }

    fn manifest(&self) -> Result<Arc<ToolManifest>> {
        match self {
            Self::Instance(entry) => entry.manifest.clone().ok_or_else(|| {
                ForgeError::ConfigInvalid(format!(
                    "{}: {}",
                    entry.label(),
                    entry.error.clone().unwrap_or_else(|| "no manifest".into())
                ))
            }),
            Self::Series { tool, .. } => Ok(Arc::clone(&tool.manifest)),
        }
    }

    /// The settings stored now (read again: another client may have changed them).
    async fn stored(&self, daemon: &Daemon) -> Result<BTreeMap<String, String>> {
        match self {
            Self::Instance(entry) => Ok(daemon
                .services
                .get(entry.row.id)
                .map_or_else(|| entry.row.settings.clone(), |e| e.row.settings)),
            Self::Series { tool, series, .. } => {
                let (id, series) = (tool.manifest.metadata.id.clone(), series.clone());
                daemon
                    .reader
                    .query(move |conn| ToolSeriesSettingsRepo::new(conn).get(&id, &series))
                    .await
            }
        }
    }

    /// Variables of the manifest strings (config file paths, `{{ value }}`).
    fn vars(&self, daemon: &Daemon) -> Result<RuntimeVars> {
        match self {
            Self::Instance(entry) => entry.vars.clone().ok_or_else(|| {
                ForgeError::ConfigInvalid(format!(
                    "{}: {}",
                    entry.label(),
                    entry.error.clone().unwrap_or_else(|| "not resolved".into())
                ))
            }),
            Self::Series { tool, versions, .. } => {
                let row = versions.first().ok_or_else(|| {
                    ForgeError::NotFound(format!("{} is not installed", self.label()))
                })?;
                let mut vars = RuntimeVars::new(daemon.layout.vars_for(&tool.manifest, row));
                vars.prefs = daemon.prefs();
                Ok(vars)
            }
        }
    }

    /// Named ports an instance can move (`ports.<name>`); none for a series.
    fn port_names(&self, manifest: &ToolManifest) -> Vec<String> {
        match self {
            Self::Instance(_) => manifest
                .spec
                .service
                .daemon
                .as_ref()
                .map(|d| d.ports.iter().map(|p| p.name.clone()).collect())
                .unwrap_or_default(),
            Self::Series { .. } => Vec::new(),
        }
    }
}

/// A pool tool's series as a scope (`rows` newest first).
fn series_scope(tool: ToolEntry, rows: &[ToolVersionRow], series: String) -> Scope {
    let versions = rows
        .iter()
        .filter(|r| catalog::series_of(&tool.manifest, &r.version) == series)
        .cloned()
        .collect();
    Scope::Series {
        tool,
        series,
        versions,
    }
}

/// `tool/name`, an instance ULID, a tool with one instance, `tool@<series|version>` of a
/// pool tool, or a pool tool with one installed series.
async fn resolve(daemon: &Daemon, target: &str) -> Result<Scope> {
    let target = target.trim();
    if target.contains('@') {
        let (tool_id, spec) = install::parse_spec(target)?;
        let tool = catalog::require_tool(&daemon.registry, &tool_id)?;
        if tool.manifest.spec.service.kind != ServiceKind::Pool {
            return Err(ForgeError::InvalidInput(format!(
                "{target}: the settings of {tool_id} belong to its instances ({tool_id}/<name>)"
            )));
        }
        let rows = catalog::installed(&daemon.reader, &tool_id, Some(&tool.manifest)).await?;
        let spec = spec.unwrap_or_default();
        let row = catalog::pick(&tool.manifest, &rows, &spec)
            .ok_or_else(|| ForgeError::NotFound(format!("{target} is not installed")))?;
        let series = catalog::series_of(&tool.manifest, &row.version);
        return Ok(series_scope(tool, &rows, series));
    }
    let err = match daemon.services.resolve(target) {
        Ok(entries) => {
            return match entries.as_slice() {
                [one] => Ok(Scope::Instance(Box::new(one.clone()))),
                _ => Err(ForgeError::InvalidInput(format!(
                    "{target} matches {} instances; use tool/name",
                    entries.len()
                ))),
            };
        }
        Err(err) => err,
    };
    // A pool tool by its id: its only installed series.
    let Some(tool) = ToolId::new(target)
        .ok()
        .and_then(|id| catalog::tool(&daemon.registry, &id))
        .filter(|t| t.manifest.spec.service.kind == ServiceKind::Pool)
    else {
        return Err(err);
    };
    let id = tool.manifest.metadata.id.clone();
    let rows = catalog::installed(&daemon.reader, &id, Some(&tool.manifest)).await?;
    let mut all: Vec<String> = rows
        .iter()
        .map(|r| catalog::series_of(&tool.manifest, &r.version))
        .collect();
    all.dedup();
    match all.as_slice() {
        [] => Err(ForgeError::NotFound(format!("{target} is not installed"))),
        [one] => {
            let series = one.clone();
            Ok(series_scope(tool, &rows, series))
        }
        many => Err(ForgeError::InvalidInput(format!(
            "{target} has several series ({}); use {target}@<series>",
            many.join(", ")
        ))),
    }
}

fn file_ref(daemon: &Daemon, role: String, path: &str, managed: bool) -> FileRefDto {
    let fs_path = std::path::Path::new(path)
        .strip_prefix(daemon.root.path())
        .ok()
        .map(forge_core::paths::posix_string);
    FileRefDto {
        role,
        path: path.replace('\\', "/"),
        fs_path,
        managed,
    }
}

/// `services.settings`: the quick settings with their values, the config files (the
/// user's file of an overlay is `managed: false`; the overlay itself is listed with role
/// `<id>.overlay`) and the named ports of an instance. `advanced` is M2-E2-T10.
async fn read(
    daemon: Daemon,
    params: ServiceTargetParams,
    _ctx: Ctx,
) -> Result<ServiceSettingsDto> {
    let scope = resolve(&daemon, params.target.as_str()).await?;
    let manifest = scope.manifest()?;
    let stored = scope.stored(&daemon).await?;
    let mut files = Vec::new();
    if let Ok(vars) = scope.vars(&daemon) {
        for file in &manifest.spec.config.files {
            if let Ok(path) = vars.substitute(&file.path) {
                files.push(file_ref(
                    &daemon,
                    file.id.clone(),
                    &path,
                    file.overlay.is_none(),
                ));
            }
            if let Some(Ok(path)) = file.overlay.as_deref().map(|o| vars.substitute(o)) {
                files.push(file_ref(
                    &daemon,
                    format!("{}.overlay", file.id),
                    &path,
                    true,
                ));
            }
        }
    }
    let ports = match &scope {
        Scope::Instance(entry) => {
            let current = daemon
                .services
                .get(entry.row.id)
                .map_or_else(|| entry.ports.clone(), |e| e.ports);
            manifest
                .spec
                .service
                .daemon
                .as_ref()
                .map(|d| {
                    d.ports
                        .iter()
                        .map(|p| ServicePortSettingDto {
                            name: p.name.clone(),
                            port: current.get(&p.name).copied(),
                            default: p.default,
                            protocol: PortProtocol::Tcp,
                            range: Some(p.range.clone()),
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
        Scope::Series { .. } => Vec::new(),
    };
    Ok(ServiceSettingsDto {
        target: ServiceTarget::from(scope.label()),
        tool: manifest.metadata.id.clone(),
        settings: settings::describe(&manifest, &stored),
        files,
        ports,
        advanced: None,
    })
}

/// Fails when a file about to be written has a managed block edited by hand: the user
/// decides first (keep or regenerate, docs/03 §3.1); nothing is written.
async fn check_drift(plans: &[forge_render::settings::SettingsFile]) -> Result<()> {
    let paths: Vec<PathBuf> = plans
        .iter()
        .filter(|p| p.overlay.is_none())
        .map(|p| p.path.clone())
        .collect();
    let drifts = forge_core::runtime::spawn_blocking_timeout(
        "checking the managed blocks",
        DRIFT_BUDGET,
        move || {
            let mut out = Vec::new();
            for path in &paths {
                out.extend(forge_render::drift::detect_in_file_any_style(path)?);
            }
            Ok(out)
        },
    )
    .await?;
    match drifts.first() {
        None => Ok(()),
        Some(drift) => Err(ForgeError::Conflict(format!(
            "{}: the managed block `{}` was edited by hand; keep or regenerate it before \
             changing settings",
            forge_core::paths::posix_string(&drift.file),
            drift.block
        ))),
    }
}

/// Formats without comments need an overlay or a render target that writes the file.
fn check_writable(tool: &str, plans: &[forge_render::settings::SettingsFile]) -> Result<()> {
    if forge_render::targets::by_id(tool).is_some() {
        return Ok(());
    }
    for plan in plans.iter().filter(|p| p.overlay.is_none()) {
        if plan.format.comment_style().prefix().is_none() {
            return Err(ForgeError::ConfigInvalid(format!(
                "{}: a {} file needs `config.files[].overlay` for its quick settings",
                plan.path.display(),
                plan.format.as_str()
            )));
        }
    }
    Ok(())
}

/// The ports an instance asks for that differ from its current ones, refused before the
/// job when another owner reserved one (`conflict`) or a process listens on it
/// (`port_in_use` with `pid`/`exe`). The job's `reserve` checks again atomically.
async fn check_ports(daemon: &Daemon, entry: &Entry, ports: &BTreeMap<String, u16>) -> Result<()> {
    let current = daemon
        .services
        .get(entry.row.id)
        .map_or_else(|| entry.ports.clone(), |e| e.ports);
    let wanted: Vec<(&String, u16)> = ports
        .iter()
        .filter(|(name, port)| current.get(*name) != Some(*port))
        .map(|(name, port)| (name, *port))
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }
    for (i, (name, port)) in wanted.iter().enumerate() {
        if let Some((other, _)) = wanted[..i].iter().find(|(_, p)| p == port) {
            return Err(ForgeError::InvalidInput(format!(
                "ports.{name}: port {port} is also asked for ports.{other}"
            )));
        }
    }
    let names = super::services::instance_names(daemon);
    for (name, port) in &wanted {
        if let Some(reserved) = daemon
            .ports
            .reservation_of(StoredProtocol::Tcp, *port)
            .await?
        {
            if reserved.owner != instance_owner(entry.row.id, name)? {
                return Err(ForgeError::Conflict(format!(
                    "ports.{name}: port {port} is reserved for {}",
                    super::services::owner_name(&names, &reserved.owner)
                )));
            }
        }
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
            // The reservation in the job checks again; a stalled table is no reason to
            // refuse here.
            tracing::warn!(%err, "port table not read; the reservation decides");
            return Ok(());
        }
    };
    for (_, port) in &wanted {
        if let Some(holder) = listeners.iter().find(|l| l.port == *port) {
            let exe = holder
                .exe_path
                .as_deref()
                .map(forge_core::paths::posix_string);
            return Err(ForgeError::PortInUse {
                port: *port,
                owner: Some(match &exe {
                    Some(exe) => format!("{exe} (PID {})", holder.pid),
                    None => format!("PID {}", holder.pid),
                }),
                pid: Some(holder.pid),
                exe,
            });
        }
    }
    Ok(())
}

/// `services.set` (job, ADR-0042). See the module docs for the order of things.
async fn set(daemon: Daemon, params: ServiceSetParams, ctx: Ctx) -> Result<JobDto> {
    let scope = resolve(&daemon, params.target.as_str()).await?;
    let manifest = scope.manifest()?;
    let changes = settings::validate(&manifest, &params.settings, &scope.port_names(&manifest))?;
    let before = scope.stored(&daemon).await?;
    let vars = scope.vars(&daemon)?;
    let plans = settings::plans(&manifest, &changes.applied_to(&before), &vars)?;
    check_writable(manifest.metadata.id.as_str(), &plans)?;
    check_drift(&plans).await?;
    if let Scope::Instance(entry) = &scope {
        check_ports(&daemon, entry, &changes.ports).await?;
    }
    let label = scope.label();
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("services.set {label}"),
        move |_job| async move {
            let turn =
                tokio::time::timeout(TURN_TIMEOUT, Arc::clone(&d.settings_turn).acquire_owned())
                    .await
                    .map_err(|_| ForgeError::Timeout {
                        operation: "waiting for another services.set".into(),
                        after: TURN_TIMEOUT,
                    })?
                    .map_err(|_| ForgeError::Internal("settings turn closed".into()))?;
            match scope {
                Scope::Instance(entry) => set_instance(&d, &manifest, &entry, changes, turn).await,
                Scope::Series {
                    tool,
                    series,
                    versions,
                } => set_series(&d, &manifest, &tool, &series, &versions, changes, turn).await,
            }
        },
    )
}

/// Range of the named port `name` of the manifest.
fn range_of(manifest: &ToolManifest, name: &str) -> Option<String> {
    manifest
        .spec
        .service
        .daemon
        .as_ref()?
        .ports
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.range.clone())
}

/// Puts back the reservations moved by a failed set. Best effort: the old port may be in
/// use by the instance itself; then the new reservation stays and the instance is rebuilt
/// on it, so the actor always follows the reservations.
async fn undo_ports(
    daemon: &Daemon,
    manifest: &ToolManifest,
    id: InstanceId,
    moved: &[(String, Option<u16>)],
) {
    let mut restored = true;
    for (name, old) in moved.iter().rev() {
        let Ok(owner) = instance_owner(id, name) else {
            continue;
        };
        let result = match old {
            Some(port) => daemon
                .ports
                .reserve(*port, range_of(manifest, name).as_deref(), owner)
                .await
                .map(|_| ()),
            None => daemon.ports.release(&owner).await.map(|_| ()),
        };
        if let Err(err) = result {
            tracing::warn!(%id, port = %name, %err, "moved port not put back");
            restored = false;
        }
    }
    if !restored {
        if let Err(err) = daemon.services.rebuild(id).await {
            tracing::warn!(%id, %err, "instance not rebuilt on its moved port");
        }
    }
}

async fn set_instance(
    d: &Daemon,
    manifest: &ToolManifest,
    entry: &Entry,
    changes: Changes,
    turn: OwnedSemaphorePermit,
) -> Result<ServiceStatusDto> {
    let id = entry.row.id;
    let current = d
        .services
        .get(id)
        .ok_or_else(|| ForgeError::NotFound(format!("instance {id}")))?;
    let before = current.row.settings.clone();
    // 1. Move the reservations (each one atomic in the store writer).
    let mut moved: Vec<(String, Option<u16>)> = Vec::new();
    for (name, port) in &changes.ports {
        let old = current.ports.get(name).copied();
        if old == Some(*port) {
            continue;
        }
        let owner = instance_owner(id, name)?;
        if let Err(err) = d
            .ports
            .reserve(*port, range_of(manifest, name).as_deref(), owner)
            .await
        {
            undo_ports(d, manifest, id, &moved).await;
            return Err(err);
        }
        moved.push((name.clone(), old));
    }
    // 2. Store the values.
    let after = changes.applied_to(&before);
    let changed = settings::changed_keys(manifest, &before, &after);
    if after != before {
        if let Err(err) = d
            .services
            .update_settings(id, changes.settings.clone())
            .await
        {
            undo_ports(d, manifest, id, &moved).await;
            return Err(err);
        }
    }
    let label = entry.label();
    if !moved.is_empty() {
        // 3a. A moved port: new spec, re-rendered, restarted when it was running.
        drop(turn);
        tracing::info!(instance = %label, ports = ?changes.ports, "port moved; rebuilding the instance");
        d.services.rebuild(id).await?;
    } else if !changed.is_empty() {
        // 3b. Values only: write the files now, then apply them.
        d.services.render(id).await?;
        drop(turn);
        if d.services.is_running(id).await {
            match settings::apply_mode(manifest, &changed) {
                Apply::Restart => {
                    d.services.restart(id).await?;
                }
                Apply::Reload => {
                    d.services.apply_reload(id).await?;
                }
                Apply::None => {}
            }
        }
        tracing::info!(instance = %label, keys = ?changed, "quick settings applied");
    }
    let fresh = d.services.get(id).into_iter().collect();
    super::services::status_of(d, fresh, Vec::new()).await
}

/// Pools of `versions` that run (serving first).
async fn running_pools(d: &Daemon, versions: &[ToolVersionRow]) -> Vec<PoolHandle> {
    let mut out: Vec<(bool, PoolHandle)> = Vec::new();
    for handle in d.pools.handles() {
        if !versions.iter().any(|v| v.id == handle.version_id()) {
            continue;
        }
        if let Ok(status) = handle.status(POOL_QUICK).await {
            if matches!(
                status.state,
                forge_fcgi::PoolState::Running | forge_fcgi::PoolState::Starting
            ) {
                out.push((status.serving, handle));
            }
        }
    }
    out.sort_by_key(|(serving, _)| !*serving);
    out.into_iter().map(|(_, h)| h).collect()
}

async fn set_series(
    d: &Daemon,
    manifest: &ToolManifest,
    tool: &ToolEntry,
    series: &str,
    versions: &[ToolVersionRow],
    changes: Changes,
    turn: OwnedSemaphorePermit,
) -> Result<ServiceStatusDto> {
    let tool_id = tool.manifest.metadata.id.clone();
    let before = {
        let (id, s) = (tool_id.clone(), series.to_owned());
        d.reader
            .query(move |conn| ToolSeriesSettingsRepo::new(conn).get(&id, &s))
            .await?
    };
    let after = changes.applied_to(&before);
    let changed = settings::changed_keys(manifest, &before, &after);
    if after != before {
        let (id, s, set) = (tool_id.clone(), series.to_owned(), changes.settings.clone());
        d.writer
            .execute(move |conn| ToolSeriesSettingsRepo::new(conn).apply(&id, &s, &set))
            .await?;
    }
    let running = running_pools(d, versions).await;
    if !changed.is_empty() {
        // The files of a series are shared by its versions: render with the one that
        // serves (its folder is in `extension_dir`), else the newest installed.
        let version = running
            .first()
            .map(PoolHandle::version_id)
            .or_else(|| versions.first().map(|v| v.id))
            .ok_or_else(|| ForgeError::NotFound(format!("{tool_id}@{series} is not installed")))?;
        d.pools.render_config(version).await?;
        drop(turn);
        if settings::apply_mode(manifest, &changed) != Apply::None {
            // `reload` and `restart` of a pool are the same thing: a rolling recycle, so
            // the new workers read the files and no request is cut (docs/03 §2.1).
            for handle in &running {
                let replaced = handle.reload(POOL_RELOAD_TIMEOUT).await?;
                tracing::info!(pool = %handle.upstream_name(), version = %handle.spec().version, replaced, "pool recycled for new settings");
            }
        }
        tracing::info!(tool = %tool_id, %series, keys = ?changed, "quick settings applied");
    }
    super::services::status_of(
        d,
        Vec::new(),
        running.iter().map(PoolHandle::version_id).collect(),
    )
    .await
}
