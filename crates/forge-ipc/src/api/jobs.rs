//! `jobs.*`: long operations in progress (docs/09 §jobs, docs/03 §7.4).
//!
//! The `job.*` event payloads are produced by `forge_core::jobs` and re-exported here as
//! part of the contract (docs/03 §8). The API snapshot is [`JobDto`], built from
//! `forge_core::jobs::JobInfo`.

use serde::{Deserialize, Serialize};
use specta::Type;

use forge_core::Timestamp;
use forge_core::ids::{JobId, RequestId};
pub use forge_core::jobs::{
    JOB_EVENT_VERSION, JobCancelledPayload, JobDonePayload, JobErrorInfo, JobFailedPayload,
    JobInfo, JobProgress, JobProgressPayload, JobStartedPayload, JobState,
};

/// Snapshot of a job. Long-running methods answer with it immediately; progress and the
/// outcome follow as `job.progress|done|failed|cancelled` events with the same `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct JobDto {
    pub id: JobId,
    /// `tool.install php@8.3`.
    pub name: String,
    pub request_id: Option<RequestId>,
    pub state: JobState,
    pub progress: JobProgress,
    pub cancel_requested: bool,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    /// Value of a `done` job (type given by `MethodInfo::job_result`).
    #[specta(type = Option<specta_typescript::Unknown>)]
    pub result: Option<serde_json::Value>,
    /// Error of a `failed` job.
    pub error: Option<JobErrorInfo>,
}

impl From<JobInfo> for JobDto {
    fn from(info: JobInfo) -> Self {
        Self {
            id: info.id,
            name: info.name,
            request_id: info.request_id,
            state: info.state,
            progress: info.progress,
            cancel_requested: info.cancel_requested,
            started_at: info.started_at,
            finished_at: info.finished_at,
            result: info.result,
            error: info.error,
        }
    }
}

/// Params of `jobs.list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct JobListParams {
    /// Only running jobs (`forge jobs --active`).
    #[serde(default)]
    pub active: bool,
}

/// Result of `jobs.list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct JobListDto {
    pub jobs: Vec<JobDto>,
}

/// Params of `jobs.get` and `jobs.cancel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct JobIdParams {
    pub job_id: JobId,
}

/// Result of `jobs.cancel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct JobCancelDto {
    pub job_id: JobId,
    /// `false` when the job had already finished.
    pub was_running: bool,
}
