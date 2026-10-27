//! `forge jobs`, `forge stats`, `forge top` (docs/09 §jobs / stats; docs/14 §4.3).

use forge_ipc::api::methods::stats_export;
use forge_ipc::api::{
    JobIdParams, MetricsSubscribeParams, SiteRef, SiteStatsDto, StatsDto, StatsExportDto,
    StatsExportFormat, StatsExportParams, StatsParams, StatsWindow,
};
use forge_ipc::base::Timestamp;
use forge_ipc::base::ids::JobId;
use forge_ipc::client::SubscriptionItem;

use crate::cli::{ExportFormatArg, JobsArgs, JobsCmd, StatsArgs, StatsCmd, WindowArg};
use crate::context::Context;
use crate::error::{CliError, CliResult};
use crate::messages::Msg;
use crate::output::{Output, RED, YELLOW, format_bytes, paint};
use crate::render;

fn job_id(ctx: &Context, text: &str) -> CliResult<JobId> {
    text.parse::<JobId>()
        .map_err(|_| CliError::usage(ctx.out.tf(Msg::InvalidJobId, &[&text])))
}

pub async fn jobs(ctx: &Context, args: JobsArgs) -> CliResult<()> {
    match args.command {
        None => {
            let client = ctx.client().await?;
            let list = crate::jobs::list(client, args.active).await?;
            let dto = forge_ipc::api::JobListDto { jobs: list };
            ctx.out.emit(&dto, |out| render::job_table(out, &dto.jobs));
            Ok(())
        }
        Some(JobsCmd::Watch { job_id: id }) => watch(ctx, job_id(ctx, &id)?).await,
        Some(JobsCmd::Cancel { job_id: id }) => {
            let cancel = ctx
                .call::<forge_ipc::api::methods::jobs_cancel>(JobIdParams {
                    job_id: job_id(ctx, &id)?,
                })
                .await?;
            ctx.out.emit(&cancel, |out| {
                out.success(out.t(if cancel.was_running {
                    Msg::JobCancelRequested
                } else {
                    Msg::JobNotRunning
                }));
            });
            Ok(())
        }
    }
}

/// Follows a job until it ends, with the same progress display as the command that
/// started it. A failed job exits with its error code.
async fn watch(ctx: &Context, id: JobId) -> CliResult<()> {
    let client = ctx.client().await?;
    let mut events = crate::jobs::open_events(client).await;
    let job = client
        .call::<forge_ipc::api::methods::jobs_get>(JobIdParams { job_id: id })
        .await?;
    let result = crate::jobs::wait(&ctx.out, client, job, events.as_mut()).await?;
    ctx.out
        .emit(&result, |out| out.success(out.t(Msg::JobFinished)));
    Ok(())
}

// --- stats --------------------------------------------------------------------------

fn window(arg: WindowArg) -> StatsWindow {
    match arg {
        WindowArg::OneMinute => StatsWindow::OneMinute,
        WindowArg::FiveMinutes => StatsWindow::FiveMinutes,
        WindowArg::OneHour => StatsWindow::OneHour,
        WindowArg::OneDay => StatsWindow::OneDay,
        WindowArg::SevenDays => StatsWindow::SevenDays,
    }
}

fn stats_table(out: &Output, sites: &[SiteStatsDto]) {
    let rows = sites
        .iter()
        .map(|s| {
            let approx = if s.approx { "~" } else { "" };
            vec![
                s.host.clone(),
                format!("{:.1}", s.rps),
                format!("{:.0} ms", s.p50_ms),
                format!("{:.0} ms", s.p90_ms),
                format!("{:.0} ms", s.p99_ms),
                if s.error_pct >= 5.0 {
                    paint(RED, format!("{:.1}%", s.error_pct))
                } else if s.error_pct > 0.0 {
                    paint(YELLOW, format!("{:.1}%", s.error_pct))
                } else {
                    "0%".into()
                },
                s.cpu_pct
                    .map_or_else(|| "-".into(), |c| format!("{approx}{c:.1}%")),
                s.mem_bytes
                    .map_or_else(|| "-".into(), |m| format!("{approx}{}", format_bytes(m))),
            ]
        })
        .collect();
    out.table(
        &["SITE", "RPS", "P50", "P90", "P99", "ERR%", "CPU", "RAM"],
        rows,
        Msg::NoStats,
    );
}

pub async fn stats(ctx: &Context, args: StatsArgs) -> CliResult<()> {
    if let Some(StatsCmd::Export {
        site,
        from,
        to,
        output,
    }) = args.command
    {
        return export(ctx, site, from, to, output).await;
    }
    let site = args.site.map(SiteRef::from);
    let window = window(args.window);
    if args.watch {
        return watch_stats(ctx, site, window).await;
    }
    let stats: StatsDto = ctx
        .call::<forge_ipc::api::methods::stats_get>(StatsParams { site, window })
        .await?;
    ctx.out.emit(&stats, |out| stats_table(out, &stats.sites));
    Ok(())
}

/// Live table from `metrics.subscribe` (a sample per site per tick, docs/14 §4.3).
async fn watch_stats(ctx: &Context, site: Option<SiteRef>, window: StatsWindow) -> CliResult<()> {
    let client = ctx.client().await?;
    let mut samples = client
        .metrics_subscribe(MetricsSubscribeParams {
            site,
            window,
            resume_from: None,
        })
        .await?;
    let mut latest: std::collections::BTreeMap<String, SiteStatsDto> = Default::default();
    loop {
        tokio::select! {
            item = samples.next_item() => match item {
                None => return Ok(()),
                Some(SubscriptionItem::Lagged { .. }) => {}
                Some(SubscriptionItem::Item(envelope)) => {
                    if ctx.out.json {
                        ctx.out.json_line(&envelope.sample);
                    } else {
                        latest.insert(envelope.sample.host.clone(), envelope.sample);
                        let all: Vec<SiteStatsDto> = latest.values().cloned().collect();
                        ctx.out.raw("\u{1b}[2J\u{1b}[H");
                        stats_table(&ctx.out, &all);
                    }
                }
            },
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

async fn export(
    ctx: &Context,
    site: String,
    from: String,
    to: Option<String>,
    output: ExportFormatArg,
) -> CliResult<()> {
    let now = Timestamp::now();
    let from = crate::parse::instant(&from, now)?;
    let to = match to {
        Some(to) => crate::parse::instant(&to, now)?,
        None => now,
    };
    if from >= to {
        return Err(CliError::usage(ctx.out.t(Msg::ExportRangeInvalid)));
    }
    let format = match output {
        ExportFormatArg::Csv => StatsExportFormat::Csv,
        ExportFormatArg::Json => StatsExportFormat::Json,
    };
    let export: StatsExportDto = ctx
        .job::<stats_export, StatsExportDto>(StatsExportParams {
            site: SiteRef::from(site),
            from,
            to,
            format,
        })
        .await?;
    ctx.out.emit(&export, |out| out.raw(&export.content));
    Ok(())
}

pub async fn top(ctx: &Context) -> CliResult<()> {
    let top = ctx
        .call::<forge_ipc::api::methods::stats_top>(forge_ipc::api::NoParams {})
        .await?;
    ctx.out.emit(&top, |out| {
        let mut processes = top.processes.clone();
        processes.sort_by(|a, b| {
            b.cpu_pct
                .partial_cmp(&a.cpu_pct)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.rss_bytes.cmp(&a.rss_bytes))
        });
        let rows = processes
            .iter()
            .map(|p| {
                vec![
                    p.pid.to_string(),
                    p.name.clone(),
                    p.owner_name.clone(),
                    format!("{:.1}%", p.cpu_pct),
                    format_bytes(p.rss_bytes),
                ]
            })
            .collect();
        out.table(
            &[
                "PID",
                out.t(Msg::HProcess),
                out.t(Msg::HOwner),
                "CPU",
                "RAM",
            ],
            rows,
            Msg::NoProcesses,
        );
    });
    Ok(())
}
