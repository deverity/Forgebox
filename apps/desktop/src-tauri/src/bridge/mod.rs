//! The IPC bridge between the webview and the daemons ([ADR-0034]).
//!
//! The webview never talks to a daemon and never sees the API token: it invokes a handful
//! of generic Tauri commands ([`commands`]) and this module forwards them through
//! forge-ipc. There is no per-method Rust code: any method of `forge_ipc::METHODS` goes
//! through [`Bridge::call`] / [`Bridge::subscribe`] after validation against the table.
//!
//! * [`contexts`]: the `Contexts` actor, one long-lived `Client` per context id (`local`
//!   in M2), lazy opening, the connection state and the daemon auto-start policy
//!   ([`policy`]).
//! * [`streams`]: `ipc_subscribe` pumps (frames of ≤ 16 ms / ≤ 256 entries, a window of
//!   acknowledged frames, `lagged`/`reset`/`end` entries) and their registry.
//! * [`errors`]: every failure as `ApiError {code, message, details}`.
//!
//! [`Bridge`] is the facade the commands (and Rust consumers such as the tray model)
//! use. Every operation is async; nothing blocks the caller's thread.
//!
//! [ADR-0034]: ../../../../docs/adr/0034-desktop-shell-architecture.md

pub mod commands;
pub mod contexts;
pub mod errors;
pub mod policy;
pub mod streams;

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;

use forge_ipc::api::{MethodInfo, MethodKind, method_info};
use forge_ipc::base::actor::{ActorHandle, ActorSpec, ActorSupervisor};
use forge_ipc::base::event::EventBus;
use forge_ipc::base::ids::RequestId;
use forge_ipc::client::local;
use forge_ipc::client::{CallOptions, Client, ConnectionState, Subscription};
use forge_ipc::{ApiError, METHODS};

pub use self::contexts::{
    AutoStartDto, ConnectionSink, ConnectionStatus, Connector, ContextStateDto, ContextsOptions,
    Endpoint, ExitEvidence, LOCAL_CONTEXT, LocalConnector, NoSink, Opened, desktop_client_options,
};
pub use self::policy::{AutoStartPolicy, Decision, PolicyConfig, StartOutcome};
pub use self::streams::{FrameSink, PumpOptions, SinkClosed, StreamEntry, StreamFrame};

use self::contexts::{Contexts, Emitter, Msg};
use self::errors::{ask_error, client_error};
use self::streams::{PumpControl, PumpEnd, Streams};

/// Tauri event carrying every [`ContextStateDto`] change.
pub const CONNECTION_EVENT: &str = "forge://connection";

/// Tuning of the bridge.
#[derive(Debug, Clone)]
pub struct BridgeOptions {
    pub contexts: ContextsOptions,
    pub pump: PumpOptions,
    /// Deadline of a question to the `Contexts` actor.
    pub ask_timeout: Duration,
    /// Upper bound of `timeout_ms` (jobs answer at once; `fs.*` takes up to 5 s).
    pub max_call_timeout: Duration,
}

impl Default for BridgeOptions {
    fn default() -> Self {
        Self {
            contexts: ContextsOptions::default(),
            pump: PumpOptions::default(),
            ask_timeout: Duration::from_secs(2),
            max_call_timeout: Duration::from_secs(120),
        }
    }
}

/// Arguments of `ipc_call`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CallArgs {
    pub context: String,
    pub method: String,
    /// An object; `null`/absent = `{}`.
    #[serde(default)]
    pub params: Option<Value>,
    /// ULID chosen per user intent and reused on retries (ADR-0009, rule 4); generated
    /// when absent.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Deadline in milliseconds (default: the client's, 10 s).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Answer of `ipc_subscribe`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscribeAnswer {
    /// Bridge stream id (frames carry it; `ipc_ack` / `ipc_unsubscribe` take it).
    pub stream_id: String,
    pub method: String,
    /// Where the daemon-side subscription would resume (`None` while pending).
    pub resume_point: Option<u64>,
    /// The daemon could not be reached: the stream subscribes on the next connection.
    pub pending: bool,
}

/// The bridge: the `Contexts` actor plus the stream registry. Cheap to clone.
#[derive(Clone)]
pub struct Bridge {
    inner: Arc<Inner>,
}

struct Inner {
    contexts: ActorHandle<Msg>,
    _supervisor: ActorSupervisor,
    streams: Arc<Streams>,
    states: broadcast::Sender<ContextStateDto>,
    options: BridgeOptions,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bridge")
            .field("streams", &self.inner.streams.len())
            .finish_non_exhaustive()
    }
}

impl Bridge {
    /// The app's bridge: the `local` context with root discovery. Needs a tokio runtime
    /// (Tauri's).
    pub fn new(sink: Arc<dyn ConnectionSink>, options: BridgeOptions) -> Self {
        let local: Arc<dyn Connector> =
            Arc::new(LocalConnector::discover(desktop_client_options()));
        Self::with_connectors(vec![(LOCAL_CONTEXT.to_owned(), local)], sink, options)
    }

    /// A bridge over explicit connectors (tests, and M6's remote profiles).
    pub fn with_connectors(
        connectors: Vec<(String, Arc<dyn Connector>)>,
        sink: Arc<dyn ConnectionSink>,
        options: BridgeOptions,
    ) -> Self {
        let (states, _) = broadcast::channel(64);
        let emitter = Emitter::new(sink, states.clone());
        // The actor's own panics are isolated and it is rebuilt (ADR-0011); the bus only
        // carries `actor.*` events nobody reads in the GUI.
        let supervisor = ActorSupervisor::new(EventBus::new());
        let contexts_options = options.contexts.clone();
        let contexts = supervisor.spawn(ActorSpec::new("desktop.contexts"), move || {
            Contexts::new(&connectors, emitter.clone(), contexts_options.clone())
        });
        Self {
            inner: Arc::new(Inner {
                contexts,
                _supervisor: supervisor,
                streams: Arc::new(Streams::default()),
                states,
                options,
            }),
        }
    }

    async fn ask<R>(
        &self,
        make: impl FnOnce(forge_ipc::base::actor::Reply<R>) -> Msg,
    ) -> Result<R, ApiError> {
        self.inner
            .contexts
            .actor_ref()
            .ask(make, self.inner.options.ask_timeout)
            .await
            .map_err(|err| ask_error(&err))
    }

    /// State of `context` (opens it on first use).
    pub async fn state(&self, context: &str) -> Result<ContextStateDto, ApiError> {
        let context = context.to_owned();
        self.ask(|reply| Msg::State { context, reply }).await?
    }

    /// Every state change, for Rust consumers (the tray model).
    pub fn watch_states(&self) -> broadcast::Receiver<ContextStateDto> {
        self.inner.states.subscribe()
    }

    /// The long-lived client of `context` (opens it on first use; it may not be connected).
    pub async fn client(&self, context: &str) -> Result<Client, ApiError> {
        let context = context.to_owned();
        self.ask(|reply| Msg::Client { context, reply }).await?
    }

    /// `ipc_start`: the user wants the daemon of `context` running. Refused on a root that
    /// was never initialized unless `initialize` (onboarding finished).
    pub async fn start(
        &self,
        context: &str,
        initialize: bool,
    ) -> Result<ContextStateDto, ApiError> {
        let context = context.to_owned();
        let ask = self
            .inner
            .contexts
            .actor_ref()
            .ask(
                |reply| Msg::Start {
                    context,
                    initialize,
                    reply,
                },
                // Starting waits for the daemon to answer (up to 10 s, ADR-0030).
                local::AUTOSTART_TIMEOUT + self.inner.options.ask_timeout * 2,
            )
            .await;
        ask.map_err(|err| ask_error(&err))?
    }

    /// `ipc_call`: one API call, validated against `METHODS` first.
    pub async fn call(&self, args: CallArgs) -> Result<Value, ApiError> {
        let info = validate_call(&args.method)?;
        let params = object_params(args.params, info.name)?;
        let request_id = match args.request_id.as_deref() {
            Some(text) => RequestId::from_str(text).map_err(|_| {
                ApiError::new(
                    "invalid_input",
                    format!("{text:?} is not a ULID request_id"),
                )
                .with_reason("invalid_request_id", json!({ "request_id": text }))
            })?,
            None => RequestId::new(),
        };
        let timeout = args
            .timeout_ms
            .map(|ms| Duration::from_millis(ms.max(1)).min(self.inner.options.max_call_timeout));
        let client = self.client(&args.context).await?;
        client
            .call_raw(
                info.name,
                params,
                CallOptions {
                    request_id: Some(request_id),
                    timeout,
                },
            )
            .await
            .map_err(|err| client_error(&err, Some(info.name)))
    }

    /// `ipc_subscribe`: opens a stream of `method` whose frames go to `sink`. `owner` is
    /// the webview label (its streams close when its page reloads).
    pub async fn subscribe(
        &self,
        context: &str,
        method: &str,
        params: Option<Value>,
        sink: Arc<dyn FrameSink>,
        owner: &str,
    ) -> Result<SubscribeAnswer, ApiError> {
        let info = validate_stream(method)?;
        let params = object_params(params, info.name)?;
        let client = self.client(context).await?;
        let first = match client.subscribe_raw(info.name, params.clone()).await {
            Ok(subscription) => Some(subscription),
            // Nobody answers now: subscribe on the next connection.
            Err(err) if local::classify(err.clone()).is_down() => None,
            Err(err) => return Err(client_error(&err, Some(info.name))),
        };
        let (stream_id, control) = self.inner.streams.register(context, owner);
        let answer = SubscribeAnswer {
            stream_id: stream_id.clone(),
            method: info.name.to_owned(),
            resume_point: first.as_ref().and_then(Subscription::resume_point),
            pending: first.is_none(),
        };
        tokio::spawn(run_stream(StreamTask {
            client,
            method: info.name,
            params,
            first,
            stream_id,
            sink,
            control,
            options: self.inner.options.pump,
            streams: Arc::clone(&self.inner.streams),
        }));
        Ok(answer)
    }

    /// `ipc_unsubscribe`.
    pub fn unsubscribe(&self, context: &str, stream_id: &str) -> Result<(), ApiError> {
        if self.inner.streams.close(context, stream_id) {
            Ok(())
        } else {
            Err(
                ApiError::new("not_found", format!("no stream {stream_id} in {context}"))
                    .with_reason("unknown_stream", json!({ "stream_id": stream_id })),
            )
        }
    }

    /// `ipc_ack`: the webview handled every frame of `stream_id` up to `frame`.
    pub fn ack(&self, stream_id: &str, frame: u64) {
        self.inner.streams.ack(stream_id, frame);
    }

    /// Closes the streams of the webview `owner` (its page is loading again).
    pub fn close_streams_of(&self, owner: &str) -> usize {
        self.inner.streams.close_owner(owner)
    }

    /// Open streams.
    pub fn stream_count(&self) -> usize {
        self.inner.streams.len()
    }

    /// Closes every stream and context (app exit, tests).
    pub async fn shutdown(&self) {
        self.inner.streams.close_all();
        let _ = self.ask(|reply| Msg::Close { reply }).await;
    }
}

/// `method` must be in `METHODS`, not a stream (→ `ipc_subscribe`) and not the
/// unsubscribe of a stream group (→ `ipc_unsubscribe`: daemon-side ids change across
/// reconnections and belong to the client).
pub fn validate_call(method: &str) -> Result<&'static MethodInfo, ApiError> {
    let info = known(method)?;
    if info.kind == MethodKind::Stream {
        return Err(ApiError::new(
            "invalid_input",
            format!("{method} is a stream: use ipc_subscribe"),
        )
        .with_reason("use_subscribe", json!({ "method": method })));
    }
    let stream_group = METHODS
        .iter()
        .any(|m| m.kind == MethodKind::Stream && m.group() == info.group());
    if stream_group && info.action() == "unsubscribe" {
        return Err(ApiError::new(
            "invalid_input",
            format!("{method} is managed by the bridge: use ipc_unsubscribe"),
        )
        .with_reason("use_unsubscribe", json!({ "method": method })));
    }
    Ok(info)
}

/// `method` must be a stream of `METHODS`.
pub fn validate_stream(method: &str) -> Result<&'static MethodInfo, ApiError> {
    let info = known(method)?;
    if info.kind != MethodKind::Stream {
        return Err(
            ApiError::new("invalid_input", format!("{method} is not a stream method"))
                .with_reason("not_a_stream", json!({ "method": method })),
        );
    }
    Ok(info)
}

fn known(method: &str) -> Result<&'static MethodInfo, ApiError> {
    method_info(method).ok_or_else(|| {
        // Same code the daemon answers for an unknown method.
        ApiError::new(
            "not_found",
            format!("{method:?} is not a method of the API"),
        )
        .with_reason("unknown_method", json!({ "method": method }))
    })
}

fn object_params(params: Option<Value>, method: &str) -> Result<Value, ApiError> {
    match params {
        None | Some(Value::Null) => Ok(Value::Object(Map::new())),
        Some(params @ Value::Object(_)) => Ok(params),
        Some(_) => Err(ApiError::new(
            "invalid_input",
            format!("the params of {method} must be an object"),
        )
        .with_reason("params_not_object", json!({ "method": method }))),
    }
}

struct StreamTask {
    client: Client,
    method: &'static str,
    params: Value,
    first: Option<Subscription<Value>>,
    stream_id: String,
    sink: Arc<dyn FrameSink>,
    control: PumpControl,
    options: PumpOptions,
    streams: Arc<Streams>,
}

async fn run_stream(task: StreamTask) {
    let StreamTask {
        client,
        method,
        params,
        first,
        stream_id,
        sink,
        mut control,
        options,
        streams,
    } = task;
    let subscription = match first {
        Some(subscription) => Some(subscription),
        None => match subscribe_when_connected(&client, method, &params, &control).await {
            Ok(subscription) => Some(subscription),
            Err(error) => {
                // `None`: cancelled before it ever subscribed; the webview knows.
                if let Some(error) = error {
                    streams::send_end(sink.as_ref(), &stream_id, &mut control, Some(error));
                }
                None
            }
        },
    };
    if let Some(mut subscription) = subscription {
        let end = streams::pump(
            &mut subscription,
            &stream_id,
            sink.as_ref(),
            &mut control,
            &options,
        )
        .await;
        if end != PumpEnd::SourceEnded {
            let _ =
                tokio::time::timeout(options.unsubscribe_timeout, subscription.unsubscribe()).await;
        }
    }
    streams.forget(&stream_id);
}

/// Subscribes once the client is connected; retries on the next connection while the
/// daemon cannot be reached. `Err(None)`: cancelled; `Err(Some(e))`: refused or closed.
async fn subscribe_when_connected(
    client: &Client,
    method: &'static str,
    params: &Value,
    control: &PumpControl,
) -> Result<Subscription<Value>, Option<ApiError>> {
    let mut state = client.state();
    loop {
        loop {
            let now = state.borrow_and_update().clone();
            if now == ConnectionState::Closed {
                return Err(Some(client_error(
                    &forge_ipc::ClientError::Closed,
                    Some(method),
                )));
            }
            if now.is_connected() {
                break;
            }
            tokio::select! {
                () = control.cancel.cancelled() => return Err(None),
                changed = state.changed() => {
                    if changed.is_err() {
                        return Err(Some(client_error(&forge_ipc::ClientError::Closed, Some(method))));
                    }
                }
            }
        }
        match client.subscribe_raw(method, params.clone()).await {
            Ok(subscription) => return Ok(subscription),
            Err(err) if local::classify(err.clone()).is_down() => {
                tokio::select! {
                    () = control.cancel.cancelled() => return Err(None),
                    () = tokio::time::sleep(Duration::from_millis(250)) => {}
                }
            }
            Err(err) => return Err(Some(client_error(&err, Some(method)))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_are_validated_against_the_method_table() {
        assert_eq!(validate_call("sites.list").unwrap().name, "sites.list");
        assert_eq!(
            validate_call("daemon.doctor").unwrap().name,
            "daemon.doctor"
        );
        let unknown = validate_call("nope.nope").unwrap_err();
        assert_eq!(
            (unknown.code.as_str(), unknown.reason()),
            ("not_found", Some("unknown_method"))
        );
        let stream = validate_call("events.subscribe").unwrap_err();
        assert_eq!(stream.reason(), Some("use_subscribe"));
        for managed in [
            "events.unsubscribe",
            "logs.unsubscribe",
            "metrics.unsubscribe",
        ] {
            assert_eq!(
                validate_call(managed).unwrap_err().reason(),
                Some("use_unsubscribe"),
                "{managed}"
            );
        }
        assert_eq!(
            validate_stream("sites.list").unwrap_err().reason(),
            Some("not_a_stream")
        );
        assert_eq!(
            validate_stream("logs.subscribe").unwrap().name,
            "logs.subscribe"
        );
    }

    #[test]
    fn params_must_be_objects() {
        assert_eq!(object_params(None, "x").unwrap(), json!({}));
        assert_eq!(object_params(Some(Value::Null), "x").unwrap(), json!({}));
        assert_eq!(
            object_params(Some(json!({"a": 1})), "x").unwrap(),
            json!({"a": 1})
        );
        let err = object_params(Some(json!([1])), "x").unwrap_err();
        assert_eq!(err.reason(), Some("params_not_object"));
    }
}
