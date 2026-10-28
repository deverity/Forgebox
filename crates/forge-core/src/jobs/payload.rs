//! Job snapshots and the versioned payloads of the `job.*` events.
//!
//! Wire format (serde, field `v` = [`JOB_EVENT_VERSION`]); `forge-ipc` re-exports these
//! types in the API contracts (`forge_ipc::api::jobs`) and exports them to TypeScript
//! (feature `specta`). Fields are only ever added; a breaking change bumps `v`.
//! [`JobInfo`] is the in-process snapshot; the API sends `forge_ipc::api::JobDto`.

use serde::{Deserialize, Serialize};

use crate::Timestamp;
use crate::error::ForgeError;
use crate::ids::{JobId, RequestId};

/// Version of the `job.*` event payloads.
pub const JOB_EVENT_VERSION: u32 = 1;

/// Progress report. Fields left `None` keep their previous value when merged into the
/// job snapshot, so a job may report only a message, only bytes, etc.
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JobProgress {
    /// Percentage in `0.0..=100.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pct: Option<f32>,
    /// Short human readable step ("extracting", "verifying sha256").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "specta", specta(type = Option<specta_typescript::Number>))]
    pub bytes_done: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "specta", specta(type = Option<specta_typescript::Number>))]
    pub bytes_total: Option<u64>,
    /// Smoothed transfer speed in bytes per second (downloads).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "specta", specta(type = Option<specta_typescript::Number>))]
    pub speed_bps: Option<u64>,
    /// Estimated seconds until completion (downloads with a known size).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "specta", specta(type = Option<specta_typescript::Number>))]
    pub eta_secs: Option<u64>,
}

impl JobProgress {
    /// Empty report (changes nothing when merged).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the percentage, clamped to `0.0..=100.0` (NaN becomes 0).
    pub fn pct(mut self, pct: f32) -> Self {
        self.pct = Some(if pct.is_nan() {
            0.0
        } else {
            pct.clamp(0.0, 100.0)
        });
        self
    }

    /// Sets the step message.
    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Sets bytes done/total (downloads, extraction).
    pub fn bytes(mut self, done: u64, total: Option<u64>) -> Self {
        self.bytes_done = Some(done);
        self.bytes_total = total.or(self.bytes_total);
        self
    }

    /// Sets speed (bytes/s) and ETA (seconds).
    pub fn rate(mut self, speed_bps: u64, eta_secs: Option<u64>) -> Self {
        self.speed_bps = Some(speed_bps);
        self.eta_secs = eta_secs;
        self
    }

    /// Overwrites the fields present in `update`.
    pub fn merge(&mut self, update: JobProgress) {
        if update.pct.is_some() {
            self.pct = update.pct;
        }
        if update.message.is_some() {
            self.message = update.message;
        }
        if update.bytes_done.is_some() {
            self.bytes_done = update.bytes_done;
        }
        if update.bytes_total.is_some() {
            self.bytes_total = update.bytes_total;
        }
        if update.speed_bps.is_some() {
            self.speed_bps = update.speed_bps;
        }
        if update.eta_secs.is_some() {
            self.eta_secs = update.eta_secs;
        }
    }
}

/// Lifecycle of a job.
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    /// Everything but `Running`.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// Error of a failed job: the stable API code ([`ForgeError::code`]) and the message.
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobErrorInfo {
    pub code: String,
    pub message: String,
    /// Structured details (`{reason, args}`, [`ForgeError::details`]); clients localize
    /// from `code` plus these. Added without bumping `v` (optional, may be missing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "specta", specta(type = Option<specta_typescript::Unknown>))]
    pub details: Option<serde_json::Value>,
}

impl From<&ForgeError> for JobErrorInfo {
    fn from(err: &ForgeError) -> Self {
        Self {
            code: err.code().to_owned(),
            message: err.to_string(),
            details: err.details(),
        }
    }
}

/// Snapshot of a job, as returned by `JobRegistry::get`/`list` (and later `jobs.list`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobInfo {
    pub id: JobId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    pub state: JobState,
    pub progress: JobProgress,
    pub cancel_requested: bool,
    pub started_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    /// Value returned by a `Done` job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error of a `Failed` job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JobErrorInfo>,
}

/// Payload of `job.started`.
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStartedPayload {
    pub v: u32,
    pub job_id: JobId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
}

/// Payload of `job.progress` (the merged progress of the job, not just the delta).
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobProgressPayload {
    pub v: u32,
    pub job_id: JobId,
    #[serde(flatten)]
    pub progress: JobProgress,
}

/// Payload of `job.done`.
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobDonePayload {
    pub v: u32,
    pub job_id: JobId,
    pub name: String,
    #[cfg_attr(feature = "specta", specta(type = specta_typescript::Number))]
    pub duration_ms: u64,
    /// Serialized return value of the job (`null` for `()`).
    #[cfg_attr(feature = "specta", specta(type = specta_typescript::Unknown))]
    pub result: serde_json::Value,
}

/// Payload of `job.failed`.
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobFailedPayload {
    pub v: u32,
    pub job_id: JobId,
    pub name: String,
    #[cfg_attr(feature = "specta", specta(type = specta_typescript::Number))]
    pub duration_ms: u64,
    pub error: JobErrorInfo,
}

/// Payload of `job.cancelled`.
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCancelledPayload {
    pub v: u32,
    pub job_id: JobId,
    pub name: String,
    #[cfg_attr(feature = "specta", specta(type = specta_typescript::Number))]
    pub duration_ms: u64,
    /// `true` when the job ignored the request past the grace period and its future
    /// was dropped.
    pub forced: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_merges_present_fields_only() {
        let mut p = JobProgress::new().pct(10.0).message("downloading");
        p.merge(JobProgress::new().bytes(512, Some(2048)));
        assert_eq!(p.pct, Some(10.0));
        assert_eq!(p.message.as_deref(), Some("downloading"));
        assert_eq!((p.bytes_done, p.bytes_total), (Some(512), Some(2048)));
        p.merge(JobProgress::new().pct(250.0).bytes(1024, None));
        assert_eq!(p.pct, Some(100.0), "clamped");
        assert_eq!((p.bytes_done, p.bytes_total), (Some(1024), Some(2048)));
        assert_eq!(JobProgress::new().pct(f32::NAN).pct, Some(0.0));
    }

    #[test]
    fn rate_fields_merge_and_are_omitted_when_unset() {
        let mut p = JobProgress::new().bytes(10, Some(100));
        assert!(serde_json::to_value(&p).unwrap().get("speed_bps").is_none());
        p.merge(JobProgress::new().rate(5, Some(18)));
        p.merge(JobProgress::new().pct(10.0));
        assert_eq!((p.speed_bps, p.eta_secs), (Some(5), Some(18)));
        assert_eq!(p.bytes_done, Some(10));
    }

    #[test]
    fn progress_payload_is_flat_and_versioned() {
        let payload = JobProgressPayload {
            v: JOB_EVENT_VERSION,
            job_id: JobId::new(),
            progress: JobProgress::new().pct(50.0),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["v"], 1);
        assert_eq!(json["pct"], 50.0);
        assert!(json.get("message").is_none());
        let back: JobProgressPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn states_and_errors_serialize_snake_case() {
        assert_eq!(
            serde_json::to_value(JobState::Cancelled).unwrap(),
            "cancelled"
        );
        assert!(JobState::Done.is_terminal());
        assert!(!JobState::Running.is_terminal());
        let info = JobErrorInfo::from(&ForgeError::NotFound("php 9".into()));
        assert_eq!(info.code, "not_found");
        assert_eq!(info.message, "not found: php 9");
        // Failed jobs carry the structured details too (ADR-0039)...
        assert_eq!(info.details.as_ref().unwrap()["reason"], "not_found");
        assert_eq!(info.details.unwrap()["args"]["what"], "php 9");
        let busy = JobErrorInfo::from(&ForgeError::PortInUse {
            port: 3306,
            owner: Some("mysqld.exe (PID 4412)".into()),
        });
        assert_eq!(busy.details.unwrap()["args"]["port"], 3306);
        // ...and leave the field out when there are none, so old clients see the same JSON.
        let plain =
            serde_json::to_value(JobErrorInfo::from(&ForgeError::Internal("x".into()))).unwrap();
        assert!(plain.get("details").is_none());
        let old: JobErrorInfo =
            serde_json::from_value(serde_json::json!({"code": "io", "message": "m"})).unwrap();
        assert_eq!(old.details, None);
    }
}
