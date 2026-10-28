//! `forge daemon status|start|stop|restart|logs|doctor` and `forge doctor`.
//!
//! These commands talk to the daemon *without* starting it: asking a stopped daemon for
//! its status must not boot it, and `forge daemon stop` must not start one just to stop it.

use std::time::{Duration, Instant};

use forge_ipc::Client;
use forge_ipc::api::methods::daemon_doctor;
use forge_ipc::api::{
    API_VERSION, CheckDto, CheckStatus, DaemonStatusDto, DaemonStopParams, DoctorParams,
    DoctorReportDto, NoParams,
};

use crate::cli::DaemonCmd;
use crate::connect;
use crate::context::Context;
use crate::error::{CliError, CliResult};
use crate::messages::Msg;
use crate::output::{BOLD, GREEN, RED, YELLOW, format_duration, paint};
use crate::render;

/// A graceful stop can take this long (docs/03 §1.3: the cascade has a global budget).
const STOP_WAIT: Duration = Duration::from_secs(60);

pub async fn run(ctx: &Context, command: DaemonCmd) -> CliResult<()> {
    match command {
        DaemonCmd::Status => status(ctx).await,
        DaemonCmd::Start => start(ctx).await,
        DaemonCmd::Stop => stop(ctx).await,
        DaemonCmd::Restart => restart(ctx).await,
        DaemonCmd::Logs(args) => {
            super::logs::run(
                ctx,
                super::logs::LogsRequest {
                    target: "daemon".into(),
                    follow: args.follow,
                    since: None,
                    grep: None,
                    lines: args.lines,
                },
            )
            .await
        }
        DaemonCmd::Doctor => doctor(ctx).await,
    }
}

fn print_status(ctx: &Context, status: &DaemonStatusDto) {
    let out = &ctx.out;
    out.pairs(&[
        (out.t(Msg::HState), paint(GREEN, out.t(Msg::StateRunning))),
        ("PID", status.pid.to_string()),
        (out.t(Msg::HVersion), status.version.clone()),
        ("API", status.api_version.to_string()),
        (out.t(Msg::HUptime), format_duration(status.uptime_secs)),
        (out.t(Msg::HRoot), status.root.clone()),
        (
            out.t(Msg::HMode),
            out.t(if status.portable {
                Msg::Portable
            } else {
                Msg::Installed
            })
            .to_owned(),
        ),
        (out.t(Msg::HJobs), status.active_jobs.to_string()),
        (out.t(Msg::HConnections), status.connections.to_string()),
    ]);
}

async fn status(ctx: &Context) -> CliResult<()> {
    match ctx.existing_client().await? {
        Some(client) => {
            let status = client.daemon_status(NoParams {}).await?;
            ctx.out.emit(&status, |_| print_status(ctx, &status));
            Ok(())
        }
        None => {
            let root = ctx.root.to_posix_string();
            ctx.out.emit(
                &serde_json::json!({ "running": false, "root": root }),
                |out| out.line(out.tf(Msg::DaemonStopped, &[&root])),
            );
            Err(CliError::exit_silently(1))
        }
    }
}

async fn start(ctx: &Context) -> CliResult<()> {
    if let Some(client) = ctx.existing_client().await? {
        let status = client.daemon_status(NoParams {}).await?;
        ctx.out.emit(&status, |out| {
            out.info(out.tf(Msg::DaemonAlreadyRunning, &[&status.pid]));
        });
        return Ok(());
    }
    ctx.out.note(
        ctx.out
            .tf(Msg::StartingDaemon, &[&ctx.root.to_posix_string()]),
    );
    let spawned = connect::spawn_daemon(&ctx.root, &ctx.out).await?;
    let client = connect::wait_ready(
        &ctx.root,
        connect::AUTOSTART_TIMEOUT,
        Some(spawned),
        &ctx.out,
    )
    .await?;
    let status = client.daemon_status(NoParams {}).await?;
    ctx.out.emit(&status, |out| {
        out.success(out.tf(Msg::DaemonStarted, &[&status.pid]));
    });
    Ok(())
}

/// Polls until nobody answers on the root any more.
async fn wait_gone(ctx: &Context) -> CliResult<()> {
    let deadline = Instant::now() + STOP_WAIT;
    while connect::is_running(&ctx.root).await {
        if Instant::now() >= deadline {
            return Err(CliError::new(
                "timeout",
                ctx.out.tf(Msg::DaemonStopTimeout, &[&STOP_WAIT.as_secs()]),
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(())
}

async fn stop(ctx: &Context) -> CliResult<()> {
    let Some(client) = ctx.existing_client().await? else {
        ctx.out
            .emit(&serde_json::json!({ "running": false }), |out| {
                out.info(out.t(Msg::DaemonAlreadyStopped));
            });
        return Ok(());
    };
    let ack = client.daemon_stop(DaemonStopParams::default()).await?;
    client.close();
    wait_gone(ctx).await?;
    ctx.out
        .emit(&ack, |out| out.success(out.t(Msg::DaemonStoppedNow)));
    Ok(())
}

async fn restart(ctx: &Context) -> CliResult<()> {
    let Some(client) = ctx.existing_client().await? else {
        return start(ctx).await;
    };
    let old = client.daemon_status(NoParams {}).await?.pid;
    client.daemon_restart(NoParams {}).await?;
    client.close();
    let deadline = Instant::now() + STOP_WAIT + connect::AUTOSTART_TIMEOUT;
    loop {
        if let Ok(new) = connect::try_connect(&ctx.root).await {
            if let Ok(status) = new.daemon_status(NoParams {}).await {
                if status.pid != old {
                    ctx.out.emit(&status, |out| {
                        out.success(out.tf(Msg::DaemonRestarted, &[&status.pid]));
                    });
                    return Ok(());
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(CliError::new(
                "timeout",
                ctx.out
                    .tf(Msg::DaemonStopTimeout, &[&deadline.elapsed().as_secs()]),
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The versions check done on this side: CLI, daemon and API.
fn version_check(ctx: &Context, daemon_version: &str, api: forge_ipc::api::ApiVersion) -> CheckDto {
    let cli = env!("CARGO_PKG_VERSION");
    let compatible = API_VERSION.is_compatible_with(api);
    let (status, fix) = if !compatible {
        (CheckStatus::Fail, Some("forge self-update".to_owned()))
    } else if cli != daemon_version {
        (CheckStatus::Warn, Some("forge self-update".to_owned()))
    } else {
        (CheckStatus::Ok, None)
    };
    CheckDto {
        id: "versions.cli".into(),
        status,
        message: ctx.out.tf(
            Msg::DoctorVersions,
            &[&cli, &daemon_version, &api, &API_VERSION],
        ),
        fix,
    }
}

pub async fn doctor(ctx: &Context) -> CliResult<()> {
    let client: &Client = ctx.client().await?;
    let mut report: DoctorReportDto = ctx
        .job::<daemon_doctor, DoctorReportDto>(DoctorParams::default())
        .await?;
    let hello = client.hello().await?;
    report
        .checks
        .push(version_check(ctx, &hello.daemon_version, hello.api_version));
    let (ok, warnings, failures) = render::check_totals(&report.checks);
    ctx.out.emit(&report, |out| {
        render::checks(out, &report.checks);
        out.line("");
        out.line(format!(
            "{}  {}  {}",
            paint(GREEN, out.tf(Msg::DoctorOk, &[&ok])),
            paint(YELLOW, out.tf(Msg::DoctorWarnings, &[&warnings])),
            paint(
                if failures > 0 { RED } else { BOLD },
                out.tf(Msg::DoctorFailures, &[&failures])
            ),
        ));
    });
    if failures > 0 {
        return Err(CliError::exit_silently(1));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::GlobalArgs;
    use crate::messages::Lang;
    use crate::output::Output;

    fn context() -> Context {
        let global = GlobalArgs {
            json: false,
            quiet: true,
            yes: false,
            root: Some(std::env::temp_dir().join("forge-cli-unit")),
            context: None,
            no_daemon: true,
            no_elevate: false,
        };
        Context::new(global, Output::new(false, true, Lang::PtBr)).unwrap()
    }

    #[test]
    fn version_check_flags_mismatches() {
        let ctx = context();
        let same = version_check(&ctx, env!("CARGO_PKG_VERSION"), API_VERSION);
        assert_eq!(same.status, CheckStatus::Ok);
        let older = version_check(&ctx, "0.0.1", API_VERSION);
        assert_eq!(older.status, CheckStatus::Warn);
        let other_api = forge_ipc::api::ApiVersion {
            major: API_VERSION.major + 1,
            minor: 0,
        };
        let incompatible = version_check(&ctx, env!("CARGO_PKG_VERSION"), other_api);
        assert_eq!(incompatible.status, CheckStatus::Fail);
    }
}
