//! `forge jobs`, `forge stats`, `forge top` (docs/09 §jobs / stats; docs/14 §4.3).

use forge_ipc::api::methods::{stats_export, stats_routes, stats_series};
use forge_ipc::api::{
    JobIdParams, MetricsSubscribeParams, RoutesDto, SeriesDto, SiteRef, SiteStatsDto, StatsDto,
    StatsExportDto, StatsExportFormat, StatsExportParams, StatsParams, StatsRoutesParams,
    StatsSeriesParams, StatsWindow,
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
    // `--series`/`--routes` require a site (clap).
    if args.routes {
        let site = site.ok_or_else(|| CliError::usage("--routes needs a site"))?;
        let routes: RoutesDto = ctx.call::<stats_routes>(StatsRoutesParams { site }).await?;
        ctx.out.emit(&routes, |out| routes_tables(out, &routes));
        return Ok(());
    }
    let (from, to) = range(ctx, args.from.as_deref(), args.to.as_deref())?;
    if args.series {
        let site = site.ok_or_else(|| CliError::usage("--series needs a site"))?;
        let series: SeriesDto = ctx
            .call::<stats_series>(StatsSeriesParams {
                site,
                range: window,
                from,
                to,
            })
            .await?;
        ctx.out.emit(&series, |out| series_table(out, &series));
        return Ok(());
    }
    let stats: StatsDto = ctx
        .call::<forge_ipc::api::methods::stats_get>(StatsParams {
            site,
            window,
            from,
            to,
        })
        .await?;
    ctx.out.emit(&stats, |out| stats_table(out, &stats.sites));
    Ok(())
}

/// `--from`/`--to` (RFC 3339 or relative to now); `--from` must be earlier than `--to`.
fn range(
    ctx: &Context,
    from: Option<&str>,
    to: Option<&str>,
) -> CliResult<(Option<Timestamp>, Option<Timestamp>)> {
    let now = Timestamp::now();
    let from = from.map(|f| crate::parse::instant(f, now)).transpose()?;
    let to = to.map(|t| crate::parse::instant(t, now)).transpose()?;
    if from.is_some_and(|f| f >= to.unwrap_or(now)) {
        return Err(CliError::usage(ctx.out.t(Msg::ExportRangeInvalid)));
    }
    Ok((from, to))
}

fn ms(value: Option<f64>) -> String {
    value.map_or_else(|| "-".into(), |v| format!("{v:.0} ms"))
}

/// `forge stats <site> --series`: one row per step (the GUI draws the same points).
fn series_table(out: &Output, series: &SeriesDto) {
    let rows = series
        .points
        .iter()
        .map(|p| {
            vec![
                p.at.to_string(),
                p.requests.to_string(),
                format!("{:.1}", p.rps),
                ms(p.p50_ms),
                ms(p.p90_ms),
                ms(p.p99_ms),
                format!("{:.1}%", p.error_pct),
            ]
        })
        .collect();
    out.table(
        &[
            out.t(Msg::HWhen),
            out.t(Msg::HRequests),
            "RPS",
            "P50",
            "P90",
            "P99",
            "ERR%",
        ],
        rows,
        Msg::NoStats,
    );
}

/// `forge stats <site> --routes`: top routes, then the recent errors.
fn routes_tables(out: &Output, routes: &RoutesDto) {
    let rows = routes
        .routes
        .iter()
        .map(|r| {
            vec![
                r.method.clone(),
                r.route.clone(),
                r.count.to_string(),
                ms(r.p50_ms),
                ms(r.p99_ms),
                format!("{:.1}%", r.error_pct),
            ]
        })
        .collect();
    out.table(
        &[
            "",
            out.t(Msg::HRoute),
            out.t(Msg::HRequests),
            "P50",
            "P99",
            "ERR%",
        ],
        rows,
        Msg::NoStats,
    );
    out.line("");
    let rows = routes
        .recent_errors
        .iter()
        .map(|e| {
            vec![
                e.at.to_string(),
                e.status.to_string(),
                e.method.clone(),
                e.route.clone(),
                format!("{:.0} ms", e.duration_ms),
            ]
        })
        .collect();
    out.table(
        &[out.t(Msg::HWhen), "HTTP", "", out.t(Msg::HRoute), "RT"],
        rows,
        Msg::NoRecentErrors,
    );
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
