//! The bridge against a real forge-ipc `Server` on the in-memory transport (M2-E1-T8):
//! validation before anything is sent, retries with the same `request_id`, the stream
//! pump (order, frames, lag) and the auto-start policy driven by real connection losses.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;

use common::*;
use forge_ipc::api::methods::sites_park;
use forge_ipc::api::{ParkParams, ParkedDirDto};
use forge_ipc::base::event::EventKind;
use forge_ipc::base::ids::RequestId;
use forge_ipc::server::{Router, ServerHandle};
use forge_ipc::transport::InMemoryTransport;
use forgebox_desktop::bridge::{
    Bridge, BridgeOptions, CallArgs, ConnectionStatus, FrameSink, LOCAL_CONTEXT, StreamEntry,
};

fn call(method: &str, params: serde_json::Value) -> CallArgs {
    CallArgs {
        context: LOCAL_CONTEXT.into(),
        method: method.into(),
        params: Some(params),
        request_id: None,
        timeout_ms: None,
    }
}

async fn plain_daemon() -> Arc<TestDaemon> {
    Arc::new(TestDaemon::spawn(Router::new()).await)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unknown_method_is_refused_before_anything_is_sent() {
    let daemon = plain_daemon().await;
    let local = FakeLocal::new(Arc::clone(&daemon), true, true);
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge_over(&local, &recorder, BridgeOptions::default());

    for (method, reason) in [
        ("nope.nope", "unknown_method"),
        ("events.subscribe", "use_subscribe"),
        ("events.unsubscribe", "use_unsubscribe"),
    ] {
        let err = bridge.call(call(method, json!({}))).await.unwrap_err();
        assert_eq!(err.reason(), Some(reason), "{method}: {err:?}");
    }
    let err = bridge
        .call(CallArgs {
            request_id: Some("not-a-ulid".into()),
            ..call("system.ping", json!({}))
        })
        .await
        .unwrap_err();
    assert_eq!(err.reason(), Some("invalid_request_id"));
    // Nothing reached the daemon: the context was not even opened.
    assert_eq!(local.opens.load(Ordering::SeqCst), 0);
    assert_eq!(daemon.hellos(), 0);
    assert_eq!(daemon.handle.connection_count(), 0);

    // An unknown context is refused too.
    let err = bridge
        .call(CallArgs {
            context: "hml".into(),
            ..call("system.ping", json!({}))
        })
        .await
        .unwrap_err();
    assert_eq!(err.reason(), Some("unknown_context"));

    // A real method goes through.
    let pong = bridge.call(call("system.ping", json!({}))).await.unwrap();
    assert!(pong.is_object(), "{pong}");
    assert_eq!(daemon.hellos(), 1);
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disconnect_mid_call_is_retried_with_the_same_request_id_and_runs_once() {
    let executions = Arc::new(AtomicUsize::new(0));
    let seen_ids = Arc::new(std::sync::Mutex::new(Vec::<RequestId>::new()));
    let handle_cell: Arc<std::sync::OnceLock<ServerHandle>> = Arc::new(std::sync::OnceLock::new());
    let mut router = Router::new();
    {
        let executions = Arc::clone(&executions);
        let seen_ids = Arc::clone(&seen_ids);
        let handle_cell = Arc::clone(&handle_cell);
        router.handle::<sites_park, _, _>(move |params: ParkParams, ctx| {
            let n = executions.fetch_add(1, Ordering::SeqCst) + 1;
            seen_ids.lock().unwrap().push(ctx.request_id());
            let handle = handle_cell.get().cloned();
            async move {
                if n == 1 {
                    // The connection drops while the daemon is still working on it.
                    if let Some(handle) = handle {
                        handle.disconnect_all();
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Ok(ParkedDirDto {
                    dir: params.dir,
                    tld: ".test".into(),
                    sites: u32::try_from(n).unwrap(),
                })
            }
        });
    }
    let daemon = Arc::new(TestDaemon::spawn(router).await);
    handle_cell.set(daemon.handle.clone()).unwrap();
    let local = FakeLocal::new(Arc::clone(&daemon), true, true);
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge_over(&local, &recorder, BridgeOptions::default());

    let request_id = RequestId::new();
    let answer = bridge
        .call(CallArgs {
            request_id: Some(request_id.to_string()),
            ..call("sites.park", json!({ "dir": "D:/clientes" }))
        })
        .await
        .unwrap();
    assert_eq!(answer["sites"], 1, "{answer}");
    assert_eq!(executions.load(Ordering::SeqCst), 1, "executed once");
    assert_eq!(*seen_ids.lock().unwrap(), [request_id]);
    assert!(daemon.hellos() >= 2, "the retry used a new connection");

    // The same intent retried by the UI later gets the same answer, still one execution.
    let again = bridge
        .call(CallArgs {
            request_id: Some(request_id.to_string()),
            ..call("sites.park", json!({ "dir": "D:/clientes" }))
        })
        .await
        .unwrap();
    assert_eq!(again, answer);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn errors_reach_the_webview_with_stable_codes() {
    let daemon = plain_daemon().await;
    let local = FakeLocal::new(Arc::clone(&daemon), true, true);
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge_over(&local, &recorder, BridgeOptions::default());
    // A method the daemon has no handler for: the daemon's own code passes through.
    let err = bridge
        .call(call("sites.list", json!({})))
        .await
        .unwrap_err();
    assert_eq!(err.code, "not_implemented");
    // Daemon gone (stopped on purpose, so the policy does not start it again).
    recorder.wait_status(ConnectionStatus::Connected).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    daemon.announce_shutdown("user");
    tokio::time::sleep(Duration::from_millis(100)).await;
    local.kill();
    recorder.wait_status(ConnectionStatus::Stopped).await;
    let err = bridge
        .call(call("system.ping", json!({})))
        .await
        .unwrap_err();
    assert_eq!(err.code, "disconnected", "{err:?}");
    assert!(
        matches!(err.reason(), Some("daemon_unreachable" | "connection_lost")),
        "{err:?}"
    );
    bridge.shutdown().await;
}

/// Publishes `n` events of a neutral kind.
fn publish(daemon: &TestDaemon, n: u64) {
    for i in 0..n {
        daemon
            .bus
            .publish(EventKind::ConfigRendered, json!({ "v": 1, "i": i }));
    }
}

/// Every `lagged` marker sits where items are missing and counts exactly them.
fn assert_markers_match_gaps(frames: &Frames) {
    let (mut previous, mut pending) = (0_u64, 0_u64);
    for entry in frames.entries() {
        match entry {
            StreamEntry::Item { item } => {
                let seq = item["seq"].as_u64().unwrap();
                assert_eq!(seq - previous - 1, pending, "gap before seq {seq}");
                (previous, pending) = (seq, 0);
            }
            StreamEntry::Lagged { missed } => pending += missed,
            _ => {}
        }
    }
}

fn assert_frames_are_well_formed(frames: &Frames) {
    let all = frames.all();
    for (i, frame) in all.iter().enumerate() {
        assert_eq!(frame.frame, i as u64 + 1, "frames are numbered 1, 2, 3...");
        assert!(
            frame.entries.len() <= 256,
            "frame {} has {}",
            frame.frame,
            frame.entries.len()
        );
        assert!(!frame.entries.is_empty());
    }
    let seqs = frames.seqs();
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "items strictly in order"
    );
    assert!(
        !frames
            .entries()
            .iter()
            .any(|e| matches!(e, StreamEntry::Reset)),
        "no spurious reset"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_pump_delivers_10000_events_in_order_and_in_frames() {
    let daemon = plain_daemon().await;
    publish(&daemon, 10_000);
    let local = FakeLocal::new(Arc::clone(&daemon), true, true);
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge_over(&local, &recorder, BridgeOptions::default());
    let frames = Frames::new(true);
    frames.bridge.set(bridge.clone()).unwrap();
    let sink: Arc<dyn FrameSink> = Arc::new(FramesSink(Arc::clone(&frames)));
    let answer = bridge
        .subscribe(
            LOCAL_CONTEXT,
            "events.subscribe",
            Some(json!({ "filter": {}, "resume_from": 0 })),
            sink,
            "main",
        )
        .await
        .unwrap();
    assert!(!answer.pending);
    assert!(
        eventually(Duration::from_secs(30), || frames.last_seq()
            == Some(10_000))
        .await,
        "last seq {:?}",
        frames.last_seq()
    );
    assert_eq!(frames.seqs(), (1..=10_000).collect::<Vec<u64>>());
    assert_eq!(frames.missed(), 0);
    assert_frames_are_well_formed(&frames);
    let count = frames.all().len();
    assert!(count >= 40, "batched: {count} frames for 10 000 items");
    assert!(
        count < 10_000 / 4,
        "batched: {count} frames for 10 000 items"
    );
    println!("10 000 events in {count} frames");

    bridge
        .unsubscribe(LOCAL_CONTEXT, &answer.stream_id)
        .unwrap();
    assert!(eventually(WAIT, || bridge.stream_count() == 0).await);
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_consumer_gets_lagged_markers_and_the_newest_events() {
    let daemon = plain_daemon().await;
    let local = FakeLocal::new(Arc::clone(&daemon), true, true);
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge_over(&local, &recorder, BridgeOptions::default());
    // The webview stalls: it does not acknowledge frames.
    let frames = Frames::new(false);
    frames.bridge.set(bridge.clone()).unwrap();
    let sink: Arc<dyn FrameSink> = Arc::new(FramesSink(Arc::clone(&frames)));
    let answer = bridge
        .subscribe(
            LOCAL_CONTEXT,
            "events.subscribe",
            Some(json!({ "filter": {} })),
            sink,
            "main",
        )
        .await
        .unwrap();
    assert_eq!(answer.resume_point, Some(0));
    publish(&daemon, 10_000);
    // The window (4 frames) fills up and stays full.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let stalled = frames.all().len();
    assert!(stalled <= 4, "{stalled} frames without acknowledgement");
    // The webview catches up.
    frames.acking.store(true, Ordering::SeqCst);
    frames.ack_all();
    assert!(
        eventually(Duration::from_secs(30), || frames.last_seq()
            == Some(10_000))
        .await,
        "last seq {:?}",
        frames.last_seq()
    );
    assert_frames_are_well_formed(&frames);
    let delivered = frames.seqs().len() as u64;
    let missed = frames.missed();
    println!("slow consumer: {delivered} delivered, {missed} reported lost");
    assert!(missed > 0, "the bounded buffer dropped the oldest");
    assert_markers_match_gaps(&frames);
    assert_eq!(
        delivered + missed,
        10_000,
        "every event delivered or counted"
    );
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_opened_while_the_daemon_is_down_subscribes_when_it_comes_back() {
    let daemon = plain_daemon().await;
    // Initialized root, daemon down; the initial auto-start brings it up.
    let local = FakeLocal::new(Arc::clone(&daemon), true, false);
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge_over(&local, &recorder, test_options(Duration::from_millis(100)));
    let frames = Frames::new(true);
    frames.bridge.set(bridge.clone()).unwrap();
    // Open the context, then kill the daemon it started before subscribing.
    bridge.state(LOCAL_CONTEXT).await.unwrap();
    recorder.wait_status(ConnectionStatus::Connected).await;
    assert_eq!(local.spawned(), 1, "initial auto-start");
    tokio::time::sleep(Duration::from_millis(300)).await;
    daemon.announce_shutdown("user");
    tokio::time::sleep(Duration::from_millis(100)).await;
    local.kill();
    recorder.wait_status(ConnectionStatus::Stopped).await;

    let sink: Arc<dyn FrameSink> = Arc::new(FramesSink(Arc::clone(&frames)));
    let answer = bridge
        .subscribe(
            LOCAL_CONTEXT,
            "events.subscribe",
            Some(json!({ "filter": { "kinds": ["config.*"] } })),
            sink,
            "main",
        )
        .await
        .unwrap();
    assert!(answer.pending, "{answer:?}");
    // The user starts it again.
    bridge.start(LOCAL_CONTEXT, false).await.unwrap();
    recorder.wait_status(ConnectionStatus::Connected).await;
    // Give the pending stream a moment to subscribe, then publish.
    tokio::time::sleep(Duration::from_millis(400)).await;
    publish(&daemon, 3);
    assert!(
        eventually(WAIT, || frames.seqs().len() == 3).await,
        "{:?}",
        frames.entries()
    );
    bridge.shutdown().await;
    assert_eq!(bridge.stream_count(), 0);
}

// --- auto-start policy against real connection losses -----------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_uninitialized_root_is_not_started() {
    let daemon = plain_daemon().await;
    let local = FakeLocal::new(Arc::clone(&daemon), false, false);
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge_over(&local, &recorder, test_options(Duration::from_millis(50)));
    let state = bridge.state(LOCAL_CONTEXT).await.unwrap();
    assert_eq!(state.context, LOCAL_CONTEXT);
    recorder.wait_status(ConnectionStatus::Uninitialized).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(local.starts.load(Ordering::SeqCst), 0, "no start attempted");
    let last = recorder.last().unwrap();
    assert_eq!(last.status, ConnectionStatus::Uninitialized);
    assert!(!last.autostart.initialized);
    assert_eq!(last.endpoint.as_deref(), Some("memory:fake-root"));

    // The user is not offered a plain start either: onboarding first.
    let err = bridge.start(LOCAL_CONTEXT, false).await.unwrap_err();
    assert_eq!(err.reason(), Some("root_uninitialized"));
    assert_eq!(local.spawned(), 0);
    // Onboarding finished: an explicit initializing start goes through.
    let state = bridge.start(LOCAL_CONTEXT, true).await.unwrap();
    assert!(state.autostart.initialized, "{state:?}");
    recorder.wait_status(ConnectionStatus::Connected).await;
    assert_eq!(local.spawned(), 1);
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_restart_after_an_unexpected_loss_and_not_a_second_within_a_minute() {
    let daemon = plain_daemon().await;
    let local = FakeLocal::new(Arc::clone(&daemon), true, false);
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge_over(&local, &recorder, test_options(Duration::from_millis(100)));
    bridge.state(LOCAL_CONTEXT).await.unwrap();
    recorder.wait_status(ConnectionStatus::Connected).await;
    assert_eq!(local.spawned(), 1, "started on open");
    let first = recorder.last().unwrap();
    assert_eq!(first.autostart.restarts, 0);
    assert!(first.hello.as_ref().is_some_and(|h| h.local));

    // Crash (nothing announced).
    local.kill();
    recorder.wait_status(ConnectionStatus::Reconnecting).await;
    recorder.wait_status(ConnectionStatus::Connected).await;
    assert_eq!(local.spawned(), 2, "restarted once");
    let back = recorder.last().unwrap();
    assert_eq!(back.autostart.restarts, 1);
    let statuses = recorder.statuses();
    let tail: Vec<_> = statuses.iter().rev().take(3).rev().copied().collect();
    assert_eq!(
        tail,
        [
            ConnectionStatus::Reconnecting,
            ConnectionStatus::Starting,
            ConnectionStatus::Connected
        ],
        "{statuses:?}"
    );

    // Crash again right away: the next restart waits for the 60 s spacing.
    local.kill();
    recorder.wait_status(ConnectionStatus::Reconnecting).await;
    assert!(
        eventually(WAIT, || recorder
            .last()
            .is_some_and(|s| s.autostart.next_restart_ms.is_some()))
        .await,
        "{:?}",
        recorder.last()
    );
    let waiting = recorder.last().unwrap();
    let next = waiting.autostart.next_restart_ms.unwrap();
    assert!(next > 50_000 && next <= 60_000, "next restart in {next} ms");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(local.spawned(), 2, "no second restart within 60 s");
    let state = bridge.state(LOCAL_CONTEXT).await.unwrap();
    assert_eq!(state.status, ConnectionStatus::Reconnecting);
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_restart_after_a_user_shutdown_until_the_user_starts_it() {
    let daemon = plain_daemon().await;
    let local = FakeLocal::new(Arc::clone(&daemon), true, true);
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge_over(&local, &recorder, test_options(Duration::from_millis(100)));
    bridge.state(LOCAL_CONTEXT).await.unwrap();
    recorder.wait_status(ConnectionStatus::Connected).await;
    assert_eq!(local.spawned(), 0, "it was already running");
    // Let the shutdown watcher subscribe.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // `daemon.stop` from anywhere (CLI, tray): the daemon announces it, then goes away.
    daemon.announce_shutdown("user");
    tokio::time::sleep(Duration::from_millis(100)).await;
    local.kill();
    recorder.wait_status(ConnectionStatus::Stopped).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(local.spawned(), 0, "never restarted after a user shutdown");
    let stopped = recorder.last().unwrap();
    assert_eq!(stopped.status, ConnectionStatus::Stopped);
    assert_eq!(stopped.autostart.stopped_reason.as_deref(), Some("user"));
    assert_eq!(stopped.autostart.next_restart_ms, None);

    // The user clicks "Iniciar".
    bridge.start(LOCAL_CONTEXT, false).await.unwrap();
    recorder.wait_status(ConnectionStatus::Connected).await;
    assert_eq!(local.spawned(), 1);
    assert_eq!(recorder.last().unwrap().autostart.stopped_reason, None);
    bridge.shutdown().await;
    assert_eq!(
        bridge.state(LOCAL_CONTEXT).await.unwrap().status,
        ConnectionStatus::Closed
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_only_methods_are_refused_on_remote_connections() {
    let daemon = Arc::new(TestDaemon::spawn_with(Router::new(), InMemoryTransport::remote()).await);
    let local = FakeLocal::new(Arc::clone(&daemon), true, true);
    let recorder = Arc::new(Recorder::default());
    let bridge = bridge_over(&local, &recorder, BridgeOptions::default());
    // `mail.open` is a viewer method, so only `local_only` can refuse it.
    let err = bridge.call(call("mail.open", json!({}))).await.unwrap_err();
    assert_eq!(err.code, "forbidden", "{err:?}");
    assert!(err.message.contains("local_only"), "{err:?}");
    let state = bridge.state(LOCAL_CONTEXT).await.unwrap();
    assert_eq!(state.hello.map(|h| h.local), Some(false));
    let _: &Bridge = &bridge;
    bridge.shutdown().await;
}
