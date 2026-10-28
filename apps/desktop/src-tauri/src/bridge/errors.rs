//! Errors of the bridge as the webview sees them: always the API's own
//! [`ApiError`] `{code, message, details}`, with `details = {reason, args}` (ADR-0039), so
//! the UI localizes from `code` + `details` and never from the English `message`.
//!
//! Daemon errors pass through unchanged. Failures that happen on this side of the pipe get
//! stable codes too ([ADR-0034](../../../../../docs/adr/0034-desktop-shell-architecture.md)):
//!
//! | code | reason | when |
//! |---|---|---|
//! | `disconnected` | `daemon_unreachable`, `daemon_not_running`, `connection_lost`, `context_closed` | the daemon could not be reached, or the connection dropped (after the retry with the same `request_id`) |
//! | `timeout` | `no_answer` | no answer within the call's deadline |
//! | `unauthorized` | `token_unreadable` (or the daemon's own `unauthorized`) | the API token could not be read or was refused |
//! | `not_found` | `unknown_context`, `unknown_method`, `unknown_stream`, `daemon_not_found`, `root_uninitialized` | refused by the bridge before anything was sent |
//! | `invalid_input` | `use_subscribe`, `use_unsubscribe`, `not_a_stream`, `params_not_object`, `invalid_request_id` | idem |
//!
//! `disconnected` is the only code that is not in `forge_core::error::ERROR_CODES`: it never
//! crosses the wire, the client invents it (`CLIENT_ERROR_CODES` in `src/ipc/errors.ts`).

use std::time::Duration;

use serde_json::json;

use forge_ipc::ApiError;
use forge_ipc::base::actor::AskError;
use forge_ipc::client::ClientError;
use forge_ipc::client::local::{DAEMON_EXE, LocalError};

/// Client-side code: the daemon could not be reached or the connection was lost.
pub const DISCONNECTED: &str = "disconnected";

fn ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The webview's view of a failed client operation on `method` (when there is one).
pub fn client_error(err: &ClientError, method: Option<&str>) -> ApiError {
    match err {
        ClientError::Api(api) => api.clone(),
        ClientError::Credentials(api) if api.code == "not_found" => ApiError::new(
            DISCONNECTED,
            "the daemon is not running: its API token does not exist yet",
        )
        .with_reason("daemon_not_running", json!({})),
        ClientError::Credentials(api) => ApiError::new(
            "unauthorized",
            format!("the API token could not be read: {}", api.message),
        )
        .with_reason("token_unreadable", json!({ "code": api.code })),
        ClientError::Transport(transport) if transport.code() == "io" => ApiError::new(
            DISCONNECTED,
            format!("the daemon is not reachable: {transport}"),
        )
        .with_reason("daemon_unreachable", json!({})),
        ClientError::Transport(transport) => ApiError::new(transport.code(), transport.to_string())
            .with_reason("transport", json!({})),
        ClientError::Disconnected => ApiError::new(
            DISCONNECTED,
            "the connection to the daemon was lost before the answer",
        )
        .with_reason("connection_lost", json!({ "method": method })),
        ClientError::Timeout(after) => ApiError::new(
            "timeout",
            format!("no answer from the daemon within {} ms", ms(*after)),
        )
        .with_reason(
            "no_answer",
            json!({ "method": method, "after_ms": ms(*after) }),
        ),
        ClientError::Protocol(message) => {
            ApiError::new("internal", format!("protocol error: {message}"))
                .with_reason("protocol", json!({}))
        }
        ClientError::Closed => ApiError::new(DISCONNECTED, "the connection context is closed")
            .with_reason("context_closed", json!({})),
        ClientError::InvalidInput(message) => ApiError::new("invalid_input", message.clone()),
    }
}

/// Why starting the local daemon failed.
pub fn local_error(err: &LocalError) -> ApiError {
    match err {
        LocalError::Down(client) | LocalError::Other(client) => client_error(client, None),
        LocalError::DaemonNotFound => ApiError::new("not_found", err.to_string())
            .with_reason("daemon_not_found", json!({ "exe": DAEMON_EXE })),
        LocalError::SpawnFailed { exe, error } => ApiError::new("io", err.to_string()).with_reason(
            "daemon_spawn_failed",
            json!({ "exe": exe.display().to_string(), "code": error.code }),
        ),
        LocalError::Exited { code } => ApiError::new("io", err.to_string())
            .with_reason("daemon_exited", json!({ "exit_code": code })),
        LocalError::Timeout { timeout, last } => ApiError::new("timeout", err.to_string())
            .with_reason(
                "daemon_start_timeout",
                json!({ "after_ms": ms(*timeout), "last": client_error(last, None).code }),
            ),
    }
}

/// The bridge's own actor did not answer (overloaded or restarting after a panic).
pub fn ask_error(err: &AskError) -> ApiError {
    match err {
        AskError::Timeout { after, .. } => ApiError::new("timeout", err.to_string())
            .with_reason("bridge_busy", json!({ "after_ms": ms(*after) })),
        AskError::MailboxFull { .. } | AskError::Stopped { .. } => {
            ApiError::new("internal", err.to_string()).with_reason("bridge_busy", json!({}))
        }
    }
}

pub fn unknown_context(context: &str) -> ApiError {
    ApiError::new("not_found", format!("unknown context {context:?}"))
        .with_reason("unknown_context", json!({ "context": context }))
}

pub fn root_uninitialized(root: &str) -> ApiError {
    ApiError::new(
        "not_found",
        format!("{root} is not an initialized Forgebox root (no state/ folder)"),
    )
    .with_reason("root_uninitialized", json!({ "root": root }))
}

#[cfg(test)]
mod tests {
    use forge_ipc::transport::TransportError;

    use super::*;

    #[test]
    fn client_failures_have_stable_codes_and_reasons() {
        let cases = [
            (ClientError::Disconnected, DISCONNECTED, "connection_lost"),
            (
                ClientError::Timeout(Duration::from_millis(1500)),
                "timeout",
                "no_answer",
            ),
            (
                ClientError::Transport(TransportError::Unavailable {
                    endpoint: "pipe".into(),
                    reason: "no listener".into(),
                }),
                DISCONNECTED,
                "daemon_unreachable",
            ),
            (
                ClientError::Credentials(ApiError::new("not_found", "no token")),
                DISCONNECTED,
                "daemon_not_running",
            ),
            (
                ClientError::Credentials(ApiError::new("io", "access denied")),
                "unauthorized",
                "token_unreadable",
            ),
            (ClientError::Closed, DISCONNECTED, "context_closed"),
            (
                ClientError::Transport(TransportError::Untrusted("x".into())),
                "forbidden",
                "transport",
            ),
            (ClientError::Protocol("x".into()), "internal", "protocol"),
        ];
        for (err, code, reason) in cases {
            let api = client_error(&err, Some("sites.list"));
            assert_eq!(api.code, code, "{err:?}");
            assert_eq!(api.reason(), Some(reason), "{err:?}");
        }
        let timeout = client_error(
            &ClientError::Timeout(Duration::from_millis(1500)),
            Some("sites.list"),
        );
        assert_eq!(timeout.args().unwrap()["after_ms"], 1500);
        assert_eq!(timeout.args().unwrap()["method"], "sites.list");
    }

    #[test]
    fn daemon_errors_pass_through_unchanged() {
        let daemon = ApiError::new("unauthorized", "bad token").with_reason("x", json!({"a": 1}));
        assert_eq!(client_error(&ClientError::Api(daemon.clone()), None), daemon);
    }

    #[test]
    fn start_failures_name_their_reason() {
        assert_eq!(
            local_error(&LocalError::DaemonNotFound).reason(),
            Some("daemon_not_found")
        );
        let exited = local_error(&LocalError::Exited { code: 3 });
        assert_eq!(
            (exited.code.as_str(), exited.reason()),
            ("io", Some("daemon_exited"))
        );
        assert_eq!(exited.args().unwrap()["exit_code"], 3);
        let slow = local_error(&LocalError::Timeout {
            timeout: Duration::from_secs(10),
            last: ClientError::Disconnected,
        });
        assert_eq!(slow.code, "timeout");
        assert_eq!(slow.args().unwrap()["last"], DISCONNECTED);
    }
}
