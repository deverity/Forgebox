//! Event streams: resubscription from `seq` (explicit and automatic after a dropped
//! connection), replay from the store, and slow subscribers (drop-oldest + lagged)
//! never affecting other clients.

mod common;

use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use common::{bus, eventually, fast_options, spawn_on};
use forge_core::event::{EventBus, EventKind};
use forge_core::store::Store;
use forge_ipc::ClientOptions;
use forge_ipc::api::{EventEnvelope, EventFilter, NoParams};
use forge_ipc::client::{Subscription, SubscriptionItem};
use forge_ipc::server::{EventSource, ServerConfig};
use forge_ipc::transport::{DEFAULT_MAX_FRAME_BYTES, InMemoryTransport, Transport};

const WAIT: Duration = Duration::from_secs(5);

async fn next_event(sub: &mut Subscription<EventEnvelope>) -> EventEnvelope {
    tokio::time::timeout(WAIT, sub.next())
        .await
        .expect("event in time")
        .expect("stream open")
}

async fn next_item(sub: &mut Subscription<EventEnvelope>) -> SubscriptionItem<EventEnvelope> {
    tokio::time::timeout(WAIT, sub.next_item())
        .await
        .expect("item in time")
        .expect("stream open")
}

async fn assert_quiet(sub: &mut Subscription<EventEnvelope>) {
    assert!(
        tokio::time::timeout(Duration::from_millis(150), sub.next())
            .await
            .is_err(),
        "no further event expected"
    );
}

fn publish(bus: &EventBus, kind: EventKind, n: u64) {
    bus.publish(kind, json!({ "n": n }));
}

#[tokio::test]
async fn resubscribing_from_the_last_seq_delivers_exactly_the_missing_events() {
    let bus = bus(1024);
    let server = spawn_on(InMemoryTransport::new(), |b| {
        b.events(EventSource::new(bus.clone()))
    })
    .await;

    let first = server.client();
    let mut sub = first
        .subscribe_events(EventFilter::kinds(["site.*"]), None)
        .await
        .unwrap();
    for n in 1..=5 {
        publish(&bus, EventKind::SiteUpdated, n);
        publish(&bus, EventKind::JobProgress, n); // filtered out
    }
    let mut last = 0;
    for n in 1..=5 {
        let envelope = next_event(&mut sub).await;
        assert_eq!(envelope.event.kind, "site.updated");
        assert_eq!(envelope.event.payload["n"], n);
        assert_eq!(envelope.seq, envelope.event.seq);
        last = envelope.seq;
    }
    // The client goes away (connection dropped) while events keep coming.
    drop(sub);
    first.close();
    drop(first);
    assert!(eventually(WAIT, || server.handle.connection_count() == 0).await);
    for n in 6..=10 {
        publish(&bus, EventKind::JobProgress, n);
        publish(&bus, EventKind::SiteUpdated, n);
    }

    let second = server.client();
    let mut resumed = second
        .subscribe_events(EventFilter::kinds(["site.*"]), Some(last))
        .await
        .unwrap();
    for n in 6..=10 {
        let envelope = next_event(&mut resumed).await;
        assert_eq!(envelope.event.payload["n"], n, "missing events, in order");
        assert!(envelope.seq > last);
        last = envelope.seq;
    }
    publish(&bus, EventKind::SiteCreated, 11);
    let live = next_event(&mut resumed).await;
    assert_eq!(
        (live.event.kind.as_str(), live.seq),
        ("site.created", last + 1)
    );
    assert_quiet(&mut resumed).await;
    assert_eq!(resumed.missed(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_client_reconnects_and_resumes_transparently() {
    let bus = bus(1024);
    let server = spawn_on(InMemoryTransport::new(), |b| {
        b.events(EventSource::new(bus.clone()))
    })
    .await;
    let client = server.client();
    let mut sub = client
        .subscribe_events(EventFilter::default(), None)
        .await
        .unwrap();
    for n in 1..=3 {
        publish(&bus, EventKind::ServiceRunning, n);
    }
    for seq in 1..=3 {
        assert_eq!(next_event(&mut sub).await.seq, seq);
    }
    let before = sub.subscription_id().unwrap();

    // Server side drop of every connection; events published meanwhile.
    server.handle.disconnect_all();
    for n in 4..=6 {
        publish(&bus, EventKind::ServiceStopped, n);
    }
    assert!(
        eventually(WAIT, || sub
            .subscription_id()
            .is_some_and(|id| id != before))
        .await,
        "resubscribed on a new connection"
    );
    publish(&bus, EventKind::ServiceStarting, 7);

    for seq in 4..=7 {
        let envelope = next_event(&mut sub).await;
        assert_eq!(envelope.seq, seq, "exactly once, in order");
        assert_eq!(envelope.event.payload["n"], seq);
    }
    assert_quiet(&mut sub).await;
    assert_eq!(sub.missed(), 0);
    // Calls work on the new connection too.
    client.system_ping(NoParams {}).await.unwrap();
}

#[tokio::test]
async fn resume_reads_events_older_than_the_ring_from_the_store() {
    let store = Store::open_in_memory().unwrap();
    let bus = bus(3);
    for n in 1..=10 {
        let event = bus.publish(EventKind::SiteUpdated, json!({ "n": n }));
        store.events().insert(&event).unwrap();
    }
    // A pruned row becomes a reported gap.
    store
        .conn()
        .execute("DELETE FROM events WHERE seq = 6", [])
        .unwrap();
    let source = EventSource::new(bus.clone()).with_store(store.reader().clone());
    let server = spawn_on(InMemoryTransport::new(), |b| b.events(source)).await;
    let client = server.client();

    let mut sub = client
        .subscribe_events(EventFilter::default(), Some(3))
        .await
        .unwrap();
    let mut seen = Vec::new();
    while seen.len() < 7 {
        match next_item(&mut sub).await {
            SubscriptionItem::Item(envelope) => seen.push(format!("{}", envelope.seq)),
            SubscriptionItem::Lagged { missed } => seen.push(format!("lag{missed}")),
        }
    }
    // 4..7 come from the store (6 is missing), 8..10 from the in-memory ring.
    assert_eq!(seen, ["4", "5", "lag1", "7", "8", "9", "10"]);
    publish(&bus, EventKind::SiteCreated, 11);
    assert_eq!(next_event(&mut sub).await.seq, 11);
}

#[tokio::test]
async fn replay_is_bounded_by_replay_limit() {
    let store = Store::open_in_memory().unwrap();
    let bus = bus(2);
    for n in 1..=20 {
        let event = bus.publish(EventKind::JobProgress, json!({ "n": n }));
        store.events().insert(&event).unwrap();
    }
    let source = EventSource::new(bus.clone()).with_store(store.reader().clone());
    let config = ServerConfig {
        replay_limit: 5,
        ..ServerConfig::default()
    };
    let server = spawn_on(InMemoryTransport::new(), |b| {
        b.events(source).config(config)
    })
    .await;
    let mut sub = server
        .client()
        .subscribe_events(EventFilter::default(), Some(0))
        .await
        .unwrap();
    // Gap 1..=18 (ring holds 19, 20): 13 reported as lag, 14..18 replayed, then 19, 20.
    assert_eq!(
        next_item(&mut sub).await,
        SubscriptionItem::Lagged { missed: 13 }
    );
    for seq in 14..=20 {
        assert_eq!(next_event(&mut sub).await.seq, seq);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_subscriber_lags_while_other_clients_are_served() {
    let bus = bus(16);
    let config = ServerConfig {
        stream_queue_capacity: 64,
        ..ServerConfig::default()
    };
    // Small pipe buffers so the slow client's connection fills up quickly.
    let transport = InMemoryTransport::with_limits(true, 4096, DEFAULT_MAX_FRAME_BYTES);
    let server = spawn_on(transport, |b| {
        b.events(EventSource::new(bus.clone())).config(config)
    })
    .await;

    // The slow client: subscribes, then stops reading.
    let mut slow = server.transport.connect().await.unwrap();
    let rid = || forge_core::ids::RequestId::new().to_string();
    for frame in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "system.hello", "request_id": rid(),
               "params": {"token": server.token.secret(), "client": {"name": "slow", "version": "0"},
                          "api_version": {"major": 1, "minor": 0}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "events.subscribe", "request_id": rid(),
               "params": {"filter": {}}}),
    ] {
        slow.sink.send(frame.to_string()).await.unwrap();
        let answer = tokio::time::timeout(WAIT, slow.stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let answer: Value = serde_json::from_str(&answer).unwrap();
        assert!(answer["result"].is_object(), "{answer}");
    }

    let fast = server.client();
    let mut fast_sub = fast
        .subscribe_events(EventFilter::default(), None)
        .await
        .unwrap();

    let started = Instant::now();
    for n in 1..=2000 {
        publish(&bus, EventKind::JobProgress, n);
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "publishing never waits for subscribers"
    );

    // The other client keeps being served promptly...
    let ping = Instant::now();
    fast.system_ping(NoParams {}).await.unwrap();
    assert!(
        ping.elapsed() < Duration::from_millis(500),
        "{:?}",
        ping.elapsed()
    );
    // ...and receives up to the newest event (drop-oldest never drops the newest).
    let mut last = 0;
    while last < 2000 {
        match next_item(&mut fast_sub).await {
            SubscriptionItem::Item(envelope) => {
                assert!(envelope.seq > last, "increasing seq");
                last = envelope.seq;
            }
            SubscriptionItem::Lagged { .. } => {}
        }
    }

    // The slow client lost the oldest events and is told so, then gets the newest.
    let mut lagged = 0u64;
    let mut last = 0u64;
    let mut received = 0u64;
    while last < 2000 {
        let frame = tokio::time::timeout(WAIT, slow.stream.next())
            .await
            .expect("frames in time")
            .unwrap()
            .unwrap();
        let note: Value = serde_json::from_str(&frame).unwrap();
        match note["method"].as_str() {
            Some("events.lagged") => lagged += note["params"]["missed"].as_u64().unwrap(),
            Some("events.event") => {
                let seq = note["params"]["seq"].as_u64().unwrap();
                assert!(seq > last);
                last = seq;
                received += 1;
            }
            other => panic!("unexpected frame {other:?}"),
        }
    }
    assert!(lagged > 0, "slow subscriber must be told it lagged");
    assert_eq!(
        lagged + received,
        2000,
        "every event is either delivered or counted"
    );
}

#[tokio::test]
async fn client_side_overflow_is_reported_as_lag() {
    let bus = bus(1024);
    let server = spawn_on(InMemoryTransport::new(), |b| {
        b.events(EventSource::new(bus.clone()))
    })
    .await;
    let client = server.client_with(ClientOptions {
        stream_capacity: 8,
        ..fast_options()
    });
    let mut sub = client
        .subscribe_events(EventFilter::default(), None)
        .await
        .unwrap();
    for n in 1..=100 {
        publish(&bus, EventKind::JobProgress, n);
    }
    assert!(eventually(WAIT, || sub.last_seq() == Some(100)).await);
    assert_eq!(
        next_item(&mut sub).await,
        SubscriptionItem::Lagged { missed: 92 }
    );
    for seq in 93..=100 {
        assert_eq!(next_event(&mut sub).await.seq, seq);
    }
    assert_eq!(sub.missed(), 92);
}

#[tokio::test]
async fn closing_the_client_ends_its_subscriptions() {
    let bus = bus(16);
    let server = spawn_on(InMemoryTransport::new(), |b| {
        b.events(EventSource::new(bus.clone()))
    })
    .await;
    let client = server.client();
    let mut sub = client
        .subscribe_events(EventFilter::default(), None)
        .await
        .unwrap();
    client.close();
    assert!(
        tokio::time::timeout(WAIT, sub.next())
            .await
            .unwrap()
            .is_none()
    );
    let err = client.system_ping(NoParams {}).await.unwrap_err();
    assert_eq!(err.code(), "cancelled");
    assert!(eventually(WAIT, || server.handle.connection_count() == 0).await);
    let _ = server.transport.endpoint();
}
