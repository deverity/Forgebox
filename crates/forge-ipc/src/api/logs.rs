//! `logs.*`: reading and following logs (`forge logs`, `forge daemon logs`).
//!
//! Targets use the CLI syntax: `daemon`, `nginx`, `nginx.error`, `php@8.3`,
//! `mysql/default`, `site:loja.test`, `app:loja.test/web`. Each target has its own
//! monotonic line `seq` (ADR-0009, rule 3), so `logs.subscribe` can resume.

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

/// One log line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LogLineDto {
    /// Monotonic per target.
    #[specta(type = specta_typescript::Number)]
    pub seq: u64,
    pub ts: Option<Timestamp>,
    pub source: LogSource,
    /// `error`, `warn`, `info`... when the format tells.
    pub level: Option<String>,
    pub text: String,
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
}

/// Result of `logs.read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LogEnvelope {
    pub subscription_id: SubscriptionId,
    /// Same as `line.seq`.
    #[specta(type = specta_typescript::Number)]
    pub seq: u64,
    pub line: LogLineDto,
}
