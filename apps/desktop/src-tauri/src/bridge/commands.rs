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
//! | `first_run_state` | `context?` | [`FirstRunState`] |
//! | `choose_root` | `path` | [`ChooseRootAnswer`] |
//! | `onboarding_get` | `context?` | [`OnboardingEntry`] |
//! | `onboarding_set` | `context?, patch` | [`OnboardingEntry`] |
//!
//! Errors are always `ApiError {code, message, details}` ([`super::errors`]). State changes
//! are pushed as the [`CONNECTION_EVENT`] (`forge://connection`) event.

use std::sync::Arc;

use serde_json::Value;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, State, Webview};

use forge_ipc::ApiError;
use forge_ipc::base::paths::DiscoverInputs;

use crate::onboarding::{
    self, ChooseRootAnswer, FirstRunState, OnboardingEntry, OnboardingPatch, OnboardingStore,
};

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
        .subscribe(
            &context,
            &method,
            params,
            Arc::new(on_frame),
            webview.label(),
        )
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

/// First-run facts for `context` (default `local`): the root and where it comes from,
/// whether a daemon initialized it, whether the daemon answers, and the stored onboarding
/// progress. No domain logic: see [`onboarding::first_run`].
#[tauri::command(rename_all = "snake_case")]
pub async fn first_run_state(
    bridge: State<'_, Bridge>,
    store: State<'_, Arc<OnboardingStore>>,
    context: Option<String>,
) -> Result<FirstRunState, ApiError> {
    let context = context.unwrap_or_else(|| super::LOCAL_CONTEXT.to_owned());
    onboarding::first_run(
        &bridge,
        store.file().to_path_buf(),
        &context,
        DiscoverInputs::from_env(None),
    )
    .await
}

/// Onboarding step 1: validates `path`, writes `root.toml`, reconnects the `local` context
/// to the new root (starting its daemon, which creates the layout). Refusals are
/// `invalid_input` with a `details.reason` ([`onboarding::RootRejection`]).
#[tauri::command(rename_all = "snake_case")]
pub async fn choose_root(
    bridge: State<'_, Bridge>,
    path: String,
) -> Result<ChooseRootAnswer, ApiError> {
    onboarding::choose_root(&bridge, &path, DiscoverInputs::from_env(None)).await
}

/// Stored onboarding progress of `context` (default `local`, keyed by its root).
#[tauri::command(rename_all = "snake_case")]
pub async fn onboarding_get(
    store: State<'_, Arc<OnboardingStore>>,
    context: Option<String>,
) -> Result<OnboardingEntry, ApiError> {
    let context = context.unwrap_or_else(|| super::LOCAL_CONTEXT.to_owned());
    let store = Arc::clone(&store);
    tokio::task::spawn_blocking(move || {
        let key = onboarding::key_for(&context, &DiscoverInputs::from_env(None))?;
        Ok(store.get(&key))
    })
    .await
    .map_err(|e| ApiError::new("internal", format!("onboarding_get: {e}")))?
}

/// Merges `patch` (`step`, `choices`, `done`) into the progress of `context`.
#[tauri::command(rename_all = "snake_case")]
pub async fn onboarding_set(
    store: State<'_, Arc<OnboardingStore>>,
    context: Option<String>,
    patch: OnboardingPatch,
) -> Result<OnboardingEntry, ApiError> {
    let context = context.unwrap_or_else(|| super::LOCAL_CONTEXT.to_owned());
    let store = Arc::clone(&store);
    tokio::task::spawn_blocking(move || {
        let key = onboarding::key_for(&context, &DiscoverInputs::from_env(None))?;
        store.update(&key, patch)
    })
    .await
    .map_err(|e| ApiError::new("internal", format!("onboarding_set: {e}")))?
}
