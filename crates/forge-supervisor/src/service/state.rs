//! The instance lifecycle as a pure state machine (docs/03 §1.4).
//!
//! ```text
//! Stopped ─start─▶ Starting ─healthy─▶ Running ─stop─▶ Stopping ─stop finished─▶ Stopped
//!    ▲                │ exited / fail        │ exited            │ (cause: unhealthy)
//!    │                ▼                      ▼                   ▼
//!    └─start──── Failed(reason) ◀──fail── Crashed(code) ◀────────┘
//!                     ▲                      │ start (restart policy or user)
//!                     │                      ▼
//!                     └───────────────── Starting
//!
//! Stopped | Crashed | Failed ─enter maintenance─▶ Maintenance ─exit maintenance─▶ Stopped
//! ```
//!
//! [`next_state`] is the only place that decides which [`Trigger`] is allowed in which
//! [`InstanceState`]; anything else is an [`InvalidTransition`] error, never a panic. The
//! instance actor feeds it every change and publishes one `service.*` event per accepted
//! transition. Requests that do not change the state (a `Start` while `Running`, a `Stop`
//! while `Stopped`) are answered by the actor without a transition; "any state →
//! Maintenance" (docs/03) is a stop followed by [`Trigger::EnterMaintenance`].
//!
//! `Stopping` remembers why ([`StopCause`]): a requested stop ends in `Stopped`, a stop
//! forced by failing health checks ends in `Crashed` (and the restart policy applies), and
//! a stop forced by the start deadline ends in `Failed(HealthTimeout)` (through
//! [`Trigger::Fail`]).

use std::fmt;

use forge_core::ForgeError;
use forge_core::jobs::JobErrorInfo;
use forge_core::platform::VcRedistFlavor;
use serde::{Deserialize, Serialize};

use crate::events::wire_exit_code;
use crate::stop::StopStrategy;

/// `STATUS_DLL_NOT_FOUND`: the loader could not find a DLL the executable imports
/// (typically the VC++ Redistributable).
pub const STATUS_DLL_NOT_FOUND: u32 = 0xC000_0135;
/// `STATUS_ENTRYPOINT_NOT_FOUND`: a DLL was found but lacks an imported function (an
/// older VC++ Redistributable than the build needs).
pub const STATUS_ENTRYPOINT_NOT_FOUND: u32 = 0xC000_0139;
/// `STATUS_ACCESS_DENIED` as a process exit code.
pub const STATUS_ACCESS_DENIED: u32 = 0xC000_0022;

/// Why an instance is `Failed` (docs/03 §1.4): structured so the UI can offer the fix
/// ("Instalar VC++", "Encerrar processo", "Trocar porta"...). Serialized with a `kind`
/// tag; same shape as `forge_ipc::api::services::FailureReasonDto`. Exit codes are the
/// bits of the Windows `DWORD` as `i32` ([`wire_exit_code`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FailureReason {
    /// A declared port is taken (pre-checks, M1-E3-T3): by `pid`/`exe` when known.
    PortInUse {
        port: u16,
        pid: Option<u32>,
        exe: Option<String>,
    },
    /// A DLL (or an entry point in one) is missing: exit `0xC0000135` / `0xC0000139`.
    MissingDll { exit_code: i32 },
    /// The configuration check (`service.check`, `nginx -t`) failed; its output
    /// (stderr, then stdout).
    ConfigInvalid { stderr: String },
    /// A system runtime of `spec.runtime_deps` is missing or too old (pre-checks): the
    /// Visual C++ Redistributable for `flavor` builds.
    MissingRuntime {
        /// Build flavor that needs it (`vs17`).
        flavor: String,
        /// Where to get a runtime that satisfies it.
        download_url: String,
        /// The older runtime found, if any (`14.29.30133.0`).
        installed: Option<String>,
    },
    /// A `depends_on` service is not `Running` (pre-checks).
    DependencyNotRunning {
        /// `mysql` or `mysql/default`.
        dependency: String,
        /// Its state (`stopped`, `failed`...; `unknown` when no such service answered).
        state: String,
    },
    /// Access denied starting the executable, or the process exited with
    /// `STATUS_ACCESS_DENIED`.
    Permission { message: String },
    /// Health did not pass `success_threshold` times within `start_timeout`.
    HealthTimeout {
        timeout_ms: u64,
        /// Last probe error, if any probe ran.
        last_error: Option<String>,
    },
    /// The process exited before it became healthy.
    ExitedDuringStart { exit_code: i32 },
    /// The restart policy gave up: `restarts` automatic restarts within `window_ms`.
    CrashLoop {
        restarts: u32,
        window_ms: u64,
        /// Exit code of the last run (`None` when it was stopped for failing health).
        exit_code: Option<i32>,
    },
    /// Anything else: `code` is a [`ForgeError::code`] (spawn errors, a stop that could
    /// not end the process, an internal error of the supervisor).
    Other { code: String, message: String },
}

impl FailureReason {
    /// Reason for an error returned while starting (spawn, pre-checks).
    pub fn from_error(err: &ForgeError) -> Self {
        match err {
            ForgeError::PortInUse {
                port,
                owner,
                pid,
                exe,
            } => match (exe, owner) {
                (Some(exe), _) => Self::PortInUse {
                    port: *port,
                    pid: *pid,
                    exe: Some(exe.clone()),
                },
                // Without the executable, the owner text (`mysqld.exe (PID 1)`, which
                // already names the pid) says more than nothing.
                (None, Some(owner)) => Self::PortInUse {
                    port: *port,
                    pid: None,
                    exe: Some(owner.clone()),
                },
                (None, None) => Self::PortInUse {
                    port: *port,
                    pid: *pid,
                    exe: None,
                },
            },
            ForgeError::ConfigInvalid(stderr) => Self::ConfigInvalid {
                stderr: stderr.clone(),
            },
            ForgeError::NeedsElevation(_) => Self::Permission {
                message: err.to_string(),
            },
            ForgeError::Io { source, .. }
                if source.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                Self::Permission {
                    message: err.to_string(),
                }
            }
            other => Self::Other {
                code: other.code().to_owned(),
                message: other.to_string(),
            },
        }
    }

    /// Reason for a process that exited with `code` before becoming healthy.
    pub fn exited_during_start(code: u32) -> Self {
        Self::for_exit_code(code).unwrap_or(Self::ExitedDuringStart {
            exit_code: wire_exit_code(code),
        })
    }

    /// Exit codes that mean "restarting cannot help": a missing DLL or an access denied.
    pub fn for_exit_code(code: u32) -> Option<Self> {
        match code {
            STATUS_DLL_NOT_FOUND | STATUS_ENTRYPOINT_NOT_FOUND => Some(Self::MissingDll {
                exit_code: wire_exit_code(code),
            }),
            STATUS_ACCESS_DENIED => Some(Self::Permission {
                message: format!("the process exited with STATUS_ACCESS_DENIED ({code:#010x})"),
            }),
            _ => None,
        }
    }

    /// Wire name of the variant (`port_in_use`, `missing_dll`...).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::PortInUse { .. } => "port_in_use",
            Self::MissingDll { .. } => "missing_dll",
            Self::ConfigInvalid { .. } => "config_invalid",
            Self::MissingRuntime { .. } => "missing_runtime",
            Self::DependencyNotRunning { .. } => "dependency_not_running",
            Self::Permission { .. } => "permission",
            Self::HealthTimeout { .. } => "health_timeout",
            Self::ExitedDuringStart { .. } => "exited_during_start",
            Self::CrashLoop { .. } => "crash_loop",
            Self::Other { .. } => "other",
        }
    }

    /// Stable error code (one of `forge_core::error::ERROR_CODES`) for `service.failed`
    /// `error.code` and for the replies of waiting requests; the precise cause is
    /// [`Self::kind`].
    pub fn code(&self) -> &str {
        match self {
            Self::PortInUse { .. } => "port_in_use",
            Self::MissingDll { .. } | Self::MissingRuntime { .. } => "not_found",
            Self::ConfigInvalid { .. } => "config_invalid",
            Self::DependencyNotRunning { .. } => "conflict",
            Self::Permission { .. } => "needs_elevation",
            Self::HealthTimeout { .. } => "timeout",
            // A supervised process failed: reported by the OS, not a daemon bug.
            Self::ExitedDuringStart { .. } | Self::CrashLoop { .. } => "io",
            Self::Other { code, .. } => code,
        }
    }

    /// One-line English description (logs, `error.message`).
    pub fn message(&self) -> String {
        match self {
            Self::PortInUse { port, pid, exe } => {
                let owner = match (exe, pid) {
                    (Some(exe), Some(pid)) => format!(" by {exe} (PID {pid})"),
                    (Some(exe), None) => format!(" by {exe}"),
                    (None, Some(pid)) => format!(" by PID {pid}"),
                    (None, None) => String::new(),
                };
                format!("port {port} is already in use{owner}")
            }
            Self::MissingDll { exit_code } => format!(
                "a DLL required by the executable is missing (exit code {:#010x}); install \
                 the Visual C++ Redistributable it needs",
                exit_code.cast_unsigned()
            ),
            Self::ConfigInvalid { stderr } => format!("invalid configuration: {stderr}"),
            Self::MissingRuntime {
                flavor,
                download_url,
                installed,
            } => {
                let minimum = flavor
                    .parse::<VcRedistFlavor>()
                    .map(|f| format!(" ({} or newer)", f.min_version()))
                    .unwrap_or_default();
                let found = installed
                    .as_ref()
                    .map(|v| format!("; found {v}"))
                    .unwrap_or_default();
                format!(
                    "the Visual C++ Redistributable (x64) for {flavor} builds{minimum} is not \
                     installed{found}; install it from {download_url}"
                )
            }
            Self::DependencyNotRunning { dependency, state } => {
                format!("dependency {dependency} is not running (state {state}); start it first")
            }
            Self::Permission { message } => format!("permission denied: {message}"),
            Self::HealthTimeout {
                timeout_ms,
                last_error,
            } => match last_error {
                Some(err) => {
                    format!("health check did not pass within {timeout_ms} ms (last error: {err})")
                }
                None => format!("health check did not pass within {timeout_ms} ms"),
            },
            Self::ExitedDuringStart { exit_code } => format!(
                "the process exited with code {exit_code} ({:#x}) before it became healthy",
                exit_code.cast_unsigned()
            ),
            Self::CrashLoop {
                restarts,
                window_ms,
                exit_code,
            } => {
                let last = exit_code.map_or_else(
                    || "failing health checks".to_owned(),
                    |code| format!("exit code {code}"),
                );
                format!(
                    "gave up after {restarts} automatic restarts within {window_ms} ms (last \
                     run ended with {last})"
                )
            }
            Self::Other { message, .. } => message.clone(),
        }
    }

    /// `{code, message}` of `service.failed`.
    pub fn error_info(&self) -> JobErrorInfo {
        JobErrorInfo {
            code: self.code().to_owned(),
            message: self.message(),
            details: None,
        }
    }

    /// Error answered to the requests waiting for the start (same code as
    /// [`Self::code`]).
    pub fn to_error(&self) -> ForgeError {
        match self {
            Self::PortInUse { port, pid, exe } => ForgeError::PortInUse {
                port: *port,
                owner: match (exe, pid) {
                    (Some(exe), Some(pid)) => Some(format!("{exe} (PID {pid})")),
                    (Some(exe), None) => Some(exe.clone()),
                    (None, Some(pid)) => Some(format!("PID {pid}")),
                    (None, None) => None,
                },
                pid: *pid,
                exe: exe.clone(),
            },
            Self::MissingDll { .. } | Self::MissingRuntime { .. } => {
                ForgeError::NotFound(self.message())
            }
            Self::ConfigInvalid { stderr } => ForgeError::ConfigInvalid(stderr.clone()),
            Self::DependencyNotRunning { .. } => ForgeError::Conflict(self.message()),
            Self::Permission { message } => ForgeError::NeedsElevation(message.clone()),
            // "timed out after 30s: waiting for the health check (last error: ...)".
            Self::HealthTimeout {
                timeout_ms,
                last_error,
            } => ForgeError::Timeout {
                operation: match last_error {
                    Some(err) => format!("waiting for the health check (last error: {err})"),
                    None => "waiting for the health check".to_owned(),
                },
                after: std::time::Duration::from_millis(*timeout_ms),
            },
            Self::ExitedDuringStart { .. } | Self::CrashLoop { .. } => {
                ForgeError::io("supervised process", std::io::Error::other(self.message()))
            }
            Self::Other { code, message } => match code.as_str() {
                "not_found" => ForgeError::NotFound(message.clone()),
                "conflict" => ForgeError::Conflict(message.clone()),
                "invalid_input" => ForgeError::InvalidInput(message.clone()),
                "cancelled" => ForgeError::Cancelled(message.clone()),
                "timeout" => ForgeError::Timeout {
                    operation: message.clone(),
                    after: std::time::Duration::ZERO,
                },
                "io" => {
                    ForgeError::io("supervised process", std::io::Error::other(message.clone()))
                }
                _ => ForgeError::Internal(message.clone()),
            },
        }
    }
}

impl fmt::Display for FailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

/// Why an instance is `Stopping`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCause {
    /// `Stop` requested (user, daemon shutdown): ends in `Stopped`.
    User,
    /// `Restart` requested: ends in `Stopped`, then a new run starts.
    Restart,
    /// Entering maintenance: ends in `Stopped`, then `Maintenance`.
    Maintenance,
    /// `failure_threshold` consecutive failed probes while `Running`: ends in `Crashed`
    /// and the restart policy decides.
    Unhealthy,
    /// The start deadline (`start_timeout`) passed: ends in `Failed(HealthTimeout)`.
    StartTimeout,
}

impl StopCause {
    /// Wire name (`user`, `unhealthy`...).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Restart => "restart",
            Self::Maintenance => "maintenance",
            Self::Unhealthy => "unhealthy",
            Self::StartTimeout => "start_timeout",
        }
    }
}

/// Lifecycle state of an instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum InstanceState {
    Stopped,
    /// Creating the process, then waiting for its health checks.
    Starting,
    /// Healthy (or merely spawned, when the instance has no health check).
    Running,
    /// Stop cascade in progress; `strategy` is the step being tried.
    Stopping {
        strategy: Option<StopStrategy>,
        cause: StopCause,
    },
    /// Exited without being asked to (or was stopped for failing health checks).
    Crashed {
        exit_code: u32,
    },
    /// Could not start, could not be stopped, or the restart policy gave up.
    Failed {
        reason: FailureReason,
    },
    /// Install/upgrade in progress: starts are refused until it ends.
    Maintenance,
}

impl InstanceState {
    /// Whether a process may be alive (`Starting`, `Running`, `Stopping`).
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping { .. })
    }

    /// Wire name of the state (`stopped`, `starting`...).
    pub fn name(&self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping { .. } => "stopping",
            Self::Crashed { .. } => "crashed",
            Self::Failed { .. } => "failed",
            Self::Maintenance => "maintenance",
        }
    }
}

/// What happened to an instance; input of [`next_state`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// A run begins (user start, restart, restart policy).
    Start,
    /// Health passed `success_threshold` times (or the process was spawned, without a
    /// health check).
    Healthy,
    /// The main process exited by itself.
    Exited {
        exit_code: u32,
    },
    /// The stop cascade begins.
    Stop {
        cause: StopCause,
    },
    /// The stop cascade ended with the process gone.
    StopFinished {
        exit_code: u32,
    },
    /// Give up: spawn error, start deadline, restart policy exhausted, stop that could
    /// not end the process, internal error.
    Fail {
        reason: FailureReason,
    },
    EnterMaintenance,
    ExitMaintenance,
}

impl Trigger {
    /// Name for errors and logs.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Healthy => "healthy",
            Self::Exited { .. } => "exited",
            Self::Stop { .. } => "stop",
            Self::StopFinished { .. } => "stop_finished",
            Self::Fail { .. } => "fail",
            Self::EnterMaintenance => "enter_maintenance",
            Self::ExitMaintenance => "exit_maintenance",
        }
    }
}

/// A trigger that is not allowed in the current state.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid transition: {trigger} while {state}")]
pub struct InvalidTransition {
    pub state: &'static str,
    pub trigger: &'static str,
}

impl From<InvalidTransition> for ForgeError {
    fn from(err: InvalidTransition) -> Self {
        ForgeError::Conflict(err.to_string())
    }
}

/// The state after `trigger` in `state`, or why it is not allowed.
pub fn next_state(
    state: &InstanceState,
    trigger: Trigger,
) -> Result<InstanceState, InvalidTransition> {
    use InstanceState as S;
    use Trigger as T;
    let invalid = InvalidTransition {
        state: state.name(),
        trigger: trigger.name(),
    };
    let next = match (state, trigger) {
        (S::Stopped | S::Crashed { .. } | S::Failed { .. }, T::Start) => S::Starting,
        (S::Starting, T::Healthy) => S::Running,
        (S::Starting, T::Exited { exit_code }) => S::Failed {
            reason: FailureReason::exited_during_start(exit_code),
        },
        (S::Running, T::Exited { exit_code }) => S::Crashed { exit_code },
        (S::Starting | S::Running, T::Stop { cause }) => S::Stopping {
            strategy: None,
            cause,
        },
        (
            S::Stopping {
                cause: StopCause::User | StopCause::Restart | StopCause::Maintenance,
                ..
            },
            T::StopFinished { .. },
        ) => S::Stopped,
        (
            S::Stopping {
                cause: StopCause::Unhealthy,
                ..
            },
            T::StopFinished { exit_code },
        ) => S::Crashed { exit_code },
        (
            S::Stopped
            | S::Starting
            | S::Running
            | S::Stopping { .. }
            | S::Crashed { .. }
            | S::Failed { .. },
            T::Fail { reason },
        ) => S::Failed { reason },
        (S::Stopped | S::Crashed { .. } | S::Failed { .. }, T::EnterMaintenance) => S::Maintenance,
        (S::Maintenance, T::ExitMaintenance) => S::Stopped,
        _ => return Err(invalid),
    };
    Ok(next)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn states() -> Vec<InstanceState> {
        let mut states = vec![
            InstanceState::Stopped,
            InstanceState::Starting,
            InstanceState::Running,
        ];
        for cause in [
            StopCause::User,
            StopCause::Restart,
            StopCause::Maintenance,
            StopCause::Unhealthy,
            StopCause::StartTimeout,
        ] {
            states.push(InstanceState::Stopping {
                strategy: Some(StopStrategy::CtrlBreak),
                cause,
            });
        }
        states.extend([
            InstanceState::Crashed { exit_code: 3 },
            InstanceState::Failed {
                reason: FailureReason::ConfigInvalid {
                    stderr: "syntax error".into(),
                },
            },
            InstanceState::Maintenance,
        ]);
        states
    }

    fn other(message: &str) -> FailureReason {
        FailureReason::Other {
            code: "internal".into(),
            message: message.into(),
        }
    }

    fn triggers() -> Vec<Trigger> {
        vec![
            Trigger::Start,
            Trigger::Healthy,
            Trigger::Exited { exit_code: 3 },
            Trigger::Exited {
                exit_code: STATUS_DLL_NOT_FOUND,
            },
            Trigger::Stop {
                cause: StopCause::User,
            },
            Trigger::Stop {
                cause: StopCause::Unhealthy,
            },
            Trigger::StopFinished { exit_code: 0 },
            Trigger::Fail {
                reason: other("boom"),
            },
            Trigger::EnterMaintenance,
            Trigger::ExitMaintenance,
        ]
    }

    fn stopping(cause: StopCause) -> InstanceState {
        InstanceState::Stopping {
            strategy: Some(StopStrategy::CtrlBreak),
            cause,
        }
    }

    /// Every allowed `(state, trigger) → state`; everything else must be rejected.
    fn allowed() -> Vec<(InstanceState, Trigger, InstanceState)> {
        use InstanceState as S;
        use Trigger as T;
        let failed = S::Failed {
            reason: FailureReason::ConfigInvalid {
                stderr: "syntax error".into(),
            },
        };
        let crashed = S::Crashed { exit_code: 3 };
        let fail = || T::Fail {
            reason: other("boom"),
        };
        let failed_boom = || S::Failed {
            reason: other("boom"),
        };
        let mut rows = vec![
            // Stopped
            (S::Stopped, T::Start, S::Starting),
            (S::Stopped, fail(), failed_boom()),
            (S::Stopped, T::EnterMaintenance, S::Maintenance),
            // Starting
            (S::Starting, T::Healthy, S::Running),
            (
                S::Starting,
                T::Exited { exit_code: 3 },
                S::Failed {
                    reason: FailureReason::ExitedDuringStart { exit_code: 3 },
                },
            ),
            (
                S::Starting,
                T::Exited {
                    exit_code: STATUS_DLL_NOT_FOUND,
                },
                S::Failed {
                    reason: FailureReason::MissingDll {
                        exit_code: wire_exit_code(STATUS_DLL_NOT_FOUND),
                    },
                },
            ),
            (
                S::Starting,
                T::Stop {
                    cause: StopCause::User,
                },
                S::Stopping {
                    strategy: None,
                    cause: StopCause::User,
                },
            ),
            (
                S::Starting,
                T::Stop {
                    cause: StopCause::Unhealthy,
                },
                S::Stopping {
                    strategy: None,
                    cause: StopCause::Unhealthy,
                },
            ),
            (S::Starting, fail(), failed_boom()),
            // Running
            (S::Running, T::Exited { exit_code: 3 }, crashed.clone()),
            (
                S::Running,
                T::Exited {
                    exit_code: STATUS_DLL_NOT_FOUND,
                },
                S::Crashed {
                    exit_code: STATUS_DLL_NOT_FOUND,
                },
            ),
            (
                S::Running,
                T::Stop {
                    cause: StopCause::User,
                },
                S::Stopping {
                    strategy: None,
                    cause: StopCause::User,
                },
            ),
            (
                S::Running,
                T::Stop {
                    cause: StopCause::Unhealthy,
                },
                S::Stopping {
                    strategy: None,
                    cause: StopCause::Unhealthy,
                },
            ),
            (S::Running, fail(), failed_boom()),
            // Crashed
            (crashed.clone(), T::Start, S::Starting),
            (crashed.clone(), fail(), failed_boom()),
            (crashed.clone(), T::EnterMaintenance, S::Maintenance),
            // Failed
            (failed.clone(), T::Start, S::Starting),
            (failed.clone(), fail(), failed_boom()),
            (failed.clone(), T::EnterMaintenance, S::Maintenance),
            // Maintenance
            (S::Maintenance, T::ExitMaintenance, S::Stopped),
        ];
        for cause in [StopCause::User, StopCause::Restart, StopCause::Maintenance] {
            rows.push((
                stopping(cause),
                T::StopFinished { exit_code: 0 },
                S::Stopped,
            ));
        }
        rows.push((
            stopping(StopCause::Unhealthy),
            T::StopFinished { exit_code: 0 },
            S::Crashed { exit_code: 0 },
        ));
        for cause in [
            StopCause::User,
            StopCause::Restart,
            StopCause::Maintenance,
            StopCause::Unhealthy,
            StopCause::StartTimeout,
        ] {
            rows.push((stopping(cause), fail(), failed_boom()));
        }
        rows
    }

    #[test]
    fn transition_table_is_exhaustive() {
        let allowed = allowed();
        let mut checked = 0;
        for state in states() {
            for trigger in triggers() {
                let expected = allowed
                    .iter()
                    .find(|(s, t, _)| *s == state && *t == trigger)
                    .map(|(_, _, next)| next.clone());
                let got = next_state(&state, trigger.clone());
                match expected {
                    Some(next) => assert_eq!(
                        got,
                        Ok(next),
                        "{} + {} should be allowed",
                        state.name(),
                        trigger.name()
                    ),
                    None => assert_eq!(
                        got,
                        Err(InvalidTransition {
                            state: state.name(),
                            trigger: trigger.name(),
                        }),
                        "{state:?} + {trigger:?} should be rejected"
                    ),
                }
                checked += 1;
            }
        }
        // 11 states (Stopping once per cause) × 10 triggers.
        assert_eq!(checked, 110);
        for (state, trigger, _) in &allowed {
            assert!(
                states().contains(state) && triggers().contains(trigger),
                "row {state:?} + {trigger:?} is outside the matrix"
            );
        }
    }

    #[test]
    fn invalid_transitions_are_conflict_errors() {
        let err = next_state(&InstanceState::Maintenance, Trigger::Start).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid transition: start while maintenance"
        );
        let err = ForgeError::from(err);
        assert_eq!(err.code(), "conflict");
    }

    #[test]
    fn state_serializes_with_tag() {
        let json = serde_json::to_value(InstanceState::Stopping {
            strategy: Some(StopStrategy::CtrlBreak),
            cause: StopCause::Unhealthy,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"state": "stopping", "strategy": "ctrl_break", "cause": "unhealthy"})
        );
        let failed = InstanceState::Failed {
            reason: FailureReason::PortInUse {
                port: 3306,
                pid: Some(4412),
                exe: Some("mysqld.exe".into()),
            },
        };
        let json = serde_json::to_value(&failed).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "state": "failed",
                "reason": {"kind": "port_in_use", "port": 3306, "pid": 4412, "exe": "mysqld.exe"}
            })
        );
        assert_eq!(
            serde_json::from_value::<InstanceState>(json).unwrap(),
            failed
        );
        assert!(InstanceState::Starting.is_active());
        assert!(!InstanceState::Crashed { exit_code: 3 }.is_active());
        assert!(!InstanceState::Maintenance.is_active());
    }

    #[test]
    fn exit_codes_are_classified() {
        assert_eq!(
            FailureReason::exited_during_start(STATUS_DLL_NOT_FOUND).kind(),
            "missing_dll"
        );
        assert_eq!(
            FailureReason::exited_during_start(STATUS_ENTRYPOINT_NOT_FOUND).kind(),
            "missing_dll"
        );
        assert_eq!(
            FailureReason::exited_during_start(STATUS_ACCESS_DENIED).kind(),
            "permission"
        );
        assert_eq!(
            FailureReason::exited_during_start(1),
            FailureReason::ExitedDuringStart { exit_code: 1 }
        );
        assert_eq!(FailureReason::for_exit_code(3), None);
        let missing = FailureReason::for_exit_code(STATUS_DLL_NOT_FOUND).unwrap();
        assert_eq!(
            serde_json::to_value(&missing).unwrap(),
            serde_json::json!({"kind": "missing_dll", "exit_code": -1_073_741_515})
        );
        assert!(
            missing.message().contains("0xc0000135"),
            "{}",
            missing.message()
        );
    }

    #[test]
    fn reasons_map_to_stable_error_codes() {
        let reasons = [
            FailureReason::PortInUse {
                port: 80,
                pid: Some(4),
                exe: Some("System".into()),
            },
            FailureReason::MissingDll {
                exit_code: -1_073_741_515,
            },
            FailureReason::ConfigInvalid {
                stderr: "line 3".into(),
            },
            FailureReason::MissingRuntime {
                flavor: "vs17".into(),
                download_url: "https://aka.ms/vc14/vc_redist.x64.exe".into(),
                installed: Some("14.29.30133.0".into()),
            },
            FailureReason::DependencyNotRunning {
                dependency: "mysql/default".into(),
                state: "stopped".into(),
            },
            FailureReason::Permission {
                message: "denied".into(),
            },
            FailureReason::HealthTimeout {
                timeout_ms: 2000,
                last_error: Some("connection refused".into()),
            },
            FailureReason::ExitedDuringStart { exit_code: 3 },
            FailureReason::CrashLoop {
                restarts: 3,
                window_ms: 60_000,
                exit_code: Some(3),
            },
            FailureReason::Other {
                code: "not_found".into(),
                message: "executable C:/x.exe".into(),
            },
        ];
        for reason in reasons {
            assert!(
                forge_core::error::ERROR_CODES.contains(&reason.code()),
                "{reason:?}"
            );
            assert_eq!(reason.to_error().code(), reason.code(), "{reason:?}");
            assert_eq!(reason.error_info().code, reason.code());
            assert!(!reason.message().is_empty());
            let json = serde_json::to_value(&reason).unwrap();
            assert_eq!(json["kind"], reason.kind());
            assert_eq!(
                serde_json::from_value::<FailureReason>(json).unwrap(),
                reason
            );
        }
        let timeout = FailureReason::HealthTimeout {
            timeout_ms: 30_000,
            last_error: Some("connect 127.0.0.1:3306: refused".into()),
        };
        assert_eq!(
            timeout.to_error().to_string(),
            "timed out after 30s: waiting for the health check (last error: connect \
             127.0.0.1:3306: refused)"
        );
    }

    #[test]
    fn precheck_reasons_explain_the_fix() {
        let runtime = FailureReason::MissingRuntime {
            flavor: "vs18".into(),
            download_url: "https://aka.ms/vc14/vc_redist.x64.exe".into(),
            installed: Some("14.44.35211.0".into()),
        };
        assert_eq!(
            runtime.message(),
            "the Visual C++ Redistributable (x64) for vs18 builds (14.50.0.0 or newer) is not \
             installed; found 14.44.35211.0; install it from \
             https://aka.ms/vc14/vc_redist.x64.exe"
        );
        assert_eq!(runtime.to_error().code(), "not_found");
        assert_eq!(
            serde_json::to_value(&runtime).unwrap(),
            serde_json::json!({
                "kind": "missing_runtime",
                "flavor": "vs18",
                "download_url": "https://aka.ms/vc14/vc_redist.x64.exe",
                "installed": "14.44.35211.0",
            })
        );
        let dependency = FailureReason::DependencyNotRunning {
            dependency: "mysql/default".into(),
            state: "stopped".into(),
        };
        assert_eq!(
            dependency.to_error().to_string(),
            "conflict: dependency mysql/default is not running (state stopped); start it first"
        );
    }

    #[test]
    fn errors_become_reasons() {
        let reason = FailureReason::from_error(&ForgeError::PortInUse {
            port: 3306,
            owner: Some("mysqld.exe (PID 4412)".into()),
            pid: Some(4412),
            exe: Some(r"C:\xampp\mysql\bin\mysqld.exe".into()),
        });
        assert_eq!(reason.kind(), "port_in_use");
        assert_eq!(
            reason,
            FailureReason::PortInUse {
                port: 3306,
                pid: Some(4412),
                exe: Some(r"C:\xampp\mysql\bin\mysqld.exe".into()),
            },
            "the holder of the error reaches the reason"
        );
        // Back to an error: the same holder, and the owner text for API 1.0 clients.
        match reason.to_error() {
            ForgeError::PortInUse {
                pid, exe, owner, ..
            } => {
                assert_eq!(pid, Some(4412));
                assert_eq!(exe.as_deref(), Some(r"C:\xampp\mysql\bin\mysqld.exe"));
                assert_eq!(
                    owner.as_deref(),
                    Some(r"C:\xampp\mysql\bin\mysqld.exe (PID 4412)")
                );
            }
            other => panic!("{other:?}"),
        }
        let reason = FailureReason::from_error(&ForgeError::NeedsElevation("x".into()));
        assert_eq!(reason.kind(), "permission");
        let reason = FailureReason::from_error(&ForgeError::io(
            "starting x",
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        ));
        assert_eq!(reason.kind(), "permission");
        let reason = FailureReason::from_error(&ForgeError::NotFound("exe".into()));
        assert_eq!(
            reason,
            FailureReason::Other {
                code: "not_found".into(),
                message: "not found: exe".into()
            }
        );
        let reason = FailureReason::from_error(&ForgeError::ConfigInvalid("bad".into()));
        assert_eq!(
            reason,
            FailureReason::ConfigInvalid {
                stderr: "bad".into()
            }
        );
    }
}
