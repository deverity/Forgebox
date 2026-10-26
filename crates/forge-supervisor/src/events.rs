//! Payloads of the `service.*` events (docs/03 §8), published by the instance actor and
//! its stop cascade on the daemon's [`EventBus`](forge_core::event::EventBus).
//!
//! Versioned serde structs (`v`), kept next to their producer like the `job.*` and
//! `actor.*` payloads of forge-core; forge-ipc re-exports them in the API contracts
//! (M1-E5-T1). Every payload starts with the instance reference ([`ServiceRef`]).

use forge_core::ids::{InstanceId, ToolId};
use serde::{Deserialize, Serialize};

use crate::stop::StopStrategy;

/// Version of the `service.*` payloads.
pub const SERVICE_EVENT_VERSION: u32 = 1;

/// Which instance an event is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRef {
    pub instance_id: InstanceId,
    pub tool: ToolId,
    /// Instance name (`default`, `legacy`...).
    pub instance: String,
}

/// `service.starting`: a process is being created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStartingPayload {
    pub v: u32,
    #[serde(flatten)]
    pub service: ServiceRef,
    /// Number of this start for the instance (1, 2, ...).
    pub run: u64,
}

/// `service.running`: the process was created and resumed (health checks: M1-E3-T2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRunningPayload {
    pub v: u32,
    #[serde(flatten)]
    pub service: ServiceRef,
    pub run: u64,
    pub pid: u32,
}

/// `service.stopping`: one step of the stop cascade starts (docs/03 §1.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStoppingPayload {
    pub v: u32,
    #[serde(flatten)]
    pub service: ServiceRef,
    pub strategy: StopStrategy,
    /// 1 = command, 2 = CTRL_BREAK, 3 = WM_CLOSE, 4 = TerminateJobObject.
    pub step: u8,
    /// How long this step waits for the process to exit.
    pub timeout_ms: u64,
}

/// `service.stopped`: the instance is down after a stop request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStoppedPayload {
    pub v: u32,
    #[serde(flatten)]
    pub service: ServiceRef,
    /// Step after which the process exited.
    pub strategy: Option<StopStrategy>,
    pub exit_code: Option<u32>,
    pub elapsed_ms: u64,
    pub leftovers_killed: u32,
}

/// `service.crashed`: the process exited without being asked to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceCrashedPayload {
    pub v: u32,
    #[serde(flatten)]
    pub service: ServiceRef,
    pub exit_code: u32,
    /// Processes left in the instance's job and terminated.
    pub leftovers_killed: u32,
}

/// `service.failed`: the instance could not be started (or stopped).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceFailedPayload {
    pub v: u32,
    #[serde(flatten)]
    pub service: ServiceRef,
    /// [`forge_core::ForgeError::code`] of the cause.
    pub code: String,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn stopping_payload_is_flat_with_strategy_name() {
        let id = InstanceId::new();
        let payload = ServiceStoppingPayload {
            v: SERVICE_EVENT_VERSION,
            service: ServiceRef {
                instance_id: id,
                tool: ToolId::new("mysql").unwrap(),
                instance: "default".into(),
            },
            strategy: StopStrategy::CtrlBreak,
            step: 2,
            timeout_ms: 15_000,
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "v": 1,
                "instance_id": id.to_string(),
                "tool": "mysql",
                "instance": "default",
                "strategy": "ctrl_break",
                "step": 2,
                "timeout_ms": 15000,
            })
        );
        let back: ServiceStoppingPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back, payload);
    }
}
