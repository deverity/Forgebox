//! Request and response documents of `forgebox-helper` and where they live during a run.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::op::HelperOp;
use crate::error::{ForgeError, Result};
use crate::ids::RequestId;

/// Version of the documents. Daemon and helper ship together; a mismatch means a broken
/// installation and is refused.
pub const HELPER_PROTOCOL_VERSION: u32 = 1;

/// File name of the helper next to the other executables (docs/08 §2).
pub const HELPER_EXE_NAME: &str = "forgebox-helper.exe";

/// Biggest request or response accepted (a full `hosts.set_block` is about 300 KiB).
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;

/// What the daemon asks the helper to do.
///
/// ```json
/// {"v": 1, "request_id": "01J...", "root": "C:\\Forgebox",
///  "op": {"name": "nrpt.add", "params": {"namespace": ".test", "nameserver": "127.0.0.1"}}}
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperRequest {
    pub v: u32,
    /// Id of the API request that caused it (ADR-0009): names the exchange files and
    /// ties the `helper.log` line to the job.
    pub request_id: RequestId,
    /// Forgebox root the operation is about; must be the helper's own root.
    pub root: PathBuf,
    pub op: HelperOp,
}

impl HelperRequest {
    /// A request with a fresh id.
    pub fn new(root: impl Into<PathBuf>, op: HelperOp) -> Self {
        Self::with_id(RequestId::new(), root, op)
    }

    /// A request carrying the id of the API call that caused it.
    pub fn with_id(request_id: RequestId, root: impl Into<PathBuf>, op: HelperOp) -> Self {
        Self {
            v: HELPER_PROTOCOL_VERSION,
            request_id,
            root: root.into(),
            op,
        }
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| ForgeError::Internal(format!("encoding a helper request: {e}")))
    }

    /// Parses a request (as the helper does). Unknown operations, unknown fields and other
    /// protocol versions are refused (`forbidden` for an operation outside the allowlist,
    /// `invalid_input` otherwise).
    pub fn from_json(text: &str) -> Result<Self> {
        check_size(text, "request")?;
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| ForgeError::InvalidInput(format!("helper request is not JSON: {e}")))?;
        check_version(&value, "request")?;
        if let Some(name) = value.pointer("/op/name").and_then(|n| n.as_str()) {
            if !HelperOp::NAMES.contains(&name) {
                return Err(ForgeError::Forbidden(format!(
                    "operation {name:?} is not in the helper allowlist"
                )));
            }
        }
        serde_json::from_value(value)
            .map_err(|e| ForgeError::InvalidInput(format!("invalid helper request: {e}")))
    }
}

/// What the helper answers.
///
/// ```json
/// {"v": 1, "request_id": "01J...", "ok": false, "code": "forbidden",
///  "message": "nrpt.add: namespace \".com\" is not one of the configured TLDs", "details": null}
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperResponse {
    pub v: u32,
    /// Copied from the request.
    pub request_id: RequestId,
    pub ok: bool,
    /// Stable error code ([`ForgeError::code`]) when `ok` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Human readable outcome, in English (logs); the UI translates by `code`.
    pub message: String,
    /// Operation specific data (rule id created, thumbprint removed...).
    #[serde(default)]
    pub details: serde_json::Value,
}

impl HelperResponse {
    /// Success for `request`.
    pub fn success(request: &HelperRequest, message: impl Into<String>) -> Self {
        Self {
            v: HELPER_PROTOCOL_VERSION,
            request_id: request.request_id,
            ok: true,
            code: None,
            message: message.into(),
            details: serde_json::Value::Null,
        }
    }

    /// Failure of the request `request_id` (the id may come from a request that could not
    /// be fully parsed).
    pub fn failure(request_id: RequestId, error: &ForgeError) -> Self {
        Self {
            v: HELPER_PROTOCOL_VERSION,
            request_id,
            ok: false,
            code: Some(error.code().to_owned()),
            message: error.to_string(),
            details: serde_json::Value::Null,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = details;
        self
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| ForgeError::Internal(format!("encoding a helper response: {e}")))
    }

    /// Parses an answer (as the daemon does).
    pub fn from_json(text: &str) -> Result<Self> {
        check_size(text, "response")?;
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| {
            ForgeError::Internal(format!(
                "forgebox-helper answered something that is not JSON: {e}"
            ))
        })?;
        check_version(&value, "response").map_err(|e| ForgeError::Internal(e.to_string()))?;
        serde_json::from_value(value)
            .map_err(|e| ForgeError::Internal(format!("invalid forgebox-helper response: {e}")))
    }

    /// Checks that this answers `request` and turns a refusal into the matching
    /// [`ForgeError`] (`forbidden`, `invalid_input`, `not_found`...).
    pub fn into_result(self, request: &HelperRequest) -> Result<Self> {
        if self.request_id != request.request_id {
            return Err(ForgeError::Internal(format!(
                "forgebox-helper answered request {} instead of {}",
                self.request_id, request.request_id
            )));
        }
        if self.ok {
            return Ok(self);
        }
        let code = self.code.as_deref().unwrap_or("internal");
        let message = format!("forgebox-helper {}: {}", request.op.name(), self.message);
        Err(match code {
            "not_found" => ForgeError::NotFound(message),
            "conflict" => ForgeError::Conflict(message),
            "needs_elevation" => ForgeError::NeedsElevation(message),
            "verification_failed" => ForgeError::VerificationFailed(message),
            "config_invalid" => ForgeError::ConfigInvalid(message),
            "invalid_input" => ForgeError::InvalidInput(message),
            "unsupported_platform" => ForgeError::UnsupportedPlatform(message),
            "not_implemented" => ForgeError::NotImplemented(message),
            "cancelled" => ForgeError::Cancelled(message),
            "unauthorized" => ForgeError::Unauthorized(message),
            "forbidden" => ForgeError::Forbidden(message),
            other => ForgeError::Internal(format!("{message} ({other})")),
        })
    }
}

fn check_size(text: &str, what: &str) -> Result<()> {
    if text.len() > MAX_DOCUMENT_BYTES {
        return Err(ForgeError::InvalidInput(format!(
            "helper {what} has {} bytes; at most {MAX_DOCUMENT_BYTES} are accepted",
            text.len()
        )));
    }
    Ok(())
}

fn check_version(value: &serde_json::Value, what: &str) -> Result<()> {
    match value.get("v").and_then(serde_json::Value::as_u64) {
        Some(v) if v == u64::from(HELPER_PROTOCOL_VERSION) => Ok(()),
        Some(v) => Err(ForgeError::InvalidInput(format!(
            "helper {what} has protocol version {v}; this build speaks {HELPER_PROTOCOL_VERSION}"
        ))),
        None => Err(ForgeError::InvalidInput(format!(
            "helper {what} has no protocol version"
        ))),
    }
}

/// `<root>/state/locks/helper-<id>.request.json`: the request of one run.
pub fn request_path(root: &Path, request_id: RequestId) -> PathBuf {
    locks(root).join(format!("helper-{request_id}.request.json"))
}

/// `<root>/state/locks/helper-<id>.response.json`: the answer of one run.
pub fn response_path(root: &Path, request_id: RequestId) -> PathBuf {
    locks(root).join(format!("helper-{request_id}.response.json"))
}

fn locks(root: &Path) -> PathBuf {
    root.join("state").join("locks")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> HelperRequest {
        HelperRequest::new(
            "C:/Forgebox",
            HelperOp::NrptAdd {
                namespace: ".test".into(),
                nameserver: "127.0.0.1".parse().unwrap(),
            },
        )
    }

    #[test]
    fn request_round_trip() {
        let req = request();
        let json = req.to_json().unwrap();
        assert!(json.contains("\"name\": \"nrpt.add\""), "{json}");
        assert_eq!(HelperRequest::from_json(&json).unwrap(), req);
        let id = RequestId::new();
        assert_eq!(
            HelperRequest::with_id(id, "x", req.op.clone()).request_id,
            id
        );
    }

    #[test]
    fn requests_outside_the_protocol_are_refused() {
        let req = request();
        let mut value = serde_json::to_value(&req).unwrap();
        value["op"] = serde_json::json!({"name": "shell.run", "params": {"cmd": "calc"}});
        let err = HelperRequest::from_json(&value.to_string()).unwrap_err();
        assert_eq!(err.code(), "forbidden", "{err}");

        let mut value = serde_json::to_value(&req).unwrap();
        value["v"] = 2.into();
        let err = HelperRequest::from_json(&value.to_string()).unwrap_err();
        assert_eq!(err.code(), "invalid_input");
        assert!(err.to_string().contains("version 2"), "{err}");

        let mut value = serde_json::to_value(&req).unwrap();
        value.as_object_mut().unwrap().remove("v");
        assert!(HelperRequest::from_json(&value.to_string()).is_err());

        let mut value = serde_json::to_value(&req).unwrap();
        value["user"] = "admin".into();
        assert_eq!(
            HelperRequest::from_json(&value.to_string())
                .unwrap_err()
                .code(),
            "invalid_input"
        );

        let mut value = serde_json::to_value(&req).unwrap();
        value["request_id"] = "not-a-ulid".into();
        assert!(HelperRequest::from_json(&value.to_string()).is_err());

        assert!(HelperRequest::from_json("not json").is_err());
        let huge = format!("{{\"v\":1,\"pad\":\"{}\"}}", "a".repeat(MAX_DOCUMENT_BYTES));
        assert_eq!(
            HelperRequest::from_json(&huge).unwrap_err().code(),
            "invalid_input"
        );
    }

    #[test]
    fn responses_map_back_to_errors() {
        let req = request();
        let ok = HelperResponse::success(&req, "rule added")
            .with_details(serde_json::json!({"rule": "{abc}"}));
        let parsed = HelperResponse::from_json(&ok.to_json().unwrap()).unwrap();
        assert_eq!(parsed, ok);
        assert_eq!(parsed.into_result(&req).unwrap().details["rule"], "{abc}");

        for err in [
            ForgeError::Forbidden("x".into()),
            ForgeError::InvalidInput("x".into()),
            ForgeError::NotFound("x".into()),
            ForgeError::VerificationFailed("x".into()),
            ForgeError::NotImplemented("x".into()),
            ForgeError::UnsupportedPlatform("x".into()),
            ForgeError::Internal("x".into()),
        ] {
            let resp = HelperResponse::failure(req.request_id, &err);
            let json = resp.to_json().unwrap();
            let back = HelperResponse::from_json(&json)
                .unwrap()
                .into_result(&req)
                .unwrap_err();
            assert_eq!(back.code(), err.code());
            assert!(back.to_string().contains("nrpt.add"), "{back}");
        }

        let other = HelperResponse::success(&request(), "x");
        assert_eq!(other.into_result(&req).unwrap_err().code(), "internal");

        let mut odd = HelperResponse::failure(req.request_id, &ForgeError::Internal("x".into()));
        odd.code = Some("brand_new_code".into());
        assert_eq!(odd.into_result(&req).unwrap_err().code(), "internal");

        assert_eq!(
            HelperResponse::from_json("{}").unwrap_err().code(),
            "internal"
        );
        assert_eq!(
            HelperResponse::from_json("garbage").unwrap_err().code(),
            "internal"
        );
    }

    #[test]
    fn exchange_files_live_in_state_locks() {
        let id = RequestId::new();
        let root = Path::new("C:/Forgebox");
        assert_eq!(
            request_path(root, id),
            root.join("state")
                .join("locks")
                .join(format!("helper-{id}.request.json"))
        );
        assert_eq!(
            response_path(root, id)
                .file_name()
                .unwrap()
                .to_str()
                .unwrap(),
            format!("helper-{id}.response.json")
        );
    }
}
