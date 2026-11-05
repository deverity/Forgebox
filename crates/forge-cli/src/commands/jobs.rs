//! `forge jobs`, `forge stats`, `forge top` (docs/09 §jobs / stats; docs/14 §4.3).

use forge_ipc::api::methods::{stats_export, stats_routes, stats_series};
use forge_ipc::api::{
    DiskRole, JobIdParams, MachineStatsDto, MetricsSubscribeParams, PortOwnerKind, RoutesDto,
    SeriesDto, SiteRef, SiteStatsDto, StatsDto, StatsExportDto, StatsExportFormat,
    StatsExportParams, StatsParams, StatsRoutesParams, StatsSeriesParams, StatsWindow, TopDto,
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

fn percent(value: f64) -> String {
    if value >= 5.0 {
        paint(RED, format!("{value:.1}%"))
    } else if value > 0.0 {
        paint(YELLOW, format!("{value:.1}%"))
    } else {
        "0%".into()
    }
}

/// `-` for a value the site does not have (metrics off, no upstream requests).
fn dash(value: Option<String>) -> String {
    value.unwrap_or_else(|| "-".into())
}

/// Headers and rows of the stats table: `SITE REQS RPS P50 P90 P99 ERR% CPU RAM`, and with
/// `wide` also the status classes, bytes, the PHP percentiles, the busy workers and the
/// last request. A site whose metrics are off says so and shows `-` (its counters are
/// zero because nothing is collected, not because nothing happened).
fn stats_rows(out: &Output, sites: &[SiteStatsDto], wide: bool) -> (Vec<String>, Vec<Vec<String>>) {
    let mut headers: Vec<String> = [
        "SITE", "REQS", "RPS", "P50", "P90", "P99", "ERR%", "CPU", "RAM",
    ]
    .map(String::from)
    .to_vec();
    if wide {
        headers.extend(
            [
                "2XX", "3XX", "4XX", "5XX", "BYTES", "PHP P50", "PHP P90", "PHP P99", "WORKERS",
            ]
            .map(String::from),
        );
        headers.push(out.t(Msg::HLastRequest).to_owned());
    }
    let rows = sites
        .iter()
        .map(|s| {
            let approx = if s.approx { "~" } else { "" };
            let off = !s.metrics_enabled;
            let on = |text: String| if off { "-".to_owned() } else { text };
            let ms = |v: f64| on(format!("{v:.0} ms"));
            let mut row = vec![
                s.host.clone(),
                if off {
                    out.t(Msg::MetricsOff).to_owned()
                } else {
                    s.requests.to_string()
                },
                on(format!("{:.1}", s.rps)),
                ms(s.p50_ms),
                ms(s.p90_ms),
                ms(s.p99_ms),
                if off {
                    "-".into()
                } else {
                    percent(s.error_pct)
                },
                dash(s.cpu_pct.map(|c| format!("{approx}{c:.1}%"))),
                dash(s.mem_bytes.map(|m| format!("{approx}{}", format_bytes(m)))),
            ];
            if wide {
                row.extend([
                    on(s.status_2xx.to_string()),
                    on(s.status_3xx.to_string()),
                    on(s.status_4xx.to_string()),
                    on(s.status_5xx.to_string()),
                    on(format_bytes(s.bytes)),
                    dash(s.php_p50_ms.map(|v| format!("{v:.0} ms"))),
                    dash(s.php_p90_ms.map(|v| format!("{v:.0} ms"))),
                    dash(s.php_p99_ms.map(|v| format!("{v:.0} ms"))),
                    dash(s.workers_active.map(|w| w.to_string())),
                    dash(s.last_request_at.map(|t| t.strftime("%F %T").to_string())),
                ]);
            }
            row
        })
        .collect();
    (headers, rows)
}

fn stats_table(out: &Output, sites: &[SiteStatsDto], wide: bool) {
    let (headers, rows) = stats_rows(out, sites, wide);
    let headers: Vec<&str> = headers.iter().map(String::as_str).collect();
    out.table(&headers, rows, Msg::NoStats);
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
        return watch_stats(ctx, site, window, args.wide).await;
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
    ctx.out
        .emit(&stats, |out| stats_table(out, &stats.sites, args.wide));
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

/// How often `--watch` redraws, whatever the cadence of the samples (2 s for `1m`/`5m`,
/// 10 s for `1h`, 60 s for `24h`/`7d`): the table of the latest samples stays on screen.
const WATCH_REDRAW: std::time::Duration = std::time::Duration::from_secs(2);

/// Live table from `metrics.subscribe` (a sample per site per cadence, docs/14 §4.3),
/// redrawn every 2 s. `--json` prints each sample as it arrives.
async fn watch_stats(
    ctx: &Context,
    site: Option<SiteRef>,
    window: StatsWindow,
    wide: bool,
) -> CliResult<()> {
    let client = ctx.client().await?;
    let mut samples = client
        .metrics_subscribe(MetricsSubscribeParams {
            site,
            window,
            resume_from: None,
        })
        .await?;
    let mut latest: std::collections::BTreeMap<String, SiteStatsDto> = Default::default();
    let mut redraw = tokio::time::interval(WATCH_REDRAW);
    redraw.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            item = samples.next_item() => match item {
                None => return Ok(()),
                Some(SubscriptionItem::Lagged { .. }) => {}
                Some(SubscriptionItem::Item(envelope)) => {
                    if ctx.out.json {
                        ctx.out.json_line(&envelope.sample);
                    } else {
                        let first = latest.is_empty();
                        latest.insert(envelope.sample.host.clone(), envelope.sample);
                        if first {
                            // Right away; the timer redraws from then on.
                            draw_stats(ctx, &latest, wide);
                            redraw.reset();
                        }
                    }
                }
            },
            _ = redraw.tick(), if !ctx.out.json && !latest.is_empty() => {
                draw_stats(ctx, &latest, wide);
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

fn draw_stats(
    ctx: &Context,
    latest: &std::collections::BTreeMap<String, SiteStatsDto>,
    wide: bool,
) {
    let all: Vec<SiteStatsDto> = latest.values().cloned().collect();
    ctx.out.raw("\u{1b}[2J\u{1b}[H");
    stats_table(&ctx.out, &all, wide);
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

/// `42.0%`, or `-` before the daemon has two samples to compare.
fn pct(value: Option<f64>) -> String {
    value.map_or_else(|| "-".into(), |v| format!("{v:.1}%"))
}

fn disk_role(out: &Output, role: DiskRole) -> &'static str {
    out.t(match role {
        DiskRole::Root => Msg::DiskRoot,
        DiskRole::Data => Msg::DiskData,
    })
}

/// The machine against the Forgebox share of it, above the process table of `forge top`.
fn machine_header(out: &Output, machine: &MachineStatsDto) {
    out.line(out.tf(
        Msg::TopCpu,
        &[
            &pct(machine.cpu_pct),
            &pct(machine.forgebox_cpu_pct),
            &machine.logical_cpus,
        ],
    ));
    let used = machine.mem_total.saturating_sub(machine.mem_available);
    let used_pct = if machine.mem_total > 0 {
        used as f64 * 100.0 / machine.mem_total as f64
    } else {
        0.0
    };
    out.line(out.tf(
        Msg::TopMemory,
        &[
            &format_bytes(used),
            &format_bytes(machine.mem_total),
            &format!("{used_pct:.0}%"),
            &format_bytes(machine.forgebox_rss),
        ],
    ));
    for disk in &machine.disks {
        out.line(out.tf(
            Msg::TopDisk,
            &[
                &disk_role(out, disk.role),
                &format_bytes(disk.free),
                &format_bytes(disk.total),
            ],
        ));
    }
    out.line("");
}

fn owner_kind(kind: PortOwnerKind) -> &'static str {
    match kind {
        PortOwnerKind::Instance => "instance",
        PortOwnerKind::Pool => "pool",
        PortOwnerKind::App => "app",
        PortOwnerKind::Site => "site",
        PortOwnerKind::Daemon => "daemon",
    }
}

fn top_table(out: &Output, top: &TopDto) {
    if let Some(machine) = &top.machine {
        machine_header(out, machine);
    }
    let mut processes = top.processes.clone();
    processes.sort_by(|a, b| {
        b.cpu_pct
            .partial_cmp(&a.cpu_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.rss_bytes.cmp(&a.rss_bytes))
            .then(a.pid.cmp(&b.pid))
    });
    let rows = processes
        .iter()
        .map(|p| {
            vec![
                p.pid.to_string(),
                p.name.clone(),
                owner_kind(p.owner_kind).to_owned(),
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
            out.t(Msg::HKind),
            out.t(Msg::HOwner),
            "CPU",
            "RAM",
        ],
        rows,
        Msg::NoProcesses,
    );
}

pub async fn top(ctx: &Context) -> CliResult<()> {
    let top = ctx
        .call::<forge_ipc::api::methods::stats_top>(forge_ipc::api::NoParams {})
        .await?;
    ctx.out.emit(&top, |out| top_table(out, &top));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::Lang;
    use crate::output::{render_table, strip_ansi};
    use forge_ipc::api::{DiskSpaceDto, ProcessUsageDto};

    fn out(lang: Lang) -> Output {
        Output::new(false, false, lang)
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn sample(host: &str, enabled: bool) -> SiteStatsDto {
        SiteStatsDto {
            site_id: forge_ipc::base::ids::SiteId::new(),
            host: host.into(),
            window: StatsWindow::FiveMinutes,
            at: ts("2026-11-01T10:05:00Z"),
            requests: if enabled { 1_200 } else { 0 },
            rps: 4.0,
            p50_ms: 12.0,
            p90_ms: 80.0,
            p99_ms: 420.0,
            error_pct: 2.5,
            cpu_pct: Some(3.2),
            mem_bytes: Some(150 * 1024 * 1024),
            approx: true,
            sampled: false,
            from: Some(ts("2026-11-01T10:00:00Z")),
            bytes: 2 * 1024 * 1024,
            status_1xx: 0,
            status_2xx: 1_100,
            status_3xx: 70,
            status_4xx: 20,
            status_5xx: 10,
            php_p50_ms: Some(9.0),
            php_p90_ms: Some(60.0),
            php_p99_ms: None,
            workers_active: Some(3),
            processes: None,
            last_request_at: Some(ts("2026-11-01T10:04:59Z")),
            metrics_enabled: enabled,
        }
    }

    fn render(out: &Output, sites: &[SiteStatsDto], wide: bool) -> String {
        let (headers, rows) = stats_rows(out, sites, wide);
        let headers: Vec<&str> = headers.iter().map(String::as_str).collect();
        strip_ansi(&render_table(&headers, rows))
    }

    #[test]
    fn the_stats_table_snapshot() {
        let sites = [sample("loja.test", true), sample("off.test", false)];
        let text = render(&out(Lang::EnUs), &sites, false);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0].split_whitespace().collect::<Vec<_>>(),
            [
                "SITE", "REQS", "RPS", "P50", "P90", "P99", "ERR%", "CPU", "RAM"
            ]
        );
        assert_eq!(
            lines[1].split_whitespace().collect::<Vec<_>>(),
            [
                "loja.test",
                "1200",
                "4.0",
                "12",
                "ms",
                "80",
                "ms",
                "420",
                "ms",
                "2.5%",
                "~3.2%",
                "~150.0",
                "MiB"
            ]
        );
        // Metrics off: nothing but the switch, never zeros that look like silence.
        assert_eq!(
            lines[2].split_whitespace().collect::<Vec<_>>()[..4],
            ["off.test", "off", "-", "-"]
        );
        let pt = render(&out(Lang::PtBr), &sites, false);
        assert!(pt.contains("desligadas"), "{pt}");
    }

    #[test]
    fn wide_adds_the_status_classes_php_and_the_last_request() {
        let text = render(&out(Lang::EnUs), &[sample("loja.test", true)], true);
        let lines: Vec<&str> = text.lines().collect();
        let header = lines[0].split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            header.ends_with("2XX 3XX 4XX 5XX BYTES PHP P50 PHP P90 PHP P99 WORKERS LAST REQ."),
            "{header}"
        );
        let row = lines[1].split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            row.ends_with("1100 70 20 10 2.0 MiB 9 ms 60 ms - 3 2026-11-01 10:04:59"),
            "{row}"
        );
    }

    fn top_dto() -> TopDto {
        let process = |pid, name: &str, kind, owner: &str, cpu, rss| ProcessUsageDto {
            pid,
            name: name.into(),
            owner_kind: kind,
            owner_id: "x".into(),
            owner_name: owner.into(),
            cpu_pct: cpu,
            rss_bytes: rss,
        };
        TopDto {
            processes: vec![
                process(
                    10,
                    "nginx.exe",
                    PortOwnerKind::Instance,
                    "nginx/default",
                    0.5,
                    1 << 20,
                ),
                process(
                    11,
                    "php-cgi.exe",
                    PortOwnerKind::Pool,
                    "php@8.4",
                    2.0,
                    40 << 20,
                ),
                process(
                    12,
                    "forgeboxd.exe",
                    PortOwnerKind::Daemon,
                    "forgeboxd",
                    0.5,
                    30 << 20,
                ),
            ],
            machine: Some(MachineStatsDto {
                at: ts("2026-11-01T10:00:00Z"),
                cpu_pct: Some(12.5),
                logical_cpus: 8,
                mem_total: 16 << 30,
                mem_available: 8 << 30,
                forgebox_cpu_pct: Some(3.0),
                forgebox_rss: 71 << 20,
                disks: vec![DiskSpaceDto {
                    role: DiskRole::Root,
                    total: 500 << 30,
                    free: 120 << 30,
                    available: 120 << 30,
                }],
            }),
        }
    }

    #[test]
    fn the_machine_header_texts() {
        let dto = top_dto();
        let machine = dto.machine.as_ref().unwrap();
        let o = out(Lang::EnUs);
        assert_eq!(
            o.tf(
                Msg::TopCpu,
                &[
                    &pct(machine.cpu_pct),
                    &pct(machine.forgebox_cpu_pct),
                    &machine.logical_cpus
                ]
            ),
            "CPU: 12.5% machine, 3.0% Forgebox (8 logical CPUs)"
        );
        assert_eq!(
            o.tf(
                Msg::TopDisk,
                &[
                    &disk_role(&o, DiskRole::Root),
                    &format_bytes(machine.disks[0].free),
                    &format_bytes(machine.disks[0].total)
                ]
            ),
            "Disk root: 120.0 GiB free of 500.0 GiB"
        );
        assert_eq!(pct(None), "-");
        let pt = out(Lang::PtBr);
        assert_eq!(
            pt.tf(
                Msg::TopMemory,
                &[&"8.0 GiB", &"16.0 GiB", &"50%", &"71.0 MiB"]
            ),
            "RAM: 8.0 GiB em uso de 16.0 GiB (50%); Forgebox 71.0 MiB"
        );
    }
}
