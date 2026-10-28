//! Helpers of the bridge tests: an in-memory forge-ipc server standing in for a daemon
//! (it can be "killed" and "started" through a switchable transport), a fake connector,
//! and sinks that record what the bridge emits.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::Value;

use forge_ipc::api::{HelloParams, Role};
use forge_ipc::auth::{Authenticator, IpcToken, TokenAuthenticator};
use forge_ipc::base::event::{EventBus, EventBusOptions, EventKind};
use forge_ipc::server::{EventSource, Router, Server, ServerHandle};
use forge_ipc::transport::{
    Connection, ConnectionInfo, InMemoryTransport, Listener, Transport, TransportError,
    TransportKind,
};
use forge_ipc::{ApiError, Client, ClientOptions, Credentials};
use forgebox_desktop::bridge::errors::root_uninitialized;
use forgebox_desktop::bridge::{
    Bridge, BridgeOptions, ConnectionSink, ConnectionStatus, Connector, ContextStateDto,
    ContextsOptions, Endpoint, FrameSink, LOCAL_CONTEXT, Opened, PolicyConfig, SinkClosed,
    StartOutcome, StreamEntry, StreamFrame, desktop_client_options,
};

pub const WAIT: Duration = Duration::from_secs(10);

/// Polls `check` every 10 ms until it holds or `timeout` passes.
pub async fn eventually(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    check()
}

/// Counts authenticated connections (one hello per connection).
pub struct CountingAuth {
    inner: TokenAuthenticator,
    pub hellos: AtomicUsize,
}

impl Authenticator for CountingAuth {
    fn authenticate(
        &self,
        hello: &HelloParams,
        connection: &ConnectionInfo,
    ) -> forge_ipc::base::Result<Role> {
        let role = self.inner.authenticate(hello, connection)?;
        self.hellos.fetch_add(1, Ordering::SeqCst);
        Ok(role)
    }
}

/// An in-memory "daemon": a forge-ipc server with an event bus.
pub struct TestDaemon {
    pub transport: Arc<InMemoryTransport>,
    pub handle: ServerHandle,
    pub token: IpcToken,
    pub bus: EventBus,
    pub auth: Arc<CountingAuth>,
}

impl TestDaemon {
    pub async fn spawn(router: Router) -> Self {
        Self::spawn_with(router, InMemoryTransport::new()).await
    }

    pub async fn spawn_with(router: Router, transport: InMemoryTransport) -> Self {
        let token = IpcToken::generate().unwrap();
        let bus = EventBus::with_options(EventBusOptions {
            capacity: 16_384,
            history: 16_384,
            next_seq: 1,
        });
        let auth = Arc::new(CountingAuth {
            inner: TokenAuthenticator::new(token.clone()),
            hellos: AtomicUsize::new(0),
        });
        let server = Server::builder(Arc::clone(&auth) as Arc<dyn Authenticator>)
            .router(router)
            .events(EventSource::new(bus.clone()))
            .build();
        let transport = Arc::new(transport);
        let (handle, _task) = server.bind_and_spawn(transport.as_ref()).await.unwrap();
        Self {
            transport,
            handle,
            token,
            bus,
            auth,
        }
    }

    pub fn hellos(&self) -> usize {
        self.auth.hellos.load(Ordering::SeqCst)
    }

    /// Publishes `daemon.shutdown {reason}` like the real daemon before it stops.
    pub fn announce_shutdown(&self, reason: &str) {
        self.bus.publish(
            EventKind::DaemonShutdown,
            serde_json::json!({ "v": 1, "reason": reason }),
        );
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

/// Client side of a daemon that can go away and come back.
pub struct Switch {
    target: Mutex<Option<Arc<InMemoryTransport>>>,
}

impl Switch {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            target: Mutex::new(None),
        })
    }

    pub fn plug(&self, daemon: &TestDaemon) {
        *self.target.lock().unwrap() = Some(Arc::clone(&daemon.transport));
    }

    /// The daemon "dies": nobody listens and every connection drops.
    pub fn kill(&self, daemon: &TestDaemon) {
        *self.target.lock().unwrap() = None;
        daemon.handle.disconnect_all();
    }

    pub fn is_plugged(&self) -> bool {
        self.target.lock().unwrap().is_some()
    }
}

impl Transport for Switch {
    fn kind(&self) -> TransportKind {
        TransportKind::InMemory
    }

    fn endpoint(&self) -> String {
        "memory:switch".into()
    }

    fn connect(&self) -> BoxFuture<'_, Result<Connection, TransportError>> {
        async move {
            let target = self.target.lock().unwrap().clone();
            match target {
                Some(transport) => transport.connect().await,
                None => Err(TransportError::Unavailable {
                    endpoint: self.endpoint(),
                    reason: "no daemon".into(),
                }),
            }
        }
        .boxed()
    }

    fn bind(&self) -> BoxFuture<'_, Result<Box<dyn Listener>, TransportError>> {
        async move {
            Err(TransportError::Unavailable {
                endpoint: self.endpoint(),
                reason: "client side only".into(),
            })
        }
        .boxed()
    }
}

/// What the fake connector's "daemon executable" does when started.
pub struct FakeLocal {
    pub switch: Arc<Switch>,
    pub daemon: Arc<TestDaemon>,
    pub initialized: AtomicBool,
    pub opens: AtomicU32,
    /// Daemons "spawned" (starts that found nobody answering).
    pub spawned: AtomicU32,
    /// Every start, including those that found the daemon running.
    pub starts: AtomicU32,
    client: OnceLock<Client>,
}

impl FakeLocal {
    pub fn new(daemon: Arc<TestDaemon>, initialized: bool, running: bool) -> Arc<Self> {
        let switch = Switch::new();
        if running {
            switch.plug(&daemon);
        }
        Arc::new(Self {
            switch,
            daemon,
            initialized: AtomicBool::new(initialized),
            opens: AtomicU32::new(0),
            spawned: AtomicU32::new(0),
            starts: AtomicU32::new(0),
            client: OnceLock::new(),
        })
    }

    pub fn kill(&self) {
        self.switch.kill(&self.daemon);
    }

    pub fn spawned(&self) -> u32 {
        self.spawned.load(Ordering::SeqCst)
    }
}

pub fn fast_client_options() -> ClientOptions {
    ClientOptions {
        call_timeout: Duration::from_secs(5),
        connect_timeout: Duration::from_secs(2),
        reconnect_min: Duration::from_millis(20),
        reconnect_max: Duration::from_millis(200),
        ..desktop_client_options()
    }
}

/// Connector over a [`FakeLocal`].
pub struct FakeConnector(pub Arc<FakeLocal>);

impl Connector for FakeConnector {
    fn open(&self) -> BoxFuture<'static, Result<Opened, ApiError>> {
        let local = Arc::clone(&self.0);
        async move {
            local.opens.fetch_add(1, Ordering::SeqCst);
            let client = local
                .client
                .get_or_init(|| {
                    Client::new(
                        Arc::clone(&local.switch) as Arc<dyn Transport>,
                        Credentials::new(local.daemon.token.clone(), "forgebox-desktop", "0.0.0"),
                        fast_client_options(),
                    )
                })
                .clone();
            let initialized = local.initialized.load(Ordering::SeqCst);
            let endpoint: Arc<dyn Endpoint> = Arc::new(FakeEndpoint {
                local: Arc::clone(&local),
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

struct FakeEndpoint {
    local: Arc<FakeLocal>,
    client: Client,
}

impl Endpoint for FakeEndpoint {
    fn client(&self) -> Client {
        self.client.clone()
    }

    fn describe(&self) -> Option<String> {
        Some("memory:fake-root".into())
    }

    fn initialized(&self) -> BoxFuture<'static, bool> {
        let initialized = self.local.initialized.load(Ordering::SeqCst);
        async move { initialized }.boxed()
    }

    fn start(
        &self,
        require_initialized: bool,
    ) -> BoxFuture<'static, Result<StartOutcome, ApiError>> {
        let local = Arc::clone(&self.local);
        async move {
            local.starts.fetch_add(1, Ordering::SeqCst);
            if require_initialized && !local.initialized.load(Ordering::SeqCst) {
                return Err(root_uninitialized("memory:fake-root"));
            }
            if local.switch.is_plugged() {
                return Ok(StartOutcome::AlreadyRunning);
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            local.switch.plug(&local.daemon);
            local.initialized.store(true, Ordering::SeqCst);
            local.spawned.fetch_add(1, Ordering::SeqCst);
            Ok(StartOutcome::Started)
        }
        .boxed()
    }
}

/// Records every emitted context state.
#[derive(Default)]
pub struct Recorder(Mutex<Vec<ContextStateDto>>);

impl ConnectionSink for Recorder {
    fn connection_changed(&self, state: &ContextStateDto) {
        self.0.lock().unwrap().push(state.clone());
    }
}

impl Recorder {
    pub fn all(&self) -> Vec<ContextStateDto> {
        self.0.lock().unwrap().clone()
    }

    pub fn last(&self) -> Option<ContextStateDto> {
        self.0.lock().unwrap().last().cloned()
    }

    /// Statuses in order, consecutive repeats collapsed.
    pub fn statuses(&self) -> Vec<ConnectionStatus> {
        let mut out: Vec<ConnectionStatus> = self.all().iter().map(|s| s.status).collect();
        out.dedup();
        out
    }

    pub async fn wait_status(&self, status: ConnectionStatus) {
        assert!(
            eventually(WAIT, || self.last().is_some_and(|s| s.status == status)).await,
            "status {status:?} expected; seen {:?}",
            self.statuses()
        );
    }
}

/// Bridge options with a short grace so loss handling is quick in tests.
pub fn test_options(grace: Duration) -> BridgeOptions {
    BridgeOptions {
        contexts: ContextsOptions {
            policy: PolicyConfig {
                grace,
                ..PolicyConfig::default()
            },
        },
        ..BridgeOptions::default()
    }
}

/// A bridge whose `local` context is `local`.
pub fn bridge_over(
    local: &Arc<FakeLocal>,
    recorder: &Arc<Recorder>,
    options: BridgeOptions,
) -> Bridge {
    let connector: Arc<dyn Connector> = Arc::new(FakeConnector(Arc::clone(local)));
    Bridge::with_connectors(
        vec![(LOCAL_CONTEXT.to_owned(), connector)],
        Arc::clone(recorder) as Arc<dyn ConnectionSink>,
        options,
    )
}

/// Collects frames; acknowledges them through the bridge while `acking`.
pub struct Frames {
    pub frames: Mutex<Vec<StreamFrame>>,
    pub bridge: OnceLock<Bridge>,
    pub acking: AtomicBool,
}

impl Frames {
    pub fn new(acking: bool) -> Arc<Self> {
        Arc::new(Self {
            frames: Mutex::new(Vec::new()),
            bridge: OnceLock::new(),
            acking: AtomicBool::new(acking),
        })
    }

    pub fn all(&self) -> Vec<StreamFrame> {
        self.frames.lock().unwrap().clone()
    }

    /// Acknowledges everything received so far.
    pub fn ack_all(&self) {
        let last = self
            .frames
            .lock()
            .unwrap()
            .last()
            .map(|f| (f.stream_id.clone(), f.frame));
        if let (Some((stream, frame)), Some(bridge)) = (last, self.bridge.get()) {
            bridge.ack(&stream, frame);
        }
    }

    pub fn entries(&self) -> Vec<StreamEntry> {
        self.all().into_iter().flat_map(|f| f.entries).collect()
    }

    /// `seq` of every item, in order.
    pub fn seqs(&self) -> Vec<u64> {
        self.entries()
            .iter()
            .filter_map(|e| match e {
                StreamEntry::Item { item } => item["seq"].as_u64(),
                _ => None,
            })
            .collect()
    }

    pub fn missed(&self) -> u64 {
        self.entries()
            .iter()
            .map(|e| match e {
                StreamEntry::Lagged { missed } => *missed,
                _ => 0,
            })
            .sum()
    }

    pub fn last_seq(&self) -> Option<u64> {
        self.seqs().last().copied()
    }
}

/// Sink wrapper (the trait is implemented on the `Arc`).
pub struct FramesSink(pub Arc<Frames>);

impl FrameSink for FramesSink {
    fn send(&self, frame: StreamFrame) -> Result<(), SinkClosed> {
        let ack = (frame.stream_id.clone(), frame.frame);
        self.0.frames.lock().unwrap().push(frame);
        if self.0.acking.load(Ordering::SeqCst) {
            if let Some(bridge) = self.0.bridge.get() {
                bridge.ack(&ack.0, ack.1);
            }
        }
        Ok(())
    }
}

pub fn item_value(entry: &StreamEntry) -> Option<&Value> {
    match entry {
        StreamEntry::Item { item } => Some(item),
        _ => None,
    }
}
