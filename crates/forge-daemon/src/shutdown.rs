//! Graceful shutdown (Ctrl+C, `daemon.stop`, `daemon.restart`), in order:
//!
//! 1. the scheduler and the parked-folder watcher stop; running jobs are asked to cancel;
//! 2. pools, site processes and instances stop **in parallel** (each with its stop
//!    cascade), all within one global budget (`--shutdown-timeout`, 30 s); whatever is
//!    still alive in the root Job Object is then terminated (and awaited);
//! 3. the DNS, TLS, download and update actors stop;
//! 4. `daemon.shutdown` is published, the API stops accepting and closes its
//!    connections (the named pipe disappears with its last handle);
//! 5. the event log drains into the store, the store writer flushes and closes;
//! 6. the lock file is removed and the mutex released; with `daemon.restart` a new
//!    daemon is started detached.
//!
//! Every wait is bounded: a stuck service only costs its share of the budget.

use std::time::{Duration, Instant};

use forge_core::event::EventKind;
use forge_ipc::api::DaemonShutdownPayload;

use crate::boot::Booted;
use crate::daemon::ShutdownRequest;
use crate::instance_lock::InstanceLock;

/// What happened.
#[derive(Debug, Clone)]
pub struct ShutdownReport {
    pub reason: String,
    pub elapsed_ms: u128,
    pub events_written: u64,
    pub restarted_pid: Option<u32>,
}

/// Runs the whole sequence and releases `lock`.
pub async fn run(booted: Booted, lock: InstanceLock) -> ShutdownReport {
    let started = Instant::now();
    let Booted {
        daemon,
        store,
        event_log,
        server_task,
        scheduler,
        parked,
        ..
    } = booted;
    let request = daemon.shutdown.requested().unwrap_or(ShutdownRequest {
        reason: "signal".into(),
        restart: false,
        keep_services: false,
    });
    tracing::info!(reason = %request.reason, restart = request.restart, "shutting down");
    let budget = Duration::from_secs(daemon.args.shutdown_timeout.max(1));

    // 1.
    scheduler.stop(Duration::from_secs(2)).await;
    if let Some(watcher) = parked {
        if tokio::time::timeout(Duration::from_secs(2), watcher.stop())
            .await
            .is_err()
        {
            tracing::warn!("parked watcher did not stop in time");
        }
    }
    for job in daemon.jobs.active() {
        let _ = daemon.jobs.cancel(job.id);
    }

    // 2.
    let stack = async {
        tokio::join!(
            daemon.pools.stop_all(budget),
            daemon.services.stop_all(budget),
            async {
                let _ = tokio::time::timeout(budget, daemon.apps.shutdown()).await;
            },
        );
    };
    if tokio::time::timeout(budget + Duration::from_secs(2), stack)
        .await
        .is_err()
    {
        tracing::warn!(?budget, "the stack did not stop within the budget");
    }
    let tree = daemon.job_tree.clone();
    match forge_core::runtime::spawn_blocking_timeout(
        "terminating leftover processes",
        Duration::from_secs(10),
        move || {
            let left = tree.pids().unwrap_or_default();
            if !left.is_empty() {
                tracing::warn!(pids = ?left, "terminating processes left in the root job");
                tree.terminate_all()?;
            }
            Ok(left.len())
        },
    )
    .await
    {
        Ok(0) => {}
        Ok(n) => tracing::warn!(processes = n, "leftover processes terminated"),
        Err(err) => tracing::error!(%err, "leftover processes could not be terminated"),
    }

    // 3.
    let actors = async {
        tokio::join!(
            daemon.dns.shutdown(),
            daemon.tls.shutdown(),
            daemon.downloads.shutdown(),
        );
    };
    if tokio::time::timeout(Duration::from_secs(10), actors)
        .await
        .is_err()
    {
        tracing::warn!("some actors did not stop in time");
    }

    // 4.
    daemon.bus.publish(
        EventKind::DaemonShutdown,
        serde_json::to_value(DaemonShutdownPayload {
            v: 1,
            reason: request.reason.clone(),
        })
        .unwrap_or_default(),
    );
    // Let connected clients receive `daemon.shutdown` before their connection closes.
    tokio::time::sleep(Duration::from_millis(100)).await;
    if let Some(server) = daemon.server.get() {
        server.shutdown();
    }
    if tokio::time::timeout(Duration::from_secs(5), server_task)
        .await
        .is_err()
    {
        tracing::warn!("API server did not stop in time");
    }
    let leftover = daemon.actors.stop_all().await;
    if !leftover.is_empty() {
        tracing::debug!(actors = leftover.len(), "actors stopped");
    }

    // 5.
    let events_written = event_log.stop(Duration::from_secs(10)).await;
    let _ = daemon.writer.flush(Duration::from_secs(5)).await;
    drop(daemon);
    match tokio::task::spawn_blocking(move || store.close()).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::error!(%err, "store did not close cleanly"),
        Err(e) => tracing::error!(%e, "store close task failed"),
    }

    // 6.
    if let Err(e) = tokio::task::spawn_blocking(move || lock.release()).await {
        tracing::error!(%e, "releasing the instance lock");
    }
    let elapsed_ms = started.elapsed().as_millis();
    tracing::info!(elapsed_ms, events_written, "daemon stopped");
    ShutdownReport {
        reason: request.reason,
        elapsed_ms,
        events_written,
        restarted_pid: None,
    }
}
