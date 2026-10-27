//! `stats.*` and `metrics.*`: per-site metrics (docs/09 §stats, docs/14).
//!
//! `metrics.subscribe` is the `metrics.site{site, window}` stream of docs/14 §3: one
//! sample per site and window, with its own `seq`.

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::{Resumable, SiteRef};
use super::services::PortOwnerKind;
use forge_core::Timestamp;
use forge_core::ids::{SiteId, SubscriptionId};

/// Aggregation window.
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

/// Metrics of one site over a window (docs/14 §1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SiteStatsDto {
    pub site_id: SiteId,
    pub host: String,
    pub window: StatsWindow,
    pub at: Timestamp,
    #[specta(type = specta_typescript::Number)]
    pub requests: u64,
    pub rps: f64,
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
}

/// Params of `stats.get`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct StatsParams {
    #[serde(default)]
    pub site: Option<SiteRef>,
    #[serde(default)]
    pub window: StatsWindow,
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

/// Result of `stats.top`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct TopDto {
    pub processes: Vec<ProcessUsageDto>,
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
