//! The `service.*` payloads published by the supervisor decode as the API contract
//! `forge_ipc::api::events::ServicePayload` (docs/03 §8): one shape for the six kinds,
//! same field names, types and enum values, and no field the contract would drop.
//!
//! The constructors are checked on every OS; on Windows the events published by real
//! instances (start, stop through the cascade, crash, failed start) are decoded too.

#[cfg(windows)]
mod support;

use std::time::Duration;

use forge_core::ForgeError;
use forge_core::event::EventKind;
use forge_core::ids::{InstanceId, ToolId};
use forge_ipc::api::events::ServicePayload;
use forge_ipc::api::services::ServiceState;
use forge_supervisor::events::{ServiceEventPayload, ServiceEventState, ServiceRef};
use forge_supervisor::{StopReport, StopStrategy};
use serde_json::Value;

/// Fields every `service.*` payload carries, `null` when they do not apply.
const BASE_FIELDS: [&str; 9] = [
    "v",
    "instance_id",
    "name",
    "tool",
    "state",
    "pid",
    "strategy",
    "exit_code",
    "error",
];

fn expected_state(kind: EventKind) -> ServiceState {
    match kind {
        EventKind::ServiceStarting => ServiceState::Starting,
        EventKind::ServiceRunning => ServiceState::Running,
        EventKind::ServiceStopping => ServiceState::Stopping,
        EventKind::ServiceStopped => ServiceState::Stopped,
        EventKind::ServiceCrashed => ServiceState::Crashed,
        EventKind::ServiceFailed => ServiceState::Failed,
        other => panic!("{other} is not a service event"),
    }
}

/// Decodes the wire form of a `kind` event as the API payload and checks that nothing is
/// lost: every field sent is a field of `ServicePayload` with the same value, and every
/// base field is sent.
fn decode(kind: EventKind, json: &Value) -> ServicePayload {
    let api: ServicePayload = serde_json::from_value(json.clone())
        .unwrap_or_else(|err| panic!("{kind} payload is not a ServicePayload: {err}\n{json}"));
    let back = serde_json::to_value(&api).unwrap();
    let sent = json.as_object().expect("payload is an object");
    for (field, value) in sent {
        assert_eq!(
            back.get(field),
            Some(value),
            "{kind}: field {field:?} is not carried by ServicePayload"
        );
    }
    for field in BASE_FIELDS {
        assert!(
            sent.contains_key(field),
            "{kind}: base field {field:?} missing"
        );
    }
    for (field, value) in back.as_object().unwrap() {
        if !sent.contains_key(field) {
            assert_eq!(value, &Value::Null, "{kind}: {field:?} invented");
        }
    }
    assert_eq!(api.state, expected_state(kind), "{kind}: state");
    assert_eq!(api.v, 1);
    api
}

fn mysql() -> ServiceRef {
    ServiceRef {
        instance_id: InstanceId::new(),
        tool: ToolId::new("mysql").unwrap(),
        instance: "default".into(),
    }
}

#[test]
fn every_constructor_decodes_as_service_payload() {
    let service = mysql();
    let report = StopReport {
        strategy: Some(StopStrategy::CtrlBreak),
        attempted: vec![StopStrategy::Command, StopStrategy::CtrlBreak],
        exit_code: Some(0xC000_013A),
        elapsed_ms: 1_234,
        leftovers_killed: 2,
    };
    let err = ForgeError::PortInUse {
        port: 3306,
        owner: Some("mysqld.exe (pid 99)".into()),
    };
    let payloads = [
        ServiceEventPayload::starting(&service, 3),
        ServiceEventPayload::running(&service, 3, 77),
        ServiceEventPayload::stopping(&service, 77, StopStrategy::WmClose, Duration::from_secs(5)),
        ServiceEventPayload::stopped(&service, Some(77), &report),
        ServiceEventPayload::crashed(&service, Some(77), 3, 1),
        ServiceEventPayload::failed(&service, None, &err),
    ];
    let kinds: Vec<EventKind> = payloads.iter().map(ServiceEventPayload::kind).collect();
    let all: Vec<EventKind> = ServiceEventState::ALL.iter().map(|s| s.kind()).collect();
    assert_eq!(kinds, all, "one constructor per kind");

    for payload in &payloads {
        let kind = payload.kind();
        let api = decode(kind, &serde_json::to_value(payload).unwrap());
        assert_eq!(api.instance_id, service.instance_id);
        assert_eq!(api.name, "mysql/default");
        assert_eq!(api.tool.as_str(), "mysql");
        match kind {
            EventKind::ServiceStarting => {
                assert_eq!((api.pid, api.run), (None, Some(3)));
            }
            EventKind::ServiceRunning => {
                assert_eq!((api.pid, api.run), (Some(77), Some(3)));
            }
            EventKind::ServiceStopping => {
                assert_eq!(api.pid, Some(77));
                assert_eq!(api.strategy.as_deref(), Some("wm_close"));
                assert_eq!((api.step, api.timeout_ms), (Some(3), Some(5_000)));
            }
            EventKind::ServiceStopped => {
                assert_eq!(api.strategy.as_deref(), Some("ctrl_break"));
                // STATUS_CONTROL_C_EXIT keeps its bits.
                assert_eq!(api.exit_code, Some(-1_073_741_510));
                assert_eq!(
                    (api.elapsed_ms, api.leftovers_killed),
                    (Some(1_234), Some(2))
                );
            }
            EventKind::ServiceCrashed => {
                assert_eq!((api.exit_code, api.leftovers_killed), (Some(3), Some(1)));
            }
            EventKind::ServiceFailed => {
                let error = api.error.expect("service.failed carries the error");
                assert_eq!(error.code, "port_in_use");
                assert_eq!(error.message, err.to_string());
            }
            _ => unreachable!(),
        }
    }
}

/// Real instances publish the six kinds; each payload decodes as `ServicePayload`.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn published_service_events_decode_as_service_payload() {
    use std::collections::BTreeSet;

    use forge_supervisor::{InstanceState, QUICK_ASK_TIMEOUT};
    use support::*;

    let (supervisor, bus) = supervisor();
    let mut stream = bus.subscribe();

    // starting, running, stopping, stopped: a polite service stopped by CTRL_BREAK.
    let polite = supervisor
        .spawn_instance(fake_spec("polite", &["--polite"]))
        .unwrap();
    let mut logs = polite.subscribe_logs(ASK).await.unwrap();
    let polite_pid = polite.start(ASK).await.unwrap();
    wait_line(&mut logs, "ready", Duration::from_secs(10)).await;
    let report = polite.stop(Duration::from_secs(30)).await.unwrap();
    assert_eq!(report.strategy, Some(StopStrategy::CtrlBreak), "{report:?}");

    // crashed: exits with code 3 on its own.
    let crashy = supervisor
        .spawn_instance(fake_spec("crashy", &["--crash-after-ms", "200"]))
        .unwrap();
    let crashy_pid = crashy.start(ASK).await.unwrap();
    crashy
        .wait_for_state(
            |s| matches!(s, InstanceState::Crashed { .. }),
            Duration::from_secs(10),
        )
        .await
        .unwrap();

    // failed: the executable does not exist.
    let mut spec = fake_spec("missing", &[]);
    spec.spawn.exe = spec.spawn.cwd.join("no-such-service.exe");
    let missing = supervisor.spawn_instance(spec).unwrap();
    missing.start(ASK).await.unwrap_err();
    // Answered after the handler that published `service.failed`.
    missing.status(QUICK_ASK_TIMEOUT).await.unwrap();

    let ids = [polite.id(), crashy.id(), missing.id()];
    let mut seen = BTreeSet::new();
    while let Some(event) = stream.try_recv() {
        if !event.kind.as_str().starts_with("service.") {
            continue;
        }
        let api = decode(event.kind, &event.payload);
        assert!(ids.contains(&api.instance_id), "{api:?}");
        assert_eq!(api.tool.as_str(), "fake");
        seen.insert(event.kind.as_str());
        if api.instance_id == polite.id() {
            assert_eq!(api.name, "fake/polite");
            match event.kind {
                EventKind::ServiceStarting => assert_eq!(api.run, Some(1)),
                EventKind::ServiceRunning => {
                    assert_eq!((api.pid, api.run), (Some(polite_pid), Some(1)));
                }
                EventKind::ServiceStopping => {
                    assert_eq!(api.pid, Some(polite_pid));
                    assert_eq!(api.strategy.as_deref(), Some("ctrl_break"));
                    assert_eq!((api.step, api.timeout_ms), (Some(2), Some(15_000)));
                }
                EventKind::ServiceStopped => {
                    assert_eq!(api.pid, Some(polite_pid));
                    assert_eq!(api.strategy.as_deref(), Some("ctrl_break"));
                    assert_eq!(api.exit_code, Some(0));
                    assert!(api.elapsed_ms.is_some() && api.leftovers_killed.is_some());
                }
                other => panic!("unexpected {other} for the polite instance"),
            }
        } else if api.instance_id == crashy.id() {
            assert_eq!(api.name, "fake/crashy");
            if event.kind == EventKind::ServiceCrashed {
                assert_eq!((api.pid, api.exit_code), (Some(crashy_pid), Some(3)));
                assert!(api.leftovers_killed.is_some());
            }
        } else {
            assert_eq!(api.name, "fake/missing");
            if event.kind == EventKind::ServiceFailed {
                let error = api.error.expect("service.failed carries the error");
                assert_eq!(error.code, "not_found");
                assert_eq!(api.pid, None);
            }
        }
    }
    let all: BTreeSet<&str> = ServiceEventState::ALL
        .iter()
        .map(|s| s.kind().as_str())
        .collect();
    assert_eq!(seen, all, "every service.* kind was published");
}
