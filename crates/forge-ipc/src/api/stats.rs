//! `stats.*` and `metrics.*`: per-site metrics (docs/09 §stats, docs/14, ADR-0044).
//!
//! `metrics.subscribe` is the `metrics.site{site, window}` stream of docs/14 §3: one
//! sample per site and window, with its own `seq`. `stats.series` (chart points and event
//! markers) and `stats.routes` (top routes and recent errors) feed the site's Performance
//! tab; `TopDto.machine` the machine monitor of the dashboard. API 1.1, additive: every field
//! added after 1.0 has a serde default, so a 1.0 peer's JSON still reads.
//!
//! No DTO of this module carries a path of the daemon's machine (ADR-0009): routes are
//! normalized URIs without query string (docs/14 §7), disks are named by role.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::{Resumable, SiteRef};
use super::services::PortOwnerKind;
use forge_core::Timestamp;
use forge_core::ids::{SiteId, SubscriptionId};

fn default_true() -> bool {
    true
}

/// Aggregation window. For `stats.series` it is the `range`, which also picks the step
/// (docs/14 §4.1): `1m`/`5m` → 2 s, `1h` → 10 s, `24h` → 1 min, `7d` → 1 h.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
pub enum StatsWindow {
    #[serde(rename = "1m")]
    OneMinute,
    #[default]
    #[serde(rename = "5m")]
    FiveMinutes,
    #[serde(rename = "1h")]
    OneHour,
    #[serde(rename = "24h")]
    OneDay,
    #[serde(rename = "7d")]
    SevenDays,
}

impl StatsWindow {
    /// Span of the window, in seconds.
    pub const fn seconds(self) -> u32 {
        match self {
            Self::OneMinute => 60,
            Self::FiveMinutes => 300,
            Self::OneHour => 3_600,
            Self::OneDay => 86_400,
            Self::SevenDays => 604_800,
        }
    }

    /// Step of a `stats.series` over this range, in milliseconds (ADR-0044): one point
    /// per tier bucket (tier 0, tier 1, minutes, hours).
    pub const fn series_step_ms(self) -> u32 {
        match self {
            Self::OneMinute | Self::FiveMinutes => 2_000,
            Self::OneHour => 10_000,
            Self::OneDay => 60_000,
            Self::SevenDays => 3_600_000,
        }
    }

    /// Cadence of `metrics.subscribe` samples for this window, in milliseconds (ADR-0044):
    /// 2 s for `1m`/`5m`, 10 s for `1h`, 60 s for `24h`/`7d`.
    pub const fn subscribe_cadence_ms(self) -> u32 {
        match self {
            Self::OneMinute | Self::FiveMinutes => 2_000,
            Self::OneHour => 10_000,
            Self::OneDay | Self::SevenDays => 60_000,
        }
    }
}

/// Metrics of one site over a window (docs/14 §1).
///
/// Percentiles always come from merged HDR histograms, never from averages of percentiles
/// (ADR-0044). The fields after `sampled` are API 1.1 (ADR-0044) and default when a 1.0
/// peer omits them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SiteStatsDto {
    pub site_id: SiteId,
    pub host: String,
    pub window: StatsWindow,
    /// End of the span covered (at most the daemon's clock).
    pub at: Timestamp,
    #[specta(type = specta_typescript::Number)]
    pub requests: u64,
    pub rps: f64,
    /// Total request time (web server's view), p50.
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub p99_ms: f64,
    pub error_pct: f64,
    /// Attributed CPU (≈ when the pool is shared).
    pub cpu_pct: Option<f64>,
    #[specta(type = Option<specta_typescript::Number>)]
    pub mem_bytes: Option<u64>,
    /// CPU/RAM are apportioned estimates (shared pool).
    pub approx: bool,
    /// Adaptive sampling was active.
    pub sampled: bool,
    /// Start of the span covered: the window or `from`/`to` aligned outward to whole
    /// buckets and limited by the retention. `None` from a 1.0 peer.
    #[serde(default)]
    pub from: Option<Timestamp>,
    /// Response body bytes.
    #[serde(default)]
    #[specta(type = specta_typescript::Number)]
    pub bytes: u64,
    /// Requests by status class (`1xx` counts WebSocket upgrades); they add up to
    /// `requests`.
    #[serde(default)]
    #[specta(type = specta_typescript::Number)]
    pub status_1xx: u64,
    #[serde(default)]
    #[specta(type = specta_typescript::Number)]
    pub status_2xx: u64,
    #[serde(default)]
    #[specta(type = specta_typescript::Number)]
    pub status_3xx: u64,
    #[serde(default)]
    #[specta(type = specta_typescript::Number)]
    pub status_4xx: u64,
    #[serde(default)]
    #[specta(type = specta_typescript::Number)]
    pub status_5xx: u64,
    /// Upstream (PHP) time percentiles, from the server's upstream time (`ut`). `None`
    /// without upstream requests or when the server does not log it.
    #[serde(default)]
    pub php_p50_ms: Option<f64>,
    #[serde(default)]
    pub php_p90_ms: Option<f64>,
    #[serde(default)]
    pub php_p99_ms: Option<f64>,
    /// Busy PHP workers attributed to the site (shared pools: apportioned, see `approx`).
    #[serde(default)]
    pub workers_active: Option<u32>,
    /// OS processes charged to the site exactly: its apps and its dedicated pool's
    /// workers. `None` when the site has neither.
    #[serde(default)]
    pub processes: Option<u32>,
    /// End of the last request seen (at most the daemon's clock), within the retention.
    #[serde(default)]
    pub last_request_at: Option<Timestamp>,
    /// The site's metrics switch (`sites.metrics`). When off, the counters are zero and the
    /// sample only says so.
    #[serde(default = "default_true")]
    pub metrics_enabled: bool,
}

/// Params of `stats.get`.
///
/// The span ends at `to` (API 1.1; default now). Without `from` it is the last `window`
/// before `to`; with `from` (API 1.1) it is `[from, to)` and `window` only labels the
/// result: an exact merge over an arbitrary range, aligned outward to whole buckets and
/// limited by the retention (ADR-0044).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct StatsParams {
    #[serde(default)]
    pub site: Option<SiteRef>,
    #[serde(default)]
    pub window: StatsWindow,
    #[serde(default)]
    pub from: Option<Timestamp>,
    #[serde(default)]
    pub to: Option<Timestamp>,
}

/// Result of `stats.get`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct StatsDto {
    pub window: StatsWindow,
    pub sites: Vec<SiteStatsDto>,
}

/// A Forgebox process with its owner (`forge top`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ProcessUsageDto {
    pub pid: u32,
    pub name: String,
    pub owner_kind: PortOwnerKind,
    pub owner_id: String,
    pub owner_name: String,
    pub cpu_pct: f32,
    #[specta(type = specta_typescript::Number)]
    pub rss_bytes: u64,
}

/// Which volume a [`DiskSpaceDto`] describes (never its path, ADR-0009).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DiskRole {
    /// The volume of the Forgebox root folder.
    Root,
    /// The volume of `data/`, only when it is not the root's.
    Data,
}

/// Size and free space of a volume (`GetDiskFreeSpaceExW`, docs/14 §4.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DiskSpaceDto {
    pub role: DiskRole,
    #[specta(type = specta_typescript::Number)]
    pub total: u64,
    #[specta(type = specta_typescript::Number)]
    pub free: u64,
    /// Free bytes the daemon's user may still write (quotas); at most `free`.
    #[specta(type = specta_typescript::Number)]
    pub available: u64,
}

/// The machine against the Forgebox share of it (dashboard machine monitor, `forge top`
/// header; docs/14 §4.2). CPU percentages are shares of the whole machine, as the Task
/// Manager shows them, over the same interval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct MachineStatsDto {
    pub at: Timestamp,
    /// Busy CPU of the machine; `None` until two samples exist.
    pub cpu_pct: Option<f64>,
    pub logical_cpus: u32,
    #[specta(type = specta_typescript::Number)]
    pub mem_total: u64,
    /// Task Manager "Available".
    #[specta(type = specta_typescript::Number)]
    pub mem_available: u64,
    /// CPU of the daemon, the instances, the pools and the apps together; never above
    /// `cpu_pct`.
    pub forgebox_cpu_pct: Option<f64>,
    /// Working set of the same processes.
    #[specta(type = specta_typescript::Number)]
    pub forgebox_rss: u64,
    /// The root's volume, then `data/`'s when it is another one.
    pub disks: Vec<DiskSpaceDto>,
}

/// Result of `stats.top`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct TopDto {
    pub processes: Vec<ProcessUsageDto>,
    /// API 1.1 (ADR-0044); `None` from a 1.0 peer.
    #[serde(default)]
    pub machine: Option<MachineStatsDto>,
}

/// Params of `stats.series` (Performance tab charts, `forge stats <site> --series`).
///
/// `range` picks the step ([`StatsWindow::series_step_ms`]). Without `from`, the series
/// covers the last `range`; with `from` it covers `[from, to)` (`to` defaults to now) at
/// the step of `range`. At most [`SERIES_MAX_POINTS`] points (`invalid_input` above).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct StatsSeriesParams {
    pub site: SiteRef,
    #[serde(default)]
    pub range: StatsWindow,
    #[serde(default)]
    pub from: Option<Timestamp>,
    #[serde(default)]
    pub to: Option<Timestamp>,
}

/// Most points one `stats.series` answers.
pub const SERIES_MAX_POINTS: u32 = 2_000;

/// One step of a series. Steps without requests are present (dense series) with zero
/// counts and no percentiles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SeriesPointDto {
    /// Start of the step.
    pub at: Timestamp,
    #[specta(type = specta_typescript::Number)]
    pub requests: u64,
    pub rps: f64,
    #[specta(type = specta_typescript::Number)]
    pub bytes: u64,
    #[specta(type = specta_typescript::Number)]
    pub status_4xx: u64,
    #[specta(type = specta_typescript::Number)]
    pub status_5xx: u64,
    pub error_pct: f64,
    /// Total request time percentiles; `None` without requests.
    pub p50_ms: Option<f64>,
    pub p90_ms: Option<f64>,
    pub p99_ms: Option<f64>,
    /// Attributed CPU/RAM (minutes and hours average their 10 s points); `None` when no
    /// resource sample covers the step.
    pub cpu_pct: Option<f64>,
    #[specta(type = Option<specta_typescript::Number>)]
    pub mem_bytes: Option<u64>,
    pub workers_active: Option<u32>,
    /// CPU/RAM are apportioned estimates (shared pool).
    pub approx: bool,
    /// Adaptive sampling was active in the step.
    pub sampled: bool,
}

/// An event drawn on the charts (PHP switch, restart, config change, update). Built from
/// the `events` table; the GUI labels it from `kind` and `args`, never from English text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SeriesMarkerDto {
    /// `seq` of the event (`events.list` gives the whole event).
    #[specta(type = specta_typescript::Number)]
    pub seq: u64,
    pub at: Timestamp,
    /// Event kind: `site.php_changed`, `site.updated`, `service.running|stopped|crashed`
    /// of the site's web server or pool, `tool.updated|rollback` of its PHP,
    /// `config.rendered` of its vhost.
    pub kind: String,
    /// Selected payload fields for the label (`from`, `to`, `tool`, `instance`, `owner`).
    /// Never an absolute path: `config.rendered` gives its root-relative `fs_path`.
    pub args: BTreeMap<String, String>,
}

/// Result of `stats.series`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SeriesDto {
    pub site_id: SiteId,
    pub host: String,
    pub range: StatsWindow,
    pub step_ms: u32,
    /// Span covered: aligned to the step, limited by the retention of the step's tier.
    pub from: Timestamp,
    pub to: Timestamp,
    /// Oldest first, one per step of `[from, to)`.
    pub points: Vec<SeriesPointDto>,
    /// Oldest first.
    pub markers: Vec<SeriesMarkerDto>,
}

/// Params of `stats.routes` ("Rotas mais lentas" and "Erros recentes", docs/14 §4.1).
/// There is no window: routes cover the two 12 h generations of the top-K (12 to 24 h,
/// `RoutesDto.since`) and errors the last 24 h (ADR-0044, ADR-0048).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct StatsRoutesParams {
    pub site: SiteRef,
}

/// One monitored route of the top-K.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct RouteStatsDto {
    /// `GET`, `POST`, ... (`OTHER` for unknown methods; empty when the log has none).
    pub method: String,
    /// Normalized route (`/users/{id}`), never with a query string.
    pub route: String,
    /// Estimated requests since `RoutesDto.since`: an upper bound.
    #[specta(type = specta_typescript::Number)]
    pub count: u64,
    /// Lower bound of the same count (`count - overcount`).
    #[specta(type = specta_typescript::Number)]
    pub count_min: u64,
    /// Requests observed since the route entered the table: what the latency and the
    /// error counts cover.
    #[specta(type = specta_typescript::Number)]
    pub observed: u64,
    #[specta(type = specta_typescript::Number)]
    pub status_4xx: u64,
    #[specta(type = specta_typescript::Number)]
    pub status_5xx: u64,
    /// `(4xx + 5xx) / observed`, in percent.
    pub error_pct: f64,
    pub mean_ms: Option<f64>,
    /// Percentiles at 2 significant digits.
    pub p50_ms: Option<f64>,
    pub p90_ms: Option<f64>,
    pub p99_ms: Option<f64>,
}

/// A failed request of the access log (v1 source of "Erros recentes", ADR-0044).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct RecentErrorDto {
    /// End of the request.
    pub at: Timestamp,
    pub status: u16,
    pub method: String,
    /// Normalized route, never with a query string.
    pub route: String,
    /// Host the request was for (the site's host or one of its aliases).
    pub host: String,
    pub duration_ms: f64,
    /// Upstream (PHP/proxy) time, when the server logs it.
    pub upstream_ms: Option<f64>,
    #[specta(type = specta_typescript::Number)]
    pub bytes: u64,
}

/// Result of `stats.routes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct RoutesDto {
    pub site_id: SiteId,
    pub host: String,
    /// Start of the oldest route generation in the report.
    pub since: Timestamp,
    pub at: Timestamp,
    /// Requests seen since `since`, monitored or not.
    #[specta(type = specta_typescript::Number)]
    pub total: u64,
    /// At most 100 routes, by decreasing `count`.
    pub routes: Vec<RouteStatsDto>,
    /// The last 50 5xx and the last 50 4xx of the last 24 h, newest first.
    pub recent_errors: Vec<RecentErrorDto>,
}

/// Export format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum StatsExportFormat {
    Csv,
    Json,
}

/// Params of `stats.export`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct StatsExportParams {
    pub site: SiteRef,
    pub from: forge_core::Timestamp,
    pub to: forge_core::Timestamp,
    pub format: StatsExportFormat,
}

/// Result of the `stats.export` job (written by the client where it wants).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct StatsExportDto {
    pub format: StatsExportFormat,
    pub content: String,
}

/// Params of `metrics.subscribe` (`forge stats --watch`, GUI performance tab).
///
/// Samples are state, not history (ADR-0044): the subscription starts with a snapshot
/// (one sample per matched site, metrics off included), then one sample per site with
/// metrics on every [`StatsWindow::subscribe_cadence_ms`]. `seq` counts the items of the
/// subscription from its `last_seq`, which is `resume_from` when given: a resumed
/// subscription answers a fresh snapshot and nothing of the gap is replayed (the charts
/// refill it with `stats.series`); queue drops are `metrics.lagged` (ADR-0015).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MetricsSubscribeParams {
    /// One site or all.
    #[serde(default)]
    pub site: Option<SiteRef>,
    #[serde(default)]
    pub window: StatsWindow,
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub resume_from: Option<u64>,
}

impl Resumable for MetricsSubscribeParams {
    fn resume_from(&self) -> Option<u64> {
        self.resume_from
    }
    fn set_resume_from(&mut self, seq: Option<u64>) {
        self.resume_from = seq;
    }
}

/// Params of the `metrics.site` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct MetricsEnvelope {
    pub subscription_id: SubscriptionId,
    #[specta(type = specta_typescript::Number)]
    pub seq: u64,
    pub sample: SiteStatsDto,
}
