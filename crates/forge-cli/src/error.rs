//! Errors of the CLI: a stable `code` (the API's, `forge_core::error::ERROR_CODES`), a
//! message for humans and the exit code of docs/09 derived from the code.

use forge_ipc::base::error::exit_code_for;
use forge_ipc::{ApiError, ClientError};
use serde_json::Value;

/// Result of a command.
pub type CliResult<T> = Result<T, CliError>;

/// Why a command failed.
#[derive(Debug, Clone, PartialEq)]
pub struct CliError {
    /// Stable machine code (`not_found`, `conflict`, `not_implemented`...).
    pub code: String,
    /// Message for the user (already localised when the CLI produced it; the daemon's
    /// messages are English like its logs).
    pub message: String,
    /// Structured details from the API (port owner, offending path...).
    pub details: Option<Value>,
    /// Process exit code.
    pub exit: i32,
    /// Nothing to print: the command already explained itself (`forge exec` passes the
    /// exit code of the child through).
    pub silent: bool,
}

impl CliError {
    /// Error with `code`; the exit code follows docs/09.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        Self {
            exit: exit_code_for(&code),
            code,
            message: message.into(),
            details: None,
            silent: false,
        }
    }

    /// Invalid use (exit 2).
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new("invalid_input", message)
    }

    /// Anything else that went wrong on this machine (exit 1).
    pub fn io(message: impl Into<String>) -> Self {
        Self::new("io", message)
    }

    /// A feature of a later roadmap task (exit 1).
    pub fn not_implemented(message: impl Into<String>) -> Self {
        Self::new("not_implemented", message)
    }

    /// The command ended with `exit` and already printed what there was to say.
    pub fn exit_silently(exit: i32) -> Self {
        Self {
            code: if exit == 0 { "ok" } else { "io" }.into(),
            message: String::new(),
            details: None,
            exit,
            silent: true,
        }
    }

    /// The `{"error": ...}` document of `--json`.
    pub fn to_json(&self) -> Value {
        let mut error = serde_json::json!({ "code": self.code, "message": self.message });
        if let Some(details) = &self.details {
            error["details"] = details.clone();
        }
        serde_json::json!({ "error": error })
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

impl From<ApiError> for CliError {
    fn from(error: ApiError) -> Self {
        Self {
            exit: error.exit_code(),
            code: error.code,
            message: error.message,
            details: error.details,
            silent: false,
        }
    }
}

impl From<ClientError> for CliError {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::Api(api) => api.into(),
            other => Self {
                exit: other.exit_code(),
                code: other.code().to_owned(),
                message: other.to_string(),
                details: None,
                silent: false,
            },
        }
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        Self::io(error.to_string())
    }
}

impl From<serde_json::Error> for CliError {
    fn from(error: serde_json::Error) -> Self {
        Self::new("internal", format!("JSON: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_the_codes_of_the_api() {
        for (code, exit) in [
            ("invalid_input", 2),
            ("config_invalid", 2),
            ("not_found", 3),
            ("conflict", 4),
            ("port_in_use", 4),
            ("needs_elevation", 5),
            ("verification_failed", 6),
            ("not_implemented", 1),
            ("io", 1),
            ("forbidden", 1),
            ("something_new", 1),
        ] {
            assert_eq!(CliError::new(code, "x").exit, exit, "{code}");
        }
    }

    #[test]
    fn api_errors_keep_code_and_details() {
        let mut api = ApiError::new("port_in_use", "port 80 is taken");
        api.details = Some(serde_json::json!({"port": 80}));
        let err = CliError::from(ClientError::Api(api));
        assert_eq!((err.code.as_str(), err.exit), ("port_in_use", 4));
        assert_eq!(err.to_json()["error"]["details"]["port"], 80);
    }

    #[test]
    fn transport_failures_are_generic_errors() {
        let err = CliError::from(ClientError::Disconnected);
        assert_eq!((err.code.as_str(), err.exit), ("io", 1));
        let err = CliError::from(ClientError::Timeout(std::time::Duration::from_secs(1)));
        assert_eq!((err.code.as_str(), err.exit), ("timeout", 1));
    }

    #[test]
    fn json_error_document_is_stable() {
        let json = CliError::usage("faltou o alvo").to_json();
        assert_eq!(json["error"]["code"], "invalid_input");
        assert_eq!(json["error"]["message"], "faltou o alvo");
    }
}
