//! Payloads of the `pool.*` events (docs/03 §8), published by the pool actor on the
//! daemon's [`EventBus`](forge_core::event::EventBus).
//!
//! The wire contracts are `forge_ipc::api::events::{PoolScaledPayload,
//! PoolWorkerRecycledPayload}`; this crate does not depend on forge-ipc, so it serializes
//! the equivalent structs below (same field names and types).
//! `tests/pool_payload_contract.rs` deserializes both into the IPC types.

use std::time::Duration;

use forge_core::event::{EventBus, EventKind};
use forge_core::ids::VersionId;
use serde::{Deserialize, Serialize};

use crate::upstream::{ScaleReason, UpstreamModel};

/// Version of the `pool.*` payloads (same as `forge_ipc`'s `EVENT_PAYLOAD_VERSION`).
pub const POOL_EVENT_VERSION: u32 = 1;

/// `pool.scaled`: the backends of a pool changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolScaledEvent {
    pub v: u32,
    pub version_id: VersionId,
    pub series: String,
    pub from_workers: u32,
    pub to_workers: u32,
    pub version: Option<String>,
    pub upstream: Option<String>,
    pub backends: Vec<String>,
    pub policy: Option<String>,
    pub reason: Option<String>,
}

impl PoolScaledEvent {
    pub fn new(
        version_id: VersionId,
        series: &str,
        version: &str,
        from_workers: usize,
        model: &UpstreamModel,
        reason: ScaleReason,
    ) -> Self {
        Self {
            v: POOL_EVENT_VERSION,
            version_id,
            series: series.to_owned(),
            from_workers: count(from_workers),
            to_workers: count(model.backends.len()),
            version: Some(version.to_owned()),
            upstream: Some(model.name.clone()),
            backends: model.backend_strings(),
            policy: Some(model.policy.as_str().to_owned()),
            reason: Some(reason.as_str().to_owned()),
        }
    }
}

/// Why a worker was recycled (`reason` of `pool.worker.recycled`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecycleReason {
    /// Served about `recycle.max_requests` requests.
    MaxRequests,
    /// Working set above `recycle.max_rss_mb`.
    MaxRss,
    /// Older than `recycle.max_age`.
    MaxAge,
    /// Exited on its own (crash, or `PHP_FCGI_MAX_REQUESTS` reached).
    Crashed,
    /// Rolling replacement requested ([`crate::PoolHandle::reload`]).
    Reload,
}

impl RecycleReason {
    /// Wire name (`max_requests`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MaxRequests => "max_requests",
            Self::MaxRss => "max_rss",
            Self::MaxAge => "max_age",
            Self::Crashed => "crashed",
            Self::Reload => "reload",
        }
    }
}

/// `pool.worker.recycled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRecycledEvent {
    pub v: u32,
    pub version_id: VersionId,
    pub series: String,
    pub port: u16,
    pub reason: String,
    pub version: Option<String>,
    pub pid: Option<u32>,
    pub requests: Option<u64>,
    pub rss_bytes: Option<u64>,
    pub age_ms: Option<u64>,
    pub replacement_port: Option<u16>,
}

/// What [`WorkerRecycledEvent::new`] needs about the recycled worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecycledWorker {
    pub port: u16,
    pub pid: Option<u32>,
    pub requests: u64,
    pub rss_bytes: Option<u64>,
    pub age: Duration,
    pub replacement_port: Option<u16>,
}

impl WorkerRecycledEvent {
    pub fn new(
        version_id: VersionId,
        series: &str,
        version: &str,
        reason: RecycleReason,
        worker: RecycledWorker,
    ) -> Self {
        Self {
            v: POOL_EVENT_VERSION,
            version_id,
            series: series.to_owned(),
            port: worker.port,
            reason: reason.as_str().to_owned(),
            version: Some(version.to_owned()),
            pid: worker.pid,
            requests: Some(worker.requests),
            rss_bytes: worker.rss_bytes,
            age_ms: Some(u64::try_from(worker.age.as_millis()).unwrap_or(u64::MAX)),
            replacement_port: worker.replacement_port,
        }
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Publishes `payload` as `kind` (logs and drops it if it cannot be serialized).
pub(crate) fn publish(bus: &EventBus, kind: EventKind, payload: &impl Serialize) {
    match serde_json::to_value(payload) {
        Ok(value) => {
            bus.publish(kind, value);
        }
        Err(err) => tracing::error!(%err, %kind, "could not serialize a pool event"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream::LbPolicy;

    #[test]
    fn payloads_carry_the_model_and_the_worker() {
        let id = VersionId::new();
        let model = UpstreamModel::new(
            "php_8_3",
            vec!["127.0.0.1:19001".parse().unwrap()],
            LbPolicy::LeastConn,
        );
        let scaled = PoolScaledEvent::new(id, "8.3", "8.3.36", 2, &model, ScaleReason::ScaleDown);
        let json = serde_json::to_value(&scaled).unwrap();
        assert_eq!(json["from_workers"], 2);
        assert_eq!(json["to_workers"], 1);
        assert_eq!(json["backends"][0], "127.0.0.1:19001");
        assert_eq!(json["reason"], "scale_down");
        assert_eq!(json["upstream"], "php_8_3");

        let recycled = WorkerRecycledEvent::new(
            id,
            "8.3",
            "8.3.36",
            RecycleReason::MaxRequests,
            RecycledWorker {
                port: 19001,
                pid: Some(7),
                requests: 21,
                rss_bytes: Some(1 << 20),
                age: Duration::from_millis(1500),
                replacement_port: Some(19002),
            },
        );
        let json = serde_json::to_value(&recycled).unwrap();
        assert_eq!(json["reason"], "max_requests");
        assert_eq!(json["age_ms"], 1500);
        assert_eq!(json["replacement_port"], 19002);
    }
}
