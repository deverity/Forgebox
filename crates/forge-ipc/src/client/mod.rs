//! Typed API client used by the CLI, the GUI (through Tauri commands) and the MCP server.
//!
//! * Transport-agnostic ([`Transport`]); connects lazily and reconnects on demand.
//! * One Rust method per API method, generated from the method table
//!   ([`crate::api::methods`]), e.g. `client.sites_add(params)`; generic
//!   [`Client::call`] for the rest; untyped [`Client::call_raw`] and
//!   [`Client::subscribe_raw`] for bridges that forward JSON (the desktop app).
//! * Every call carries a fresh `request_id`; when the connection drops before the
//!   answer, the call is retried on a new connection **with the same `request_id`**, so
//!   the daemon does not act twice (ADR-0009, rule 4).
//! * Streams ([`Client::subscribe`], [`Client::subscribe_raw`],
//!   [`Client::subscribe_events`]) resume after a reconnection from the last `seq`
//!   received, or from where they were taken when nothing arrived yet ([`subscription`]).
//! * The connection state is observable ([`Client::state`], a `watch` of
//!   [`ConnectionState`]). A maintenance task reports a lost connection and, while a
//!   subscription is open or [`ClientOptions::keep_connected`] is set, reconnects with
//!   backoff.
//! * Credentials come from a [`CredentialsProvider`] asked again on every connection
//!   attempt: a token written after the client was created (first boot of the daemon, a
//!   restarted daemon with a new token) is picked up by the next reconnection.
//! * [`local`]: the local daemon of a root, with the auto-start shared by `forge` and
//!   `Forgebox.exe` (ADR-0030).
//!
//! Locks: `slot` and `subs` are `OrderedMutex`es of rank `ipc.client`, never nested and
//! never held across `.await` (ADR-0014); the state `watch` is never written while one
//! of them is held.

mod conn;
pub mod local;
mod methods;
mod subscription;

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::{BoxFuture, Shared as SharedFuture};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use forge_core::ids::{RequestId, SubscriptionId};
use forge_core::paths::ForgeRoot;
use forge_core::sync::{LockLevel, OrderedMutex};

use self::conn::Conn;
use self::subscription::{SubShared, unsubscribe_method};
pub use self::subscription::{Subscription, SubscriptionItem};
use crate::api::{
    API_VERSION, ApiError, ApiMethod, ClientInfoDto, EventEnvelope, EventFilter,
    EventsSubscribeParams, HelloDto, HelloParams, MethodInfo, MethodKind, NoParams, StreamMethod,
    UnsubscribeParams,
};
use crate::auth::IpcToken;
use crate::transport::{ConnectionInfo, Transport, TransportError};

/// Failure of a client call.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ClientError {
    /// The daemon answered with an error (stable `code`).
    #[error("{0}")]
    Api(ApiError),
    /// The daemon could not be reached.
    #[error(transparent)]
    Transport(TransportError),
    /// The connection dropped before the answer (after the configured retries).
    #[error("the connection to the daemon was lost")]
    Disconnected,
    /// No answer in time.
    #[error("no answer from the daemon within {0:?}")]
    Timeout(Duration),
    /// The daemon sent something this client does not understand.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// [`Client::close`] was called.
    #[error("the client is closed")]
    Closed,
    /// The [`CredentialsProvider`] failed; e.g. `not_found`: there is no
    /// `state/ipc.token` yet (the daemon never ran on this root).
    #[error("credentials: {0}")]
    Credentials(ApiError),
    /// Refused by the client before anything was sent (a method that is not a stream
    /// given to [`Client::subscribe_raw`], params that are not an object).
    #[error("{0}")]
    InvalidInput(String),
}

impl ClientError {
    /// Stable error code (`forge --json`, exit codes).
    pub fn code(&self) -> &str {
        match self {
            ClientError::Api(error) | ClientError::Credentials(error) => &error.code,
            ClientError::Transport(error) => error.code(),
            ClientError::Disconnected => "io",
            ClientError::Timeout(_) => "timeout",
            ClientError::Protocol(_) => "internal",
            ClientError::Closed => "cancelled",
            ClientError::InvalidInput(_) => "invalid_input",
        }
    }

    /// CLI exit code (docs/09).
    pub fn exit_code(&self) -> i32 {
        forge_core::error::exit_code_for(self.code())
    }

    /// The API error, when the daemon answered with one.
    pub fn api(&self) -> Option<&ApiError> {
        match self {
            ClientError::Api(error) => Some(error),
            _ => None,
        }
    }
}

/// Who the client is and its secret.
#[derive(Clone)]
pub struct Credentials {
    pub token: IpcToken,
    pub client: ClientInfoDto,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

impl Credentials {
    pub fn new(token: IpcToken, name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            token,
            client: ClientInfoDto {
                name: name.into(),
                version: version.into(),
            },
        }
    }

    /// Reads `state/ipc.token` of a local installation. Blocking.
    pub fn local(
        root: &ForgeRoot,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> forge_core::Result<Self> {
        Ok(Self::new(IpcToken::load(&root.ipc_token())?, name, version))
    }

    fn hello(&self) -> HelloParams {
        HelloParams {
            token: self.token.secret().to_owned(),
            client: self.client.clone(),
            api_version: API_VERSION,
        }
    }
}

type CredentialsFuture = BoxFuture<'static, Result<Credentials, ClientError>>;

/// Where a [`Client`] gets its [`Credentials`]: asked once per connection attempt and
/// never cached by the client, so a token that appears or changes later is used by the
/// next reconnection. Cheap to clone.
#[derive(Clone)]
pub struct CredentialsProvider {
    get: Arc<dyn Fn() -> CredentialsFuture + Send + Sync>,
}

impl std::fmt::Debug for CredentialsProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialsProvider")
            .finish_non_exhaustive()
    }
}

impl CredentialsProvider {
    /// Provider from an async closure (it runs on the client's runtime: blocking reads
    /// belong in `spawn_blocking`).
    pub fn new<F, Fut>(provide: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Credentials, ClientError>> + Send + 'static,
    {
        Self {
            get: Arc::new(move || provide().boxed()),
        }
    }

    /// Always the same credentials.
    pub fn fixed(credentials: Credentials) -> Self {
        Self::new(move || {
            let credentials = credentials.clone();
            async move { Ok(credentials) }
        })
    }

    /// Reads `state/ipc.token` of `root` (in `spawn_blocking`) on every connection
    /// attempt. A missing file is [`ClientError::Credentials`] with code `not_found`.
    pub fn local(root: &ForgeRoot, name: impl Into<String>, version: impl Into<String>) -> Self {
        let path = root.ipc_token();
        let client = ClientInfoDto {
            name: name.into(),
            version: version.into(),
        };
        Self::new(move || {
            let path = path.clone();
            let client = client.clone();
            async move {
                let token = tokio::task::spawn_blocking(move || IpcToken::load(&path))
                    .await
                    .map_err(|e| ClientError::Protocol(format!("token reader: {e}")))?
                    .map_err(|e| ClientError::Credentials(ApiError::from(&e)))?;
                Ok(Credentials { token, client })
            }
        })
    }

    /// The credentials for one connection attempt.
    pub async fn get(&self) -> Result<Credentials, ClientError> {
        (self.get)().await
    }
}

impl From<Credentials> for CredentialsProvider {
    fn from(credentials: Credentials) -> Self {
        Self::fixed(credentials)
    }
}

/// State of the connection of a [`Client`], observed through [`Client::state`].
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    /// Not connected and nobody needs a connection now; the next call connects.
    Idle,
    /// Connecting from [`ConnectionState::Idle`] (the first connection, or a call after
    /// a loss nobody was waiting on).
    Connecting,
    /// Authenticated. `generation` grows with every new connection.
    Connected {
        hello: Box<HelloDto>,
        generation: u64,
    },
    /// Not connected, and the client is getting the connection back by itself (open
    /// subscriptions, or [`ClientOptions::keep_connected`]). `attempt` counts the failed
    /// attempts so far (0: none yet); the next one starts in `next_retry_ms` (0: now or
    /// in progress); `last_error` is why the connection was lost or why the last attempt
    /// failed.
    Reconnecting {
        attempt: u32,
        next_retry_ms: u64,
        last_error: ClientError,
    },
    /// [`Client::close`] was called, or every handle of the client was dropped. Final.
    Closed,
}

impl ConnectionState {
    pub fn is_connected(&self) -> bool {
        matches!(self, ConnectionState::Connected { .. })
    }
}

/// Tuning of a [`Client`].
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// Deadline of a call, network included (the daemon answers within ~1 s).
    pub call_timeout: Duration,
    /// Deadline of connecting plus `system.hello` (and, separately, of getting the
    /// credentials).
    pub connect_timeout: Duration,
    /// Retries (same `request_id`) when the connection drops before the answer.
    pub retries: u32,
    /// Reconnection backoff.
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
    /// Items buffered per subscription before the oldest are dropped.
    pub stream_capacity: usize,
    /// Reconnect whenever the connection is lost or an attempt fails, even without open
    /// subscriptions. Long-lived clients (the desktop app) set it; one-shot clients (the
    /// CLI) leave it off and reconnect on demand.
    pub keep_connected: bool,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            call_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(5),
            retries: 1,
            reconnect_min: Duration::from_millis(50),
            reconnect_max: Duration::from_secs(5),
            stream_capacity: 4096,
            keep_connected: false,
        }
    }
}

/// Per-call options.
#[derive(Debug, Clone, Default)]
pub struct CallOptions {
    /// Idempotency key; generated when absent. Reuse it to retry an operation whose
    /// outcome is unknown.
    pub request_id: Option<RequestId>,
    /// Overrides [`ClientOptions::call_timeout`].
    pub timeout: Option<Duration>,
}

type ConnectFuture = SharedFuture<BoxFuture<'static, Result<Arc<Conn>, ClientError>>>;

enum Slot {
    Idle,
    Connecting {
        generation: u64,
        future: ConnectFuture,
    },
    Ready(Arc<Conn>),
}

pub(crate) struct Shared {
    transport: Arc<dyn Transport>,
    credentials: CredentialsProvider,
    options: ClientOptions,
    /// Lock: `ipc.client` (rank 10), never nested with `subs`.
    slot: OrderedMutex<Slot>,
    /// Lock: `ipc.client` (rank 10), never nested with `slot`.
    subs: OrderedMutex<HashMap<u64, Arc<SubShared>>>,
    /// Written only while neither `slot` nor `subs` is held.
    state: watch::Sender<ConnectionState>,
    generation: AtomicU64,
    next_sub: AtomicU64,
    /// Last `system.ping` round trip in microseconds (0: none yet).
    last_rtt_us: AtomicU64,
    /// Wakes the maintenance task: an authenticated connection was lost, or a connection
    /// attempt failed while a connection is wanted.
    wake: Arc<Notify>,
    closed: CancellationToken,
    maintenance: OnceLock<()>,
}

impl Shared {
    /// The current connection, if it is up.
    fn current_conn(&self) -> Option<Arc<Conn>> {
        match &*self.slot.lock() {
            Slot::Ready(conn) if !conn.is_closed() => Some(Arc::clone(conn)),
            _ => None,
        }
    }

    fn live_conn(&self, generation: u64) -> Option<Arc<Conn>> {
        self.current_conn()
            .filter(|conn| conn.generation == generation)
    }

    /// Someone needs the connection back without waiting for the next call.
    fn wanted(&self) -> bool {
        self.options.keep_connected || !self.subs.lock().is_empty()
    }

    fn open_subs(&self) -> Vec<Arc<SubShared>> {
        self.subs.lock().values().cloned().collect()
    }

    /// Applies `change` unless the client is closed (`Closed` is final).
    fn update_state(&self, change: impl FnOnce(&mut ConnectionState) -> bool) {
        self.state.send_if_modified(|state| {
            if matches!(state, ConnectionState::Closed) {
                false
            } else {
                change(state)
            }
        });
    }

    fn state_connecting(&self) {
        self.update_state(|state| match state {
            ConnectionState::Reconnecting { .. } | ConnectionState::Connecting => false,
            _ => {
                *state = ConnectionState::Connecting;
                true
            }
        });
    }

    /// `Connected` for `conn` unless it is already gone. The reader cancels `closed`
    /// before it wakes the maintenance task, so a lost connection is never reported
    /// connected after its loss was reported.
    fn state_connected(&self, conn: &Conn) {
        let Some(hello) = conn.hello() else {
            return;
        };
        self.update_state(|state| {
            let current = matches!(
                state,
                ConnectionState::Connected { generation, .. } if *generation == conn.generation
            );
            if conn.is_closed() || current {
                return false;
            }
            *state = ConnectionState::Connected {
                hello: Box::new(hello.clone()),
                generation: conn.generation,
            };
            true
        });
    }

    /// A connection attempt started from `Connecting` failed.
    fn state_connect_failed(&self, err: &ClientError) {
        let wanted = self.wanted();
        let mut was_connecting = false;
        self.update_state(|state| {
            if !matches!(state, ConnectionState::Connecting) {
                return false;
            }
            was_connecting = true;
            *state = if wanted {
                ConnectionState::Reconnecting {
                    attempt: 0,
                    next_retry_ms: 0,
                    last_error: err.clone(),
                }
            } else {
                ConnectionState::Idle
            };
            true
        });
        // From `Reconnecting` the maintenance task is already retrying.
        if wanted && was_connecting {
            self.wake.notify_one();
        }
        self.fix_up_state();
    }

    fn state_reconnecting(&self, attempt: u32, next_retry: Duration, last_error: &ClientError) {
        let next = ConnectionState::Reconnecting {
            attempt,
            next_retry_ms: u64::try_from(next_retry.as_millis()).unwrap_or(u64::MAX),
            last_error: last_error.clone(),
        };
        self.update_state(|state| {
            if *state == next {
                return false;
            }
            *state = next;
            true
        });
        self.fix_up_state();
    }

    fn state_idle(&self) {
        self.update_state(|state| match state {
            ConnectionState::Connected { .. } | ConnectionState::Reconnecting { .. } => {
                *state = ConnectionState::Idle;
                true
            }
            _ => false,
        });
        self.fix_up_state();
    }

    /// A call may have connected between a check of the maintenance task and its state
    /// update: the live connection wins.
    fn fix_up_state(&self) {
        if let Some(conn) = self.current_conn() {
            self.state_connected(&conn);
        }
    }

    pub(crate) fn forget_subscription(&self, local_id: u64) {
        self.subs.lock().remove(&local_id);
    }

    /// Unsubscribe without waiting (from `Drop`).
    pub(crate) fn unsubscribe_detached(
        &self,
        generation: u64,
        stream: &'static MethodInfo,
        id: SubscriptionId,
    ) {
        let (Some(conn), Some(method)) = (self.live_conn(generation), unsubscribe_method(stream))
        else {
            return;
        };
        conn.forget_route(id);
        if let Ok(params) = serde_json::to_value(UnsubscribeParams {
            subscription_id: id,
        }) {
            conn.send_detached(method.name, &params);
        }
    }

    pub(crate) async fn unsubscribe_on(
        &self,
        generation: u64,
        stream: &'static MethodInfo,
        id: SubscriptionId,
    ) {
        let (Some(conn), Some(method)) = (self.live_conn(generation), unsubscribe_method(stream))
        else {
            return;
        };
        conn.forget_route(id);
        if let Ok(params) = serde_json::to_value(UnsubscribeParams {
            subscription_id: id,
        }) {
            let _ = conn
                .request(
                    method.name,
                    &params,
                    RequestId::new(),
                    self.options.call_timeout,
                    None,
                )
                .await;
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.state.send_replace(ConnectionState::Closed);
        self.closed.cancel();
        if let Slot::Ready(conn) = &*self.slot.lock() {
            conn.closed.cancel();
        }
        let subs: Vec<Arc<SubShared>> = self.subs.lock().drain().map(|(_, s)| s).collect();
        for sub in subs {
            sub.close();
        }
    }
}

/// API client. Cheap to clone; clones share the connection and subscriptions.
#[derive(Clone)]
pub struct Client {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("transport", &self.shared.transport.endpoint())
            .field("state", &*self.shared.state.borrow())
            .finish()
    }
}

impl Client {
    /// Client that connects on first use (or in the background after
    /// [`Client::start`]). `credentials` is a [`Credentials`] or a
    /// [`CredentialsProvider`].
    pub fn new(
        transport: Arc<dyn Transport>,
        credentials: impl Into<CredentialsProvider>,
        options: ClientOptions,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                transport,
                credentials: credentials.into(),
                options,
                slot: OrderedMutex::new(LockLevel::IPC_CLIENT, Slot::Idle),
                subs: OrderedMutex::new(LockLevel::IPC_CLIENT, HashMap::new()),
                state: watch::Sender::new(ConnectionState::Idle),
                generation: AtomicU64::new(0),
                next_sub: AtomicU64::new(1),
                last_rtt_us: AtomicU64::new(0),
                wake: Arc::new(Notify::new()),
                closed: CancellationToken::new(),
                maintenance: OnceLock::new(),
            }),
        }
    }

    /// Connects and authenticates now (fails fast when the daemon is not reachable).
    pub async fn connect(
        transport: Arc<dyn Transport>,
        credentials: impl Into<CredentialsProvider>,
        options: ClientOptions,
    ) -> Result<Self, ClientError> {
        let client = Self::new(transport, credentials, options);
        client.connection().await?;
        Ok(client)
    }

    /// Local client of the installation at `root` (named pipe / unix socket, token from
    /// `state/ipc.token`, read again on every reconnection). It does not start the
    /// daemon; [`local`] does.
    pub async fn connect_local(
        root: &ForgeRoot,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, ClientError> {
        Self::connect(
            crate::transport::local_transport(root),
            CredentialsProvider::local(root, name, version),
            ClientOptions::default(),
        )
        .await
    }

    /// Starts the background maintenance now (needs a tokio runtime). With
    /// [`ClientOptions::keep_connected`] it connects at once and keeps retrying with
    /// backoff until the daemon answers; follow it with [`Client::state`].
    pub fn start(&self) {
        ensure_maintenance(&self.shared);
        if self.shared.options.keep_connected && self.shared.current_conn().is_none() {
            self.shared.wake.notify_one();
        }
    }

    /// The connection state. The receiver sees the changes from now on; a slow reader
    /// sees the latest one (use `wait_for` to wait for a condition).
    pub fn state(&self) -> watch::Receiver<ConnectionState> {
        self.shared.state.subscribe()
    }

    /// Measures one `system.ping` round trip on the current connection (connecting
    /// first if needed; connecting is not counted).
    pub async fn ping(&self) -> Result<Duration, ClientError> {
        let conn = self.connection().await?;
        let params = serde_json::to_value(NoParams {})
            .map_err(|e| ClientError::Protocol(format!("ping params: {e}")))?;
        let started = Instant::now();
        conn.request(
            "system.ping",
            &params,
            RequestId::new(),
            self.shared.options.call_timeout,
            None,
        )
        .await?;
        let rtt = started.elapsed();
        let micros = u64::try_from(rtt.as_micros()).unwrap_or(u64::MAX).max(1);
        self.shared.last_rtt_us.store(micros, Ordering::Relaxed);
        Ok(rtt)
    }

    /// The last round trip measured by [`Client::ping`].
    pub fn last_rtt(&self) -> Option<Duration> {
        match self.shared.last_rtt_us.load(Ordering::Relaxed) {
            0 => None,
            micros => Some(Duration::from_micros(micros)),
        }
    }

    /// Answer of `system.hello` on the current connection.
    pub async fn hello(&self) -> Result<HelloDto, ClientError> {
        let conn = self.connection().await?;
        conn.hello()
            .cloned()
            .ok_or_else(|| ClientError::Protocol("connection without hello".into()))
    }

    /// Transport facts of the current connection.
    pub async fn connection_info(&self) -> Result<ConnectionInfo, ClientError> {
        Ok(self.connection().await?.info.clone())
    }

    /// Closes the connection and every subscription; the state becomes `Closed`.
    pub fn close(&self) {
        self.shared.state.send_replace(ConnectionState::Closed);
        self.shared.closed.cancel();
        if let Slot::Ready(conn) = &*self.shared.slot.lock() {
            conn.closed.cancel();
        }
        let subs: Vec<Arc<SubShared>> = self.shared.subs.lock().drain().map(|(_, s)| s).collect();
        for sub in subs {
            sub.close();
        }
    }

    /// Drops the current connection (tests and "reconnect" buttons); the next call or
    /// the maintenance task reconnects.
    pub fn disconnect(&self) {
        if let Slot::Ready(conn) = &*self.shared.slot.lock() {
            conn.closed.cancel();
        }
    }

    async fn connection(&self) -> Result<Arc<Conn>, ClientError> {
        connection(&self.shared).await
    }

    /// Calls `M` with a fresh `request_id`.
    pub async fn call<M: ApiMethod>(&self, params: M::Params) -> Result<M::Result, ClientError> {
        self.call_with::<M>(params, CallOptions::default()).await
    }

    /// Calls `M` with explicit options.
    pub async fn call_with<M: ApiMethod>(
        &self,
        params: M::Params,
        options: CallOptions,
    ) -> Result<M::Result, ClientError> {
        let params = serde_json::to_value(&params)
            .map_err(|e| ClientError::Protocol(format!("params of {}: {e}", M::INFO.name)))?;
        let value = self.call_raw(M::INFO.name, params, options).await?;
        serde_json::from_value(value)
            .map_err(|e| ClientError::Protocol(format!("answer of {}: {e}", M::INFO.name)))
    }

    /// Untyped call (method names outside the table are answered `not_found`).
    pub async fn call_raw(
        &self,
        method: &str,
        params: Value,
        options: CallOptions,
    ) -> Result<Value, ClientError> {
        let request_id = options.request_id.unwrap_or_default();
        let timeout = options.timeout.unwrap_or(self.shared.options.call_timeout);
        call_value(&self.shared, method, &params, request_id, timeout, None).await
    }

    /// Opens a stream subscription of `M`, resumed automatically after reconnections.
    pub async fn subscribe<M: StreamMethod>(
        &self,
        params: M::Params,
    ) -> Result<Subscription<M::Item>, ClientError> {
        let params = serde_json::to_value(&params)
            .map_err(|e| ClientError::Protocol(format!("params of {}: {e}", M::INFO.name)))?;
        let info = crate::api::method_info(M::INFO.name)
            .ok_or_else(|| ClientError::Protocol(format!("unknown stream {}", M::INFO.name)))?;
        self.open_subscription(info, params).await
    }

    /// Untyped [`Client::subscribe`]. `method` must be a `stream` row of
    /// [`crate::METHODS`] (otherwise [`ClientError::InvalidInput`] and nothing is sent)
    /// and `params` an object (`null` counts as `{}`). The items are the notification
    /// params as the daemon sent them (`{subscription_id, seq, ...}`), with the same
    /// resume cursor, floor and `seq` dedupe as the typed subscriptions.
    pub async fn subscribe_raw(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Subscription<Value>, ClientError> {
        let info = crate::api::method_info(method)
            .filter(|info| info.kind == MethodKind::Stream)
            .ok_or_else(|| {
                ClientError::InvalidInput(format!("{method} is not a stream method of the API"))
            })?;
        let params = match params {
            Value::Null => Value::Object(serde_json::Map::new()),
            Value::Object(_) => params,
            _ => {
                return Err(ClientError::InvalidInput(format!(
                    "the params of {method} must be an object"
                )));
            }
        };
        self.open_subscription(info, params).await
    }

    async fn open_subscription<T: DeserializeOwned>(
        &self,
        info: &'static MethodInfo,
        params: Value,
    ) -> Result<Subscription<T>, ClientError> {
        let local_id = self.shared.next_sub.fetch_add(1, Ordering::Relaxed);
        let sub = Arc::new(SubShared::new(
            local_id,
            info,
            params,
            self.shared.options.stream_capacity,
        ));
        self.shared.subs.lock().insert(local_id, Arc::clone(&sub));
        let result = call_value(
            &self.shared,
            info.name,
            &sub.resume_params(),
            RequestId::new(),
            self.shared.options.call_timeout,
            Some(Arc::clone(&sub)),
        )
        .await;
        match result {
            Ok(_) => Ok(Subscription::new(sub, Arc::clone(&self.shared))),
            Err(err) => {
                sub.close();
                self.shared.forget_subscription(local_id);
                Err(err)
            }
        }
    }

    /// `events.subscribe {filter, resume_from}`.
    pub async fn subscribe_events(
        &self,
        filter: EventFilter,
        resume_from: Option<u64>,
    ) -> Result<Subscription<EventEnvelope>, ClientError> {
        self.subscribe::<crate::api::methods::events_subscribe>(EventsSubscribeParams {
            filter,
            resume_from,
        })
        .await
    }
}

/// Spawns the maintenance task once (on the caller's runtime).
fn ensure_maintenance(shared: &Arc<Shared>) {
    shared.maintenance.get_or_init(|| {
        tokio::spawn(maintain(
            Arc::downgrade(shared),
            Arc::clone(&shared.wake),
            shared.closed.clone(),
        ));
    });
}

/// One connection attempt: credentials (asked again every time), transport, hello.
fn connect_future(shared: &Shared, generation: u64) -> ConnectFuture {
    let transport = Arc::clone(&shared.transport);
    let credentials = shared.credentials.clone();
    let timeout = shared.options.connect_timeout;
    let wake = Arc::clone(&shared.wake);
    async move {
        let credentials = tokio::time::timeout(timeout, credentials.get())
            .await
            .map_err(|_| ClientError::Timeout(timeout))??;
        conn::connect(transport, credentials.hello(), timeout, generation, wake).await
    }
    .boxed()
    .shared()
}

async fn connection(shared: &Arc<Shared>) -> Result<Arc<Conn>, ClientError> {
    if shared.closed.is_cancelled() {
        return Err(ClientError::Closed);
    }
    ensure_maintenance(shared);
    let (generation, future, started) = {
        let mut slot = shared.slot.lock();
        match &*slot {
            Slot::Ready(conn) if !conn.is_closed() => return Ok(Arc::clone(conn)),
            Slot::Connecting { generation, future } => (*generation, future.clone(), false),
            _ => {
                let generation = shared.generation.fetch_add(1, Ordering::Relaxed) + 1;
                let future = connect_future(shared, generation);
                *slot = Slot::Connecting {
                    generation,
                    future: future.clone(),
                };
                (generation, future, true)
            }
        }
    };
    if started {
        shared.state_connecting();
    }
    let result = future.await;
    let installed = {
        let mut slot = shared.slot.lock();
        let ours = matches!(&*slot, Slot::Connecting { generation: g, .. } if *g == generation);
        if ours {
            *slot = match &result {
                Ok(conn) => Slot::Ready(Arc::clone(conn)),
                Err(_) => Slot::Idle,
            };
        }
        ours
    };
    if shared.closed.is_cancelled() {
        if let Ok(conn) = &result {
            conn.closed.cancel();
        }
        return Err(ClientError::Closed);
    }
    match &result {
        Ok(conn) => shared.state_connected(conn),
        Err(err) if installed => shared.state_connect_failed(err),
        Err(_) => {}
    }
    result
}

async fn call_value(
    shared: &Arc<Shared>,
    method: &str,
    params: &Value,
    request_id: RequestId,
    timeout: Duration,
    route: Option<Arc<SubShared>>,
) -> Result<Value, ClientError> {
    let mut attempt = 0;
    loop {
        let conn = connection(shared).await?;
        match conn
            .request(method, params, request_id, timeout, route.clone())
            .await
        {
            Err(ClientError::Disconnected) if attempt < shared.options.retries => {
                attempt += 1;
                tracing::debug!(method, %request_id, attempt, "retrying after disconnect");
            }
            other => return other,
        }
    }
}

/// Background maintenance: reports a lost connection in the state and, while someone
/// wants the connection (open subscriptions, `keep_connected`), reconnects with backoff
/// and resubscribes. It keeps only a `Weak` between attempts, so dropping every
/// `Client` and `Subscription` ends it.
async fn maintain(shared: Weak<Shared>, wake: Arc<Notify>, closed: CancellationToken) {
    loop {
        tokio::select! {
            () = closed.cancelled() => return,
            () = wake.notified() => {}
        }
        let mut failures: u32 = 0;
        let mut last_error = ClientError::Disconnected;
        let mut backoff: Option<Duration> = None;
        loop {
            let Some(strong) = shared.upgrade() else {
                return;
            };
            if strong.current_conn().is_none() {
                if !strong.wanted() {
                    strong.state_idle();
                    break;
                }
                // From `Idle` (the first connection of a `keep_connected` client) the
                // attempt reports `Connecting`; after a loss, `Reconnecting`.
                if *strong.state.borrow() != ConnectionState::Idle {
                    strong.state_reconnecting(failures, Duration::ZERO, &last_error);
                }
            }
            match connection(&strong).await {
                Ok(conn) => {
                    let subs = strong.open_subs();
                    if restore(&strong, &conn, &subs).await {
                        strong.state_connected(&conn);
                        break;
                    }
                }
                Err(ClientError::Closed) => return,
                Err(err) => {
                    tracing::debug!(%err, "reconnection failed");
                    failures = failures.saturating_add(1);
                    last_error = err;
                }
            }
            let (min, max) = (strong.options.reconnect_min, strong.options.reconnect_max);
            let delay = backoff.map_or(min, |previous| (previous * 2).min(max));
            backoff = Some(delay);
            if strong.current_conn().is_none() {
                strong.state_reconnecting(failures, delay, &last_error);
            }
            drop(strong);
            tokio::select! {
                () = closed.cancelled() => return,
                () = tokio::time::sleep(delay) => {}
            }
        }
    }
}

/// Re-subscribes every subscription not attached to `conn`. `true` when all are.
async fn restore(shared: &Arc<Shared>, conn: &Arc<Conn>, subs: &[Arc<SubShared>]) -> bool {
    let mut all = true;
    for sub in subs {
        if sub.is_closed() {
            continue;
        }
        if sub
            .attachment()
            .is_some_and(|(generation, _)| generation == conn.generation)
        {
            continue;
        }
        let result = conn
            .request(
                sub.method.name,
                &sub.resume_params(),
                RequestId::new(),
                shared.options.call_timeout,
                Some(Arc::clone(sub)),
            )
            .await;
        if let Err(err) = result {
            tracing::debug!(%err, method = sub.method.name, "resubscription failed");
            all = false;
        }
    }
    all
}
