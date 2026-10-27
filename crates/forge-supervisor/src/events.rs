//! Payloads of the `service.*` events (docs/03 §8), published by the instance actor and
//! its stop cascade on the daemon's [`EventBus`](forge_core::event::EventBus).
//!
//! The wire contract of the six kinds is one type, `forge_ipc::api::events::ServicePayload`
//! (mapped in `EVENT_PAYLOADS` and in the GUI's `methods.ts`). forge-supervisor does not
//! depend on forge-ipc, so it serializes the equivalent [`ServiceEventPayload`]: the same
//! field names, types and enum values. The fields after `error` (`run`, `step`,
//! `timeout_ms`, `elapsed_ms`, `leftovers_killed`) are optional in `ServicePayload` too
//! and are left out when they do not apply to the kind. `tests/service_payload_contract.rs`
//! deserializes every kind into `ServicePayload`.

use std::time::Duration;

use forge_core::ForgeError;
use forge_core::event::EventKind;
use forge_core::ids::{InstanceId, ToolId};
use forge_core::jobs::JobErrorInfo;
use serde::{Deserialize, Serialize};

use crate::stop::{StopReport, StopStrategy};

/// Version of the `service.*` payloads (same as `forge_ipc`'s `EVENT_PAYLOAD_VERSION`).
pub const SERVICE_EVENT_VERSION: u32 = 1;

/// Which instance an event is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRef {
    pub instance_id: InstanceId,
    pub tool: ToolId,
    /// Instance name (`default`, `legacy`...).
    pub instance: String,
}

impl ServiceRef {
    /// `<tool>/<instance>` (`mysql/default`): the `name` of the payloads.
    pub fn name(&self) -> String {
        format!("{}/{}", self.tool, self.instance)
    }
}

/// `state` of a payload: the state the instance is in when the event is published. The
/// values are those of `forge_ipc::api::services::ServiceState` (which also has
/// `maintenance`, never set by the supervisor).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceEventState {
    Starting,
    Running,
    Stopping,
    Stopped,
    Crashed,
    Failed,
}

impl ServiceEventState {
    /// Every state, in lifecycle order.
    pub const ALL: [Self; 6] = [
        Self::Starting,
        Self::Running,
        Self::Stopping,
        Self::Stopped,
        Self::Crashed,
        Self::Failed,
    ];

    /// The event kind announcing this state (`service.<state>`).
    pub fn kind(self) -> EventKind {
        match self {
            Self::Starting => EventKind::ServiceStarting,
            Self::Running => EventKind::ServiceRunning,
            Self::Stopping => EventKind::ServiceStopping,
            Self::Stopped => EventKind::ServiceStopped,
            Self::Crashed => EventKind::ServiceCrashed,
            Self::Failed => EventKind::ServiceFailed,
        }
    }
}

/// Process exit code on the wire: the bits of the Windows `DWORD` as `i32`
/// (`0xC000013A` → `-1073741510`), like `ServicePayload::exit_code`.
pub fn wire_exit_code(code: u32) -> i32 {
    code.cast_signed()
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Payload of every `service.*` event; serializes as `ServicePayload` (see the module
/// docs). Build it with the constructor of the kind, publish it under [`Self::kind`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEventPayload {
    pub v: u32,
    pub instance_id: InstanceId,
    /// `<tool>/<instance>` (`mysql/default`).
    pub name: String,
    pub tool: ToolId,
    pub state: ServiceEventState,
    /// Main process: the one started (`running`), being stopped (`stopping`), stopped
    /// (`stopped`), that exited (`crashed`) or left behind (`failed`).
    pub pid: Option<u32>,
    /// `stopping`: step being tried; `stopped`: step after which the process exited.
    pub strategy: Option<StopStrategy>,
    /// `stopped`, `crashed`: see [`wire_exit_code`].
    pub exit_code: Option<i32>,
    /// `failed`: the cause ([`ForgeError::code`] and message).
    pub error: Option<JobErrorInfo>,
    /// `starting`, `running`: number of this start for the instance (1, 2, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<u64>,
    /// `stopping`: position of `strategy` in the cascade (1 = command, 2 = CTRL_BREAK,
    /// 3 = WM_CLOSE, 4 = TerminateJobObject).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u8>,
    /// `stopping`: how long this step waits for the process to exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// `stopped`: duration of the stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// `stopped`, `crashed`: processes left in the instance's job after the main process
    /// exited, then terminated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leftovers_killed: Option<u32>,
}

impl ServiceEventPayload {
    fn new(service: &ServiceRef, state: ServiceEventState) -> Self {
        Self {
            v: SERVICE_EVENT_VERSION,
            instance_id: service.instance_id,
            name: service.name(),
            tool: service.tool.clone(),
            state,
            pid: None,
            strategy: None,
            exit_code: None,
            error: None,
            run: None,
            step: None,
            timeout_ms: None,
            elapsed_ms: None,
            leftovers_killed: None,
        }
    }

    /// `service.starting`: run `run` is creating its process.
    pub fn starting(service: &ServiceRef, run: u64) -> Self {
        Self {
            run: Some(run),
            ..Self::new(service, ServiceEventState::Starting)
        }
    }

    /// `service.running`: the process of run `run` was created and resumed (health
    /// checks: M1-E3-T2).
    pub fn running(service: &ServiceRef, run: u64, pid: u32) -> Self {
        Self {
            pid: Some(pid),
            run: Some(run),
            ..Self::new(service, ServiceEventState::Running)
        }
    }

    /// `service.stopping`: one step of the stop cascade starts and waits up to `wait`
    /// (docs/03 §1.3).
    pub fn stopping(
        service: &ServiceRef,
        pid: u32,
        strategy: StopStrategy,
        wait: Duration,
    ) -> Self {
        Self {
            pid: Some(pid),
            strategy: Some(strategy),
            step: Some(strategy.step()),
            timeout_ms: Some(millis(wait)),
            ..Self::new(service, ServiceEventState::Stopping)
        }
    }

    /// `service.stopped`: the instance is down after a stop request (`pid`: the process
    /// that was stopped, `None` when nothing was running).
    pub fn stopped(service: &ServiceRef, pid: Option<u32>, report: &StopReport) -> Self {
        Self {
            pid,
            strategy: report.strategy,
            exit_code: report.exit_code.map(wire_exit_code),
            elapsed_ms: Some(report.elapsed_ms),
            leftovers_killed: Some(report.leftovers_killed),
            ..Self::new(service, ServiceEventState::Stopped)
        }
    }

    /// `service.crashed`: process `pid` exited without being asked to.
    pub fn crashed(
        service: &ServiceRef,
        pid: Option<u32>,
        exit_code: u32,
        leftovers_killed: u32,
    ) -> Self {
        Self {
            pid,
            exit_code: Some(wire_exit_code(exit_code)),
            leftovers_killed: Some(leftovers_killed),
            ..Self::new(service, ServiceEventState::Crashed)
        }
    }

    /// `service.failed`: the instance could not be started (or stopped); `pid` is the
    /// process left behind, if any.
    pub fn failed(service: &ServiceRef, pid: Option<u32>, err: &ForgeError) -> Self {
        Self {
            pid,
            error: Some(JobErrorInfo::from(err)),
            ..Self::new(service, ServiceEventState::Failed)
        }
    }

    /// Kind to publish this payload under (`service.<state>`).
    pub fn kind(&self) -> EventKind {
        self.state.kind()
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn mysql() -> ServiceRef {
        ServiceRef {
            instance_id: InstanceId::new(),
            tool: ToolId::new("mysql").unwrap(),
            instance: "default".into(),
        }
    }

    #[test]
    fn stopping_payload_has_the_service_payload_fields() {
        let service = mysql();
        let payload = ServiceEventPayload::stopping(
            &service,
            4242,
            StopStrategy::CtrlBreak,
            Duration::from_secs(15),
        );
        assert_eq!(payload.kind(), EventKind::ServiceStopping);
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "v": 1,
                "instance_id": service.instance_id.to_string(),
                "name": "mysql/default",
                "tool": "mysql",
                "state": "stopping",
                "pid": 4242,
                "strategy": "ctrl_break",
                "exit_code": null,
                "error": null,
                "step": 2,
                "timeout_ms": 15000,
            })
        );
        let back: ServiceEventPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn every_state_maps_to_its_kind() {
        for state in ServiceEventState::ALL {
            let wire = serde_json::to_value(state).unwrap();
            assert_eq!(
                format!("service.{}", wire.as_str().unwrap()),
                state.kind().as_str()
            );
        }
    }

    #[test]
    fn exit_codes_keep_their_bits() {
        assert_eq!(wire_exit_code(0), 0);
        assert_eq!(wire_exit_code(3), 3);
        assert_eq!(wire_exit_code(0xC000_013A), -1_073_741_510);
        let crashed = ServiceEventPayload::crashed(&mysql(), Some(7), u32::MAX, 0);
        assert_eq!(crashed.exit_code, Some(-1));
    }

    #[test]
    fn failed_payload_carries_code_and_message() {
        let err = ForgeError::NotFound("bin/mysql/8.4.3/bin/mysqld.exe".into());
        let payload = ServiceEventPayload::failed(&mysql(), None, &err);
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["state"], "failed");
        assert_eq!(json["error"]["code"], "not_found");
        assert_eq!(json["error"]["message"], err.to_string());
        assert!(json.get("run").is_none());
        assert_eq!(json["pid"], serde_json::Value::Null);
    }
}
