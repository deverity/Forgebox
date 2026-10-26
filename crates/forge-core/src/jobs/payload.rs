//! Job snapshots and the versioned payloads of the `job.*` events.
//!
//! Wire format (serde, field `v` = [`JOB_EVENT_VERSION`]); `forge-ipc` re-exports these
//! types when the API contracts are written (M1-E5-T1). Fields are only ever added; a
//! breaking change bumps `v`.

use serde::{Deserialize, Serialize};

use crate::Timestamp;
use crate::error::ForgeError;
use crate::ids::{JobId, RequestId};

/// Version of the `job.*` event payloads.
pub const JOB_EVENT_VERSION: u32 = 1;

/// Progress report. Fields left `None` keep their previous value when merged into the
/// job snapshot, so a job may report only a message, only bytes, etc.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JobProgress {
    /// Percentage in `0.0..=100.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pct: Option<f32>,
    /// Short human readable step ("extracting", "verifying sha256").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_done: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_total: Option<u64>,
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
    }
}

/// Lifecycle of a job.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobErrorInfo {
    pub code: String,
    pub message: String,
}

impl From<&ForgeError> for JobErrorInfo {
    fn from(err: &ForgeError) -> Self {
        Self {
            code: err.code().to_owned(),
            message: err.to_string(),
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStartedPayload {
    pub v: u32,
    pub job_id: JobId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
}

/// Payload of `job.progress` (the merged progress of the job, not just the delta).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobProgressPayload {
    pub v: u32,
    pub job_id: JobId,
    #[serde(flatten)]
    pub progress: JobProgress,
}

/// Payload of `job.done`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobDonePayload {
    pub v: u32,
    pub job_id: JobId,
    pub name: String,
    pub duration_ms: u64,
    /// Serialized return value of the job (`null` for `()`).
    pub result: serde_json::Value,
}

/// Payload of `job.failed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobFailedPayload {
    pub v: u32,
    pub job_id: JobId,
    pub name: String,
    pub duration_ms: u64,
    pub error: JobErrorInfo,
}

/// Payload of `job.cancelled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCancelledPayload {
    pub v: u32,
    pub job_id: JobId,
    pub name: String,
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
    }
}
