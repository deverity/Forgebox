//! What GUIs need from the client (M2-E1-T2): the connection state as a `watch`
//! (connect, loss, reconnection, close), untyped resumable subscriptions
//! (`subscribe_raw`) and credentials read again on every connection attempt (a token
//! written after the client was created, or rotated by a daemon restart).

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use common::{
    SwitchableTransport, TestServer, bus, eventually, fast_options, spawn_on, spawn_with_token,
};
use forge_core::event::{EventBus, EventKind};
use forge_core::paths::ForgeRoot;
use forge_ipc::api::NoParams;
use forge_ipc::auth::IpcToken;
use forge_ipc::client::{ConnectionState, CredentialsProvider, Subscription};
use forge_ipc::server::EventSource;
use forge_ipc::transport::{InMemoryTransport, Transport};
use forge_ipc::{Client, ClientError, ClientOptions, Credentials};

const WAIT: Duration = Duration::from_secs(5);

fn kind(state: &ConnectionState) -> &'static str {
    match state {
        ConnectionState::Idle => "idle",
        ConnectionState::Connecting => "connecting",
        ConnectionState::Connected { .. } => "connected",
        ConnectionState::Reconnecting { .. } => "reconnecting",
        ConnectionState::Closed => "closed",
    }
}

/// Every state a `watch` subscriber saw, in order.
#[derive(Clone, Default)]
struct StateLog(Arc<Mutex<Vec<ConnectionState>>>);

impl StateLog {
    fn record(client: &Client) -> Self {
        let log = Self::default();
        let mut state = client.state();
        let sink = log.clone();
        tokio::spawn(async move {
            loop {
                let current = state.borrow_and_update().clone();
                let closed = current == ConnectionState::Closed;
                sink.0.lock().unwrap().push(current);
                if closed || state.changed().await.is_err() {
                    return;
                }
            }
        });
        log
    }

    fn all(&self) -> Vec<ConnectionState> {
        self.0.lock().unwrap().clone()
    }

    fn last(&self) -> Option<ConnectionState> {
        self.0.lock().unwrap().last().cloned()
    }

    /// Kinds in order, consecutive repeats collapsed (`Reconnecting` changes its attempt
    /// counter while it retries).
    fn kinds(&self) -> Vec<&'static str> {
        let mut kinds: Vec<&'static str> = self.all().iter().map(kind).collect();
        kinds.dedup();
        kinds
    }

    async fn wait_last(&self, what: &str, check: impl Fn(&ConnectionState) -> bool) {
        assert!(
            eventually(WAIT, || self.last().as_ref().is_some_and(&check)).await,
            "state {what} expected, log: {:?}",
            self.all()
        );
    }
}

fn gui_options() -> ClientOptions {
    ClientOptions {
        keep_connected: true,
        ..fast_options()
    }
}

async fn events_server(
    transport: InMemoryTransport,
    token: IpcToken,
    bus: &EventBus,
) -> TestServer {
    let source = EventSource::new(bus.clone());
    spawn_with_token(transport, token, |b| b.events(source)).await
}

/// The daemon goes away: no listener, every connection closed.
fn stop(server: &TestServer, switch: &SwitchableTransport) {
    switch.unplug();
    server.handle.shutdown();
    server.handle.disconnect_all();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_state_follows_connect_loss_return_and_close() {
    let bus = bus(1024);
    let token = IpcToken::generate().unwrap();
    let first = events_server(InMemoryTransport::new(), token.clone(), &bus).await;
    let switch = SwitchableTransport::new();
    switch.point_to(&first);
    switch.set_gate(false);

    let client = Client::new(
        Arc::clone(&switch) as Arc<dyn Transport>,
        Credentials::new(token.clone(), "forgebox-desktop", "0.0.0"),
        gui_options(),
    );
    assert_eq!(*client.state().borrow(), ConnectionState::Idle);
    let log = StateLog::record(&client);

    // 1. Connecting: the attempt is held at the transport.
    client.start();
    log.wait_last("connecting", |s| *s == ConnectionState::Connecting)
        .await;

    // 2. Connected.
    switch.set_gate(true);
    log.wait_last(
        "connected",
        |s| matches!(s, ConnectionState::Connected { generation: 1, hello } if hello.local),
    )
    .await;
    let rtt = client.ping().await.unwrap();
    assert_eq!(
        client.last_rtt().map(|d| d.as_micros()),
        Some(rtt.as_micros().max(1))
    );

    // 3. The daemon stops: reconnecting, retrying with backoff while it is gone.
    stop(&first, &switch);
    log.wait_last("reconnecting with failed attempts", |s| {
        matches!(
            s,
            ConnectionState::Reconnecting {
                attempt,
                last_error: ClientError::Transport(_),
                ..
            } if *attempt >= 2
        )
    })
    .await;
    assert!(
        log.all().iter().any(|s| matches!(
            s,
            ConnectionState::Reconnecting { next_retry_ms, .. } if *next_retry_ms > 0
        )),
        "the next retry is announced: {:?}",
        log.all()
    );

    // 4. The daemon is back (a new server on the same endpoint): connected again.
    let second = events_server(InMemoryTransport::new(), token.clone(), &bus).await;
    switch.point_to(&second);
    log.wait_last(
        "connected again",
        |s| matches!(s, ConnectionState::Connected { generation, .. } if *generation > 1),
    )
    .await;
    client.system_ping(NoParams {}).await.unwrap();

    // 5. Closed, for good.
    client.close();
    log.wait_last("closed", |s| *s == ConnectionState::Closed)
        .await;
    assert_eq!(
        client.system_ping(NoParams {}).await.unwrap_err(),
        ClientError::Closed
    );
    assert_eq!(
        log.kinds(),
        [
            "idle",
            "connecting",
            "connected",
            "reconnecting",
            "connected",
            "closed"
        ]
    );
}

#[tokio::test]
async fn without_keep_connected_a_loss_is_idle_and_the_next_call_reconnects() {
    let server = spawn_on(InMemoryTransport::new(), |b| b).await;
    let client = server.client();
    let mut state = client.state();
    client.system_ping(NoParams {}).await.unwrap();
    assert!(matches!(
        &*state.borrow_and_update(),
        ConnectionState::Connected { generation: 1, .. }
    ));

    server.handle.disconnect_all();
    let idle = tokio::time::timeout(WAIT, state.wait_for(|s| *s == ConnectionState::Idle))
        .await
        .map(|found| found.is_ok());
    assert_eq!(idle, Ok(true), "idle after the loss: {:?}", *state.borrow());

    client.system_ping(NoParams {}).await.unwrap();
    assert!(matches!(
        &*state.borrow_and_update(),
        ConnectionState::Connected { generation: 2, .. }
    ));

    // Dropping the last handle closes it too.
    drop(client);
    assert_eq!(*state.borrow(), ConnectionState::Closed);
}

#[tokio::test]
async fn a_failed_first_connection_without_keep_connected_goes_back_to_idle() {
    let switch = SwitchableTransport::new();
    let client = Client::new(
        Arc::clone(&switch) as Arc<dyn Transport>,
        Credentials::new(IpcToken::generate().unwrap(), "forge", "0"),
        fast_options(),
    );
    let err = client.system_ping(NoParams {}).await.unwrap_err();
    assert!(matches!(err, ClientError::Transport(_)), "{err:?}");
    assert_eq!(*client.state().borrow(), ConnectionState::Idle);
}

// ------------------------------------------------------------------ subscribe_raw

fn publish(bus: &EventBus, kind: EventKind, n: u64) {
    bus.publish(kind, json!({ "n": n }));
}

async fn next_raw(sub: &mut Subscription<Value>) -> Value {
    tokio::time::timeout(WAIT, sub.next())
        .await
        .expect("item in time")
        .expect("stream open")
}

async fn assert_quiet(sub: &mut Subscription<Value>) {
    assert!(
        tokio::time::timeout(Duration::from_millis(150), sub.next())
            .await
            .is_err(),
        "no further item expected"
    );
}

async fn wait_resubscribed(sub: &Subscription<Value>, before: forge_core::ids::SubscriptionId) {
    assert!(
        eventually(WAIT, || sub
            .subscription_id()
            .is_some_and(|id| id != before))
        .await,
        "resubscribed on a new connection"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscribe_raw_resumes_across_a_forced_disconnect_exactly_once() {
    let bus = bus(1024);
    let server = spawn_on(InMemoryTransport::new(), |b| {
        b.events(EventSource::new(bus.clone()))
    })
    .await;
    let client = server.client();
    let mut state = client.state();
    let mut sub = client
        .subscribe_raw(
            "events.subscribe",
            json!({ "filter": { "kinds": ["service.*"] } }),
        )
        .await
        .unwrap();
    for n in 1..=3 {
        publish(&bus, EventKind::ServiceRunning, n);
        publish(&bus, EventKind::JobProgress, n); // filtered out
    }
    let mut seqs = Vec::new();
    for n in 1..=3 {
        let item = next_raw(&mut sub).await;
        assert_eq!(item["event"]["payload"]["n"], n);
        assert_eq!(item["event"]["kind"], "service.running");
        assert!(item["subscription_id"].is_string());
        seqs.push(item["seq"].as_u64().unwrap());
    }

    // Server-side drop; events published meanwhile.
    let before = sub.subscription_id().unwrap();
    server.handle.disconnect_all();
    for n in 4..=6 {
        publish(&bus, EventKind::ServiceStopped, n);
        publish(&bus, EventKind::JobProgress, n);
    }
    wait_resubscribed(&sub, before).await;
    // Client-side drop as well (a "reconnect" button).
    let before = sub.subscription_id().unwrap();
    client.disconnect();
    publish(&bus, EventKind::ServiceStarting, 7);
    wait_resubscribed(&sub, before).await;
    publish(&bus, EventKind::ServiceRunning, 8);

    for n in 4..=8 {
        let item = next_raw(&mut sub).await;
        assert_eq!(item["event"]["payload"]["n"], n, "exactly once, in order");
        seqs.push(item["seq"].as_u64().unwrap());
    }
    assert_quiet(&mut sub).await;
    assert_eq!(sub.missed(), 0);
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    // The subscription kept the client reconnecting: it is connected again.
    assert!(state.borrow_and_update().is_connected());
}

// Current-thread runtime: nothing runs between `disconnect_all` and the publications,
// so they certainly happen while the client is disconnected.
#[tokio::test]
async fn subscribe_raw_resumes_from_the_floor_before_the_first_item() {
    let bus = bus(1024);
    let server = spawn_on(InMemoryTransport::new(), |b| {
        b.events(EventSource::new(bus.clone()))
    })
    .await;
    for n in 1..=3 {
        publish(&bus, EventKind::JobProgress, n);
    }
    let client = server.client();
    let mut sub = client
        .subscribe_raw("events.subscribe", Value::Null)
        .await
        .unwrap();
    assert_eq!((sub.last_seq(), sub.resume_point()), (None, Some(3)));
    let before = sub.subscription_id().unwrap();
    server.handle.disconnect_all();
    publish(&bus, EventKind::DaemonReady, 4);
    publish(&bus, EventKind::ServiceRunning, 5);
    wait_resubscribed(&sub, before).await;
    publish(&bus, EventKind::ServiceStopped, 6);
    for seq in 4..=6u64 {
        assert_eq!(next_raw(&mut sub).await["seq"], seq);
    }
    assert_quiet(&mut sub).await;
}

#[tokio::test]
async fn subscribe_raw_refuses_what_is_not_a_stream_before_sending_anything() {
    let server = spawn_on(InMemoryTransport::new(), |b| b).await;
    let client = server.client();
    for (method, params) in [
        ("nope.subscribe", json!({})),
        ("system.ping", json!({})),
        ("sites.add", json!({})),
        ("events.unsubscribe", json!({})),
        ("events.subscribe", json!([1, 2])),
        ("events.subscribe", json!("filter")),
    ] {
        let err = client.subscribe_raw(method, params).await.unwrap_err();
        assert!(
            matches!(err, ClientError::InvalidInput(_)),
            "{method}: {err:?}"
        );
        assert_eq!(err.code(), "invalid_input");
        assert_eq!(err.exit_code(), 2);
    }
    assert_eq!(server.handle.connection_count(), 0, "nothing was sent");
    assert_eq!(*client.state().borrow(), ConnectionState::Idle);
}

// ------------------------------------------------------------------ credentials

/// Writes `state/ipc.token` the way the daemon does (temp file, then rename), so a
/// reader never sees half a token.
fn write_token(root: &ForgeRoot, token: &IpcToken) {
    let path = root.ipc_token();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, token.secret()).unwrap();
    std::fs::rename(&tmp, &path).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_token_written_after_the_client_was_created_is_used_on_the_next_reconnect() {
    let dir = tempfile::tempdir().unwrap();
    let root = ForgeRoot::new(dir.path()).unwrap();
    let bus = bus(1024);
    let token = IpcToken::generate().unwrap();
    let first = events_server(InMemoryTransport::new(), token.clone(), &bus).await;
    let switch = SwitchableTransport::new();
    switch.point_to(&first);

    // First boot: the client exists before the daemon wrote its token.
    assert!(!root.ipc_token().exists());
    let client = Client::new(
        Arc::clone(&switch) as Arc<dyn Transport>,
        CredentialsProvider::local(&root, "forgebox-desktop", "0.0.0"),
        gui_options(),
    );
    let log = StateLog::record(&client);
    client.start();
    log.wait_last("reconnecting without a token", |s| {
        matches!(
            s,
            ConnectionState::Reconnecting {
                attempt,
                last_error: ClientError::Credentials(api),
                ..
            } if *attempt >= 1 && api.code == "not_found"
        )
    })
    .await;
    assert_eq!(
        first.handle.connection_count(),
        0,
        "no hello without a token"
    );

    // A stale token (another daemon's) is refused...
    write_token(&root, &IpcToken::generate().unwrap());
    log.wait_last("reconnecting with a stale token", |s| {
        matches!(
            s,
            ConnectionState::Reconnecting { last_error: ClientError::Api(api), .. }
                if api.code == "unauthorized"
        )
    })
    .await;

    // ...and the right one, written now, authenticates the next attempt.
    write_token(&root, &token);
    log.wait_last("connected", |s| s.is_connected()).await;
    let mut sub = client
        .subscribe_events(Default::default(), None)
        .await
        .unwrap();
    publish(&bus, EventKind::ServiceRunning, 1);
    let first_seq = tokio::time::timeout(WAIT, sub.next())
        .await
        .unwrap()
        .unwrap()
        .seq;

    // The daemon restarts with a new token: the subscription resumes on it.
    stop(&first, &switch);
    log.wait_last("reconnecting after the restart", |s| {
        matches!(s, ConnectionState::Reconnecting { .. })
    })
    .await;
    publish(&bus, EventKind::ServiceStopped, 2);
    let rotated = IpcToken::generate().unwrap();
    let second = events_server(InMemoryTransport::new(), rotated.clone(), &bus).await;
    write_token(&root, &rotated);
    switch.point_to(&second);
    log.wait_last(
        "connected with the new token",
        |s| matches!(s, ConnectionState::Connected { generation, .. } if *generation > 1),
    )
    .await;
    let resumed = tokio::time::timeout(WAIT, sub.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed.seq, first_seq + 1);
    assert_eq!(resumed.event.payload["n"], 2);
    client.close();
}

#[tokio::test]
async fn a_provider_closure_is_asked_on_every_connection_attempt() {
    let server = spawn_on(InMemoryTransport::new(), |b| b).await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let token = server.token.clone();
    let counter = Arc::clone(&calls);
    let provider = CredentialsProvider::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let token = token.clone();
        async move { Ok(Credentials::new(token, "mcp", "0")) }
    });
    let client = Client::new(
        server.transport.clone() as Arc<dyn Transport>,
        provider,
        fast_options(),
    );
    client.system_ping(NoParams {}).await.unwrap();
    client.system_ping(NoParams {}).await.unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    client.disconnect();
    client.system_ping(NoParams {}).await.unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// A subscription opened while a `keep_connected` client makes its first connection is
/// subscribed once. The maintenance task restores subscriptions that are not attached to
/// the new connection; one whose own subscribe call is still in flight is not attached
/// yet and must be left to that call (M2-E1-T8: the desktop bridge saw every such stream
/// twice on the daemon, double traffic and phantom `lagged` counts).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_subscription_opened_during_the_first_connection_is_subscribed_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use forge_ipc::api::SubscriptionDto;
    use forge_ipc::api::methods::metrics_subscribe;
    use forge_ipc::server::Router;

    let calls = Arc::new(AtomicUsize::new(0));
    let mut router = Router::new();
    let counter = Arc::clone(&calls);
    router.handle::<metrics_subscribe, _, _>(move |_params, ctx| {
        counter.fetch_add(1, Ordering::SeqCst);
        async move {
            // A slow answer keeps the first subscribe call in flight while the
            // maintenance task restores.
            tokio::time::sleep(Duration::from_millis(150)).await;
            let sender = ctx.open_stream(None)?;
            Ok(SubscriptionDto {
                subscription_id: sender.id(),
                stream: "metrics".into(),
                last_seq: 0,
            })
        }
    });
    let token = IpcToken::generate().unwrap();
    let server = spawn_with_token(InMemoryTransport::new(), token.clone(), |b| {
        b.router(router)
    })
    .await;
    let switch = SwitchableTransport::new();
    switch.point_to(&server);
    switch.set_gate(false);
    let client = Client::new(
        Arc::clone(&switch) as Arc<dyn Transport>,
        Credentials::new(token, "forgebox-desktop", "0.0.0"),
        gui_options(),
    );
    // The subscribe call waits for the first connection...
    let opening = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .subscribe_raw("metrics.subscribe", json!({ "site": null }))
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    // ...which the maintenance task makes too.
    client.start();
    switch.set_gate(true);
    let sub = opening.await.unwrap().unwrap();
    assert!(sub.subscription_id().is_some());
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "subscribed once");
    assert!(client.state().borrow().is_connected());
    client.close();
}
