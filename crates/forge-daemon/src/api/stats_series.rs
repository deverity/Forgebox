//! `stats.series`, `stats.routes`, `stats.export` and `stats.top` (docs/14 §3.6, §6,
//! ADR-0044 §8-§9); `stats.get` and `metrics.subscribe` live in [`super::stats`].
//!
//! * `stats.series`: a dense series (one point per step of `[from, to)`, steps without
//!   requests with zero counts and no percentiles) at the step of the range (tier 0, tier 1,
//!   minutes or hours). The part older than the in-memory retention is read from the stored
//!   minutes and hours; event markers come from the `events` table (`events_ts` index).
//! * `stats.routes`: the top-K routes and the recent 4xx/5xx of the site.
//! * `stats.export`: a job; one row per minute (spans up to 24 h) or per hour (longer),
//!   only the steps that had requests, CSV or JSON. Routes never reach it, so no query
//!   string can.
//! * `stats.top`: see [`crate::top`].

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::event::{Event, EventKind};
use forge_core::store::{EventRepo, Site, SiteKind};
use forge_ipc::api::{
    JobDto, NoParams, RecentErrorDto, RouteStatsDto, RoutesDto, SERIES_MAX_POINTS, SeriesDto,
    SeriesMarkerDto, SeriesPointDto, StatsExportDto, StatsExportFormat, StatsExportParams,
    StatsRoutesParams, StatsSeriesParams, StatsWindow, TopDto,
};
use forge_ipc::server::Ctx;
use forge_metrics::{SeriesPoint, SiteRoute, Step};
use jiff::Timestamp;

use crate::daemon::Daemon;

const ASK_TIMEOUT: Duration = Duration::from_secs(2);
/// A series over stored minutes and hours reads SQLite outside the actor.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(8);
/// Events read for the markers of one series (the oldest first).
const MARKER_SCAN_LIMIT: usize = 2_000;
/// Markers answered at most (a site with a restart loop must not flood the chart).
const MARKERS_MAX: usize = 500;
/// Rows of one export at most (7 d of minutes is 10,080).
const EXPORT_MAX_ROWS: i64 = 20_000;
/// A span up to this long is exported by the minute, a longer one by the hour.
const EXPORT_MINUTE_SPAN_SECS: i64 = 24 * 3_600;

fn floor_to(secs: i64, step: i64) -> i64 {
    secs.div_euclid(step) * step
}

fn ceil_to(secs: i64, step: i64) -> i64 {
    -((-secs).div_euclid(step)) * step
}

/// Whole seconds at or after `t` (a step in progress ends after its instant).
fn ceil_secs(t: Timestamp) -> i64 {
    t.as_second() + i64::from(t.subsec_nanosecond() > 0)
}

fn at(secs: i64) -> Timestamp {
    Timestamp::from_second(secs).unwrap_or(Timestamp::UNIX_EPOCH)
}

fn route_of(site: &Site) -> SiteRoute {
    SiteRoute {
        id: site.id,
        host: site.host.clone(),
        aliases: site.aliases.clone(),
        metrics_enabled: site.metrics_enabled,
    }
}

/// Tier of a series range (ADR-0044): the step and how far back that tier keeps data.
fn tier(range: StatsWindow) -> (Step, i64, i64) {
    let step_secs = i64::from(range.series_step_ms() / 1000);
    let (step, retention) = match range {
        StatsWindow::OneMinute | StatsWindow::FiveMinutes => (Step::TwoSeconds, 600),
        StatsWindow::OneHour => (Step::TenSeconds, 24 * 3_600),
        StatsWindow::OneDay => (Step::Minute, 7 * 24 * 3_600),
        StatsWindow::SevenDays => (Step::Hour, 90 * 24 * 3_600),
    };
    (step, step_secs, retention)
}

fn ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

fn point_dto(p: Option<&SeriesPoint>, start: i64, step_secs: i64) -> SeriesPointDto {
    let Some(p) = p else {
        return SeriesPointDto {
            at: at(start),
            requests: 0,
            rps: 0.0,
            bytes: 0,
            status_4xx: 0,
            status_5xx: 0,
            error_pct: 0.0,
            p50_ms: None,
            p90_ms: None,
            p99_ms: None,
            cpu_pct: None,
            mem_bytes: None,
            workers_active: None,
            approx: false,
            sampled: false,
        };
    };
    let rt = p.rt.filter(|_| p.requests > 0);
    SeriesPointDto {
        at: at(start),
        requests: p.requests,
        rps: p.requests as f64 / step_secs as f64,
        bytes: p.bytes,
        status_4xx: p.err4xx,
        status_5xx: p.err5xx,
        error_pct: if p.requests > 0 {
            (p.err4xx + p.err5xx) as f64 * 100.0 / p.requests as f64
        } else {
            0.0
        },
        p50_ms: rt.map(|r| ms(r.p50_us)),
        p90_ms: rt.map(|r| ms(r.p90_us)),
        p99_ms: rt.map(|r| ms(r.p99_us)),
        cpu_pct: p.resources.as_ref().map(|r| r.cpu_pct),
        mem_bytes: p.resources.as_ref().map(|r| r.mem_bytes),
        workers_active: p.resources.as_ref().map(|r| r.workers_active),
        approx: p.resources.as_ref().is_some_and(|r| r.approx),
        sampled: p.sampled,
    }
}

/// The span and the step of a series request.
#[derive(Debug)]
struct Grid {
    step: Step,
    step_secs: i64,
    start: i64,
    end: i64,
}

impl Grid {
    fn len(&self) -> usize {
        usize::try_from((self.end - self.start) / self.step_secs).unwrap_or(0)
    }
}

/// `[start, end)` aligned to the step: the last `range` up to `to` (default now), or from
/// `from`; limited by what the step's tier keeps.
fn grid(params: &StatsSeriesParams, now: Timestamp) -> Result<Grid> {
    let (step, step_secs, retention) = tier(params.range);
    let to = params.to.unwrap_or(now).min(now);
    let end = ceil_to(ceil_secs(to), step_secs);
    let start = match params.from {
        Some(from) => {
            if from >= to {
                return Err(ForgeError::InvalidInput(
                    "from must be earlier than to".into(),
                ));
            }
            floor_to(from.as_second(), step_secs)
        }
        None => end - i64::from(params.range.seconds()),
    };
    let start = start.max(end - retention);
    let grid = Grid {
        step,
        step_secs,
        start,
        end,
    };
    if grid.len() > SERIES_MAX_POINTS as usize {
        return Err(ForgeError::InvalidInput(format!(
            "a series has at most {SERIES_MAX_POINTS} points; {} asked at a step of {} s",
            grid.len(),
            step_secs
        )));
    }
    Ok(grid)
}

async fn points_of(
    daemon: &Daemon,
    route: SiteRoute,
    grid: &Grid,
) -> Result<HashMap<i64, SeriesPoint>> {
    let report = daemon
        .metrics
        .series(
            route,
            at(grid.start),
            Some(at(grid.end)),
            grid.step,
            ANSWER_TIMEOUT - Duration::from_secs(1),
        )
        .await?;
    Ok(report
        .points
        .into_iter()
        .map(|p| (p.start.as_second(), p))
        .collect())
}

pub async fn series(
    daemon: Daemon,
    params: StatsSeriesParams,
    _ctx: Ctx,
) -> Result<forge_ipc::api::SeriesDto> {
    let site = super::sites::find(&daemon, &params.site).await?;
    let grid = grid(&params, Timestamp::now())?;
    let mut by_start = points_of(&daemon, route_of(&site), &grid).await?;
    let points: Vec<SeriesPointDto> = (0..grid.len() as i64)
        .map(|i| {
            let start = grid.start + i * grid.step_secs;
            point_dto(by_start.remove(&start).as_ref(), start, grid.step_secs)
        })
        .collect();
    let markers = markers(&daemon, &site, at(grid.start), at(grid.end)).await?;
    Ok(SeriesDto {
        site_id: site.id,
        host: site.host,
        range: params.range,
        step_ms: params.range.series_step_ms(),
        from: at(grid.start),
        to: at(grid.end),
        points,
        markers,
    })
}

// ------------------------------------------------------------------------------ markers

fn kinds() -> Vec<EventKind> {
    vec![
        EventKind::SitePhpChanged,
        EventKind::SiteUpdated,
        EventKind::ServiceRunning,
        EventKind::ServiceStopped,
        EventKind::ServiceCrashed,
        EventKind::ToolUpdated,
        EventKind::ToolRollback,
        EventKind::ConfigRendered,
    ]
}

/// What decides which events belong to a site.
struct Relevance {
    site_id: String,
    /// Tool of the web server in front of the site.
    server: String,
    php: bool,
    /// `8.4` the site follows, if any.
    series: Option<String>,
    /// `8.4.27` the site is pinned to, if any.
    exact: Option<String>,
}

fn text<'a>(event: &'a Event, key: &str) -> Option<&'a str> {
    event.payload.get(key).and_then(serde_json::Value::as_str)
}

impl Relevance {
    fn of(site: &Site, default_server: &str) -> Self {
        Self {
            site_id: site.id.to_string(),
            server: site
                .server
                .clone()
                .unwrap_or_else(|| default_server.to_owned()),
            php: site.kind == SiteKind::Php,
            series: site.php.series.clone(),
            exact: site.php.exact.clone(),
        }
    }

    /// First segment of `nginx/default`, `php@8.4`, `site:loja.test`.
    fn head(owner: &str) -> &str {
        owner
            .split(['/', '@', ':'])
            .next()
            .filter(|h| !h.is_empty())
            .unwrap_or(owner)
    }

    fn concerns(&self, event: &Event) -> bool {
        match event.kind {
            EventKind::SitePhpChanged | EventKind::SiteUpdated => {
                text(event, "site_id") == Some(self.site_id.as_str())
            }
            EventKind::ServiceRunning | EventKind::ServiceStopped | EventKind::ServiceCrashed => {
                text(event, "site_id") == Some(self.site_id.as_str())
                    || text(event, "tool")
                        .is_some_and(|tool| tool == self.server || (self.php && tool == "php"))
            }
            EventKind::ToolUpdated | EventKind::ToolRollback => {
                if !self.php || text(event, "tool") != Some("php") {
                    return false;
                }
                let touches =
                    |v: &str| text(event, "from") == Some(v) || text(event, "to") == Some(v);
                match (&self.exact, &self.series) {
                    (Some(exact), _) => touches(exact),
                    (None, Some(series)) => {
                        text(event, "series").is_none_or(|s| s == series.as_str())
                    }
                    (None, None) => true,
                }
            }
            EventKind::ConfigRendered => text(event, "owner").is_some_and(|owner| {
                let head = Self::head(owner);
                head == self.server || (self.php && head == "php")
            }),
            _ => false,
        }
    }

    /// The label arguments of a marker: selected fields, never an absolute path.
    fn args(event: &Event) -> BTreeMap<String, String> {
        let keys: &[&str] = match event.kind {
            EventKind::SitePhpChanged | EventKind::ToolUpdated | EventKind::ToolRollback => {
                &["from", "to", "tool", "series"]
            }
            EventKind::SiteUpdated => &["host"],
            EventKind::ConfigRendered => &["owner", "fs_path"],
            _ => &["tool", "name", "instance_id", "exit_code"],
        };
        let mut out = BTreeMap::new();
        for key in keys {
            let value = match event.payload.get(*key) {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Number(n)) => n.to_string(),
                _ => continue,
            };
            let name = if *key == "name" { "instance" } else { key };
            out.insert((*name).to_owned(), value);
        }
        out
    }
}

async fn markers(
    daemon: &Daemon,
    site: &Site,
    from: Timestamp,
    to: Timestamp,
) -> Result<Vec<SeriesMarkerDto>> {
    let wanted = kinds();
    let events = daemon
        .reader
        .query(move |conn| EventRepo::new(conn).list_between(from, to, &wanted, MARKER_SCAN_LIMIT))
        .await?;
    let relevance = Relevance::of(site, daemon.config().default_server.as_str());
    let mut out: Vec<SeriesMarkerDto> = events
        .iter()
        .filter(|e| relevance.concerns(e))
        .map(|e| SeriesMarkerDto {
            seq: e.seq,
            at: e.ts,
            kind: e.kind.as_str().to_owned(),
            args: Relevance::args(e),
        })
        .collect();
    if out.len() > MARKERS_MAX {
        // The newest ones: the oldest scrolled out of any chart a person reads.
        out.drain(..out.len() - MARKERS_MAX);
    }
    Ok(out)
}

// ------------------------------------------------------------------------------- routes

pub async fn routes(daemon: Daemon, params: StatsRoutesParams, _ctx: Ctx) -> Result<RoutesDto> {
    let site = super::sites::find(&daemon, &params.site).await?;
    let snapshot = daemon.metrics.routes(route_of(&site), ASK_TIMEOUT).await?;
    let (since, total, routes) = match &snapshot.report {
        Some(report) => (
            report.since,
            report.total,
            report.routes.iter().map(route_dto).collect(),
        ),
        None => (snapshot.at, 0, Vec::new()),
    };
    // 5xx and 4xx rings merge newest first (a stable sort keeps the rings' own order for
    // requests that ended in the same instant, 5xx first).
    let mut recent_errors: Vec<RecentErrorDto> = snapshot
        .errors
        .server
        .iter()
        .chain(snapshot.errors.client.iter())
        .map(|e| RecentErrorDto {
            at: e.at,
            status: e.status,
            method: e.method.clone(),
            route: e.route.clone(),
            host: e.host.clone(),
            duration_ms: ms(e.duration_us),
            upstream_ms: e.upstream_us.map(ms),
            bytes: e.bytes,
        })
        .collect();
    recent_errors.sort_by_key(|e| std::cmp::Reverse(e.at));
    Ok(RoutesDto {
        site_id: site.id,
        host: site.host,
        since,
        at: snapshot.at,
        total,
        routes,
        recent_errors,
    })
}

fn route_dto(r: &forge_metrics::RouteStats) -> RouteStatsDto {
    let errors = r.err4xx + r.err5xx;
    RouteStatsDto {
        method: r.method.clone(),
        route: r.route.clone(),
        count: r.count,
        count_min: r.count.saturating_sub(r.overcount),
        observed: r.observed,
        status_4xx: r.err4xx,
        status_5xx: r.err5xx,
        error_pct: if r.observed > 0 {
            errors as f64 * 100.0 / r.observed as f64
        } else {
            0.0
        },
        mean_ms: (r.observed > 0).then(|| r.rt_sum_us as f64 / r.observed as f64 / 1000.0),
        p50_ms: r.rt.map(|p| ms(p.p50_us)),
        p90_ms: r.rt.map(|p| ms(p.p90_us)),
        p99_ms: r.rt.map(|p| ms(p.p99_us)),
    }
}

// ------------------------------------------------------------------------------- export

const CSV_HEADER: &str = "timestamp,requests,rps,bytes,status_4xx,status_5xx,error_pct,\
p50_ms,p90_ms,p99_ms,php_p50_ms,php_p90_ms,php_p99_ms,cpu_pct,mem_bytes";

fn opt(value: Option<f64>) -> String {
    value.map_or_else(String::new, |v| format!("{v:.3}"))
}

/// One export row: `SeriesPointDto` plus the upstream percentiles.
#[derive(serde::Serialize)]
struct ExportRow {
    timestamp: Timestamp,
    requests: u64,
    rps: f64,
    bytes: u64,
    status_4xx: u64,
    status_5xx: u64,
    error_pct: f64,
    p50_ms: Option<f64>,
    p90_ms: Option<f64>,
    p99_ms: Option<f64>,
    php_p50_ms: Option<f64>,
    php_p90_ms: Option<f64>,
    php_p99_ms: Option<f64>,
    cpu_pct: Option<f64>,
    mem_bytes: Option<u64>,
}

#[derive(serde::Serialize)]
struct ExportDoc {
    site: String,
    /// `minute` or `hour`.
    step: &'static str,
    from: Timestamp,
    to: Timestamp,
    rows: Vec<ExportRow>,
}

fn render_csv(rows: &[ExportRow]) -> String {
    let mut out = String::with_capacity(CSV_HEADER.len() + rows.len() * 96);
    out.push_str(CSV_HEADER);
    out.push('\n');
    for r in rows {
        let _ = writeln!(
            out,
            "{},{},{:.3},{},{},{},{:.3},{},{},{},{},{},{},{},{}",
            r.timestamp,
            r.requests,
            r.rps,
            r.bytes,
            r.status_4xx,
            r.status_5xx,
            r.error_pct,
            opt(r.p50_ms),
            opt(r.p90_ms),
            opt(r.p99_ms),
            opt(r.php_p50_ms),
            opt(r.php_p90_ms),
            opt(r.php_p99_ms),
            opt(r.cpu_pct),
            r.mem_bytes.map_or_else(String::new, |m| m.to_string()),
        );
    }
    out
}

/// Rows of `[from, to)` for `route`, by the minute (spans up to 24 h) or the hour.
async fn export_rows(
    daemon: &Daemon,
    route: SiteRoute,
    from: Timestamp,
    to: Timestamp,
) -> Result<(&'static str, i64, i64, Vec<ExportRow>)> {
    if from >= to {
        return Err(ForgeError::InvalidInput(
            "from must be earlier than to".into(),
        ));
    }
    let now = Timestamp::now();
    let to = to.min(now);
    let span = to.as_second() - from.as_second();
    let (name, step, step_secs) = if span <= EXPORT_MINUTE_SPAN_SECS {
        ("minute", Step::Minute, 60)
    } else {
        ("hour", Step::Hour, 3_600)
    };
    let start = floor_to(from.as_second(), step_secs);
    let end = ceil_to(ceil_secs(to), step_secs).max(start);
    if (end - start) / step_secs > EXPORT_MAX_ROWS {
        return Err(ForgeError::InvalidInput(format!(
            "an export has at most {EXPORT_MAX_ROWS} rows"
        )));
    }
    let g = Grid {
        step,
        step_secs,
        start,
        end,
    };
    let mut by_start: Vec<(i64, SeriesPoint)> =
        points_of(daemon, route, &g).await?.into_iter().collect();
    by_start.sort_by_key(|(start, _)| *start);
    let rows = by_start
        .into_iter()
        .filter(|(_, p)| p.requests > 0)
        .map(|(start, p)| {
            let dto = point_dto(Some(&p), start, step_secs);
            ExportRow {
                timestamp: dto.at,
                requests: dto.requests,
                rps: dto.rps,
                bytes: dto.bytes,
                status_4xx: dto.status_4xx,
                status_5xx: dto.status_5xx,
                error_pct: dto.error_pct,
                p50_ms: dto.p50_ms,
                p90_ms: dto.p90_ms,
                p99_ms: dto.p99_ms,
                php_p50_ms: p.ut.map(|u| ms(u.p50_us)),
                php_p90_ms: p.ut.map(|u| ms(u.p90_us)),
                php_p99_ms: p.ut.map(|u| ms(u.p99_us)),
                cpu_pct: dto.cpu_pct,
                mem_bytes: dto.mem_bytes,
            }
        })
        .collect();
    Ok((name, start, end, rows))
}

pub async fn export(daemon: Daemon, params: StatsExportParams, ctx: Ctx) -> Result<JobDto> {
    let site = super::sites::find(&daemon, &params.site).await?;
    let d = daemon.clone();
    let label = site.host.clone();
    daemon.job(
        &ctx,
        format!("stats.export {label}"),
        move |_job| async move {
            let (step, start, end, rows) =
                export_rows(&d, route_of(&site), params.from, params.to).await?;
            let content = match params.format {
                StatsExportFormat::Csv => render_csv(&rows),
                StatsExportFormat::Json => {
                    let doc = ExportDoc {
                        site: site.host.clone(),
                        step,
                        from: at(start),
                        to: at(end),
                        rows,
                    };
                    serde_json::to_string_pretty(&doc)
                        .map_err(|e| ForgeError::Internal(format!("export json: {e}")))?
                        + "\n"
                }
            };
            Ok(StatsExportDto {
                format: params.format,
                content,
            })
        },
    )
}

// ---------------------------------------------------------------------------------- top

pub async fn top(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<TopDto> {
    crate::top::top(&daemon).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn params(range: StatsWindow) -> StatsSeriesParams {
        StatsSeriesParams {
            site: forge_ipc::api::SiteRef::from("a.test".to_owned()),
            range,
            from: None,
            to: None,
        }
    }

    #[test]
    fn grids_hold_the_documented_number_of_points() {
        let now = ts("2026-11-01T11:00:07Z");
        for (range, points) in [
            (StatsWindow::OneMinute, 30),
            (StatsWindow::FiveMinutes, 150),
            (StatsWindow::OneHour, 360),
            (StatsWindow::OneDay, 1_440),
            (StatsWindow::SevenDays, 168),
        ] {
            let g = grid(&params(range), now).unwrap();
            assert_eq!(g.len(), points, "{range:?}");
            assert_eq!(g.end % g.step_secs, 0);
            assert_eq!(g.start % g.step_secs, 0);
            // The step in progress is the last point.
            assert!(g.end > now.as_second());
        }
    }

    #[test]
    fn grids_validate_and_limit_the_range() {
        let now = ts("2026-11-01T11:00:00Z");
        let mut p = params(StatsWindow::OneMinute);
        p.from = Some(now);
        assert_eq!(grid(&p, now).unwrap_err().code(), "invalid_input");
        // 2 s steps keep 10 minutes: an older `from` is limited, not refused.
        p.from = Some(ts("2026-10-01T00:00:00Z"));
        assert_eq!(grid(&p, now).unwrap().len(), 300);
        // 1 min steps over a month: too many points at the minute tier's 7 d.
        let mut p = params(StatsWindow::OneDay);
        p.from = Some(ts("2026-10-20T00:00:00Z"));
        assert_eq!(grid(&p, now).unwrap_err().code(), "invalid_input");
    }

    fn event(kind: EventKind, payload: serde_json::Value) -> Event {
        Event {
            id: forge_core::ids::EventId::new(),
            seq: 1,
            ts: ts("2026-11-01T10:00:00Z"),
            kind,
            payload,
        }
    }

    fn relevance() -> Relevance {
        Relevance {
            site_id: "site_1".into(),
            server: "nginx".into(),
            php: true,
            series: Some("8.4".into()),
            exact: None,
        }
    }

    #[test]
    fn markers_belong_to_the_site_its_server_and_its_php() {
        let r = relevance();
        let site = |id: &str| serde_json::json!({ "site_id": id, "host": "a.test" });
        assert!(r.concerns(&event(EventKind::SitePhpChanged, site("site_1"))));
        assert!(!r.concerns(&event(EventKind::SitePhpChanged, site("site_2"))));
        let svc =
            |tool: &str| serde_json::json!({ "tool": tool, "name": format!("{tool}/default") });
        assert!(r.concerns(&event(EventKind::ServiceRunning, svc("nginx"))));
        assert!(r.concerns(&event(EventKind::ServiceCrashed, svc("php"))));
        assert!(!r.concerns(&event(EventKind::ServiceRunning, svc("mysql"))));
        assert!(!r.concerns(&event(EventKind::ServiceRunning, svc("httpd"))));
        let upd = |tool: &str, series: &str| serde_json::json!({ "tool": tool, "series": series, "from": "8.4.26", "to": "8.4.27" });
        assert!(r.concerns(&event(EventKind::ToolUpdated, upd("php", "8.4"))));
        assert!(!r.concerns(&event(EventKind::ToolUpdated, upd("php", "8.3"))));
        assert!(!r.concerns(&event(EventKind::ToolRollback, upd("mysql", "8.4"))));
        let cfg =
            |owner: &str| serde_json::json!({ "owner": owner, "fs_path": "etc/nginx/nginx.conf" });
        assert!(r.concerns(&event(EventKind::ConfigRendered, cfg("nginx/default"))));
        assert!(r.concerns(&event(EventKind::ConfigRendered, cfg("php@8.4"))));
        assert!(!r.concerns(&event(EventKind::ConfigRendered, cfg("mysql/default"))));
        // Static sites have no PHP.
        let mut r = relevance();
        r.php = false;
        assert!(!r.concerns(&event(EventKind::ToolUpdated, upd("php", "8.4"))));
    }

    #[test]
    fn marker_args_never_carry_an_absolute_path() {
        let e = event(
            EventKind::ConfigRendered,
            serde_json::json!({
                "owner": "nginx/default",
                "path": "C:/forgebox/etc/nginx/nginx.conf",
                "fs_path": "etc/nginx/nginx.conf",
            }),
        );
        let args = Relevance::args(&e);
        assert_eq!(args["fs_path"], "etc/nginx/nginx.conf");
        assert!(!args.values().any(|v| v.contains("C:/")));
        let e = event(
            EventKind::ServiceCrashed,
            serde_json::json!({ "tool": "php", "name": "php@8.4", "exit_code": -1 }),
        );
        let args = Relevance::args(&e);
        assert_eq!(args["instance"], "php@8.4");
        assert_eq!(args["exit_code"], "-1");
    }

    #[test]
    fn csv_has_a_header_and_one_line_per_row() {
        let row = |at: &str, n: u64| ExportRow {
            timestamp: ts(at),
            requests: n,
            rps: n as f64 / 60.0,
            bytes: 10,
            status_4xx: 0,
            status_5xx: 1,
            error_pct: 1.0,
            p50_ms: Some(12.5),
            p90_ms: None,
            p99_ms: None,
            php_p50_ms: None,
            php_p90_ms: None,
            php_p99_ms: None,
            cpu_pct: None,
            mem_bytes: Some(2048),
        };
        let csv = render_csv(&[
            row("2026-11-01T10:00:00Z", 60),
            row("2026-11-01T10:01:00Z", 30),
        ]);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("timestamp,requests,rps"));
        assert!(lines[1].starts_with("2026-11-01T10:00:00Z,60,1.000,10,0,1,1.000,12.500,"));
        assert_eq!(lines[1].split(',').count(), CSV_HEADER.split(',').count());
        assert!(lines[1].ends_with(",,2048"));
    }
}
