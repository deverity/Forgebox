//! JSON-RPC server of the daemon API (docs/03 §7.4, ADR-0015).
//!
//! * The accept loop only accepts: every connection gets its own task; frames are
//!   parsed there, never in the accept task.
//! * Each connection has a reader (parse, authenticate, dispatch) and a writer
//!   ([`outbox`]); each request runs in its own task with a deadline (~1 s by default;
//!   long-running methods answer with a `JobDto` at once). A slow handler never blocks
//!   the connection, a slow client never blocks the daemon.
//! * The first request must be `system.hello` (token, client, API version).
//! * Role and `local_only` are enforced from the method table; `request_id` is
//!   mandatory and mutating methods are deduplicated by it ([`idempotency`]).
//! * Built-in methods (`system.ping`, `daemon.status`, `events.*`, `jobs.*`, `fs.*`,
//!   `*.unsubscribe`) are provided when the matching component is configured; the daemon
//!   registers everything else on a [`Router`]. Known methods without a handler answer
//!   `not_implemented`.

mod builtin;
mod connection;
pub mod events;
pub mod fs;
mod idempotency;
mod outbox;
mod router;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use forge_core::Timestamp;
use forge_core::jobs::JobRegistry;
use forge_core::paths::ForgeRoot;
use forge_core::sync::{LockLevel, OrderedMutex};

pub use self::events::EventSource;
pub use self::fs::FsScope;
use self::idempotency::IdempotencyCache;
pub use self::outbox::StreamSender;
pub use self::router::{Ctx, Handler, Router, Session};
use crate::auth::Authenticator;
use crate::transport::{ConnectionInfo, Listener, Transport, TransportError};

/// Limits and identity of the server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Version string reported by `system.hello` and `daemon.status`.
    pub daemon_version: String,
    /// Deadline of a non-stream request (docs/03 §7.4: ~1 s).
    pub request_timeout: Duration,
    /// A connection that does not authenticate within this time is closed.
    pub hello_timeout: Duration,
    /// Requests of one connection running or waiting to be written.
    pub max_in_flight: usize,
    /// Items queued per stream subscription before the oldest are dropped.
    pub stream_queue_capacity: usize,
    /// Subscriptions per connection.
    pub max_subscriptions: usize,
    /// Simultaneous connections.
    pub max_connections: usize,
    /// How long a closing connection waits for its pending answers to be written.
    pub drain_timeout: Duration,
    /// `request_id` cache size and lifetime.
    pub idempotency_capacity: usize,
    pub idempotency_ttl: Duration,
    /// Maximum events replayed from the store on `resume_from`; older ones are
    /// reported as lagged.
    pub replay_limit: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            request_timeout: Duration::from_secs(1),
            hello_timeout: Duration::from_secs(5),
            max_in_flight: 64,
            stream_queue_capacity: 1024,
            max_subscriptions: 64,
            max_connections: 128,
            drain_timeout: Duration::from_secs(2),
            idempotency_capacity: 4096,
            idempotency_ttl: Duration::from_secs(600),
            replay_limit: 10_000,
        }
    }
}

/// Facts shared with the built-in handlers.
pub(crate) struct ServerInfo {
    pub(crate) config: ServerConfig,
    pub(crate) started_at: Timestamp,
    pub(crate) started: std::time::Instant,
    pub(crate) root: Option<ForgeRoot>,
    pub(crate) events: Option<EventSource>,
    pub(crate) jobs: Option<JobRegistry>,
    pub(crate) connections: AtomicUsize,
}

struct ConnEntry {
    info: ConnectionInfo,
    cancel: CancellationToken,
}

pub(crate) struct ServerShared {
    pub(crate) info: Arc<ServerInfo>,
    pub(crate) router: Router,
    pub(crate) auth: Arc<dyn Authenticator>,
    pub(crate) idempotency: IdempotencyCache,
    /// Lock: `ipc.connections` (rank 10).
    connections: OrderedMutex<HashMap<u64, ConnEntry>>,
    shutdown: CancellationToken,
}

impl ServerShared {
    fn register(&self, id: u64, info: ConnectionInfo, cancel: CancellationToken) {
        self.connections
            .lock()
            .insert(id, ConnEntry { info, cancel });
        self.info.connections.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn unregister(&self, id: u64) {
        if self.connections.lock().remove(&id).is_some() {
            self.info.connections.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// Builder of a [`Server`].
pub struct ServerBuilder {
    config: ServerConfig,
    auth: Arc<dyn Authenticator>,
    router: Router,
    root: Option<ForgeRoot>,
    events: Option<EventSource>,
    jobs: Option<JobRegistry>,
    fs: Option<FsScope>,
}

impl ServerBuilder {
    /// Daemon handlers (they take precedence over built-ins).
    pub fn router(mut self, router: Router) -> Self {
        self.router = router;
        self
    }

    pub fn config(mut self, config: ServerConfig) -> Self {
        self.config = config;
        self
    }

    /// Installation root (reported by `daemon.status`).
    pub fn root(mut self, root: ForgeRoot) -> Self {
        self.root = Some(root);
        self
    }

    /// Enables `events.subscribe|unsubscribe|list`.
    pub fn events(mut self, events: EventSource) -> Self {
        self.events = Some(events);
        self
    }

    /// Enables `jobs.list|get|cancel`.
    pub fn jobs(mut self, jobs: JobRegistry) -> Self {
        self.jobs = Some(jobs);
        self
    }

    /// Enables `fs.read|write|tail|list`.
    pub fn fs(mut self, fs: FsScope) -> Self {
        self.fs = Some(fs);
        self
    }

    pub fn build(self) -> Server {
        let info = Arc::new(ServerInfo {
            config: self.config.clone(),
            started_at: Timestamp::now(),
            started: std::time::Instant::now(),
            root: self.root,
            events: self.events,
            jobs: self.jobs,
            connections: AtomicUsize::new(0),
        });
        let mut router = self.router;
        router.merge_missing(builtin::router(&info, self.fs));
        Server {
            shared: Arc::new(ServerShared {
                router,
                auth: self.auth,
                idempotency: IdempotencyCache::new(
                    self.config.idempotency_capacity,
                    self.config.idempotency_ttl,
                ),
                connections: OrderedMutex::new(LockLevel::IPC_CONNECTIONS, HashMap::new()),
                shutdown: CancellationToken::new(),
                info,
            }),
        }
    }
}

/// The API server.
pub struct Server {
    shared: Arc<ServerShared>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("router", &self.shared.router)
            .finish_non_exhaustive()
    }
}

/// Control of a running server. Cheap to clone.
#[derive(Clone)]
pub struct ServerHandle {
    shared: Arc<ServerShared>,
}

impl std::fmt::Debug for ServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerHandle")
            .field("connections", &self.connection_count())
            .finish()
    }
}

impl ServerHandle {
    /// Stops accepting and closes every connection.
    pub fn shutdown(&self) {
        self.shared.shutdown.cancel();
    }

    /// Closes every connection (clients reconnect and resume); keeps listening.
    pub fn disconnect_all(&self) {
        let tokens: Vec<CancellationToken> = self
            .shared
            .connections
            .lock()
            .values()
            .map(|c| c.cancel.clone())
            .collect();
        for token in tokens {
            token.cancel();
        }
    }

    /// Live connections.
    pub fn connection_count(&self) -> usize {
        self.shared.info.connections.load(Ordering::Relaxed)
    }

    /// Transports of the live connections (diagnostics).
    pub fn connections(&self) -> Vec<ConnectionInfo> {
        self.shared
            .connections
            .lock()
            .values()
            .map(|c| c.info.clone())
            .collect()
    }
}

impl Server {
    /// Builder with the authenticator that checks `system.hello`.
    pub fn builder(auth: Arc<dyn Authenticator>) -> ServerBuilder {
        ServerBuilder {
            config: ServerConfig::default(),
            auth,
            router: Router::new(),
            root: None,
            events: None,
            jobs: None,
            fs: None,
        }
    }

    pub fn handle(&self) -> ServerHandle {
        ServerHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Accepts connections until [`ServerHandle::shutdown`] or until the listener closes.
    pub async fn serve(self, mut listener: Box<dyn Listener>) {
        let shared = self.shared;
        let endpoint = listener.endpoint();
        tracing::info!(%endpoint, "API server listening");
        let mut next_id: u64 = 0;
        let mut failures: u32 = 0;
        loop {
            let accepted = tokio::select! {
                biased;
                () = shared.shutdown.cancelled() => break,
                accepted = listener.accept() => accepted,
            };
            match accepted {
                Ok(connection) => {
                    failures = 0;
                    if shared.info.connections.load(Ordering::Relaxed)
                        >= shared.info.config.max_connections
                    {
                        tracing::warn!(peer = %connection.info.peer, "connection limit reached; refusing");
                        continue;
                    }
                    next_id += 1;
                    let cancel = shared.shutdown.child_token();
                    shared.register(next_id, connection.info.clone(), cancel.clone());
                    tokio::spawn(connection::run(
                        Arc::clone(&shared),
                        connection,
                        next_id,
                        cancel,
                    ));
                }
                Err(TransportError::Closed) => break,
                Err(err) => {
                    failures = failures.saturating_add(1);
                    let backoff = Duration::from_millis(10u64 << failures.min(7));
                    tracing::warn!(%err, ?backoff, "accept failed");
                    tokio::select! {
                        () = shared.shutdown.cancelled() => break,
                        () = tokio::time::sleep(backoff) => {}
                    }
                }
            }
        }
        shared.shutdown.cancel();
        tracing::info!(%endpoint, "API server stopped");
    }

    /// Runs [`Server::serve`] in a task.
    pub fn spawn(self, listener: Box<dyn Listener>) -> (ServerHandle, JoinHandle<()>) {
        let handle = self.handle();
        (handle, tokio::spawn(self.serve(listener)))
    }

    /// Binds `transport` and spawns the server.
    pub async fn bind_and_spawn(
        self,
        transport: &dyn Transport,
    ) -> Result<(ServerHandle, JoinHandle<()>), TransportError> {
        let listener = transport.bind().await?;
        Ok(self.spawn(listener))
    }
}
