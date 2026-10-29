//! `forge start|stop|restart|reload|status` and `forge service ...` (docs/09 §service).

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::time::Duration;

use forge_ipc::api::methods::{
    net_port_check, services_create, services_kill_port_holder, services_reload, services_remove,
    services_restart, services_set, services_start, services_stop,
};
use forge_ipc::api::{
    EventFilter, InstanceDto, KillPortHolderParams, NoParams, PortCheckParams, PortMapDto,
    PortProtocol, PortState, ServiceCreateParams, ServiceRemoveParams, ServiceSetParams,
    ServiceStatusDto, ServiceTarget, ServiceTargetParams, ServiceTargetsParams, ToolSpec,
};
use forge_ipc::client::SubscriptionItem;

use crate::cli::{ReloadArgs, ServiceCmd, StatusArgs, TargetsArgs};
use crate::context::Context;
use crate::error::{CliError, CliResult};
use crate::messages::Msg;
use crate::output::{RED, YELLOW, paint};
use crate::parse;
use crate::render;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Start,
    Stop,
    Restart,
}

fn targets(args: TargetsArgs) -> Vec<ServiceTarget> {
    args.targets.into_iter().map(ServiceTarget::from).collect()
}

pub async fn control(ctx: &Context, action: Control, args: TargetsArgs) -> CliResult<()> {
    let params = ServiceTargetsParams {
        targets: targets(args),
    };
    let status: ServiceStatusDto = match action {
        Control::Start => ctx.job::<services_start, ServiceStatusDto>(params).await?,
        Control::Stop => ctx.job::<services_stop, ServiceStatusDto>(params).await?,
        Control::Restart => {
            ctx.job::<services_restart, ServiceStatusDto>(params)
                .await?
        }
    };
    ctx.out
        .emit(&status, |out| render::service_table(out, &status));
    report_failures(ctx, &status);
    Ok(())
}

/// Instances that did not come up are explained under the table.
fn report_failures(ctx: &Context, status: &ServiceStatusDto) {
    if ctx.out.json {
        return;
    }
    for instance in &status.instances {
        if let Some(error) = &instance.last_error {
            ctx.out.warn(format!(
                "{}/{}: {}",
                instance.tool, instance.name, error.message
            ));
        }
    }
}

pub async fn reload(ctx: &Context, args: ReloadArgs) -> CliResult<()> {
    let status: ServiceStatusDto = ctx
        .job::<services_reload, ServiceStatusDto>(ServiceTargetParams {
            target: ServiceTarget::from(args.target),
        })
        .await?;
    ctx.out
        .emit(&status, |out| render::service_table(out, &status));
    Ok(())
}

pub async fn status(ctx: &Context, args: StatusArgs) -> CliResult<()> {
    let snapshot = ctx
        .call::<forge_ipc::api::methods::services_status>(NoParams {})
        .await?;
    if !args.watch {
        ctx.out
            .emit(&snapshot, |out| render::service_table(out, &snapshot));
        return Ok(());
    }
    watch(ctx, snapshot).await
}

/// Redraws the table whenever a service or pool changes state (and on the periodic
/// metrics samples), until Ctrl+C.
async fn watch(ctx: &Context, first: ServiceStatusDto) -> CliResult<()> {
    let client = ctx.client().await?;
    let mut events = client
        .subscribe_events(EventFilter::kinds(["service.*", "pool.*"]), None)
        .await?;
    let tty = std::io::stdout().is_terminal() && ctx.out.human();
    let draw = |snapshot: &ServiceStatusDto| {
        if ctx.out.json {
            ctx.out.json_line(snapshot);
        } else {
            if tty {
                ctx.out.raw("\u{1b}[2J\u{1b}[H");
            }
            render::service_table(&ctx.out, snapshot);
            if !tty {
                ctx.out.line("");
            }
        }
    };
    draw(&first);
    loop {
        tokio::select! {
            item = events.next_item() => match item {
                None => return Ok(()),
                Some(SubscriptionItem::Item(_) | SubscriptionItem::Lagged { .. }) => {
                    // Bursts (a start is several events) become one redraw.
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    while tokio::time::timeout(Duration::ZERO, events.next_item()).await.is_ok() {}
                    let snapshot = client
                        .services_status(NoParams {})
                        .await
                        .map_err(CliError::from)?;
                    draw(&snapshot);
                }
            },
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

pub async fn service(ctx: &Context, command: ServiceCmd) -> CliResult<()> {
    match command {
        ServiceCmd::Create {
            tool,
            name,
            port,
            autostart,
        } => {
            parse::tool_spec(&tool)?;
            let instance: InstanceDto = ctx
                .job::<services_create, InstanceDto>(ServiceCreateParams {
                    tool: ToolSpec::from(tool),
                    name,
                    port,
                    autostart,
                })
                .await?;
            ctx.out.emit(&instance, |out| {
                out.success(out.tf(Msg::InstanceCreated, &[&instance.tool, &instance.name]));
            });
            Ok(())
        }
        ServiceCmd::Remove {
            instance,
            delete_data,
        } => {
            let question = if delete_data {
                ctx.out.tf(Msg::ConfirmRemoveInstanceData, &[&instance])
            } else {
                ctx.out.tf(Msg::ConfirmRemoveInstance, &[&instance])
            };
            ctx.confirm(&question)?;
            let ack: forge_ipc::api::Ack = ctx
                .job::<services_remove, forge_ipc::api::Ack>(ServiceRemoveParams {
                    instance: instance.clone().into(),
                    delete_data,
                    confirm: true,
                })
                .await?;
            ctx.out.emit(&ack, |out| {
                out.success(out.tf(Msg::InstanceRemoved, &[&instance]))
            });
            Ok(())
        }
        ServiceCmd::Config { target } => config(ctx, target).await,
        ServiceCmd::Set { target, settings } => {
            let settings: BTreeMap<String, String> = settings
                .iter()
                .map(|s| parse::key_value(s))
                .collect::<CliResult<_>>()?;
            let status: ServiceStatusDto = ctx
                .job::<services_set, ServiceStatusDto>(ServiceSetParams {
                    target: ServiceTarget::from(target),
                    settings,
                })
                .await?;
            ctx.out
                .emit(&status, |out| render::service_table(out, &status));
            Ok(())
        }
        ServiceCmd::Ports => {
            let ports = ctx
                .call::<forge_ipc::api::methods::services_ports>(NoParams {})
                .await?;
            ctx.out.emit(&ports, |out| port_table(out, &ports));
            Ok(())
        }
        ServiceCmd::KillPort { port, pid } => kill_port(ctx, port, pid).await,
    }
}

/// `forge service kill-port <port> [--pid <n>]`: names the process holding the port, asks,
/// then `services.kill_port_holder` (the daemon checks again that this pid still holds the
/// port and may be terminated).
async fn kill_port(ctx: &Context, port: u16, pid: Option<u32>) -> CliResult<()> {
    let check = ctx
        .call::<net_port_check>(PortCheckParams {
            port,
            protocol: PortProtocol::Tcp,
        })
        .await?;
    let (pid, exe) = match (pid, check.holder) {
        (None, Some(holder)) => (holder.pid, holder.exe),
        (Some(pid), Some(holder)) if holder.pid == pid => (pid, holder.exe),
        // A pid the table does not show: the daemon decides (it refuses a pid that does not
        // hold the port).
        (Some(pid), _) => (pid, format!("PID {pid}")),
        (None, None) => {
            return Err(CliError::new(
                "not_found",
                ctx.out.tf(Msg::PortHolderNone, &[&port]),
            ));
        }
    };
    ctx.confirm(&ctx.out.tf(Msg::ConfirmKillPortHolder, &[&exe, &pid, &port]))?;
    let ack = ctx
        .call::<services_kill_port_holder>(KillPortHolderParams {
            port,
            pid,
            confirm: true,
        })
        .await?;
    ctx.out.emit(&ack, |out| {
        out.success(out.tf(Msg::PortHolderKilled, &[&exe, &port]));
    });
    Ok(())
}

/// Lists the config files and, on a terminal, opens them in the editor.
async fn config(ctx: &Context, target: String) -> CliResult<()> {
    let files = ctx
        .call::<forge_ipc::api::methods::services_config>(ServiceTargetParams {
            target: ServiceTarget::from(target),
        })
        .await?;
    ctx.out.emit(&files, |out| render::files(out, &files.files));
    if ctx.out.human() && !ctx.out.quiet && std::io::stdout().is_terminal() {
        let paths: Vec<std::path::PathBuf> = files
            .files
            .iter()
            .map(|f| std::path::PathBuf::from(&f.path))
            .collect();
        let refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
        if !refs.is_empty() {
            crate::open::open_in_editor(&refs)?;
        }
    }
    Ok(())
}

fn port_table(out: &crate::output::Output, ports: &PortMapDto) {
    let mut rows: Vec<Vec<String>> = ports
        .ports
        .iter()
        .map(|p| {
            let state = match p.state {
                PortState::Bound => out.t(Msg::PortBound).to_owned(),
                PortState::Reserved => out.t(Msg::PortReserved).to_owned(),
                PortState::Conflict => paint(RED, out.t(Msg::PortConflict)),
            };
            vec![
                p.port.to_string(),
                format!("{:?}", p.protocol).to_lowercase(),
                p.owner_name.clone(),
                p.name.clone(),
                state,
                p.conflict
                    .as_ref()
                    .map_or_else(|| "-".into(), |h| format!("{} (PID {})", h.exe, h.pid)),
            ]
        })
        .collect();
    rows.extend(ports.external.iter().map(|e| {
        vec![
            e.port.to_string(),
            format!("{:?}", e.protocol).to_lowercase(),
            paint(YELLOW, out.t(Msg::PortExternal)),
            "-".into(),
            out.t(Msg::PortBound).to_owned(),
            format!("{} (PID {})", e.holder.exe, e.holder.pid),
        ]
    }));
    out.table(
        &[
            out.t(Msg::HPort),
            "PROTO",
            out.t(Msg::HOwner),
            out.t(Msg::HName),
            out.t(Msg::HState),
            out.t(Msg::HHolder),
        ],
        rows,
        Msg::NoPorts,
    );
}
