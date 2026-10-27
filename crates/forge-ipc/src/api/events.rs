//! `events.*`: the event stream (docs/03 §8, ADR-0009 rule 3) and the versioned payloads
//! of the canonical events.
//!
//! `events.subscribe {filter, resume_from}` answers with a `SubscriptionDto`, then sends
//! `events.event` notifications ([`EventEnvelope`]) in `seq` order. With `resume_from: N`
//! the daemon first replays every matching event with `seq > N` (from its in-memory ring
//! and the `events` table), then continues live, without duplicates. A subscriber too
//! slow to keep up loses the oldest notifications and gets `events.lagged {missed}`.
//!
//! Payload structs carry `v` (only fields are added; a breaking change bumps `v`). The
//! `job.*` and `actor.*` payloads come from `forge-core` (re-exported); the others are
//! defined here. [`EVENT_PAYLOADS`] maps each kind to its payload type for `methods.ts`.

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::{ApiVersion, Resumable};
use super::net::DnsPolicyState;
use super::services::{PortHolderDto, PortProtocol, ServiceState};
use forge_core::Timestamp;
pub use forge_core::actor::{ACTOR_EVENT_VERSION, ActorFailedPayload, ActorRestartedPayload};
use forge_core::event::Event;
use forge_core::ids::{EventId, InstanceId, JobId, SiteId, SubscriptionId, ToolId, VersionId};
use forge_core::jobs::{JobErrorInfo, JobProgress};

/// Version of the payloads defined in this module.
pub const EVENT_PAYLOAD_VERSION: u32 = 1;

/// An event as sent to clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct EventDto {
    pub id: EventId,
    /// Global, strictly increasing by one (gaps only mean lost events).
    #[specta(type = specta_typescript::Number)]
    pub seq: u64,
    pub ts: Timestamp,
    /// Canonical dotted name (`site.created`). A string, not an enum: newer daemons may
    /// send kinds older clients do not know.
    pub kind: String,
    /// Versioned payload (see [`EVENT_PAYLOADS`]).
    #[specta(type = specta_typescript::Unknown)]
    pub payload: serde_json::Value,
}

impl From<Event> for EventDto {
    fn from(event: Event) -> Self {
        Self {
            id: event.id,
            seq: event.seq,
            ts: event.ts,
            kind: event.kind.as_str().to_owned(),
            payload: event.payload,
        }
    }
}

impl From<&Event> for EventDto {
    fn from(event: &Event) -> Self {
        Self::from(event.clone())
    }
}

/// Which events a subscription wants. Every set criterion must match.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct EventFilter {
    /// Exact kinds (`site.created`) or prefixes ending in `.*` (`job.*`). Empty = all.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Only events whose payload has this `job_id`.
    #[serde(default)]
    pub job_id: Option<JobId>,
    /// Only events whose payload has this `site_id`.
    #[serde(default)]
    pub site_id: Option<SiteId>,
    /// Only events whose payload has this `instance_id`.
    #[serde(default)]
    pub instance_id: Option<InstanceId>,
}

impl EventFilter {
    /// Filter on kinds only.
    pub fn kinds<I, S>(kinds: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            kinds: kinds.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    /// Whether `kind` passes the kind criterion.
    pub fn matches_kind(&self, kind: &str) -> bool {
        self.kinds.is_empty()
            || self
                .kinds
                .iter()
                .any(|pattern| match pattern.strip_suffix('*') {
                    Some(prefix) => kind.starts_with(prefix),
                    None => pattern == kind,
                })
    }

    /// Whether the event passes every criterion.
    pub fn matches(&self, kind: &str, payload: &serde_json::Value) -> bool {
        fn field_is(payload: &serde_json::Value, field: &str, want: Option<String>) -> bool {
            want.is_none_or(|want| payload.get(field).and_then(|v| v.as_str()) == Some(&want))
        }
        self.matches_kind(kind)
            && field_is(payload, "job_id", self.job_id.map(|id| id.to_string()))
            && field_is(payload, "site_id", self.site_id.map(|id| id.to_string()))
            && field_is(
                payload,
                "instance_id",
                self.instance_id.map(|id| id.to_string()),
            )
    }

    /// [`EventFilter::matches`] on a domain event.
    pub fn matches_event(&self, event: &Event) -> bool {
        self.matches(event.kind.as_str(), &event.payload)
    }
}

/// Params of `events.subscribe`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct EventsSubscribeParams {
    #[serde(default)]
    pub filter: EventFilter,
    /// Replay every matching event with `seq > resume_from` before going live.
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub resume_from: Option<u64>,
}

impl Resumable for EventsSubscribeParams {
    fn resume_from(&self) -> Option<u64> {
        self.resume_from
    }
    fn set_resume_from(&mut self, seq: Option<u64>) {
        self.resume_from = seq;
    }
}

/// Params of the `events.event` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct EventEnvelope {
    pub subscription_id: SubscriptionId,
    /// Same as `event.seq`.
    #[specta(type = specta_typescript::Number)]
    pub seq: u64,
    pub event: EventDto,
}

/// Params of `events.list` (history page).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct EventsListParams {
    /// Events with `seq > after_seq`.
    #[serde(default)]
    #[specta(type = specta_typescript::Number)]
    pub after_seq: u64,
    /// Default 500, maximum 5000.
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub filter: EventFilter,
}

/// Result of `events.list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct EventPageDto {
    pub events: Vec<EventDto>,
    /// Cursor for the next page (`after_seq`).
    #[specta(type = specta_typescript::Number)]
    pub next_after: u64,
    /// `seq` of the newest event when the page was read.
    #[specta(type = specta_typescript::Number)]
    pub last_seq: u64,
    pub has_more: bool,
}

/// `daemon.ready`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DaemonReadyPayload {
    pub v: u32,
    pub version: String,
    pub api_version: ApiVersion,
}

/// `daemon.shutdown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DaemonShutdownPayload {
    pub v: u32,
    /// `user`, `restart`, `update`, `signal`.
    pub reason: String,
}

/// `fs.case_sensitivity_applied|unavailable`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct FsCaseSensitivityPayload {
    pub v: u32,
    pub dir: String,
    pub site_id: Option<SiteId>,
    /// Why it is unavailable.
    pub reason: Option<String>,
}

/// `tool.install.started|progress|completed|failed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ToolInstallPayload {
    pub v: u32,
    pub job_id: Option<JobId>,
    pub tool: ToolId,
    pub version: String,
    /// Set once the version is registered.
    pub version_id: Option<VersionId>,
    pub progress: Option<JobProgress>,
    pub error: Option<JobErrorInfo>,
}

/// `tool.updated|rollback`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolVersionChangePayload {
    pub v: u32,
    pub tool: ToolId,
    pub from: String,
    pub to: String,
    pub version_id: VersionId,
}

/// `service.starting|running|stopping|stopped|crashed|failed`.
///
/// Published by `forge_supervisor::events::ServiceEventPayload`, which serializes the
/// same fields (docs/03 §8). The fields after `error` were added without bumping `v`:
/// they may be missing, and older daemons never send them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ServicePayload {
    pub v: u32,
    pub instance_id: InstanceId,
    /// `mysql/default`.
    pub name: String,
    pub tool: ToolId,
    pub state: ServiceState,
    pub pid: Option<u32>,
    /// `service.stopping`: strategy being tried (`command`, `ctrl_break`, `wm_close`,
    /// `terminate`). `service.stopped`: step after which the process exited.
    pub strategy: Option<String>,
    /// Process exit code (Windows `DWORD` bits as `i32`: `0xC000013A` is negative).
    pub exit_code: Option<i32>,
    /// `service.failed`: why the instance could not be started or stopped.
    pub error: Option<JobErrorInfo>,
    /// Number of this start for the instance (1, 2, ...); `service.starting|running`.
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub run: Option<u64>,
    /// `service.stopping`: position of `strategy` in the stop cascade (1 = command,
    /// 2 = CTRL_BREAK, 3 = WM_CLOSE, 4 = TerminateJobObject; docs/03 §1.3).
    #[serde(default)]
    pub step: Option<u8>,
    /// `service.stopping`: how long this step waits for the process to exit.
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub timeout_ms: Option<u64>,
    /// `service.stopped`: duration of the stop.
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub elapsed_ms: Option<u64>,
    /// `service.stopped|crashed`: processes left in the instance's job after the main
    /// process exited, then terminated.
    #[serde(default)]
    pub leftovers_killed: Option<u32>,
}

/// `pool.scaled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PoolScaledPayload {
    pub v: u32,
    pub version_id: VersionId,
    pub series: String,
    pub from_workers: u32,
    pub to_workers: u32,
}

/// `pool.worker.recycled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PoolWorkerRecycledPayload {
    pub v: u32,
    pub version_id: VersionId,
    pub series: String,
    pub port: u16,
    /// `max_requests`, `max_rss`, `max_age`, `crashed`.
    pub reason: String,
}

/// `site.created|updated|removed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SitePayload {
    pub v: u32,
    pub site_id: SiteId,
    pub host: String,
}

/// `site.php_changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SitePhpChangedPayload {
    pub v: u32,
    pub site_id: SiteId,
    pub host: String,
    pub from: Option<String>,
    pub to: String,
}

/// `config.rendered|drift_detected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ConfigFilePayload {
    pub v: u32,
    /// What the file configures (`nginx`, `site:loja.test`, `php@8.3`).
    pub owner: String,
    /// Informative absolute path.
    pub path: String,
    /// Root-relative path for `fs.read`, when under `etc/`.
    pub fs_path: Option<String>,
}

/// `dns.policy_applied`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DnsPolicyPayload {
    pub v: u32,
    pub tld: String,
    pub policy: DnsPolicyState,
}

/// `tls.ca_created|leaf_issued|renewed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TlsPayload {
    pub v: u32,
    /// Leaf host; `None` for the CA.
    pub host: Option<String>,
    pub site_id: Option<SiteId>,
    pub thumbprint_sha256: String,
    pub not_after: Timestamp,
}

/// `port.conflict`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PortConflictPayload {
    pub v: u32,
    pub port: u16,
    pub protocol: PortProtocol,
    /// Forgebox owner that wanted the port (`mysql/default`).
    pub owner_name: String,
    pub instance_id: Option<InstanceId>,
    pub holder: Option<PortHolderDto>,
}

/// `helper.op` (docs/11 §1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct HelperOpPayload {
    pub v: u32,
    /// Allowlisted operation (`nrpt.add`, `trust.add_machine_root`...).
    pub name: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// Payload type of every canonical event kind (docs/03 §8), for `methods.ts`.
pub const EVENT_PAYLOADS: &[(&str, &str)] = &[
    ("daemon.ready", "DaemonReadyPayload"),
    ("daemon.shutdown", "DaemonShutdownPayload"),
    ("job.started", "JobStartedPayload"),
    ("job.progress", "JobProgressPayload"),
    ("job.done", "JobDonePayload"),
    ("job.failed", "JobFailedPayload"),
    ("job.cancelled", "JobCancelledPayload"),
    ("actor.restarted", "ActorRestartedPayload"),
    ("actor.failed", "ActorFailedPayload"),
    ("fs.case_sensitivity_applied", "FsCaseSensitivityPayload"),
    (
        "fs.case_sensitivity_unavailable",
        "FsCaseSensitivityPayload",
    ),
    ("tool.install.started", "ToolInstallPayload"),
    ("tool.install.progress", "ToolInstallPayload"),
    ("tool.install.completed", "ToolInstallPayload"),
    ("tool.install.failed", "ToolInstallPayload"),
    ("tool.updated", "ToolVersionChangePayload"),
    ("tool.rollback", "ToolVersionChangePayload"),
    ("service.starting", "ServicePayload"),
    ("service.running", "ServicePayload"),
    ("service.stopping", "ServicePayload"),
    ("service.stopped", "ServicePayload"),
    ("service.crashed", "ServicePayload"),
    ("service.failed", "ServicePayload"),
    ("pool.scaled", "PoolScaledPayload"),
    ("pool.worker.recycled", "PoolWorkerRecycledPayload"),
    ("site.created", "SitePayload"),
    ("site.updated", "SitePayload"),
    ("site.removed", "SitePayload"),
    ("site.php_changed", "SitePhpChangedPayload"),
    ("config.rendered", "ConfigFilePayload"),
    ("config.drift_detected", "ConfigFilePayload"),
    ("dns.policy_applied", "DnsPolicyPayload"),
    ("tls.ca_created", "TlsPayload"),
    ("tls.leaf_issued", "TlsPayload"),
    ("tls.renewed", "TlsPayload"),
    ("port.conflict", "PortConflictPayload"),
    ("helper.op", "HelperOpPayload"),
];

/// Registers every payload type for the TypeScript export.
pub(crate) fn register_payload_types(types: specta::Types) -> specta::Types {
    types
        .register::<DaemonReadyPayload>()
        .register::<DaemonShutdownPayload>()
        .register::<forge_core::jobs::JobStartedPayload>()
        .register::<forge_core::jobs::JobProgressPayload>()
        .register::<forge_core::jobs::JobDonePayload>()
        .register::<forge_core::jobs::JobFailedPayload>()
        .register::<forge_core::jobs::JobCancelledPayload>()
        .register::<ActorRestartedPayload>()
        .register::<ActorFailedPayload>()
        .register::<FsCaseSensitivityPayload>()
        .register::<ToolInstallPayload>()
        .register::<ToolVersionChangePayload>()
        .register::<ServicePayload>()
        .register::<PoolScaledPayload>()
        .register::<PoolWorkerRecycledPayload>()
        .register::<SitePayload>()
        .register::<SitePhpChangedPayload>()
        .register::<ConfigFilePayload>()
        .register::<DnsPolicyPayload>()
        .register::<TlsPayload>()
        .register::<PortConflictPayload>()
        .register::<HelperOpPayload>()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn filter_by_kind_patterns() {
        let f = EventFilter::kinds(["job.*", "site.created"]);
        assert!(f.matches_kind("job.done"));
        assert!(f.matches_kind("site.created"));
        assert!(!f.matches_kind("site.removed"));
        assert!(EventFilter::default().matches_kind("anything"));
    }

    #[test]
    fn filter_by_payload_ids() {
        let job = JobId::new();
        let f = EventFilter {
            job_id: Some(job),
            ..EventFilter::default()
        };
        assert!(f.matches("job.done", &json!({ "job_id": job.to_string() })));
        assert!(!f.matches("job.done", &json!({ "job_id": JobId::new().to_string() })));
        assert!(!f.matches("job.done", &json!({})));
    }

    #[test]
    fn every_canonical_kind_has_a_payload_type() {
        for kind in forge_core::event::EventKind::ALL {
            assert!(
                EVENT_PAYLOADS.iter().any(|(k, _)| *k == kind.as_str()),
                "{kind} has no payload type"
            );
        }
        assert_eq!(
            EVENT_PAYLOADS.len(),
            forge_core::event::EventKind::ALL.len()
        );
    }
}
