//! Typed API client used by the CLI, the GUI (through Tauri commands) and the MCP server.
//!
//! * Transport-agnostic ([`Transport`]); connects lazily and reconnects on demand.
//! * One Rust method per API method, generated from the method table
//!   ([`crate::api::methods`]), e.g. `client.sites_add(params)`; generic
//!   [`Client::call`] for the rest.
//! * Every call carries a fresh `request_id`; when the connection drops before the
//!   answer, the call is retried on a new connection **with the same `request_id`**, so
//!   the daemon does not act twice (ADR-0009, rule 4).
//! * Streams ([`Client::subscribe`], [`Client::subscribe_events`]) resume after a
//!   reconnection from the last `seq` received ([`subscription`]).

mod conn;
mod methods;
mod subscription;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use futures::FutureExt;
use futures::future::{BoxFuture, Shared as SharedFuture};
use serde_json::Value;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use forge_core::ids::{RequestId, SubscriptionId};
use forge_core::paths::ForgeRoot;
use forge_core::sync::{LockLevel, OrderedMutex};

use self::conn::Conn;
use self::subscription::{SubShared, unsubscribe_method};
pub use self::subscription::{Subscription, SubscriptionItem};
use crate::api::{
    API_VERSION, ApiError, ApiMethod, ClientInfoDto, EventEnvelope, EventFilter,
    EventsSubscribeParams, HelloDto, HelloParams, MethodInfo, StreamMethod, UnsubscribeParams,
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
}

impl ClientError {
    /// Stable error code (`forge --json`, exit codes).
    pub fn code(&self) -> &str {
        match self {
            ClientError::Api(error) => &error.code,
            ClientError::Transport(error) => error.code(),
            ClientError::Disconnected => "io",
            ClientError::Timeout(_) => "timeout",
            ClientError::Protocol(_) => "internal",
            ClientError::Closed => "cancelled",
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

/// Tuning of a [`Client`].
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// Deadline of a call, network included (the daemon answers within ~1 s).
    pub call_timeout: Duration,
    /// Deadline of connecting plus `system.hello`.
    pub connect_timeout: Duration,
    /// Retries (same `request_id`) when the connection drops before the answer.
    pub retries: u32,
    /// Reconnection backoff for subscriptions.
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
    /// Items buffered per subscription before the oldest are dropped.
    pub stream_capacity: usize,
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
    credentials: Credentials,
    options: ClientOptions,
    /// Lock: `ipc.client` (rank 10), never nested with `subs`.
    slot: OrderedMutex<Slot>,
    /// Lock: `ipc.client` (rank 10), never nested with `slot`.
    subs: OrderedMutex<HashMap<u64, Arc<SubShared>>>,
    generation: AtomicU64,
    next_sub: AtomicU64,
    lost: Arc<Notify>,
    closed: CancellationToken,
    resubscriber: OnceLock<()>,
}

impl Shared {
    fn live_conn(&self, generation: u64) -> Option<Arc<Conn>> {
        match &*self.slot.lock() {
            Slot::Ready(conn) if conn.generation == generation && !conn.is_closed() => {
                Some(Arc::clone(conn))
            }
            _ => None,
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
            .field("client", &self.shared.credentials.client.name)
            .finish()
    }
}

impl Client {
    /// Client that connects on first use.
    pub fn new(
        transport: Arc<dyn Transport>,
        credentials: Credentials,
        options: ClientOptions,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                transport,
                credentials,
                options,
                slot: OrderedMutex::new(LockLevel::IPC_CLIENT, Slot::Idle),
                subs: OrderedMutex::new(LockLevel::IPC_CLIENT, HashMap::new()),
                generation: AtomicU64::new(0),
                next_sub: AtomicU64::new(1),
                lost: Arc::new(Notify::new()),
                closed: CancellationToken::new(),
                resubscriber: OnceLock::new(),
            }),
        }
    }

    /// Connects and authenticates now (fails fast when the daemon is not reachable).
    pub async fn connect(
        transport: Arc<dyn Transport>,
        credentials: Credentials,
        options: ClientOptions,
    ) -> Result<Self, ClientError> {
        let client = Self::new(transport, credentials, options);
        client.connection().await?;
        Ok(client)
    }

    /// Local client of the installation at `root` (named pipe / unix socket, token from
    /// `state/ipc.token`).
    pub async fn connect_local(
        root: &ForgeRoot,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, ClientError> {
        let credentials = Credentials::local(root, name, version)
            .map_err(|e| ClientError::Api(ApiError::from(&e)))?;
        Self::connect(
            crate::transport::local_transport(root),
            credentials,
            ClientOptions::default(),
        )
        .await
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

    /// Closes the connection and every subscription.
    pub fn close(&self) {
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
    /// the subscription maintenance reconnects.
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
        let local_id = self.shared.next_sub.fetch_add(1, Ordering::Relaxed);
        let sub = Arc::new(SubShared::new(
            local_id,
            info,
            params,
            self.shared.options.stream_capacity,
        ));
        self.shared.subs.lock().insert(local_id, Arc::clone(&sub));
        self.ensure_resubscriber();
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

    fn ensure_resubscriber(&self) {
        self.shared.resubscriber.get_or_init(|| {
            tokio::spawn(resubscriber(
                Arc::downgrade(&self.shared),
                Arc::clone(&self.shared.lost),
                self.shared.closed.clone(),
            ));
        });
    }
}

async fn connection(shared: &Arc<Shared>) -> Result<Arc<Conn>, ClientError> {
    if shared.closed.is_cancelled() {
        return Err(ClientError::Closed);
    }
    let (generation, future) = {
        let mut slot = shared.slot.lock();
        match &*slot {
            Slot::Ready(conn) if !conn.is_closed() => return Ok(Arc::clone(conn)),
            Slot::Connecting { generation, future } => (*generation, future.clone()),
            _ => {
                let generation = shared.generation.fetch_add(1, Ordering::Relaxed) + 1;
                let future = conn::connect(
                    Arc::clone(&shared.transport),
                    shared.credentials.hello(),
                    shared.options.connect_timeout,
                    generation,
                    Arc::clone(&shared.lost),
                )
                .boxed()
                .shared();
                *slot = Slot::Connecting {
                    generation,
                    future: future.clone(),
                };
                (generation, future)
            }
        }
    };
    let result = future.await;
    {
        let mut slot = shared.slot.lock();
        if matches!(&*slot, Slot::Connecting { generation: g, .. } if *g == generation) {
            *slot = match &result {
                Ok(conn) => Slot::Ready(Arc::clone(conn)),
                Err(_) => Slot::Idle,
            };
        }
    }
    if shared.closed.is_cancelled() {
        if let Ok(conn) = &result {
            conn.closed.cancel();
        }
        return Err(ClientError::Closed);
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

/// Keeps subscriptions alive across reconnections.
async fn resubscriber(shared: Weak<Shared>, lost: Arc<Notify>, closed: CancellationToken) {
    loop {
        tokio::select! {
            () = closed.cancelled() => return,
            () = lost.notified() => {}
        }
        let mut backoff = match shared.upgrade() {
            Some(s) => s.options.reconnect_min,
            None => return,
        };
        loop {
            let Some(strong) = shared.upgrade() else {
                return;
            };
            let subs: Vec<Arc<SubShared>> = strong.subs.lock().values().cloned().collect();
            if subs.is_empty() {
                break;
            }
            let max_backoff = strong.options.reconnect_max;
            let restored = match connection(&strong).await {
                Ok(conn) => restore(&strong, &conn, &subs).await,
                Err(ClientError::Closed) => return,
                Err(err) => {
                    tracing::debug!(%err, "reconnection failed");
                    false
                }
            };
            drop(strong);
            if restored {
                break;
            }
            tokio::select! {
                () = closed.cancelled() => return,
                () = tokio::time::sleep(backoff) => {}
            }
            backoff = (backoff * 2).min(max_backoff);
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
