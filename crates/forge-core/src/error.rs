//! Error type shared by the domain crates, with stable codes for the API and CLI.
//!
//! [`ForgeError::code`] is part of the public API contract (ADR-0009): clients match on
//! it, so codes are never renamed. [`ForgeError::exit_code`] follows docs/09-cli.md:
//! `0` ok · `1` generic · `2` invalid usage · `3` not found · `4` conflict ·
//! `5` needs elevation · `6` verification failed.

use std::io;
use std::path::Path;
use std::time::Duration;

/// Result alias used across Forgebox crates.
pub type Result<T, E = ForgeError> = std::result::Result<T, E>;

/// Every error the domain can report.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ForgeError {
    /// An entity, file or version does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// The operation collides with existing state (host taken, link already there...).
    #[error("conflict: {0}")]
    Conflict(String),

    /// A TCP/UDP port is taken, optionally by a known owner (`mysqld.exe (PID 4412)`).
    #[error("port {port} is already in use{}", owner_suffix(owner.as_deref()))]
    PortInUse { port: u16, owner: Option<String> },

    /// The operation must go through `forgebox-helper` (docs/11).
    #[error("operation needs elevation: {0}")]
    NeedsElevation(String),

    /// Checksum, signature or size verification failed.
    #[error("verification failed: {0}")]
    VerificationFailed(String),

    /// File system or other I/O failure, with a human readable context.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },

    /// SQLite failure.
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    /// The database schema cannot be used by this build (newer version, broken migration).
    #[error("database schema error: {0}")]
    Schema(String),

    /// `forgebox.toml`, `root.toml` or another configuration value is invalid.
    #[error("invalid configuration: {0}")]
    ConfigInvalid(String),

    /// An argument is malformed (bad id, bad path component, bad host...).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// The OS, file system or volume does not support the operation.
    #[error("unsupported on this platform: {0}")]
    UnsupportedPlatform(String),

    /// An operation exceeded its time budget (ADR-0011: every message has a timeout).
    #[error("timed out after {after:?}: {operation}")]
    Timeout { operation: String, after: Duration },

    /// The feature exists in the contracts but is not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(String),

    /// An API method (or part of one) that belongs to a later roadmap task. Same code and
    /// text format as [`ForgeError::NotImplemented`], but keeps the pieces apart so the
    /// API can send them as structured details (`reason: "not_implemented"`, args
    /// `{method, task, what}`) and clients name the task without parsing the message.
    #[error("not implemented: {method} is not implemented yet: {what} (roadmap {task})")]
    NotImplementedYet {
        /// `sites.share`.
        method: String,
        /// Roadmap task that delivers it (`M3-E3`).
        task: String,
        /// What is missing, in English (`public tunnels`).
        what: String,
    },

    /// The operation was cancelled on request (`job.cancel`, Ctrl+C in the CLI).
    #[error("cancelled: {0}")]
    Cancelled(String),

    /// Invariant broken inside the daemon: an actor or the store writer stopped, a job
    /// panicked. Never caused by user input; always worth a bug report.
    #[error("internal error: {0}")]
    Internal(String),

    /// The API caller is not authenticated: missing `system.hello`, wrong token
    /// (docs/11 §4).
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    /// The API caller is authenticated but not allowed to do this: its role is below the
    /// method's `min_role`, the method is `local_only` on a remote connection, or a path
    /// is outside the `fs.*` scope (ADR-0009, rules 5-7).
    #[error("forbidden: {0}")]
    Forbidden(String),
}

fn owner_suffix(owner: Option<&str>) -> String {
    owner.map(|o| format!(" by {o}")).unwrap_or_default()
}

/// Every value [`ForgeError::code`] can return. Stable; only additions are allowed.
pub const ERROR_CODES: &[&str] = &[
    "not_found",
    "conflict",
    "port_in_use",
    "needs_elevation",
    "verification_failed",
    "io",
    "db",
    "config_invalid",
    "invalid_input",
    "unsupported_platform",
    "timeout",
    "not_implemented",
    "cancelled",
    "internal",
    "unauthorized",
    "forbidden",
];

/// CLI exit code for a stable error code (docs/09-cli.md). Unknown codes (sent by a newer
/// daemon) map to `1`. Clients that only see the code on the wire (`forge` talking to the
/// API) use this instead of [`ForgeError::exit_code`]; both always agree.
pub fn exit_code_for(code: &str) -> i32 {
    match code {
        "config_invalid" | "invalid_input" => 2,
        "not_found" => 3,
        "conflict" | "port_in_use" => 4,
        "needs_elevation" => 5,
        "verification_failed" => 6,
        _ => 1,
    }
}

impl ForgeError {
    /// Rebuilds an error from the `code` and `message` a finished job reports
    /// (`JobErrorInfo`), so a handler that waits for another job fails with the same
    /// code (and exit code) instead of `internal`. `message` is the job error's `Display`
    /// text; its own prefix (`not found: `) is not repeated. Codes whose variant needs
    /// more than text (`port_in_use`, `timeout`, `io`, `db`) come back as `Conflict`,
    /// `Internal`... with the message kept; unknown codes are `Internal`.
    pub fn from_code(code: &str, message: &str) -> Self {
        let text = |prefix: &str| message.strip_prefix(prefix).unwrap_or(message).to_owned();
        match code {
            "not_found" => Self::NotFound(text("not found: ")),
            "conflict" => Self::Conflict(text("conflict: ")),
            "port_in_use" => Self::Conflict(message.to_owned()),
            "needs_elevation" => Self::NeedsElevation(text("operation needs elevation: ")),
            "verification_failed" => Self::VerificationFailed(text("verification failed: ")),
            "config_invalid" => Self::ConfigInvalid(text("invalid configuration: ")),
            "invalid_input" => Self::InvalidInput(text("invalid input: ")),
            "unsupported_platform" => {
                Self::UnsupportedPlatform(text("unsupported on this platform: "))
            }
            "not_implemented" => Self::NotImplemented(text("not implemented: ")),
            "cancelled" => Self::Cancelled(text("cancelled: ")),
            "unauthorized" => Self::Unauthorized(text("unauthorized: ")),
            "forbidden" => Self::Forbidden(text("forbidden: ")),
            _ => Self::Internal(format!("{code}: {message}")),
        }
    }

    /// Stable machine-readable code, used by the API and by `forge --json`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::PortInUse { .. } => "port_in_use",
            Self::NeedsElevation(_) => "needs_elevation",
            Self::VerificationFailed(_) => "verification_failed",
            Self::Io { .. } => "io",
            Self::Db(_) | Self::Schema(_) => "db",
            Self::ConfigInvalid(_) => "config_invalid",
            Self::InvalidInput(_) => "invalid_input",
            Self::UnsupportedPlatform(_) => "unsupported_platform",
            Self::Timeout { .. } => "timeout",
            Self::NotImplemented(_) | Self::NotImplementedYet { .. } => "not_implemented",
            Self::Cancelled(_) => "cancelled",
            Self::Internal(_) => "internal",
            Self::Unauthorized(_) => "unauthorized",
            Self::Forbidden(_) => "forbidden",
        }
    }

    /// Structured details for clients (docs/09 "Detalhes estruturados de erro", ADR-0039):
    /// `{"reason": <stable snake_case id>, "args": {...}}`. Clients localize from `code`
    /// plus these, never from the English message. Only the errors a GUI acts on carry
    /// them: `port_in_use` (`port`, `owner`), `needs_elevation` (`detail`), `not_found`
    /// (`what`), `not_implemented` (`method`, `task`, `what`) and `timeout`
    /// (`operation`, `after_ms`).
    pub fn details(&self) -> Option<serde_json::Value> {
        use serde_json::json;
        let (reason, args) = match self {
            Self::PortInUse { port, owner } => {
                ("port_in_use", json!({ "port": port, "owner": owner }))
            }
            Self::NeedsElevation(detail) => ("needs_elevation", json!({ "detail": detail })),
            Self::NotFound(what) => ("not_found", json!({ "what": what })),
            Self::NotImplemented(what) => ("not_implemented", json!({ "what": what })),
            Self::NotImplementedYet { method, task, what } => (
                "not_implemented",
                json!({ "method": method, "task": task, "what": what }),
            ),
            Self::Timeout { operation, after } => (
                "timeout",
                json!({
                    "operation": operation,
                    "after_ms": u64::try_from(after.as_millis()).unwrap_or(u64::MAX),
                }),
            ),
            _ => return None,
        };
        Some(json!({ "reason": reason, "args": args }))
    }

    /// Process exit code for the CLI (docs/09-cli.md), see [`exit_code_for`].
    pub fn exit_code(&self) -> i32 {
        exit_code_for(self.code())
    }

    /// I/O error with context.
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// I/O error about `path`, mapping `NotFound` to [`ForgeError::NotFound`].
    pub fn io_at(action: &str, path: &Path, source: io::Error) -> Self {
        if source.kind() == io::ErrorKind::NotFound {
            return Self::NotFound(format!("{} ({action})", path.display()));
        }
        Self::io(format!("{action} {}", path.display()), source)
    }
}

impl From<io::Error> for ForgeError {
    fn from(source: io::Error) -> Self {
        Self::io("I/O error", source)
    }
}

/// Adds context to `std::io::Result` values.
pub trait IoResultExt<T> {
    /// Wraps the error as [`ForgeError::Io`] (or `NotFound`) mentioning `action` and `path`.
    fn at(self, action: &str, path: &Path) -> Result<T>;
}

impl<T> IoResultExt<T> for io::Result<T> {
    fn at(self, action: &str, path: &Path) -> Result<T> {
        self.map_err(|e| ForgeError::io_at(action, path, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<ForgeError> {
        vec![
            ForgeError::NotFound("x".into()),
            ForgeError::Conflict("x".into()),
            ForgeError::PortInUse {
                port: 80,
                owner: None,
            },
            ForgeError::NeedsElevation("nrpt.add".into()),
            ForgeError::VerificationFailed("sha256".into()),
            ForgeError::io("reading", io::Error::other("boom")),
            ForgeError::Db(rusqlite::Error::InvalidQuery),
            ForgeError::Schema("x".into()),
            ForgeError::ConfigInvalid("x".into()),
            ForgeError::InvalidInput("x".into()),
            ForgeError::UnsupportedPlatform("x".into()),
            ForgeError::Timeout {
                operation: "stop".into(),
                after: Duration::from_secs(1),
            },
            ForgeError::NotImplemented("x".into()),
            ForgeError::NotImplementedYet {
                method: "sites.share".into(),
                task: "M3-E3".into(),
                what: "public tunnels".into(),
            },
            ForgeError::Cancelled("x".into()),
            ForgeError::Internal("x".into()),
            ForgeError::Unauthorized("x".into()),
            ForgeError::Forbidden("x".into()),
        ]
    }

    #[test]
    fn errors_are_rebuilt_from_a_job_failure_with_the_same_exit_code() {
        for err in samples() {
            let rebuilt = ForgeError::from_code(err.code(), &err.to_string());
            assert_eq!(
                rebuilt.exit_code(),
                err.exit_code(),
                "{} -> {}",
                err.code(),
                rebuilt.code()
            );
        }
        let rebuilt = ForgeError::from_code("not_found", "not found: php 8.3");
        assert_eq!(rebuilt.to_string(), "not found: php 8.3");
        assert_eq!(rebuilt.code(), "not_found");
        assert_eq!(ForgeError::from_code("brand_new", "x").code(), "internal");
    }

    #[test]
    fn codes_are_stable_and_listed() {
        for err in samples() {
            assert!(ERROR_CODES.contains(&err.code()), "{}", err.code());
        }
        let mut seen: Vec<_> = samples().iter().map(ForgeError::code).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), ERROR_CODES.len(), "every code has a variant");
    }

    #[test]
    fn exit_codes_follow_cli_doc() {
        for err in samples() {
            let expected = match err.code() {
                "config_invalid" | "invalid_input" => 2,
                "not_found" => 3,
                "conflict" | "port_in_use" => 4,
                "needs_elevation" => 5,
                "verification_failed" => 6,
                _ => 1,
            };
            assert_eq!(err.exit_code(), expected, "{}", err.code());
            assert_eq!(exit_code_for(err.code()), expected, "{}", err.code());
        }
        assert_eq!(exit_code_for("code_from_a_newer_daemon"), 1);
    }

    #[test]
    fn port_in_use_message_mentions_owner() {
        let err = ForgeError::PortInUse {
            port: 3306,
            owner: Some("mysqld.exe (PID 4412)".into()),
        };
        assert_eq!(
            err.to_string(),
            "port 3306 is already in use by mysqld.exe (PID 4412)"
        );
        let err = ForgeError::PortInUse {
            port: 80,
            owner: None,
        };
        assert_eq!(err.to_string(), "port 80 is already in use");
    }

    #[test]
    fn structured_details_follow_the_reason_args_convention() {
        let details = |err: ForgeError| err.details().expect("details");
        let d = details(ForgeError::PortInUse {
            port: 3306,
            owner: Some("mysqld.exe (PID 4412)".into()),
        });
        assert_eq!(d["reason"], "port_in_use");
        assert_eq!(d["args"]["port"], 3306);
        assert_eq!(d["args"]["owner"], "mysqld.exe (PID 4412)");
        let d = details(ForgeError::PortInUse {
            port: 80,
            owner: None,
        });
        assert!(d["args"]["owner"].is_null());
        let d = details(ForgeError::NeedsElevation("nrpt.add".into()));
        assert_eq!(d["reason"], "needs_elevation");
        assert_eq!(d["args"]["detail"], "nrpt.add");
        let d = details(ForgeError::NotFound("site loja.test".into()));
        assert_eq!(d["reason"], "not_found");
        assert_eq!(d["args"]["what"], "site loja.test");
        let d = details(ForgeError::NotImplementedYet {
            method: "sites.share".into(),
            task: "M3-E3".into(),
            what: "public tunnels".into(),
        });
        assert_eq!(d["reason"], "not_implemented");
        assert_eq!(d["args"]["task"], "M3-E3");
        assert_eq!(d["args"]["method"], "sites.share");
        let d = details(ForgeError::Timeout {
            operation: "stop".into(),
            after: Duration::from_millis(1500),
        });
        assert_eq!(d["args"]["after_ms"], 1500);
        // Errors nobody acts on structurally stay without details.
        assert!(ForgeError::Internal("x".into()).details().is_none());
        assert!(ForgeError::Conflict("x".into()).details().is_none());
    }

    #[test]
    fn the_not_implemented_variant_keeps_the_table_message() {
        let err = ForgeError::NotImplementedYet {
            method: "sites.share".into(),
            task: "M3-E3".into(),
            what: "public tunnels".into(),
        };
        assert_eq!(
            err.to_string(),
            "not implemented: sites.share is not implemented yet: public tunnels (roadmap M3-E3)"
        );
        assert_eq!(err.code(), "not_implemented");
    }

    #[test]
    fn io_at_maps_not_found() {
        let path = Path::new("x.toml");
        let err = ForgeError::io_at("reading", path, io::ErrorKind::NotFound.into());
        assert_eq!(err.code(), "not_found");
        let err = ForgeError::io_at("reading", path, io::ErrorKind::PermissionDenied.into());
        assert_eq!(err.code(), "io");
        assert!(err.to_string().starts_with("reading x.toml"));
    }
}
