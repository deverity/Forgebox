//! `services.*`: instances (start/stop/restart/reload/status/create/remove), pools and
//! ports.

use std::collections::BTreeMap;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::event::EventKind;
use forge_core::ids::{InstanceId, VersionId};
use forge_core::model::port::{OwnerKind, PortReservation};
use forge_core::platform::{KILL_EXIT_CODE, KillPolicy};
use forge_ipc::api::methods::{
    services_config, services_config_files, services_create, services_kill_port_holder,
    services_ports, services_reload, services_remove, services_restart, services_start,
    services_status, services_stop,
};
use forge_ipc::api::{
    Ack, ConfigFilesDto, ExternalPortDto, FileRefDto, InstanceDto, JobDto, KillPortHolderParams,
    NoParams, PoolDto, PortHolderDto, PortHolderKilledPayload, PortMapDto, PortProtocol,
    ServiceCreateParams, ServiceRemoveParams, ServiceStatusDto, ServiceTarget, ServiceTargetParams,
    ServiceTargetsParams,
};
use forge_ipc::server::{Ctx, Router};
use forge_supervisor::QUICK_ASK_TIMEOUT;

use super::convert;
use super::route;
use crate::catalog;
use crate::daemon::Daemon;
use crate::install;
use crate::services::Entry;

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, services_start, start);
    route!(router, daemon, services_stop, stop);
    route!(router, daemon, services_restart, restart);
    route!(router, daemon, services_reload, reload);
    route!(router, daemon, services_status, status);
    route!(router, daemon, services_create, create);
    route!(router, daemon, services_remove, remove);
    route!(router, daemon, services_config, config);
    route!(router, daemon, services_config_files, config_files);
    route!(router, daemon, services_ports, ports);
    route!(router, daemon, services_kill_port_holder, kill_port_holder);
    // Service list, port table, termination and the wait for the exit (5 s at most).
    router.timeout::<services_kill_port_holder>(KILL_BUDGET);
}

/// Budget of `services.kill_port_holder`: the OS work runs in one `spawn_blocking`
/// bounded by [`KILL_BLOCKING`]; the rest is a margin for the reply.
const KILL_BUDGET: Duration = Duration::from_secs(12);
/// Budget of the blocking part (inspect, re-check the table, terminate, wait the exit).
const KILL_BLOCKING: Duration = Duration::from_secs(10);
/// Version of the `port.holder_killed` payload.
const PORT_HOLDER_KILLED_V: u32 = 1;

/// What a target designates: instances and/or pools (`php@8.3`).
struct Targets {
    instances: Vec<Entry>,
    pools: Vec<VersionId>,
}

async fn resolve(daemon: &Daemon, targets: &[ServiceTarget]) -> Result<Targets> {
    let mut out = Targets {
        instances: Vec::new(),
        pools: Vec::new(),
    };
    let all = targets.is_empty() || targets.iter().any(|t| t.as_str() == "all");
    if all {
        out.instances = daemon.services.entries();
        out.pools = daemon
            .pools
            .handles()
            .iter()
            .map(|h| h.version_id())
            .collect();
        return Ok(out);
    }
    for target in targets {
        let text = target.as_str();
        if let Some((tool, version)) = text.split_once('@') {
            let (tool_id, _) = install::parse_spec(text)?;
            let entry = catalog::require_tool(&daemon.registry, &tool_id)?;
            let rows = catalog::installed(&daemon.reader, &tool_id, Some(&entry.manifest)).await?;
            let row = catalog::pick(&entry.manifest, &rows, version).ok_or_else(|| {
                ForgeError::NotFound(format!("{tool}@{version} is not installed"))
            })?;
            out.pools.push(row.id);
            continue;
        }
        match daemon.services.resolve(text) {
            Ok(entries) => out.instances.extend(entries),
            Err(err) => {
                // A pool tool without `@`: every pool of it.
                let pools: Vec<VersionId> = daemon
                    .pools
                    .handles()
                    .iter()
                    .filter(|h| h.spec().tool.as_str() == text)
                    .map(|h| h.version_id())
                    .collect();
                if pools.is_empty() {
                    return Err(err);
                }
                out.pools.extend(pools);
            }
        }
    }
    Ok(out)
}

async fn ports_of(daemon: &Daemon) -> BTreeMap<String, Vec<PortReservation>> {
    let mut map: BTreeMap<String, Vec<PortReservation>> = BTreeMap::new();
    if let Ok(all) = daemon.ports.list().await {
        for r in all {
            map.entry(format!("{}:{}", r.owner.kind.as_str(), r.owner.id))
                .or_default()
                .push(r);
        }
    }
    map
}

async fn snapshot(daemon: &Daemon, only: Option<&Targets>) -> Result<ServiceStatusDto> {
    let ports = ports_of(daemon).await;
    let mut instances: Vec<InstanceDto> = Vec::new();
    for (entry, status) in daemon.services.statuses().await {
        if let Some(only) = only {
            if !only.instances.iter().any(|e| e.row.id == entry.row.id) {
                continue;
            }
        }
        let metrics = match &entry.handle {
            Some(handle) => handle.metrics(QUICK_ASK_TIMEOUT).await.ok().flatten(),
            None => None,
        };
        let key = format!("{}:{}", OwnerKind::Instance.as_str(), entry.row.id);
        let owned = ports.get(&key).cloned().unwrap_or_default();
        instances.push(convert::instance(
            &entry,
            status.as_ref(),
            &owned,
            metrics.as_ref(),
        ));
    }
    let sites = daemon.sites.list().await.unwrap_or_default();
    let plan = daemon.web.php_plan(&sites).await.unwrap_or_default();
    let statuses = daemon.pools.statuses().await;
    // Only with pools to describe: the state reads the store.
    let debugger = if statuses.is_empty() {
        BTreeMap::new()
    } else {
        super::php::debugger_states(daemon).await
    };
    let mut pools: Vec<PoolDto> = Vec::new();
    for status in statuses {
        if let Some(only) = only {
            if !only.pools.contains(&status.version_id) {
                continue;
            }
        }
        let count = plan
            .values()
            .filter(|p| p.version.id == status.version_id)
            .count();
        pools.push(convert::pool(
            &status,
            u32::try_from(count).unwrap_or(u32::MAX),
            debugger.get(&status.version_id).is_some_and(|s| s.enabled),
        ));
    }
    Ok(ServiceStatusDto { instances, pools })
}

async fn status(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<ServiceStatusDto> {
    snapshot(&daemon, None).await
}

/// `ServiceStatusDto` of only these instances and pools (the answer of a job about them).
pub(crate) async fn status_of(
    daemon: &Daemon,
    instances: Vec<Entry>,
    pools: Vec<VersionId>,
) -> Result<ServiceStatusDto> {
    snapshot(daemon, Some(&Targets { instances, pools })).await
}

#[derive(Clone, Copy)]
enum Action {
    Start,
    Stop,
    Restart,
}

async fn act(
    daemon: Daemon,
    targets: Vec<ServiceTarget>,
    action: Action,
    ctx: Ctx,
) -> Result<JobDto> {
    let resolved = resolve(&daemon, &targets).await?;
    let name = match action {
        Action::Start => "services.start",
        Action::Stop => "services.stop",
        Action::Restart => "services.restart",
    };
    let label = targets
        .iter()
        .map(ServiceTarget::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("{name} {label}").trim().to_owned(),
        move |_job| async move {
            let mut errors = Vec::new();
            let runs = resolved.instances.iter().map(|entry| {
                let d = d.clone();
                let id: InstanceId = entry.row.id;
                let label = entry.label();
                async move {
                    let result = match action {
                        Action::Start => d.services.start(id).await.map(|_| ()),
                        Action::Stop => d.services.stop(id).await.map(|_| ()),
                        Action::Restart => d.services.restart(id).await.map(|_| ()),
                    };
                    (label, result)
                }
            });
            for (label, result) in futures::future::join_all(runs).await {
                if let Err(err) = result {
                    errors.push(format!("{label}: {err}"));
                }
            }
            for version in &resolved.pools {
                let result = match action {
                    Action::Start => d.pools.ensure_started(*version).await.map(|_| ()),
                    Action::Stop => d.pools.stop(*version).await,
                    Action::Restart => match d.pools.get(*version) {
                        Some(handle) => handle.reload(Duration::from_secs(5)).await.map(|_| ()),
                        None => d.pools.ensure_started(*version).await.map(|_| ()),
                    },
                };
                if let Err(err) = result {
                    errors.push(format!("pool {version}: {err}"));
                }
            }
            if matches!(action, Action::Start | Action::Restart) && !resolved.pools.is_empty() {
                let _ = d.web.apply_web("pool started").await;
            }
            let snapshot = snapshot(&d, Some(&resolved)).await?;
            if errors.is_empty() {
                Ok(snapshot)
            } else {
                Err(ForgeError::Conflict(errors.join("; ")))
            }
        },
    )
}

async fn start(daemon: Daemon, params: ServiceTargetsParams, ctx: Ctx) -> Result<JobDto> {
    act(daemon, params.targets, Action::Start, ctx).await
}

async fn stop(daemon: Daemon, params: ServiceTargetsParams, ctx: Ctx) -> Result<JobDto> {
    act(daemon, params.targets, Action::Stop, ctx).await
}

async fn restart(daemon: Daemon, params: ServiceTargetsParams, ctx: Ctx) -> Result<JobDto> {
    act(daemon, params.targets, Action::Restart, ctx).await
}

async fn reload(daemon: Daemon, params: ServiceTargetParams, ctx: Ctx) -> Result<JobDto> {
    let resolved = resolve(&daemon, std::slice::from_ref(&params.target)).await?;
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("services.reload {}", params.target),
        move |_job| async move {
            for entry in &resolved.instances {
                d.services.reload(entry.row.id).await?;
            }
            for version in &resolved.pools {
                if let Some(handle) = d.pools.get(*version) {
                    handle.reload(Duration::from_secs(5)).await?;
                }
            }
            snapshot(&d, Some(&resolved)).await
        },
    )
}

async fn create(daemon: Daemon, params: ServiceCreateParams, ctx: Ctx) -> Result<JobDto> {
    if params.data_dir.is_some() {
        // API 1.1 (ADR-0041): ignoring it would put the data where the user did not ask.
        return Err(ForgeError::NotImplementedYet {
            method: "services.create".into(),
            task: super::not_implemented::SERVICES_DATA_DIR_TASK.into(),
            what: "choosing the data folder of a new instance (--data-dir)".into(),
        });
    }
    let (tool_id, version) = install::parse_spec(params.tool.as_str())?;
    let tool = catalog::require_tool(&daemon.registry, &tool_id)?;
    let rows = catalog::installed(&daemon.reader, &tool_id, Some(&tool.manifest)).await?;
    let row = match version.as_deref() {
        Some(v) => catalog::pick(&tool.manifest, &rows, v),
        None => rows.first(),
    }
    .cloned()
    .ok_or_else(|| ForgeError::NotFound(format!("{} is not installed", params.tool)))?;
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("services.create {} {}", params.tool, params.name),
        move |_job| async move {
            let entry = d
                .services
                .create(&tool, &row, &params.name, params.port, params.autostart)
                .await?;
            let ports = d
                .ports
                .list()
                .await?
                .into_iter()
                .filter(|r| {
                    r.owner.kind == OwnerKind::Instance && r.owner.id == entry.row.id.to_string()
                })
                .collect::<Vec<_>>();
            let status = match &entry.handle {
                Some(h) => h.status(QUICK_ASK_TIMEOUT).await.ok(),
                None => None,
            };
            Ok(convert::instance(&entry, status.as_ref(), &ports, None))
        },
    )
}

async fn remove(daemon: Daemon, params: ServiceRemoveParams, ctx: Ctx) -> Result<JobDto> {
    if !params.confirm {
        return Err(ForgeError::InvalidInput(
            "removing an instance is destructive: confirm must be true".into(),
        ));
    }
    let entries = daemon.services.resolve(params.instance.as_str())?;
    let [entry] = entries.as_slice() else {
        return Err(ForgeError::InvalidInput(format!(
            "{} matches {} instances; use tool/name",
            params.instance,
            entries.len()
        )));
    };
    let id = entry.row.id;
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("services.remove {}", params.instance),
        move |_job| async move {
            d.services.remove(id, params.delete_data).await?;
            Ok(Ack::ok())
        },
    )
}

fn config_dto(daemon: &Daemon, entries: &[Entry]) -> ConfigFilesDto {
    let mut files = Vec::new();
    for entry in entries {
        for (id, path) in &entry.config_files {
            let fs_path = std::path::Path::new(path)
                .strip_prefix(daemon.root.path())
                .ok()
                .map(forge_core::paths::posix_string);
            files.push(FileRefDto {
                role: id.clone(),
                path: path.replace('\\', "/"),
                fs_path,
                managed: true,
            });
        }
    }
    ConfigFilesDto { files }
}

/// `services.config` (local only): the files to open; the CLI opens them in the editor
/// of the machine it runs on, which is this one.
async fn config(daemon: Daemon, params: ServiceTargetParams, _ctx: Ctx) -> Result<ConfigFilesDto> {
    let entries = daemon.services.resolve(params.target.as_str())?;
    Ok(config_dto(&daemon, &entries))
}

async fn config_files(
    daemon: Daemon,
    params: ServiceTargetParams,
    _ctx: Ctx,
) -> Result<ConfigFilesDto> {
    let entries = daemon.services.resolve(params.target.as_str())?;
    Ok(config_dto(&daemon, &entries))
}

/// `instance id -> "<tool>/<instance>"` (`mysql/default`) for every instance, to name the
/// owner of a reserved port ([`owner_name`]). Read from the in-memory table: no await.
pub(crate) fn instance_names(daemon: &Daemon) -> BTreeMap<String, String> {
    daemon
        .services
        .entries()
        .into_iter()
        .map(|e| (e.row.id.to_string(), e.label()))
        .collect()
}

/// Name of the owner of a port reservation: `<tool>/<instance>` for an instance
/// (`mysql/default`, from [`instance_names`]), `<tool>@<series>/<extension>` for the
/// client port of a series (`php@8.4/xdebug`, M2-E2-T9), `<kind>/<id>` for any other
/// owner.
pub(crate) fn owner_name(
    instances: &BTreeMap<String, String>,
    owner: &forge_core::model::port::ReservationOwner,
) -> String {
    match (owner.kind, instances.get(&owner.id)) {
        (OwnerKind::Instance, Some(name)) => name.clone(),
        _ if crate::php_ext::is_client_port(owner) => format!("{}/{}", owner.id, owner.port_name),
        _ => format!("{}/{}", owner.kind.as_str(), owner.id),
    }
}

async fn ports(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<PortMapDto> {
    let reservations = daemon.ports.list().await?;
    let names = instance_names(&daemon);
    let ports = reservations
        .iter()
        .map(|r| convert::port(r, owner_name(&names, &r.owner)))
        .collect();
    let table = std::sync::Arc::clone(daemon.platform.port_table());
    let listeners = forge_core::runtime::spawn_blocking_timeout(
        "listeners",
        Duration::from_secs(5),
        move || table.listeners(),
    )
    .await
    .unwrap_or_default();
    let config = daemon.config();
    let tree = daemon.job_tree.clone();
    let ours =
        forge_core::runtime::spawn_blocking_timeout("pids", Duration::from_secs(5), move || {
            tree.pids()
        })
        .await
        .unwrap_or_default();
    let mut seen = std::collections::BTreeSet::new();
    let external = listeners
        .into_iter()
        .filter(|l| !ours.contains(&l.pid) && l.pid != std::process::id())
        .filter(|l| {
            config
                .ports
                .ranges
                .values()
                .any(|range| range.contains(l.port))
        })
        .filter(|l| seen.insert((l.port, l.pid)))
        .map(|l| ExternalPortDto {
            port: l.port,
            protocol: PortProtocol::Tcp,
            holder: PortHolderDto {
                pid: l.pid,
                exe: l
                    .exe_path
                    .as_deref()
                    .map(forge_core::paths::posix_string)
                    .unwrap_or_else(|| "?".into()),
            },
        })
        .collect();
    Ok(PortMapDto { ports, external })
}

/// `services.kill_port_holder` (admin, `local_only`, ADR-0045): ends the process that
/// holds `port`, after checking **again**, with a handle on that process open (its pid
/// cannot be recycled meanwhile), that it still listens on `port` and that
/// [`KillPolicy`] allows it: never the daemon, a Forgebox process (Job tree, the root's
/// executables, Forgebox programs), a critical or system process, a service, session 0,
/// another user's process or a Windows program. Publishes the audit event
/// `port.holder_killed` and logs it.
async fn kill_port_holder(daemon: Daemon, params: KillPortHolderParams, ctx: Ctx) -> Result<Ack> {
    let KillPortHolderParams { port, pid, confirm } = params;
    if !confirm {
        return Err(ForgeError::InvalidInput(
            "ending a process is destructive: confirm must be true".into(),
        ));
    }
    if port == 0 {
        return Err(ForgeError::InvalidInput("port 0 is not a port".into()));
    }
    let tree = daemon.job_tree.clone();
    let ours = forge_core::runtime::spawn_blocking_timeout(
        "listing the Forgebox processes",
        Duration::from_secs(2),
        move || tree.pids(),
    )
    .await?;
    let policy = KillPolicy::for_daemon(daemon.root.path(), ours);
    // Cheap refusals before any process is opened.
    policy.check_pid(pid)?;
    let table = std::sync::Arc::clone(daemon.platform.port_table());
    let terminator = std::sync::Arc::clone(daemon.platform.process_terminator());
    let facts = forge_core::runtime::spawn_blocking_timeout(
        "ending the port holder",
        KILL_BLOCKING,
        move || {
            terminator.terminate_checked(pid, KILL_EXIT_CODE, &mut |facts| {
                // With the handle open the pid is this process: does it still listen?
                // (First, so a holder that went away reads as such.)
                let holds = table
                    .listeners()?
                    .iter()
                    .any(|l| l.port == port && l.pid == pid);
                if !holds {
                    return Err(ForgeError::Conflict(format!(
                        "PID {pid} no longer listens on port {port}: check the port again"
                    )));
                }
                policy.check(facts)
            })
        },
    )
    .await;
    let client = ctx.session().client.name.clone();
    let facts = match facts {
        Ok(facts) => facts,
        Err(err) => {
            tracing::warn!(port, pid, %client, %err, "services.kill_port_holder refused");
            return Err(err);
        }
    };
    let exe = facts
        .exe_path
        .as_deref()
        .map(forge_core::paths::posix_string);
    tracing::warn!(
        port,
        pid,
        exe = exe.as_deref().unwrap_or("?"),
        %client,
        "audit: port holder terminated"
    );
    let payload = PortHolderKilledPayload {
        v: PORT_HOLDER_KILLED_V,
        port,
        pid,
        exe,
        client: Some(client),
    };
    daemon.bus.publish(
        EventKind::PortHolderKilled,
        serde_json::to_value(payload).unwrap_or_default(),
    );
    Ok(Ack::ok())
}
