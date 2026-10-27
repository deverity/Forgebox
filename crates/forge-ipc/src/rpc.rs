//! JSON-RPC 2.0 messages as used by Forgebox (ADR-0015).
//!
//! * Request: `{"jsonrpc":"2.0","id":7,"method":"sites.add","request_id":"01J…","params":{…}}`.
//!   `id` correlates the response on this connection; `request_id` (a ULID, extension
//!   member) is the idempotency key that survives retries on new connections and is
//!   **required** (ADR-0009, rule 4).
//! * Response: `{"jsonrpc":"2.0","id":7,"result":…}` or
//!   `{"jsonrpc":"2.0","id":7,"error":{"code":-32000,"message":"…","data":{"code":"not_found","message":"…"}}}`.
//!   `error.data` is always an [`ApiError`] with the stable code.
//! * Notification (server → client, streams): `{"jsonrpc":"2.0","method":"events.event","params":{…}}`.
//!
//! Batches (JSON arrays) are not supported and answered with `invalid_request`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::ApiError;
use forge_core::ids::RequestId;

/// Value of the `jsonrpc` member.
pub const JSONRPC_VERSION: &str = "2.0";

/// Standard and Forgebox JSON-RPC error codes. The numeric code only tells the class;
/// clients match on `error.data.code` ([`ApiError::code`]).
pub mod codes {
    /// Invalid JSON.
    pub const PARSE_ERROR: i64 = -32700;
    /// Not a valid request object (missing `request_id`, batch, wrong `jsonrpc`).
    pub const INVALID_REQUEST: i64 = -32600;
    /// Unknown method name.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Params do not match the method's params type.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Bug in the daemon (code `internal`).
    pub const INTERNAL_ERROR: i64 = -32603;
    /// Any other error reported by a method (see `error.data.code`).
    pub const SERVER_ERROR: i64 = -32000;
    /// Not authenticated (`unauthorized`).
    pub const UNAUTHORIZED: i64 = -32001;
    /// Authenticated but not allowed (`forbidden`).
    pub const FORBIDDEN: i64 = -32003;
}

/// Numeric code for an [`ApiError`] returned by a handler.
pub fn code_for(error: &ApiError) -> i64 {
    match error.code.as_str() {
        "internal" => codes::INTERNAL_ERROR,
        "unauthorized" => codes::UNAUTHORIZED,
        "forbidden" => codes::FORBIDDEN,
        _ => codes::SERVER_ERROR,
    }
}

/// The `error` member of a response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ApiError>,
}

impl RpcErrorObject {
    /// Error object for `error`, with its numeric class.
    pub fn new(code: i64, error: ApiError) -> Self {
        Self {
            code,
            message: error.message.clone(),
            data: Some(error),
        }
    }

    /// The API error, synthesised from the numeric code when the peer sent no `data`.
    pub fn into_api_error(self) -> ApiError {
        match self.data {
            Some(data) => data,
            None => {
                let code = match self.code {
                    codes::PARSE_ERROR | codes::INVALID_REQUEST | codes::INVALID_PARAMS => {
                        "invalid_input"
                    }
                    codes::METHOD_NOT_FOUND => "not_found",
                    codes::UNAUTHORIZED => "unauthorized",
                    codes::FORBIDDEN => "forbidden",
                    _ => "internal",
                };
                ApiError::new(code, self.message)
            }
        }
    }
}

/// A request as sent by the client.
#[derive(Debug, Serialize)]
pub struct OutgoingRequest<'a, P: Serialize> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'a str,
    pub request_id: RequestId,
    pub params: &'a P,
}

/// Serializes a request frame.
pub fn request_frame<P: Serialize>(
    id: u64,
    method: &str,
    request_id: RequestId,
    params: &P,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&OutgoingRequest {
        jsonrpc: JSONRPC_VERSION,
        id,
        method,
        request_id,
        params,
    })
}

/// Serializes a success response frame.
pub fn response_ok(id: &Value, result: &Value) -> String {
    serde_json::json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "result": result }).to_string()
}

/// Serializes an error response frame.
pub fn response_err(id: &Value, error: &RpcErrorObject) -> String {
    serde_json::json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "error": error }).to_string()
}

/// Serializes a notification frame.
pub fn notification_frame<P: Serialize>(
    method: &str,
    params: &P,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&serde_json::json!({
        "jsonrpc": JSONRPC_VERSION,
        "method": method,
        "params": params,
    }))
}

/// A message received by the server.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    Request {
        /// Echoed back in the response (number or string).
        id: Value,
        method: String,
        /// `None` when absent or not a ULID (rejected by the dispatcher).
        request_id: Option<RequestId>,
        request_id_raw: Option<Value>,
        /// `{}` when absent or `null`.
        params: Value,
    },
    /// Client notification (no `id`): ignored, no response (JSON-RPC 2.0 §4.1).
    Notification { method: String },
}

/// Why a frame is not a usable message: the response to send (with `id` when known).
#[derive(Debug, Clone, PartialEq)]
pub struct InvalidMessage {
    pub id: Value,
    pub error: RpcErrorObject,
}

fn invalid(id: Value, code: i64, message: impl Into<String>) -> Box<InvalidMessage> {
    Box::new(InvalidMessage {
        id,
        error: RpcErrorObject::new(code, ApiError::new("invalid_input", message)),
    })
}

/// Parses a frame received by the server.
pub fn parse_client_message(frame: &str) -> Result<ClientMessage, Box<InvalidMessage>> {
    let value: Value = serde_json::from_str(frame).map_err(|e| {
        invalid(
            Value::Null,
            codes::PARSE_ERROR,
            format!("invalid JSON: {e}"),
        )
    })?;
    let Value::Object(mut object) = value else {
        return Err(invalid(
            Value::Null,
            codes::INVALID_REQUEST,
            "expected a JSON-RPC request object (batches are not supported)",
        ));
    };
    let id = object.remove("id");
    let id_for_errors = id.clone().unwrap_or(Value::Null);
    if object.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
        return Err(invalid(
            id_for_errors,
            codes::INVALID_REQUEST,
            "\"jsonrpc\" must be \"2.0\"",
        ));
    }
    let Some(Value::String(method)) = object.remove("method") else {
        return Err(invalid(
            id_for_errors,
            codes::INVALID_REQUEST,
            "\"method\" must be a string",
        ));
    };
    let Some(id) = id else {
        return Ok(ClientMessage::Notification { method });
    };
    if !(id.is_string() || id.is_number()) {
        return Err(invalid(
            Value::Null,
            codes::INVALID_REQUEST,
            "\"id\" must be a string or a number",
        ));
    }
    let request_id_raw = object.remove("request_id");
    let request_id = request_id_raw
        .as_ref()
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<RequestId>().ok());
    let params = match object.remove("params") {
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(params @ (Value::Object(_) | Value::Array(_))) => params,
        Some(_) => {
            return Err(invalid(
                id,
                codes::INVALID_PARAMS,
                "\"params\" must be an object",
            ));
        }
    };
    Ok(ClientMessage::Request {
        id,
        method,
        request_id,
        request_id_raw,
        params,
    })
}

/// A message received by the client.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    Response {
        id: Option<u64>,
        outcome: Result<Value, RpcErrorObject>,
    },
    Notification {
        method: String,
        params: Value,
    },
}

#[derive(Deserialize)]
struct RawServerMessage {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcErrorObject>,
}

/// Parses a frame received by the client.
pub fn parse_server_message(frame: &str) -> Result<ServerMessage, String> {
    let raw: RawServerMessage =
        serde_json::from_str(frame).map_err(|e| format!("invalid frame from server: {e}"))?;
    if let Some(method) = raw.method {
        return Ok(ServerMessage::Notification {
            method,
            params: raw.params.unwrap_or(Value::Null),
        });
    }
    let id = raw.id.as_ref().and_then(Value::as_u64);
    let outcome = match (raw.result, raw.error) {
        (_, Some(error)) => Err(error),
        (Some(result), None) => Ok(result),
        // `"result": null` is deserialized as `None` by serde's Option handling.
        (None, None) => Ok(Value::Null),
    };
    Ok(ServerMessage::Response { id, outcome })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_requests_and_defaults_params() {
        let rid = RequestId::new();
        let frame =
            format!(r#"{{"jsonrpc":"2.0","id":1,"method":"sites.list","request_id":"{rid}"}}"#);
        match parse_client_message(&frame).unwrap() {
            ClientMessage::Request {
                id,
                method,
                request_id,
                params,
                ..
            } => {
                assert_eq!(id, json!(1));
                assert_eq!(method, "sites.list");
                assert_eq!(request_id, Some(rid));
                assert_eq!(params, json!({}));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rejects_bad_frames_with_proper_codes() {
        let err = parse_client_message("{nope").unwrap_err();
        assert_eq!(err.error.code, codes::PARSE_ERROR);
        let err = parse_client_message("[]").unwrap_err();
        assert_eq!(err.error.code, codes::INVALID_REQUEST);
        let err = parse_client_message(r#"{"jsonrpc":"1.0","id":3,"method":"x"}"#).unwrap_err();
        assert_eq!((err.id, err.error.code), (json!(3), codes::INVALID_REQUEST));
        let err = parse_client_message(r#"{"jsonrpc":"2.0","id":3,"method":"x","params":5}"#)
            .unwrap_err();
        assert_eq!(err.error.code, codes::INVALID_PARAMS);
        assert_eq!(err.error.data.unwrap().code, "invalid_input");
    }

    #[test]
    fn notifications_have_no_id() {
        assert_eq!(
            parse_client_message(r#"{"jsonrpc":"2.0","method":"x"}"#).unwrap(),
            ClientMessage::Notification { method: "x".into() }
        );
    }

    #[test]
    fn server_messages_round_trip() {
        let ok = response_ok(&json!(4), &json!({"a": 1}));
        assert_eq!(
            parse_server_message(&ok).unwrap(),
            ServerMessage::Response {
                id: Some(4),
                outcome: Ok(json!({"a": 1}))
            }
        );
        let err = RpcErrorObject::new(codes::SERVER_ERROR, ApiError::new("not_found", "x"));
        let frame = response_err(&json!(5), &err);
        match parse_server_message(&frame).unwrap() {
            ServerMessage::Response {
                id: Some(5),
                outcome: Err(e),
            } => assert_eq!(e.into_api_error().code, "not_found"),
            other => panic!("{other:?}"),
        }
        let note = notification_frame("events.event", &json!({"seq": 1})).unwrap();
        assert!(matches!(
            parse_server_message(&note).unwrap(),
            ServerMessage::Notification { method, .. } if method == "events.event"
        ));
        let null = response_ok(&json!(6), &Value::Null);
        assert!(matches!(
            parse_server_message(&null).unwrap(),
            ServerMessage::Response {
                id: Some(6),
                outcome: Ok(Value::Null)
            }
        ));
    }

    #[test]
    fn request_frame_has_request_id_member() {
        let rid = RequestId::new();
        let frame = request_frame(9, "system.ping", rid, &json!({})).unwrap();
        let value: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(value["request_id"], rid.to_string());
        assert_eq!(value["jsonrpc"], "2.0");
    }

    #[test]
    fn missing_data_is_synthesised() {
        let err = RpcErrorObject {
            code: codes::METHOD_NOT_FOUND,
            message: "nope".into(),
            data: None,
        };
        assert_eq!(err.into_api_error().code, "not_found");
    }
}
