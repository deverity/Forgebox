//! Job integration tests (docs/03 §7.4): id returned at once, ordered `job.*` events,
//! cooperative and forced cancellation, failures, panics, idempotency and retention.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use forge_core::error::ForgeError;
use forge_core::event::{Event, EventBus, EventKind, EventStream};
use forge_core::ids::{JobId, RequestId};
use forge_core::jobs::{
    JOB_EVENT_VERSION, JobCancelledPayload, JobConfig, JobDonePayload, JobFailedPayload,
    JobProgress, JobProgressPayload, JobRegistry, JobSpec, JobStartedPayload, JobState,
};
use serde_json::json;

const SECOND: Duration = Duration::from_secs(1);

fn registry(bus: &EventBus, config: JobConfig) -> JobRegistry {
    JobRegistry::with_config(bus.clone(), config)
}

fn eager() -> JobConfig {
    JobConfig {
        progress_interval: Duration::ZERO,
        ..JobConfig::default()
    }
}

/// Events of `id` until (and including) its terminal event.
async fn job_events(stream: &mut EventStream, id: JobId) -> Vec<Event> {
    let mut out = Vec::new();
    loop {
        let event = tokio::time::timeout(SECOND, stream.recv())
            .await
            .expect("job event in time")
            .expect("bus alive");
        if event.payload["job_id"] != json!(id.to_string()) {
            continue;
        }
        let terminal = matches!(
            event.kind,
            EventKind::JobDone | EventKind::JobFailed | EventKind::JobCancelled
        );
        out.push(event);
        if terminal {
            return out;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_are_ordered_started_progress_done() {
    let bus = EventBus::new();
    let mut stream = bus.subscribe();
    let jobs = registry(&bus, eager());
    let id = jobs.start("tool.install php@8.3", |ctx| async move {
        ctx.progress(JobProgress::new().pct(10.0).message("downloading"));
        ctx.progress(JobProgress::new().bytes(512, Some(1024)));
        ctx.progress(JobProgress::new().pct(100.0).message("done"));
        Ok(json!({"version": "8.3.14"}))
    });

    let events = job_events(&mut stream, id).await;
    let kinds: Vec<EventKind> = events.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        vec![
            EventKind::JobStarted,
            EventKind::JobProgress,
            EventKind::JobProgress,
            EventKind::JobProgress,
            EventKind::JobDone,
        ]
    );
    for pair in events.windows(2) {
        assert!(pair[0].seq < pair[1].seq, "seq grows: {pair:?}");
    }

    let started: JobStartedPayload = serde_json::from_value(events[0].payload.clone()).unwrap();
    assert_eq!(
        started,
        JobStartedPayload {
            v: JOB_EVENT_VERSION,
            job_id: id,
            name: "tool.install php@8.3".into(),
            request_id: None,
        }
    );
    let second: JobProgressPayload = serde_json::from_value(events[2].payload.clone()).unwrap();
    assert_eq!(second.progress.pct, Some(10.0), "progress is merged");
    assert_eq!(second.progress.message.as_deref(), Some("downloading"));
    assert_eq!(second.progress.bytes_done, Some(512));
    let done: JobDonePayload = serde_json::from_value(events[4].payload.clone()).unwrap();
    assert_eq!(done.result, json!({"version": "8.3.14"}));
    assert_eq!(done.name, "tool.install php@8.3");

    let info = jobs.get(id).unwrap();
    assert_eq!(info.state, JobState::Done);
    assert_eq!(info.progress.pct, Some(100.0));
    assert_eq!(info.result, Some(json!({"version": "8.3.14"})));
    assert!(info.finished_at.unwrap() >= info.started_at);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_returns_immediately_and_cancel_is_cooperative() {
    let bus = EventBus::new();
    let mut stream = bus.subscribe();
    let jobs = registry(&bus, eager());
    let before = Instant::now();
    let handle = jobs.spawn(JobSpec::new("db.export"), |ctx| async move {
        while !ctx.is_cancelled() {
            tokio::select! {
                () = ctx.cancelled() => {}
                () = tokio::time::sleep(Duration::from_millis(5)) => {
                    ctx.progress(JobProgress::new().message("exporting"));
                }
            }
        }
        ctx.check_cancelled()
    });
    assert!(before.elapsed() < Duration::from_millis(50));
    assert_eq!(jobs.active().len(), 1);
    tokio::time::sleep(Duration::from_millis(30)).await;

    assert!(jobs.cancel(handle.id()).unwrap());
    let info = handle.wait(SECOND).await.unwrap();
    assert_eq!(info.state, JobState::Cancelled);
    assert!(info.cancel_requested);
    assert_eq!(info.error, None);

    let events = job_events(&mut stream, handle.id()).await;
    let last = events.last().unwrap();
    assert_eq!(last.kind, EventKind::JobCancelled);
    let payload: JobCancelledPayload = serde_json::from_value(last.payload.clone()).unwrap();
    assert!(!payload.forced);
    assert!(
        events.iter().any(|e| e.kind == EventKind::JobProgress),
        "progress was reported before the cancel"
    );
    assert!(!jobs.cancel(handle.id()).unwrap(), "already finished");
    assert!(!handle.cancel());
    assert!(jobs.active().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failure_and_panic_become_job_failed() {
    let bus = EventBus::new();
    let mut stream = bus.subscribe();
    let jobs = registry(&bus, eager());

    let failing = jobs.start("tool.install php@9.9", |_ctx| async move {
        Err::<(), _>(ForgeError::NotFound("php 9.9 in the registry".into()))
    });
    let events = job_events(&mut stream, failing).await;
    assert_eq!(events.last().unwrap().kind, EventKind::JobFailed);
    let payload: JobFailedPayload =
        serde_json::from_value(events.last().unwrap().payload.clone()).unwrap();
    assert_eq!(payload.error.code, "not_found");
    assert_eq!(payload.error.message, "not found: php 9.9 in the registry");
    let info = jobs.get(failing).unwrap();
    assert_eq!(info.state, JobState::Failed);
    assert_eq!(info.error.unwrap().code, "not_found");

    let panicking = jobs.start("buggy", |_ctx| async move {
        let explode = true;
        if explode {
            panic!("job exploded");
        }
        Ok(())
    });
    let events = job_events(&mut stream, panicking).await;
    let payload: JobFailedPayload =
        serde_json::from_value(events.last().unwrap().payload.clone()).unwrap();
    assert_eq!(payload.error.code, "internal");
    assert!(
        payload.error.message.contains("job exploded"),
        "{payload:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn job_ignoring_cancel_is_dropped_after_grace() {
    let bus = EventBus::new();
    let jobs = registry(
        &bus,
        JobConfig {
            cancel_grace: Duration::from_millis(50),
            ..eager()
        },
    );
    let dropped = Arc::new(AtomicBool::new(false));
    struct SetOnDrop(Arc<AtomicBool>);
    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let handle = {
        let dropped = Arc::clone(&dropped);
        jobs.spawn(JobSpec::new("stubborn"), move |_ctx| async move {
            let _guard = SetOnDrop(dropped);
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(())
        })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    let started = Instant::now();
    assert!(handle.cancel());
    let info = handle.wait(SECOND).await.unwrap();
    assert_eq!(info.state, JobState::Cancelled);
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(dropped.load(Ordering::SeqCst), "the job future was dropped");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_request_id_returns_same_job() {
    let bus = EventBus::new();
    let jobs = registry(&bus, eager());
    let request = RequestId::new();
    let (release, gate) = tokio::sync::oneshot::channel::<()>();
    let first = jobs.spawn(
        JobSpec::new("site.create").request_id(request),
        |_| async move {
            let _ = gate.await;
            Ok("created")
        },
    );
    let ran_twice = Arc::new(AtomicBool::new(false));
    let second = {
        let ran_twice = Arc::clone(&ran_twice);
        jobs.spawn(
            JobSpec::new("site.create").request_id(request),
            move |_| async move {
                ran_twice.store(true, Ordering::SeqCst);
                Ok("again")
            },
        )
    };
    assert_eq!(first.id(), second.id());
    assert_eq!(jobs.list().len(), 1);
    release.send(()).unwrap();
    let info = second.wait(SECOND).await.unwrap();
    assert_eq!(info.result, Some(json!("created")));
    assert_eq!(info.request_id, Some(request));
    assert!(!ran_twice.load(Ordering::SeqCst));

    // Still idempotent after completion, while retained.
    let third = jobs.spawn(JobSpec::new("site.create").request_id(request), |_| async {
        Ok("third")
    });
    assert_eq!(third.id(), first.id());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_last_finished_jobs_are_retained() {
    let bus = EventBus::new();
    let jobs = registry(
        &bus,
        JobConfig {
            retain_finished: 2,
            ..eager()
        },
    );
    let mut handles = Vec::new();
    for n in 0..4 {
        let handle = jobs.spawn(
            JobSpec::new(format!("job {n}")),
            move |_| async move { Ok(n) },
        );
        handle.wait(SECOND).await.unwrap();
        handles.push(handle);
    }
    let names: Vec<String> = jobs.list().into_iter().map(|j| j.name).collect();
    assert_eq!(names, vec!["job 2", "job 3"]);
    assert!(jobs.get(handles[0].id()).is_none());
    let err = jobs.cancel(handles[0].id()).unwrap_err();
    assert_eq!(err.code(), "not_found");
    // A handle keeps its final snapshot even after the registry forgot the job.
    assert_eq!(handles[0].info().result, Some(json!(0)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_events_are_throttled_but_snapshot_is_current() {
    let bus = EventBus::new();
    let mut stream = bus.subscribe();
    let jobs = registry(
        &bus,
        JobConfig {
            progress_interval: Duration::from_secs(3600),
            ..JobConfig::default()
        },
    );
    let (release, gate) = tokio::sync::oneshot::channel::<()>();
    let handle = jobs.spawn(JobSpec::new("download"), |ctx| async move {
        for n in 1..=100_u64 {
            ctx.progress(JobProgress::new().bytes(n * 10, Some(1000)));
        }
        let _ = gate.await;
        Ok(())
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let snapshot = jobs.get(handle.id()).unwrap();
    assert_eq!(snapshot.progress.bytes_done, Some(1000));
    release.send(()).unwrap();
    let events = job_events(&mut stream, handle.id()).await;
    let progress = events
        .iter()
        .filter(|e| e.kind == EventKind::JobProgress)
        .count();
    assert_eq!(progress, 1, "only the first report within the interval");
}
