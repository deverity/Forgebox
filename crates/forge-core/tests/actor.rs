//! Actor integration tests (docs/03 §7.2, §7.6): a stuck actor does not affect others,
//! a panic restarts the actor, the restart budget is enforced, mailboxes are bounded and
//! stops are graceful but never hold the caller.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use forge_core::actor::{
    ACTOR_EVENT_VERSION, Actor, ActorFailedPayload, ActorRestartedPayload, ActorSpec, ActorStatus,
    ActorSupervisor, AskError, Context, Reply, RestartInfo, RestartPolicy, StopOutcome, TellError,
};
use forge_core::event::{Event, EventBus, EventKind, EventStream};
use forge_core::runtime::{RuntimeConfig, build_runtime};
use parking_lot::Mutex;
use tokio::sync::oneshot;

const SECOND: Duration = Duration::from_secs(1);

enum Msg {
    Ping(Reply<&'static str>),
    /// Blocks the worker thread (simulates a sync call that hangs).
    Sleep(Duration),
    Panic,
    PanicOnAsk(Reply<()>),
    Count(Reply<u64>),
    Block {
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    },
    Inc,
    StopSelf,
    Myself(Reply<bool>),
}

#[derive(Default)]
struct TestActor {
    handled: u64,
    log: Option<Arc<Mutex<Vec<String>>>>,
}

impl TestActor {
    fn logging(log: &Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            handled: 0,
            log: Some(Arc::clone(log)),
        }
    }

    fn record(&self, entry: String) {
        if let Some(log) = &self.log {
            log.lock().push(entry);
        }
    }
}

impl Actor for TestActor {
    type Msg = Msg;

    async fn handle(&mut self, msg: Msg, ctx: &mut Context<Self>) {
        self.handled += 1;
        match msg {
            Msg::Ping(reply) => {
                let _ = reply.send("pong");
            }
            Msg::Sleep(duration) => std::thread::sleep(duration),
            Msg::Panic => panic!("boom"),
            Msg::PanicOnAsk(_reply) => panic!("boom while answering"),
            Msg::Count(reply) => {
                let _ = reply.send(self.handled);
            }
            Msg::Block { entered, release } => {
                let _ = entered.send(());
                let _ = release.await;
            }
            Msg::Inc => self.record(format!("inc {}", self.handled)),
            Msg::StopSelf => ctx.stop(),
            Msg::Myself(reply) => {
                let me = ctx.myself().expect("referenced by the caller");
                let _ = reply.send(me.name() == ctx.name());
            }
        }
    }

    async fn on_start(&mut self, ctx: &mut Context<Self>) {
        self.record(format!("start {}", ctx.restart_count()));
    }

    async fn on_stop(&mut self, _ctx: &mut Context<Self>) {
        self.record(format!("stop after {}", self.handled));
    }

    async fn on_restart(&mut self, _ctx: &mut Context<Self>, info: &RestartInfo) {
        self.record(format!("restart {} ({})", info.restart_count, info.reason));
    }
}

async fn next_event(stream: &mut EventStream, kind: EventKind) -> Event {
    loop {
        let event = tokio::time::timeout(SECOND, stream.recv())
            .await
            .expect("event in time")
            .expect("bus alive");
        if event.kind == kind {
            return event;
        }
    }
}

/// Acceptance (roadmap M0-E1-T5): an actor whose handler blocks its thread for 5 s does
/// not affect another actor; asking the stuck one times out.
#[test]
fn stuck_actor_does_not_affect_other_actors() {
    let rt = build_runtime(&RuntimeConfig::for_cores(4).thread_name("stuck-test")).unwrap();
    rt.block_on(async {
        let supervisor = ActorSupervisor::new(EventBus::new());
        let stuck = supervisor.spawn(
            ActorSpec::new("stuck").stop_timeout(Duration::from_millis(200)),
            TestActor::default,
        );
        let healthy = supervisor.spawn(ActorSpec::new("healthy"), TestActor::default);
        assert_eq!(stuck.actor_ref().ask(Msg::Ping, SECOND).await, Ok("pong"));
        assert_eq!(healthy.actor_ref().ask(Msg::Ping, SECOND).await, Ok("pong"));

        stuck
            .actor_ref()
            .tell(Msg::Sleep(Duration::from_secs(5)))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let started = Instant::now();
        let err = stuck
            .actor_ref()
            .ask(Msg::Ping, Duration::from_millis(100))
            .await
            .unwrap_err();
        let waited = started.elapsed();
        assert!(matches!(err, AskError::Timeout { .. }), "{err:?}");
        assert!(
            waited >= Duration::from_millis(100) && waited < SECOND,
            "{waited:?}"
        );

        for _ in 0..20 {
            let started = Instant::now();
            assert_eq!(healthy.actor_ref().ask(Msg::Ping, SECOND).await, Ok("pong"));
            let took = started.elapsed();
            assert!(
                took < Duration::from_millis(50),
                "healthy ask took {took:?}"
            );
        }

        // The supervisor itself is not held either: a new actor starts and answers.
        let late = supervisor.spawn(ActorSpec::new("late"), TestActor::default);
        let started = Instant::now();
        assert_eq!(late.actor_ref().ask(Msg::Ping, SECOND).await, Ok("pong"));
        assert!(started.elapsed() < Duration::from_millis(50));

        // Stopping the stuck actor returns after its stop timeout instead of hanging.
        let started = Instant::now();
        assert_eq!(stuck.stop().await, StopOutcome::Aborted);
        assert!(started.elapsed() < SECOND, "{:?}", started.elapsed());
        assert_eq!(healthy.stop().await, StopOutcome::Stopped);
    });
    // The sleeping worker thread is not joined.
    rt.shutdown_background();
}

/// Acceptance (roadmap M0-E1-T5): a panic restarts the actor, the next ask works and
/// `actor.restarted` is published; past the budget the actor is `Failed`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn panic_restarts_actor_until_budget_is_exhausted() {
    let bus = EventBus::new();
    let mut events = bus.subscribe();
    let supervisor = ActorSupervisor::new(bus.clone());
    let created = Arc::new(AtomicU32::new(0));
    let log = Arc::new(Mutex::new(Vec::new()));
    let policy = RestartPolicy {
        max_restarts: 2,
        window: Duration::from_secs(60),
        backoff_initial: Duration::from_millis(10),
        backoff_max: Duration::from_millis(50),
    };
    let handle = {
        let created = Arc::clone(&created);
        let log = Arc::clone(&log);
        supervisor.spawn(ActorSpec::new("flaky").restart(policy), move || {
            created.fetch_add(1, Ordering::SeqCst);
            TestActor::logging(&log)
        })
    };
    let actor = handle.actor_ref().clone();
    assert_eq!(actor.ask(Msg::Count, SECOND).await, Ok(1));

    // 1st panic: restarted, the queued ask is answered by the new instance.
    actor.tell(Msg::Panic).unwrap();
    assert_eq!(actor.ask(Msg::Count, SECOND).await, Ok(1), "fresh state");
    assert_eq!(created.load(Ordering::SeqCst), 2);
    let event = next_event(&mut events, EventKind::ActorRestarted).await;
    let payload: ActorRestartedPayload = serde_json::from_value(event.payload).unwrap();
    assert_eq!(
        payload,
        ActorRestartedPayload {
            v: ACTOR_EVENT_VERSION,
            actor: "flaky".into(),
            reason: "boom".into(),
            restart_count: 1,
            backoff_ms: 10,
        }
    );

    // 2nd panic, while answering an ask: the asker sees `Stopped`, the actor restarts.
    let err = actor.ask(Msg::PanicOnAsk, SECOND).await.unwrap_err();
    assert!(matches!(err, AskError::Stopped { .. }), "{err:?}");
    assert_eq!(actor.ask(Msg::Ping, SECOND).await, Ok("pong"));
    let event = next_event(&mut events, EventKind::ActorRestarted).await;
    let payload: ActorRestartedPayload = serde_json::from_value(event.payload).unwrap();
    assert_eq!(payload.restart_count, 2);
    assert_eq!(payload.backoff_ms, 20, "backoff doubles");
    assert_eq!(payload.reason, "boom while answering");

    // 3rd panic exceeds 2 restarts per window: Failed, asks fail with Stopped.
    actor.tell(Msg::Panic).unwrap();
    let status = handle
        .wait_for_status(ActorStatus::is_terminal, SECOND)
        .await
        .unwrap();
    assert_eq!(
        status,
        ActorStatus::Failed {
            reason: "boom".into()
        }
    );
    let event = next_event(&mut events, EventKind::ActorFailed).await;
    let payload: ActorFailedPayload = serde_json::from_value(event.payload).unwrap();
    assert_eq!(payload.actor, "flaky");
    assert_eq!(payload.restart_count, 2);
    let err = actor.ask(Msg::Ping, SECOND).await.unwrap_err();
    assert!(matches!(err, AskError::Stopped { .. }), "{err:?}");
    assert!(matches!(
        actor.tell(Msg::Inc),
        Err(TellError::Stopped { .. })
    ));
    assert!(actor.is_closed());
    assert_eq!(created.load(Ordering::SeqCst), 3);
    assert_eq!(handle.stop().await, StopOutcome::AlreadyFinished);

    let children = supervisor.children();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].name, "flaky");
    assert!(matches!(children[0].status, ActorStatus::Failed { .. }));

    let log = log.lock().clone();
    assert_eq!(
        log,
        vec![
            "start 0",
            "restart 1 (boom)",
            "start 1",
            "restart 2 (boom while answering)",
            "start 2",
        ],
        "hooks run on every incarnation; on_stop never runs on a panicked instance"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_mailbox_rejects_tell_and_ask() {
    let supervisor = ActorSupervisor::new(EventBus::new());
    let handle = supervisor.spawn(ActorSpec::new("small").mailbox(2), TestActor::default);
    let actor = handle.actor_ref().clone();
    assert_eq!(actor.max_capacity(), 2);

    let (entered_tx, entered) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    actor
        .tell(Msg::Block {
            entered: entered_tx,
            release: release_rx,
        })
        .unwrap();
    entered.await.unwrap();

    actor.tell(Msg::Inc).unwrap();
    actor.tell(Msg::Inc).unwrap();
    assert_eq!(actor.capacity(), 0);
    let err = actor.tell(Msg::Inc).unwrap_err();
    assert!(err.is_full(), "{err:?}");
    assert_eq!(err.actor(), "small");
    assert!(matches!(err.into_inner(), Msg::Inc));
    let err = actor.ask(Msg::Count, SECOND).await.unwrap_err();
    assert!(matches!(err, AskError::MailboxFull { .. }), "{err:?}");

    release.send(()).unwrap();
    let deadline = Instant::now() + SECOND;
    while actor.capacity() < 2 {
        assert!(Instant::now() < deadline, "mailbox never drained");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    // Block + 2 × Inc + this Count: the rejected messages were never queued.
    assert_eq!(actor.ask(Msg::Count, SECOND).await, Ok(4));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_stop_drains_mailbox_then_runs_on_stop() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let supervisor = ActorSupervisor::new(EventBus::new());
    let handle = {
        let log = Arc::clone(&log);
        supervisor.spawn(ActorSpec::new("drain"), move || TestActor::logging(&log))
    };
    let actor = handle.actor_ref().clone();
    let (entered_tx, entered) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    actor
        .tell(Msg::Block {
            entered: entered_tx,
            release: release_rx,
        })
        .unwrap();
    entered.await.unwrap();
    actor.tell(Msg::Inc).unwrap();
    actor.tell(Msg::Inc).unwrap();

    let stopping = tokio::spawn({
        let supervisor = supervisor.clone();
        async move { supervisor.stop_all().await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    release.send(()).unwrap();
    let outcomes = stopping.await.unwrap();
    assert_eq!(outcomes, vec![("drain".to_owned(), StopOutcome::Stopped)]);
    assert_eq!(handle.status(), ActorStatus::Stopped);
    assert!(handle.is_finished());
    assert_eq!(
        log.lock().clone(),
        vec!["start 0", "inc 2", "inc 3", "stop after 3"],
        "queued messages are handled before on_stop"
    );
    let err = actor.ask(Msg::Ping, SECOND).await.unwrap_err();
    assert!(matches!(err, AskError::Stopped { .. }), "{err:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_stops_itself_or_when_unreferenced() {
    let supervisor = ActorSupervisor::new(EventBus::new());

    let handle = supervisor.spawn(ActorSpec::new("self-stop"), TestActor::default);
    assert_eq!(handle.actor_ref().ask(Msg::Myself, SECOND).await, Ok(true));
    handle.actor_ref().tell(Msg::StopSelf).unwrap();
    let status = handle
        .wait_for_status(ActorStatus::is_terminal, SECOND)
        .await
        .unwrap();
    assert_eq!(status, ActorStatus::Stopped);

    let dropped = supervisor.spawn(ActorSpec::new("dropped"), TestActor::default);
    assert_eq!(dropped.actor_ref().ask(Msg::Ping, SECOND).await, Ok("pong"));
    drop(dropped);
    let deadline = Instant::now() + SECOND;
    loop {
        let child = supervisor
            .children()
            .into_iter()
            .find(|c| c.name == "dropped")
            .expect("registered");
        if child.status == ActorStatus::Stopped {
            break;
        }
        assert!(Instant::now() < deadline, "still {:?}", child.status);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// `children()` reads each status once: a second read of the same `watch` while the first
/// is still borrowed deadlocks (the reader and the actor that publishes its new state)
/// when the actor's write queues between the two reads, since the lock is fair to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn children_never_deadlocks_with_actors_changing_state() {
    use std::sync::atomic::AtomicBool;

    let supervisor = ActorSupervisor::new(EventBus::new());
    let reader = supervisor.clone();
    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut rows = 0_usize;
        while !flag.load(Ordering::Relaxed) {
            rows = rows.wrapping_add(reader.children().len());
        }
        let _ = finished_tx.send(rows);
    });
    for i in 0..300 {
        let handle = supervisor.spawn(ActorSpec::new(format!("churn-{i}")), TestActor::default);
        assert_eq!(handle.actor_ref().ask(Msg::Ping, SECOND).await, Ok("pong"));
        // Every reference dropped: Running -> Stopping -> Stopped while `children()` reads.
        drop(handle);
        tokio::task::yield_now().await;
    }
    done.store(true, Ordering::Relaxed);
    let finished =
        tokio::task::spawn_blocking(move || finished_rx.recv_timeout(Duration::from_secs(10)))
            .await
            .unwrap();
    assert!(finished.is_ok(), "children() deadlocked on an actor status");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_policy_fails_on_first_panic() {
    let supervisor = ActorSupervisor::new(EventBus::new());
    let handle = supervisor.spawn(
        ActorSpec::new("fragile").restart(RestartPolicy::never()),
        TestActor::default,
    );
    handle.actor_ref().tell(Msg::Panic).unwrap();
    let status = handle
        .wait_for_status(ActorStatus::is_terminal, SECOND)
        .await
        .unwrap();
    assert!(matches!(status, ActorStatus::Failed { .. }), "{status:?}");
}
