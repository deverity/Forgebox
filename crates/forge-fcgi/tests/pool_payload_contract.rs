//! The `pool.*` payloads produced here deserialize into the API contract types of
//! `forge_ipc::api::events` (docs/03 §8): same field names and types.

use std::time::Duration;

use forge_core::ids::VersionId;
use forge_fcgi::events::{PoolScaledEvent, RecycleReason, RecycledWorker, WorkerRecycledEvent};
use forge_fcgi::{LbPolicy, ScaleReason, UpstreamModel};
use forge_ipc::api::events::{EVENT_PAYLOAD_VERSION, PoolScaledPayload, PoolWorkerRecycledPayload};
use pretty_assertions::assert_eq;

#[test]
fn pool_scaled_matches_the_api_contract() {
    let id = VersionId::new();
    let model = UpstreamModel::new(
        "php_8_3",
        vec![
            "127.0.0.1:19083".parse().unwrap(),
            "127.0.0.1:19084".parse().unwrap(),
        ],
        LbPolicy::LeastConn,
    );
    for reason in [
        ScaleReason::Start,
        ScaleReason::ScaleUp,
        ScaleReason::ScaleDown,
        ScaleReason::Recycle,
        ScaleReason::WorkerExit,
        ScaleReason::Reload,
        ScaleReason::Drain,
        ScaleReason::Stop,
    ] {
        let produced = PoolScaledEvent::new(id, "8.3", "8.3.36", 1, &model, reason);
        let wire = serde_json::to_value(&produced).unwrap();
        let api: PoolScaledPayload = serde_json::from_value(wire).unwrap();
        assert_eq!(api.v, EVENT_PAYLOAD_VERSION);
        assert_eq!(api.version_id, id);
        assert_eq!((api.from_workers, api.to_workers), (1, 2));
        assert_eq!(api.series, "8.3");
        assert_eq!(api.version.as_deref(), Some("8.3.36"));
        assert_eq!(api.upstream.as_deref(), Some("php_8_3"));
        assert_eq!(api.backends, vec!["127.0.0.1:19083", "127.0.0.1:19084"]);
        assert_eq!(api.policy.as_deref(), Some("least_conn"));
        assert_eq!(api.reason.as_deref(), Some(reason.as_str()));
    }
}

#[test]
fn pool_worker_recycled_matches_the_api_contract() {
    let id = VersionId::new();
    for reason in [
        RecycleReason::MaxRequests,
        RecycleReason::MaxRss,
        RecycleReason::MaxAge,
        RecycleReason::Crashed,
        RecycleReason::Reload,
    ] {
        let produced = WorkerRecycledEvent::new(
            id,
            "8.4",
            "8.4.27",
            reason,
            RecycledWorker {
                port: 19090,
                pid: Some(4242),
                requests: 501,
                rss_bytes: Some(300 * 1024 * 1024),
                age: Duration::from_secs(61),
                replacement_port: Some(19091),
            },
        );
        let wire = serde_json::to_value(&produced).unwrap();
        let api: PoolWorkerRecycledPayload = serde_json::from_value(wire).unwrap();
        assert_eq!(api.v, EVENT_PAYLOAD_VERSION);
        assert_eq!(api.version_id, id);
        assert_eq!(api.port, 19090);
        assert_eq!(api.reason, reason.as_str());
        assert_eq!(api.pid, Some(4242));
        assert_eq!(api.requests, Some(501));
        assert_eq!(api.rss_bytes, Some(300 * 1024 * 1024));
        assert_eq!(api.age_ms, Some(61_000));
        assert_eq!(api.replacement_port, Some(19091));
    }
}

/// Payloads of older producers (only the original fields) still deserialize.
#[test]
fn added_fields_are_optional() {
    let id = VersionId::new();
    let scaled: PoolScaledPayload = serde_json::from_value(serde_json::json!({
        "v": 1, "version_id": id, "series": "8.3", "from_workers": 2, "to_workers": 3
    }))
    .unwrap();
    assert!(scaled.backends.is_empty() && scaled.reason.is_none());
    let recycled: PoolWorkerRecycledPayload = serde_json::from_value(serde_json::json!({
        "v": 1, "version_id": id, "series": "8.3", "port": 19000, "reason": "crashed"
    }))
    .unwrap();
    assert_eq!(recycled.requests, None);
}
