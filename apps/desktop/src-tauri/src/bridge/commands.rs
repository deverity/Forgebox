//! The bridge's Tauri commands. All `async` (they run on Tauri's tokio runtime, never on
//! the UI thread) and thin: each one forwards to [`Bridge`]. Argument names are snake_case
//! on the JS side too (`rename_all = "snake_case"`), like every DTO of the API.
//!
//! | command | args | answer |
//! |---|---|---|
//! | `ipc_state` | `context` | [`ContextStateDto`] |
//! | `ipc_call` | `context, method, params?, request_id?, timeout_ms?` | the method's result |
//! | `ipc_subscribe` | `context, method, params?, on_frame: Channel<StreamFrame>` | [`SubscribeAnswer`] |
//! | `ipc_ack` | `stream_id, frame` | — |
//! | `ipc_unsubscribe` | `context, stream_id` | — |
//! | `ipc_start` | `context, initialize?` | [`ContextStateDto`] |
//!
//! Errors are always `ApiError {code, message, details}` ([`super::errors`]). State changes
//! are pushed as the [`CONNECTION_EVENT`] (`forge://connection`) event.

use std::sync::Arc;

use serde_json::Value;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, State, Webview};

use forge_ipc::ApiError;

use super::{
    Bridge, CONNECTION_EVENT, CallArgs, ConnectionSink, ContextStateDto, StreamFrame,
    SubscribeAnswer,
};

/// Emits [`CONNECTION_EVENT`] to every webview of the app.
pub struct TauriConnectionSink(pub AppHandle);

impl ConnectionSink for TauriConnectionSink {
    fn connection_changed(&self, state: &ContextStateDto) {
        if let Err(err) = self.0.emit(CONNECTION_EVENT, state) {
            tracing::debug!(%err, "forge://connection not delivered");
        }
    }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn ipc_state(
    bridge: State<'_, Bridge>,
    context: String,
) -> Result<ContextStateDto, ApiError> {
    bridge.state(&context).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn ipc_call(
    bridge: State<'_, Bridge>,
    context: String,
    method: String,
    params: Option<Value>,
    request_id: Option<String>,
    timeout_ms: Option<u64>,
) -> Result<Value, ApiError> {
    bridge
        .call(CallArgs {
            context,
            method,
            params,
            request_id,
            timeout_ms,
        })
        .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn ipc_subscribe(
    bridge: State<'_, Bridge>,
    webview: Webview,
    context: String,
    method: String,
    params: Option<Value>,
    on_frame: Channel<StreamFrame>,
) -> Result<SubscribeAnswer, ApiError> {
    bridge
        .subscribe(&context, &method, params, Arc::new(on_frame), webview.label())
        .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn ipc_ack(
    bridge: State<'_, Bridge>,
    stream_id: String,
    frame: u64,
) -> Result<(), ApiError> {
    bridge.ack(&stream_id, frame);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn ipc_unsubscribe(
    bridge: State<'_, Bridge>,
    context: String,
    stream_id: String,
) -> Result<(), ApiError> {
    bridge.unsubscribe(&context, &stream_id)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn ipc_start(
    bridge: State<'_, Bridge>,
    context: String,
    initialize: Option<bool>,
) -> Result<ContextStateDto, ApiError> {
    bridge.start(&context, initialize.unwrap_or(false)).await
}
