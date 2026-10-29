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
use super::net::{DnsMode, DnsPolicyReason, DnsPolicyState};
use super::services::{FailureReasonDto, PortHolderDto, PortProtocol, ServiceState};
use super::tls::TrustScopeDto;
use forge_core::Timestamp;
pub use forge_core::actor::{ACTOR_EVENT_VERSION, ActorFailedPayload, ActorRestartedPayload};
use forge_core::event::Event;
use forge_core::ids::{
    EventId, InstanceId, JobId, SiteId, SubscriptionId, TerminalId, ToolId, UpdateId, VersionId,
};
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
    /// Why it is unavailable (pt-BR text).
    pub reason: Option<String>,
    /// Stable code of `reason`: `unsupported`, `disabled`, `insensitive`, `partial`,
    /// `missing`, `error`, `initialized`, `kept_sensitive`
    /// (`forge_core::fs::case::CaseReasonCode`). For a database data folder (docs/15 §5)
    /// `insensitive`/`unsupported`/`missing`/`error` mean the fallback to
    /// case-insensitive table names; `initialized` that the instance keeps them;
    /// `kept_sensitive` that it keeps case-sensitive ones on a folder that now reads as
    /// not case-sensitive (the server may refuse to start).
    pub reason_code: Option<String>,
}

/// `tool.install.started|progress|completed|failed`.
///
/// Published by `forge_provider::layout::ToolInstallEventPayload`. `step` and
/// `canonical_name` were added without bumping `v` (optional, may be missing).
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
    /// Step running (`progress`) or that failed (`failed`): `verify`, `extract`,
    /// `post_install`, `verify_command`, `move`, `register`, `current`, `shims`.
    #[serde(default)]
    pub step: Option<String>,
    /// Folder name of the version (`8.3.37-nts-vs17-x64`).
    #[serde(default)]
    pub canonical_name: Option<String>,
}

/// `tool.updated|rollback`.
///
/// Published by `forge_provider::update::ToolUpdateEventPayload`. `series`, `job_id`,
/// `update_id`, `rollback_of` and `removed` were added without bumping `v` (optional, may
/// be missing).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolVersionChangePayload {
    pub v: u32,
    pub tool: ToolId,
    pub from: String,
    pub to: String,
    /// Version serving after the update (or the rollback).
    pub version_id: VersionId,
    /// Series of the replaced version (`8.3`).
    #[serde(default)]
    pub series: Option<String>,
    #[serde(default)]
    pub job_id: Option<JobId>,
    /// Row of the update history (`forge tool rollback` undoes the newest one).
    #[serde(default)]
    pub update_id: Option<UpdateId>,
    /// On `tool.rollback`: the update that was undone.
    #[serde(default)]
    pub rollback_of: Option<UpdateId>,
    /// Old version folders removed by retention.
    #[serde(default)]
    pub removed: Vec<String>,
}

/// One update of `tool.updates_available`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PendingUpdateDto {
    pub tool: ToolId,
    /// Series of the active version (`8.3`): the unit of update and rollback.
    pub series: String,
    /// Active version (`8.3.36`).
    pub from: String,
    /// Version the check found (`8.3.37`).
    pub to: String,
}

/// `tool.updates_available`: what the update checks found. The daemon publishes it after
/// each scheduled check and after `tool.updated|rollback`, always with the **whole** list
/// of pending updates (so a client replaces what it shows; an empty list clears the
/// badge). Blocked updates (a build that needs a migration the user must start) are not
/// listed. Added in API 1.1 (ADR-0039).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolUpdatesAvailablePayload {
    pub v: u32,
    pub updates: Vec<PendingUpdateDto>,
    /// `updates.len()`.
    pub count: u32,
}

/// `tool.current_changed`: the `bin/<tool>/current` junction was switched (`forge use`).
///
/// Published by `forge_provider::layout::ToolCurrentEventPayload` (same fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ToolCurrentPayload {
    pub v: u32,
    pub tool: ToolId,
    /// Folder name `current` pointed at before (`8.3.37-nts-vs17-x64`); `None` when it did
    /// not exist.
    pub from: Option<String>,
    /// Folder name it points at now.
    pub to: String,
    /// Registered version behind `to`.
    pub version_id: Option<VersionId>,
    /// Duration of the junction switch itself.
    #[specta(type = specta_typescript::Number)]
    pub elapsed_us: u64,
}

/// `service.starting|running|stopping|stopped|crashed|failed|maintenance`.
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
    /// `service.stopping`: why (`user`, `restart`, `maintenance`, `unhealthy`,
    /// `start_timeout`); `service.crashed`: `unhealthy` when it was stopped for failing
    /// health checks.
    #[serde(default)]
    pub cause: Option<String>,
    /// `service.crashed`: the restart policy starts it again after this delay (absent: no
    /// automatic restart).
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub restart_in_ms: Option<u64>,
    /// `service.failed`: structured diagnosis.
    #[serde(default)]
    pub reason: Option<FailureReasonDto>,
}

/// `service.metrics`: periodic CPU/memory sample of an instance (sum over the processes
/// of its Job Object; opt-in per instance, docs/03 §6). Published by
/// `forge_supervisor::metrics::ServiceMetricsPayload` (same shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ServiceMetricsPayload {
    pub v: u32,
    pub instance_id: InstanceId,
    /// `mysql/default`.
    pub name: String,
    pub tool: ToolId,
    /// Main process of the run.
    pub pid: Option<u32>,
    /// Share of the whole machine since the previous sample (0–100, like Task Manager).
    pub cpu_pct: f64,
    /// Working sets, summed.
    #[specta(type = specta_typescript::Number)]
    pub rss_bytes: u64,
    /// Private commit, summed.
    #[specta(type = specta_typescript::Number)]
    pub private_bytes: u64,
    #[specta(type = specta_typescript::Number)]
    pub handles: u64,
    /// Processes in the instance's job.
    pub processes: u32,
    /// Uptime of the main process.
    #[specta(type = specta_typescript::Number)]
    pub uptime_ms: u64,
    /// Wall time covered by `cpu_pct`.
    #[specta(type = specta_typescript::Number)]
    pub window_ms: u64,
}

/// `pool.scaled`: the backends of a pool changed (workers added, removed or replaced).
/// Produced by `forge_fcgi::PoolScaledEvent` (same fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PoolScaledPayload {
    pub v: u32,
    pub version_id: VersionId,
    pub series: String,
    pub from_workers: u32,
    pub to_workers: u32,
    /// Exact PHP version of the pool (`8.3.36`).
    #[serde(default)]
    pub version: Option<String>,
    /// Upstream name in the web server configs (`php_8_3`).
    #[serde(default)]
    pub upstream: Option<String>,
    /// Backends now in the upstream (`127.0.0.1:19083`).
    #[serde(default)]
    pub backends: Vec<String>,
    /// Balancing policy of the upstream (`least_conn`).
    #[serde(default)]
    pub policy: Option<String>,
    /// `start`, `scale_up`, `scale_down`, `recycle`, `worker_exit`, `reload`, `drain`,
    /// `stop`.
    #[serde(default)]
    pub reason: Option<String>,
}

/// `pool.worker.recycled`: a worker was replaced or removed for a recycle reason.
/// Produced by `forge_fcgi::WorkerRecycledEvent` (same fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PoolWorkerRecycledPayload {
    pub v: u32,
    pub version_id: VersionId,
    pub series: String,
    pub port: u16,
    /// `max_requests`, `max_rss`, `max_age`, `crashed`, `reload`.
    pub reason: String,
    /// Exact PHP version of the pool (`8.3.36`).
    #[serde(default)]
    pub version: Option<String>,
    /// Process of the recycled worker.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Requests it served (estimated from its connections, docs/03 §2.1).
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub requests: Option<u64>,
    /// Last working set sampled.
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub rss_bytes: Option<u64>,
    /// Time it served.
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub age_ms: Option<u64>,
    /// Port of the worker that took its place, if one did.
    #[serde(default)]
    pub replacement_port: Option<u16>,
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
    /// Effective mode (`nrpt`, `hosts`, `none`); additive (ADR-0033).
    #[serde(default)]
    pub mode: DnsMode,
    /// Why the TLD is not routed by NRPT.
    #[serde(default)]
    pub reason: Option<DnsPolicyReason>,
    /// Result of the verification through the OS resolver, when one ran.
    #[serde(default)]
    pub probe_ok: Option<bool>,
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
    /// Forgebox owner that wanted the port: `<tool>/<instance>` (`mysql/default`) for an
    /// instance; `<kind>/<id>` for other owners.
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
    /// `nrpt.add`: whether Windows follows the local rule (`false` while the Group Policy
    /// NRPT table exists); additive (ADR-0033).
    #[serde(default)]
    pub effective: Option<bool>,
    /// `nrpt.add`: why the rule is not effective (`group_policy_table_present`).
    #[serde(default)]
    pub reason: Option<String>,
}

/// `config.changed`: `forgebox.toml` was written (`system.config_set`, any client). Added
/// in API 1.1 (ADR-0040).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ConfigChangedPayload {
    pub v: u32,
    /// Dotted keys written (`dns.enabled`, `tld`).
    pub keys: Vec<String>,
    /// Follow-up actions the change needs (`tls.ca_rotate`), as in
    /// `ConfigValueDto.requires`.
    #[serde(default)]
    pub requires: Vec<String>,
}

/// `tls.ca_untrusted`: the local CA was removed from a trust store (`tls.ca_uninstall`).
/// Added in API 1.1 (ADR-0040).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TlsCaUntrustedPayload {
    pub v: u32,
    pub scope: TrustScopeDto,
    /// SHA-1 thumbprint (40 hex, uppercase) of the removed certificate.
    pub thumbprint_sha1: String,
}

/// `port.holder_killed` (audit): `services.kill_port_holder` terminated the process that
/// held a port. Added in API 1.1 (ADR-0040).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PortHolderKilledPayload {
    pub v: u32,
    pub port: u16,
    pub pid: u32,
    /// Executable as `PortHolderDto.exe` showed it.
    pub exe: Option<String>,
    /// Client that asked (`ClientInfoDto.name` of the connection: `forge`, `Forgebox.exe`).
    #[serde(default)]
    pub client: Option<String>,
}

/// `mail.changed`: the captured mailbox changed. Published by the daemon's watcher while a
/// mail instance runs (and after `mail.*` changes), only when the counts move. Added in API
/// 1.1 (ADR-0040).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailChangedPayload {
    pub v: u32,
    pub instance_id: InstanceId,
    /// Messages in the mailbox.
    pub messages: u32,
    pub unread: u32,
    /// Messages received since the previous `mail.changed` (0 for deletions/reads).
    pub new_messages: u32,
}

/// `terminal.exited`: a daemon-hosted terminal session ended (the process exited or the
/// session was closed). Added in API 1.1 (ADR-0040).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TerminalExitedPayload {
    pub v: u32,
    pub terminal_id: TerminalId,
    /// Exit code (Windows `DWORD` bits as `i32`); `None` when it was killed without one.
    pub exit_code: Option<i32>,
    /// Site the session was opened in.
    #[serde(default)]
    pub site_id: Option<SiteId>,
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
    ("tool.updates_available", "ToolUpdatesAvailablePayload"),
    ("tool.updated", "ToolVersionChangePayload"),
    ("tool.rollback", "ToolVersionChangePayload"),
    ("tool.current_changed", "ToolCurrentPayload"),
    ("service.starting", "ServicePayload"),
    ("service.running", "ServicePayload"),
    ("service.stopping", "ServicePayload"),
    ("service.stopped", "ServicePayload"),
    ("service.crashed", "ServicePayload"),
    ("service.failed", "ServicePayload"),
    ("service.maintenance", "ServicePayload"),
    ("service.metrics", "ServiceMetricsPayload"),
    ("pool.scaled", "PoolScaledPayload"),
    ("pool.worker.recycled", "PoolWorkerRecycledPayload"),
    ("site.created", "SitePayload"),
    ("site.updated", "SitePayload"),
    ("site.removed", "SitePayload"),
    ("site.php_changed", "SitePhpChangedPayload"),
    ("config.rendered", "ConfigFilePayload"),
    ("config.drift_detected", "ConfigFilePayload"),
    ("config.changed", "ConfigChangedPayload"),
    ("dns.policy_applied", "DnsPolicyPayload"),
    ("tls.ca_created", "TlsPayload"),
    ("tls.leaf_issued", "TlsPayload"),
    ("tls.renewed", "TlsPayload"),
    ("tls.ca_untrusted", "TlsCaUntrustedPayload"),
    ("port.conflict", "PortConflictPayload"),
    ("port.holder_killed", "PortHolderKilledPayload"),
    ("helper.op", "HelperOpPayload"),
    ("mail.changed", "MailChangedPayload"),
    ("terminal.exited", "TerminalExitedPayload"),
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
        .register::<ToolUpdatesAvailablePayload>()
        .register::<ToolVersionChangePayload>()
        .register::<ToolCurrentPayload>()
        .register::<ServicePayload>()
        .register::<ServiceMetricsPayload>()
        .register::<PoolScaledPayload>()
        .register::<PoolWorkerRecycledPayload>()
        .register::<SitePayload>()
        .register::<SitePhpChangedPayload>()
        .register::<ConfigFilePayload>()
        .register::<ConfigChangedPayload>()
        .register::<DnsPolicyPayload>()
        .register::<TlsPayload>()
        .register::<TlsCaUntrustedPayload>()
        .register::<PortConflictPayload>()
        .register::<PortHolderKilledPayload>()
        .register::<HelperOpPayload>()
        .register::<MailChangedPayload>()
        .register::<TerminalExitedPayload>()
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
