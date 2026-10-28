//! `daemon.*`: status, stop/restart and diagnostics (docs/09 §daemon).
//!
//! `forge daemon start` is client-side (it launches `forgeboxd`); `forge daemon logs`
//! uses `logs.read`/`logs.subscribe` with target `daemon`.

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::{ApiVersion, CheckDto};
use forge_core::Timestamp;

/// Facts about the running daemon (`forge daemon status`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct DaemonStatusDto {
    pub version: String,
    pub api_version: ApiVersion,
    /// `windows`, `linux`, `macos`.
    pub platform: String,
    pub pid: u32,
    pub started_at: Timestamp,
    #[specta(type = specta_typescript::Number)]
    pub uptime_secs: u64,
    /// Installation root on the daemon's machine (informative, `/` separators).
    pub root: String,
    pub portable: bool,
    /// `seq` of the last event published (resume point for `events.subscribe`).
    #[specta(type = specta_typescript::Number)]
    pub last_event_seq: u64,
    pub active_jobs: u32,
    pub connections: u32,
}

/// Params of `daemon.stop`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DaemonStopParams {
    /// Keep services running? `false` (default) stops the whole stack first.
    #[serde(default)]
    pub keep_services: bool,
}

/// Params of `daemon.doctor`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DoctorParams {
    /// Check ids or prefixes to run (`dns`, `tls.ca_trusted`); empty = all.
    #[serde(default)]
    pub checks: Vec<String>,
}

/// Result of the `daemon.doctor` job: PATH, DNS, CA, ports, vcredist, Defender, versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DoctorReportDto {
    pub checks: Vec<CheckDto>,
}
