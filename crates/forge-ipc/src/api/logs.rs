//! `logs.*`: reading and following logs (`forge logs`, `forge daemon logs`).
//!
//! Targets use the CLI syntax: `daemon`, `nginx`, `nginx.error`, `php@8.3`,
//! `mysql/default`, `site:loja.test`, `app:loja.test/web`. Each target has its own
//! monotonic line `seq` (ADR-0009, rule 3; ADR-0046), so `logs.subscribe` can resume and
//! `logs.read {before_seq}` can page back. `logs.targets` lists what exists (ADR-0040).

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::Resumable;
use forge_core::Timestamp;
use forge_core::ids::SubscriptionId;

/// Where a log line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LogSource {
    Stdout,
    Stderr,
    File,
}

/// The fields of a web server access-log line (`site:<host>` targets; parsed by the
/// manifest's access-log field mapping, never by tool id). API 1.1 (ADR-0040).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct AccessLineDto {
    /// Host of the request, lowercase, without the port.
    pub host: String,
    pub status: u16,
    pub method: String,
    /// Path and query as logged.
    pub uri: String,
    /// Request time in milliseconds (fractional: servers log µs or ms).
    #[serde(default)]
    pub rt_ms: Option<f64>,
    /// Upstream address that served it (`127.0.0.1:19083`), when proxied.
    #[serde(default)]
    pub upstream: Option<String>,
}

/// One log line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct LogLineDto {
    /// Monotonic per target.
    #[specta(type = specta_typescript::Number)]
    pub seq: u64,
    pub ts: Option<Timestamp>,
    pub source: LogSource,
    /// `error`, `warn`, `info`... when the format tells.
    pub level: Option<String>,
    pub text: String,
    /// Access-log lines: the parsed request. API 1.1 (absent from 1.0 daemons).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AccessLineDto>,
}

/// Params of `logs.read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LogsReadParams {
    pub target: String,
    /// Only lines newer than this many seconds (relative: client and daemon clocks may
    /// differ).
    #[serde(default)]
    pub since_secs: Option<u32>,
    /// Case-sensitive substring filter.
    #[serde(default)]
    pub grep: Option<String>,
    /// Last N lines (default 200).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Only lines with `seq < before_seq` (the newest `limit` of them): pages back from the
    /// oldest line a client has. API 1.1 (ADR-0040).
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub before_seq: Option<u64>,
}

/// Result of `logs.read`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct LogChunkDto {
    pub target: String,
    pub lines: Vec<LogLineDto>,
    /// `seq` of the newest line of the target (resume point for `logs.subscribe`).
    #[specta(type = specta_typescript::Number)]
    pub last_seq: u64,
}

/// Params of `logs.subscribe` (`forge logs -f`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LogsSubscribeParams {
    pub target: String,
    #[serde(default)]
    pub grep: Option<String>,
    /// Send the last N lines first (ignored when `resume_from` is set).
    #[serde(default)]
    pub tail: Option<u32>,
    /// Resume after this line `seq`.
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub resume_from: Option<u64>,
}

impl Resumable for LogsSubscribeParams {
    fn resume_from(&self) -> Option<u64> {
        self.resume_from
    }
    fn set_resume_from(&mut self, seq: Option<u64>) {
        self.resume_from = seq;
    }
}

/// Params of the `logs.line` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct LogEnvelope {
    pub subscription_id: SubscriptionId,
    /// Same as `line.seq`.
    #[specta(type = specta_typescript::Number)]
    pub seq: u64,
    pub line: LogLineDto,
}

/// What a log target is (the target syntax of docs/09).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LogTargetKind {
    /// `daemon`: the daemon's own log.
    Daemon,
    /// `mysql/default`, `nginx`: an instance's stdout/stderr.
    Instance,
    /// `nginx.error`: a file the manifest declares (`service.daemon.logs`).
    InstanceFile,
    /// `php@8.3`: a PHP pool.
    Pool,
    /// `site:loja.test`: the access lines of a site (and its aliases).
    Site,
    /// `app:loja.test/web`: a process of a site.
    App,
}

/// A log target that `logs.read`/`logs.subscribe` accept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LogTargetDto {
    /// What to pass as `target` (`nginx.error`).
    pub target: String,
    /// Human label (`nginx · error`); English, from the manifest when it names the log.
    pub label: String,
    pub kind: LogTargetKind,
    /// Grouping key for pickers: the tool (`nginx`), `daemon`, `php` or the site host.
    pub group: String,
    /// Line format of the manifest (`nginx-error`, `php`, `forgebox-json`...), when known:
    /// picks the parser for `level`/`ts`.
    #[serde(default)]
    pub format: Option<String>,
    /// Lines arrive live (a running instance or a followed file).
    #[serde(default)]
    pub live: bool,
    /// Root-relative path of the current file (for `fs.tail`), when there is one.
    #[serde(default)]
    pub file: Option<String>,
}

/// Result of `logs.targets` (`forge logs --list`). API 1.1 (ADR-0040).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LogTargetListDto {
    pub targets: Vec<LogTargetDto>,
}
