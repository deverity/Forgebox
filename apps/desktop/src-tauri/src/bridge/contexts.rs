//! `Contexts`: the actor that owns one long-lived [`Client`] per context id ([ADR-0034]).
//!
//! Only [`LOCAL_CONTEXT`] exists in M2 (remote profiles are M6), but every operation names
//! its context so nothing assumes the daemon is on this machine (ADR-0009).
//!
//! * A context opens lazily, on the first command that names it (the shell paints first
//!   and connects afterwards). Opening goes through a [`Connector`]: for `local`,
//!   [`LocalConnector`] discovers the root (`spawn_blocking`) and builds a client with
//!   `keep_connected`, so forge-ipc reconnects and resumes streams by itself.
//! * The actor follows the client's `ConnectionState` (a forwarding task), the daemon's
//!   `daemon.shutdown` announcements (an `events.subscribe` watcher) and runs the
//!   [`AutoStartPolicy`]: starts go through [`Endpoint::start`] (for `local`,
//!   `forge_ipc::client::local::connect_or_start`, the same code as `forge`) in their own
//!   task and report back by message.
//! * Every change of a context's [`ContextStateDto`] is emitted through the
//!   [`ConnectionSink`] (the `forge://connection` Tauri event) and a broadcast for Rust
//!   consumers (the tray model).
//!
//! The actor never awaits I/O while handling a message: discovery, initialization checks,
//! starts, timers and pokes run in spawned tasks that `tell` their result.
//!
//! [ADR-0034]: ../../../../../docs/adr/0034-desktop-shell-architecture.md

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;
use tokio::task::AbortHandle;
use tokio::time::Instant;

use forge_ipc::api::HelloDto;
use forge_ipc::base::actor::{Actor, Context, Reply, WeakActorRef};
use forge_ipc::base::paths::ForgeRoot;
use forge_ipc::client::local::{self, AutoStart, LocalClientConfig};
use forge_ipc::client::{Client, ClientOptions, ConnectionState, SubscriptionItem};
use forge_ipc::{ApiError, ClientError};

use super::errors::{self, client_error};
use super::policy::{AutoStartPolicy, Decision, PolicyConfig, StartOutcome};

/// The context of the daemon of this machine.
pub const LOCAL_CONTEXT: &str = "local";

/// `client.name` the desktop app sends in `system.hello`.
pub const CLIENT_NAME: &str = "forgebox-desktop";

/// Where a context stands, as the UI shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    /// Not connected, nothing in progress.
    Idle,
    /// The root is not initialized (no `state/`): onboarding, no automatic start.
    Uninitialized,
    /// The bridge is starting the daemon.
    Starting,
    /// First connection attempt (or discovery) in progress.
    Connecting,
    Connected,
    /// Lost; the client is reconnecting (and the policy may restart the daemon).
    Reconnecting,
    /// The daemon announced a deliberate shutdown (`user`, `signal`): no automatic start.
    Stopped,
    /// The bridge is shutting down.
    Closed,
}

/// Auto-start facts of a context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutoStartDto {
    /// The root has a `state/` folder (it was initialized by a daemon).
    pub initialized: bool,
    /// `daemon.shutdown.reason` of a deliberate stop, while it holds.
    pub stopped_reason: Option<String>,
    /// Milliseconds until the next automatic restart may run, when one is scheduled.
    pub next_restart_ms: Option<u64>,
    /// Automatic restarts done by this app so far.
    pub restarts: u32,
}

/// State of a context (`ipc_state`, `forge://connection`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextStateDto {
    pub context: String,
    pub status: ConnectionStatus,
    /// Grows with every emitted change of this context: a consumer keeps the highest.
    pub version: u64,
    /// `system.hello` of the current connection.
    pub hello: Option<HelloDto>,
    /// Connection number of the client (grows with every new connection).
    pub generation: Option<u64>,
    /// Failed reconnection attempts so far, and when the next one starts.
    pub attempt: u32,
    pub next_retry_ms: u64,
    /// Why the last connection or start attempt failed.
    pub last_error: Option<ApiError>,
    /// What the context points at, for display (the root folder for `local`).
    pub endpoint: Option<String>,
    pub autostart: AutoStartDto,
}

/// Receives every change of a context's state (the `forge://connection` event).
pub trait ConnectionSink: Send + Sync + 'static {
    fn connection_changed(&self, state: &ContextStateDto);
}

/// A sink that drops everything (tests, headless use).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSink;

impl ConnectionSink for NoSink {
    fn connection_changed(&self, _: &ContextStateDto) {}
}

/// An opened context: its client and how to (re)start its daemon.
pub trait Endpoint: Send + Sync + 'static {
    /// The context's long-lived client (always the same one).
    fn client(&self) -> Client;
    /// What it points at, for display.
    fn describe(&self) -> Option<String>;
    /// Whether the root is initialized right now.
    fn initialized(&self) -> BoxFuture<'static, bool>;
    /// Starts the daemon unless one answers; resolves once it answers. With
    /// `require_initialized`, refuses (`not_found`, `root_uninitialized`) a root that was
    /// never initialized.
    fn start(&self, require_initialized: bool) -> BoxFuture<'static, Result<StartOutcome, ApiError>>;
}

/// What [`Connector::open`] found.
pub struct Opened {
    pub endpoint: Arc<dyn Endpoint>,
    pub initialized: bool,
}

/// Opens a context (discovery, client construction). Called again after a failure.
pub trait Connector: Send + Sync + 'static {
    fn open(&self) -> BoxFuture<'static, Result<Opened, ApiError>>;
}

/// The local daemon: root discovery + `connect_or_start` (ADR-0030, M2-E1-T2).
#[derive(Debug, Clone)]
pub struct LocalConnector {
    /// Fixed root (tests); `None` = `ForgeRoot::discover` (`FORGEBOX_ROOT`, portable flag,
    /// `root.toml`, default).
    root: Option<ForgeRoot>,
    options: ClientOptions,
    version: String,
}

impl LocalConnector {
    /// Discovers the root when opened.
    pub fn discover(options: ClientOptions) -> Self {
        Self {
            root: None,
            options,
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Always `root`.
    pub fn with_root(root: ForgeRoot, options: ClientOptions) -> Self {
        Self {
            root: Some(root),
            ..Self::discover(options)
        }
    }
}

impl Connector for LocalConnector {
    fn open(&self) -> BoxFuture<'static, Result<Opened, ApiError>> {
        let fixed = self.root.clone();
        let options = self.options.clone();
        let version = self.version.clone();
        async move {
            let (root, initialized) = tokio::task::spawn_blocking(move || {
                let root = match fixed {
                    Some(root) => root,
                    None => ForgeRoot::discover(None).map_err(ApiError::from)?.0,
                };
                let initialized = root.state().is_dir();
                Ok::<_, ApiError>((root, initialized))
            })
            .await
            .map_err(|e| ApiError::new("internal", format!("root discovery: {e}")))??;
            let config = LocalClientConfig::new(CLIENT_NAME, version).with_options(options);
            let client = config.client(&root);
            let endpoint: Arc<dyn Endpoint> = Arc::new(LocalEndpoint {
                root,
                config,
                client,
            });
            Ok(Opened {
                endpoint,
                initialized,
            })
        }
        .boxed()
    }
}

struct LocalEndpoint {
    root: ForgeRoot,
    config: LocalClientConfig,
    client: Client,
}

impl Endpoint for LocalEndpoint {
    fn client(&self) -> Client {
        self.client.clone()
    }

    fn describe(&self) -> Option<String> {
        Some(self.root.to_posix_string())
    }

    fn initialized(&self) -> BoxFuture<'static, bool> {
        let state = self.root.state();
        async move {
            tokio::task::spawn_blocking(move || state.is_dir())
                .await
                .unwrap_or(false)
        }
        .boxed()
    }

    fn start(&self, require_initialized: bool) -> BoxFuture<'static, Result<StartOutcome, ApiError>> {
        let root = self.root.clone();
        // A short-lived client: the context's own client reconnects by itself.
        let config = LocalClientConfig::new(self.config.name.clone(), self.config.version.clone());
        let initialized = self.initialized();
        async move {
            if require_initialized && !initialized.await {
                return Err(errors::root_uninitialized(&root.to_posix_string()));
            }
            let mut spawned = false;
            let result =
                local::connect_or_start(&root, &config, AutoStart::default(), || spawned = true)
                    .await;
            match result {
                Ok(client) => {
                    client.close();
                    Ok(if spawned {
                        StartOutcome::Started
                    } else {
                        StartOutcome::AlreadyRunning
                    })
                }
                Err(err) => Err(errors::local_error(&err)),
            }
        }
        .boxed()
    }
}

/// Messages of the [`Contexts`] actor.
pub enum Msg {
    /// The client of a context (opening it first if needed).
    Client {
        context: String,
        reply: Reply<Result<Client, ApiError>>,
    },
    /// The current state of a context (opening it if needed).
    State {
        context: String,
        reply: Reply<Result<ContextStateDto, ApiError>>,
    },
    /// The user asks for the daemon (`ipc_start`). `initialize`: also on a root that was
    /// never initialized (onboarding finished).
    Start {
        context: String,
        initialize: bool,
        reply: Reply<Result<ContextStateDto, ApiError>>,
    },
    /// Closes every context (app exit, tests).
    Close { reply: Reply<()> },
    // Internal: results of the actor's own tasks. `open` names the open instance so
    // stale reports (a context reopened since) are ignored.
    Opened {
        context: String,
        open: u64,
        result: Result<Opened, ApiError>,
    },
    ClientState {
        context: String,
        open: u64,
        state: ConnectionState,
    },
    Announced {
        context: String,
        open: u64,
        seq: u64,
        reason: String,
    },
    Check {
        context: String,
        open: u64,
        token: u64,
    },
    Checked {
        context: String,
        open: u64,
        token: u64,
        initialized: bool,
    },
    StartDone {
        context: String,
        open: u64,
        result: Result<StartOutcome, ApiError>,
    },
}

/// Who asked for a start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartKind {
    /// When the context opened.
    Initial,
    /// The policy, after a loss.
    Restart,
    /// `ipc_start`.
    User,
}

struct StartRun {
    kind: StartKind,
    waiters: Vec<Reply<Result<ContextStateDto, ApiError>>>,
}

enum Waiter {
    Client(Reply<Result<Client, ApiError>>),
    Start {
        initialize: bool,
        reply: Reply<Result<ContextStateDto, ApiError>>,
    },
}

struct Live {
    endpoint: Arc<dyn Endpoint>,
    client: Client,
    client_state: ConnectionState,
    initialized: bool,
    policy: AutoStartPolicy,
    start: Option<StartRun>,
    start_error: Option<ApiError>,
    /// Invalidates scheduled checks.
    check_token: u64,
    next_check: Option<Instant>,
    timer: Option<AbortHandle>,
    tasks: Vec<AbortHandle>,
    watcher: bool,
}

impl Live {
    fn cancel_check(&mut self) {
        self.check_token += 1;
        self.next_check = None;
        if let Some(timer) = self.timer.take() {
            timer.abort();
        }
    }

    fn shutdown(&mut self) {
        self.cancel_check();
        for task in self.tasks.drain(..) {
            task.abort();
        }
        self.client.close();
    }
}

enum Phase {
    /// Not open: never opened, failed to open, or closed.
    Closed { error: Option<ApiError>, final_: bool },
    Opening { waiters: Vec<Waiter> },
    Open(Box<Live>),
}

struct Entry {
    connector: Arc<dyn Connector>,
    open: u64,
    phase: Phase,
    version: u64,
    /// Last emitted state (version 0, no countdown) and the scheduled check behind it.
    last: Option<(ContextStateDto, Option<Instant>)>,
}

/// Where state changes go.
#[derive(Clone)]
pub struct Emitter {
    sink: Arc<dyn ConnectionSink>,
    states: broadcast::Sender<ContextStateDto>,
}

impl Emitter {
    pub fn new(sink: Arc<dyn ConnectionSink>, states: broadcast::Sender<ContextStateDto>) -> Self {
        Self { sink, states }
    }

    fn emit(&self, state: &ContextStateDto) {
        self.sink.connection_changed(state);
        let _ = self.states.send(state.clone());
    }
}

/// Options of the actor.
#[derive(Debug, Clone, Default)]
pub struct ContextsOptions {
    pub policy: PolicyConfig,
}

static OPEN_COUNTER: AtomicU64 = AtomicU64::new(1);

/// The actor.
pub struct Contexts {
    entries: HashMap<String, Entry>,
    emitter: Emitter,
    options: ContextsOptions,
}

impl Contexts {
    pub fn new(
        connectors: &[(String, Arc<dyn Connector>)],
        emitter: Emitter,
        options: ContextsOptions,
    ) -> Self {
        let entries = connectors
            .iter()
            .map(|(id, connector)| {
                (
                    id.clone(),
                    Entry {
                        connector: Arc::clone(connector),
                        open: 0,
                        phase: Phase::Closed {
                            error: None,
                            final_: false,
                        },
                        version: 0,
                        last: None,
                    },
                )
            })
            .collect();
        Self {
            entries,
            emitter,
            options,
        }
    }
}

/// Sends `msg` to the actor from one of its tasks; gives up when it is gone.
fn tell(weak: &WeakActorRef<Msg>, msg: Msg) {
    if let Some(actor) = weak.upgrade() {
        if let Err(err) = actor.tell(msg) {
            tracing::warn!(%err, "contexts actor did not take a message");
        }
    }
}

impl Actor for Contexts {
    type Msg = Msg;

    async fn handle(&mut self, msg: Msg, ctx: &mut Context<Self>) {
        let weak = ctx.myself_weak();
        match msg {
            Msg::Client { context, reply } => self.on_client(&context, reply, &weak),
            Msg::State { context, reply } => {
                let answer = self.state_of(&context, &weak);
                let _ = reply.send(answer);
            }
            Msg::Start {
                context,
                initialize,
                reply,
            } => self.on_start(&context, initialize, reply, &weak),
            Msg::Close { reply } => {
                self.close_all();
                let _ = reply.send(());
            }
            Msg::Opened {
                context,
                open,
                result,
            } => self.on_opened(&context, open, result, &weak),
            Msg::ClientState {
                context,
                open,
                state,
            } => self.on_client_state(&context, open, state, &weak),
            Msg::Announced {
                context,
                open,
                seq,
                reason,
            } => self.on_announced(&context, open, seq, &reason),
            Msg::Check {
                context,
                open,
                token,
            } => self.on_check(&context, open, token, &weak),
            Msg::Checked {
                context,
                open,
                token,
                initialized,
            } => self.on_checked(&context, open, token, initialized, &weak),
            Msg::StartDone {
                context,
                open,
                result,
            } => self.on_start_done(&context, open, result, &weak),
        }
    }

    async fn on_stop(&mut self, _ctx: &mut Context<Self>) {
        self.close_all();
    }
}

impl Contexts {
    fn on_client(
        &mut self,
        context: &str,
        reply: Reply<Result<Client, ApiError>>,
        weak: &WeakActorRef<Msg>,
    ) {
        let Some(entry) = self.entries.get_mut(context) else {
            let _ = reply.send(Err(errors::unknown_context(context)));
            return;
        };
        match &mut entry.phase {
            Phase::Open(live) => {
                let _ = reply.send(Ok(live.client.clone()));
            }
            Phase::Opening { waiters } => waiters.push(Waiter::Client(reply)),
            Phase::Closed { final_: true, .. } => {
                let _ = reply.send(Err(client_error(&ClientError::Closed, None)));
            }
            Phase::Closed { .. } => {
                Self::begin_open(context, entry, vec![Waiter::Client(reply)], weak);
                self.emit(context);
            }
        }
    }

    fn state_of(
        &mut self,
        context: &str,
        weak: &WeakActorRef<Msg>,
    ) -> Result<ContextStateDto, ApiError> {
        let Some(entry) = self.entries.get_mut(context) else {
            return Err(errors::unknown_context(context));
        };
        if matches!(entry.phase, Phase::Closed { final_: false, .. }) {
            Self::begin_open(context, entry, Vec::new(), weak);
            self.emit(context);
        }
        let entry = &self.entries[context];
        let (mut dto, next_check) = snapshot(context, entry);
        dto.version = entry.version;
        dto.autostart.next_restart_ms = countdown(next_check);
        Ok(dto)
    }

    fn on_start(
        &mut self,
        context: &str,
        initialize: bool,
        reply: Reply<Result<ContextStateDto, ApiError>>,
        weak: &WeakActorRef<Msg>,
    ) {
        let Some(entry) = self.entries.get_mut(context) else {
            let _ = reply.send(Err(errors::unknown_context(context)));
            return;
        };
        match &mut entry.phase {
            Phase::Open(_) => {
                self.user_start(context, initialize, reply, weak);
            }
            Phase::Opening { waiters } => waiters.push(Waiter::Start { initialize, reply }),
            Phase::Closed { final_: true, .. } => {
                let _ = reply.send(Err(client_error(&ClientError::Closed, None)));
            }
            Phase::Closed { .. } => {
                Self::begin_open(
                    context,
                    entry,
                    vec![Waiter::Start { initialize, reply }],
                    weak,
                );
                self.emit(context);
            }
        }
    }

    fn begin_open(
        context: &str,
        entry: &mut Entry,
        waiters: Vec<Waiter>,
        weak: &WeakActorRef<Msg>,
    ) {
        let open = OPEN_COUNTER.fetch_add(1, Ordering::Relaxed);
        entry.open = open;
        entry.phase = Phase::Opening { waiters };
        let opening = entry.connector.open();
        let weak = weak.clone();
        let context = context.to_owned();
        tokio::spawn(async move {
            let result = opening.await;
            tell(
                &weak,
                Msg::Opened {
                    context,
                    open,
                    result,
                },
            );
        });
    }

    fn on_opened(
        &mut self,
        context: &str,
        open: u64,
        result: Result<Opened, ApiError>,
        weak: &WeakActorRef<Msg>,
    ) {
        let policy_config = self.options.policy;
        let Some(entry) = self.entries.get_mut(context) else {
            return;
        };
        if entry.open != open || !matches!(entry.phase, Phase::Opening { .. }) {
            if let Ok(opened) = result {
                opened.endpoint.client().close();
            }
            return;
        }
        let Phase::Opening { waiters } = std::mem::replace(
            &mut entry.phase,
            Phase::Closed {
                error: None,
                final_: false,
            },
        ) else {
            return;
        };
        let opened = match result {
            Ok(opened) => opened,
            Err(error) => {
                tracing::warn!(context, %error, "context could not be opened");
                entry.phase = Phase::Closed {
                    error: Some(error.clone()),
                    final_: false,
                };
                for waiter in waiters {
                    match waiter {
                        Waiter::Client(reply) => {
                            let _ = reply.send(Err(error.clone()));
                        }
                        Waiter::Start { reply, .. } => {
                            let _ = reply.send(Err(error.clone()));
                        }
                    }
                }
                self.emit(context);
                return;
            }
        };
        let client = opened.endpoint.client();
        let forwarder = spawn_state_forwarder(context, open, &client, weak);
        let mut live = Box::new(Live {
            endpoint: Arc::clone(&opened.endpoint),
            client: client.clone(),
            client_state: client.state().borrow().clone(),
            initialized: opened.initialized,
            policy: AutoStartPolicy::new(policy_config),
            start: None,
            start_error: None,
            check_token: 0,
            next_check: None,
            timer: None,
            tasks: vec![forwarder],
            watcher: false,
        });
        // `keep_connected`: connects now and keeps reconnecting with backoff.
        client.start();
        if live.policy.on_open(live.initialized) == Decision::Start {
            begin_start(context, open, &mut live, StartKind::Initial, Vec::new(), weak);
        }
        entry.phase = Phase::Open(live);
        let mut starts = Vec::new();
        for waiter in waiters {
            match waiter {
                Waiter::Client(reply) => {
                    let _ = reply.send(Ok(client.clone()));
                }
                Waiter::Start { initialize, reply } => starts.push((initialize, reply)),
            }
        }
        for (initialize, reply) in starts {
            self.user_start(context, initialize, reply, weak);
        }
        self.emit(context);
    }

    fn user_start(
        &mut self,
        context: &str,
        initialize: bool,
        reply: Reply<Result<ContextStateDto, ApiError>>,
        weak: &WeakActorRef<Msg>,
    ) {
        let Some(entry) = self.entries.get_mut(context) else {
            let _ = reply.send(Err(errors::unknown_context(context)));
            return;
        };
        let open = entry.open;
        let Phase::Open(live) = &mut entry.phase else {
            let _ = reply.send(Err(client_error(&ClientError::Closed, None)));
            return;
        };
        if live.client_state.is_connected() {
            let _ = reply.send(Ok(current(context, entry)));
            return;
        }
        if let Some(run) = live.start.as_mut() {
            run.waiters.push(reply);
            return;
        }
        if !live.initialized && !initialize {
            let root = live.endpoint.describe().unwrap_or_default();
            let _ = reply.send(Err(errors::root_uninitialized(&root)));
            return;
        }
        live.policy.on_user_start();
        live.cancel_check();
        let require_initialized = !initialize;
        begin_start_with(
            context,
            open,
            live,
            StartKind::User,
            vec![reply],
            require_initialized,
            weak,
        );
        self.emit(context);
    }

    fn on_client_state(
        &mut self,
        context: &str,
        open: u64,
        state: ConnectionState,
        weak: &WeakActorRef<Msg>,
    ) {
        let Some(live) = self.live(context, open) else {
            return;
        };
        let was_connected = live.client_state.is_connected();
        let now_connected = state.is_connected();
        let closed = state == ConnectionState::Closed;
        live.client_state = state;
        let now = Instant::now();
        if now_connected {
            if !was_connected {
                live.policy.on_connected(now);
                live.start_error = None;
                live.initialized = true;
                live.cancel_check();
            }
            if !live.watcher {
                live.watcher = true;
                let task = spawn_shutdown_watcher(context, open, &live.client, weak);
                live.tasks.push(task);
            }
        } else if was_connected && !closed {
            live.policy.on_lost(now);
            let grace = live.policy.config().grace;
            schedule_check(context, open, live, now + grace, weak);
        }
        self.emit(context);
    }

    fn on_announced(&mut self, context: &str, open: u64, seq: u64, reason: &str) {
        let Some(live) = self.live(context, open) else {
            return;
        };
        // Replayed from before the current connection (the daemon persists its events):
        // only an announcement newer than this connection's hello counts.
        if let ConnectionState::Connected { hello, .. } = &live.client_state {
            if seq <= hello.last_event_seq {
                return;
            }
        }
        tracing::info!(context, reason, "daemon announced its shutdown");
        live.policy.on_announced(reason, Instant::now());
        self.emit(context);
    }

    fn on_check(&mut self, context: &str, open: u64, token: u64, weak: &WeakActorRef<Msg>) {
        let Some(live) = self.live(context, open) else {
            return;
        };
        if token != live.check_token || live.client_state.is_connected() || live.start.is_some() {
            return;
        }
        live.timer = None;
        let initialized = live.endpoint.initialized();
        let weak = weak.clone();
        let context = context.to_owned();
        tokio::spawn(async move {
            let initialized = initialized.await;
            tell(
                &weak,
                Msg::Checked {
                    context,
                    open,
                    token,
                    initialized,
                },
            );
        });
    }

    fn on_checked(
        &mut self,
        context: &str,
        open: u64,
        token: u64,
        initialized: bool,
        weak: &WeakActorRef<Msg>,
    ) {
        let Some(live) = self.live(context, open) else {
            return;
        };
        if token != live.check_token || live.client_state.is_connected() || live.start.is_some() {
            return;
        }
        live.initialized = initialized;
        live.next_check = None;
        match live.policy.decide(initialized, Instant::now()) {
            Decision::Start => {
                tracing::info!(context, "restarting the daemon after an unexpected loss");
                begin_start(context, open, live, StartKind::Restart, Vec::new(), weak);
            }
            Decision::CheckAt(at) => schedule_check(context, open, live, at, weak),
            Decision::Uninitialized | Decision::StoppedOnPurpose => {}
        }
        self.emit(context);
    }

    fn on_start_done(
        &mut self,
        context: &str,
        open: u64,
        result: Result<StartOutcome, ApiError>,
        weak: &WeakActorRef<Msg>,
    ) {
        let Some(live) = self.live(context, open) else {
            return;
        };
        let Some(run) = live.start.take() else {
            return;
        };
        let now = Instant::now();
        match &result {
            Ok(outcome) => {
                live.initialized = true;
                live.start_error = None;
                if run.kind == StartKind::Restart {
                    live.policy.on_restart_succeeded(*outcome);
                }
                // Connect the context's client now instead of at its next backoff tick.
                let client = live.client.clone();
                tokio::spawn(async move {
                    let _ = client.ping().await;
                });
            }
            Err(error) => {
                tracing::warn!(context, %error, kind = ?run.kind, "daemon start failed");
                live.start_error = Some(error.clone());
                if error.reason() == Some("root_uninitialized") {
                    live.initialized = false;
                }
                match run.kind {
                    StartKind::Initial => live.policy.on_initial_start_failed(now),
                    StartKind::Restart | StartKind::User => {}
                }
                // Automatic starts keep following the policy (spaced retries).
                if run.kind != StartKind::User && !live.client_state.is_connected() {
                    schedule_check(context, open, live, now, weak);
                }
            }
        }
        let Some(entry) = self.entries.get(context) else {
            return;
        };
        let answer = result.map(|_| current(context, entry));
        for waiter in run.waiters {
            let _ = waiter.send(answer.clone());
        }
        self.emit(context);
    }

    fn close_all(&mut self) {
        let ids: Vec<String> = self.entries.keys().cloned().collect();
        for id in ids {
            let Some(entry) = self.entries.get_mut(&id) else {
                continue;
            };
            let previous = std::mem::replace(
                &mut entry.phase,
                Phase::Closed {
                    error: None,
                    final_: true,
                },
            );
            match previous {
                Phase::Open(mut live) => {
                    live.shutdown();
                    let closed = Err(client_error(&ClientError::Closed, None));
                    if let Some(run) = live.start.take() {
                        for waiter in run.waiters {
                            let _ = waiter.send(closed.clone());
                        }
                    }
                }
                Phase::Opening { waiters } => {
                    for waiter in waiters {
                        let closed = client_error(&ClientError::Closed, None);
                        match waiter {
                            Waiter::Client(reply) => {
                                let _ = reply.send(Err(closed));
                            }
                            Waiter::Start { reply, .. } => {
                                let _ = reply.send(Err(closed));
                            }
                        }
                    }
                }
                Phase::Closed { .. } => {}
            }
            self.emit(&id);
        }
    }

    fn live(&mut self, context: &str, open: u64) -> Option<&mut Live> {
        let entry = self.entries.get_mut(context)?;
        if entry.open != open {
            return None;
        }
        match &mut entry.phase {
            Phase::Open(live) => Some(live),
            _ => None,
        }
    }

    /// Emits the state of `context` if it changed.
    fn emit(&mut self, context: &str) {
        let Some(entry) = self.entries.get_mut(context) else {
            return;
        };
        let key = snapshot(context, entry);
        if entry.last.as_ref() == Some(&key) {
            return;
        }
        entry.version += 1;
        let (mut dto, next_check) = key.clone();
        entry.last = Some(key);
        dto.version = entry.version;
        dto.autostart.next_restart_ms = countdown(next_check);
        self.emitter.emit(&dto);
    }
}

fn countdown(at: Option<Instant>) -> Option<u64> {
    at.map(|at| {
        u64::try_from(at.saturating_duration_since(Instant::now()).as_millis()).unwrap_or(u64::MAX)
    })
}

/// The state as of now, with its version.
fn current(context: &str, entry: &Entry) -> ContextStateDto {
    let (mut dto, next_check) = snapshot(context, entry);
    dto.version = entry.version;
    dto.autostart.next_restart_ms = countdown(next_check);
    dto
}

/// The state without version and countdown (for change detection), and the scheduled check.
fn snapshot(context: &str, entry: &Entry) -> (ContextStateDto, Option<Instant>) {
    let mut dto = ContextStateDto {
        context: context.to_owned(),
        status: ConnectionStatus::Idle,
        version: 0,
        hello: None,
        generation: None,
        attempt: 0,
        next_retry_ms: 0,
        last_error: None,
        endpoint: None,
        autostart: AutoStartDto {
            initialized: false,
            stopped_reason: None,
            next_restart_ms: None,
            restarts: 0,
        },
    };
    match &entry.phase {
        Phase::Closed { error, final_ } => {
            dto.status = if *final_ {
                ConnectionStatus::Closed
            } else {
                ConnectionStatus::Idle
            };
            dto.last_error.clone_from(error);
            (dto, None)
        }
        Phase::Opening { .. } => {
            dto.status = ConnectionStatus::Connecting;
            (dto, None)
        }
        Phase::Open(live) => {
            dto.endpoint = live.endpoint.describe();
            dto.autostart.initialized = live.initialized;
            dto.autostart.stopped_reason = live.policy.stopped_reason().map(str::to_owned);
            dto.autostart.restarts = live.policy.restarts();
            let mut client_error_now = None;
            dto.status = match &live.client_state {
                ConnectionState::Idle => ConnectionStatus::Idle,
                ConnectionState::Connecting => ConnectionStatus::Connecting,
                ConnectionState::Connected { hello, generation } => {
                    dto.hello = Some((**hello).clone());
                    dto.generation = Some(*generation);
                    ConnectionStatus::Connected
                }
                ConnectionState::Reconnecting {
                    attempt,
                    next_retry_ms,
                    last_error,
                } => {
                    dto.attempt = *attempt;
                    dto.next_retry_ms = *next_retry_ms;
                    client_error_now = Some(client_error(last_error, None));
                    ConnectionStatus::Reconnecting
                }
                ConnectionState::Closed => ConnectionStatus::Closed,
            };
            if !matches!(
                dto.status,
                ConnectionStatus::Connected | ConnectionStatus::Closed
            ) {
                if live.start.is_some() {
                    dto.status = ConnectionStatus::Starting;
                } else if !live.initialized {
                    dto.status = ConnectionStatus::Uninitialized;
                } else if dto.autostart.stopped_reason.is_some() {
                    dto.status = ConnectionStatus::Stopped;
                }
            }
            if dto.status != ConnectionStatus::Connected {
                dto.last_error = live.start_error.clone().or(client_error_now);
            }
            (dto, live.next_check)
        }
    }
}

fn begin_start(
    context: &str,
    open: u64,
    live: &mut Live,
    kind: StartKind,
    waiters: Vec<Reply<Result<ContextStateDto, ApiError>>>,
    weak: &WeakActorRef<Msg>,
) {
    begin_start_with(context, open, live, kind, waiters, true, weak);
}

fn begin_start_with(
    context: &str,
    open: u64,
    live: &mut Live,
    kind: StartKind,
    waiters: Vec<Reply<Result<ContextStateDto, ApiError>>>,
    require_initialized: bool,
    weak: &WeakActorRef<Msg>,
) {
    live.cancel_check();
    live.start_error = None;
    live.start = Some(StartRun { kind, waiters });
    let starting = live.endpoint.start(require_initialized);
    let weak = weak.clone();
    let context = context.to_owned();
    tokio::spawn(async move {
        let result = starting.await;
        tell(
            &weak,
            Msg::StartDone {
                context,
                open,
                result,
            },
        );
    });
}

fn schedule_check(
    context: &str,
    open: u64,
    live: &mut Live,
    at: Instant,
    weak: &WeakActorRef<Msg>,
) {
    live.cancel_check();
    let token = live.check_token;
    live.next_check = Some(at);
    let weak = weak.clone();
    let context = context.to_owned();
    let timer = tokio::spawn(async move {
        tokio::time::sleep_until(at).await;
        tell(
            &weak,
            Msg::Check {
                context,
                open,
                token,
            },
        );
    });
    live.timer = Some(timer.abort_handle());
}

/// Forwards every change of the client's `ConnectionState` to the actor.
fn spawn_state_forwarder(
    context: &str,
    open: u64,
    client: &Client,
    weak: &WeakActorRef<Msg>,
) -> AbortHandle {
    let mut state = client.state();
    let weak = weak.clone();
    let context = context.to_owned();
    tokio::spawn(async move {
        loop {
            let current = state.borrow_and_update().clone();
            let closed = current == ConnectionState::Closed;
            let Some(actor) = weak.upgrade() else {
                return;
            };
            let mut msg = Msg::ClientState {
                context: context.clone(),
                open,
                state: current,
            };
            // A full mailbox only delays the report; the latest state wins.
            loop {
                match actor.tell(msg) {
                    Ok(()) => break,
                    Err(err) if err.is_full() => {
                        msg = err.into_inner();
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        if let Msg::ClientState { state: s, .. } = &mut msg {
                            *s = state.borrow_and_update().clone();
                        }
                    }
                    Err(_) => return,
                }
            }
            drop(actor);
            if closed || state.changed().await.is_err() {
                return;
            }
        }
    })
    .abort_handle()
}

/// Watches `daemon.shutdown` on the context's client: the reason decides whether a loss
/// that follows is deliberate. The subscription resumes across reconnections by itself.
fn spawn_shutdown_watcher(
    context: &str,
    open: u64,
    client: &Client,
    weak: &WeakActorRef<Msg>,
) -> AbortHandle {
    let client = client.clone();
    let weak = weak.clone();
    let context = context.to_owned();
    tokio::spawn(async move {
        let mut state = client.state();
        let mut subscription = loop {
            let params = json!({ "filter": { "kinds": ["daemon.shutdown"] } });
            match client.subscribe_raw("events.subscribe", params).await {
                Ok(subscription) => break subscription,
                Err(ClientError::Closed) => return,
                Err(err) => {
                    tracing::debug!(%err, "shutdown watcher could not subscribe yet");
                    // Try again on the next connection.
                    loop {
                        if state.changed().await.is_err() {
                            return;
                        }
                        let now = state.borrow_and_update().clone();
                        if now == ConnectionState::Closed {
                            return;
                        }
                        if now.is_connected() {
                            break;
                        }
                    }
                }
            }
        };
        while let Some(item) = subscription.next_item().await {
            let SubscriptionItem::Item(envelope) = item else {
                continue;
            };
            let event = &envelope["event"];
            if event["kind"] != "daemon.shutdown" {
                continue;
            }
            let reason = event["payload"]["reason"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let seq = envelope["seq"].as_u64().unwrap_or(0);
            tell(
                &weak,
                Msg::Announced {
                    context: context.clone(),
                    open,
                    seq,
                    reason,
                },
            );
        }
    })
    .abort_handle()
}

/// Client options of the desktop app: keep connected, short reconnection backoff.
pub fn desktop_client_options() -> ClientOptions {
    ClientOptions {
        keep_connected: true,
        reconnect_max: Duration::from_secs(2),
        ..ClientOptions::default()
    }
}
