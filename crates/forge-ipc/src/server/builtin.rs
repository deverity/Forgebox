//! Methods served by the server itself, from the components it was built with. The
//! daemon may replace any of them by registering its own handler.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use forge_core::Timestamp;
use forge_core::error::{ForgeError, Result};

use super::fs::{self as fs_ops, FsScope};
use super::router::{Ctx, Router};
use super::{ServerInfo, events};
use crate::api::methods::{
    daemon_status, events_list, events_subscribe, events_unsubscribe, fs_list, fs_read, fs_tail,
    fs_write, jobs_cancel, jobs_get, jobs_list, logs_unsubscribe, metrics_unsubscribe, system_ping,
    terminal_unsubscribe,
};
use crate::api::{
    API_VERSION, Ack, DaemonStatusDto, JobCancelDto, JobDto, JobListDto, PongDto, UnsubscribeParams,
};

/// Deadline of `fs.*` (disk I/O on possibly slow volumes).
const FS_TIMEOUT: Duration = Duration::from_secs(5);

async fn blocking<T, F>(f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ForgeError::Internal(format!("blocking task failed: {e}")))?
}

fn unsubscribe(params: UnsubscribeParams, ctx: Ctx) -> Result<Ack> {
    if ctx.close_stream(params.subscription_id) {
        Ok(Ack::ok())
    } else {
        Err(ForgeError::NotFound(format!(
            "subscription {} on this connection",
            params.subscription_id
        )))
    }
}

/// Router with every built-in that `info` (and `fs`) allows.
pub(crate) fn router(info: &Arc<ServerInfo>, fs: Option<FsScope>) -> Router {
    let mut router = Router::new();

    router.handle::<system_ping, _, _>(|_, _| async {
        Ok(PongDto {
            server_time: Timestamp::now(),
        })
    });

    let status_info = Arc::clone(info);
    router.handle::<daemon_status, _, _>(move |_, _| {
        let info = Arc::clone(&status_info);
        async move {
            // The process counters are a blocking OS call (docs/03 §7.1).
            let meter = Arc::clone(&info);
            let usage = tokio::task::spawn_blocking(move || meter.meter.usage())
                .await
                .map_err(|e| ForgeError::Internal(format!("blocking task failed: {e}")))
                .and_then(|usage| usage);
            let usage = usage.unwrap_or_else(|err| {
                tracing::debug!(%err, "daemon.status: process counters unavailable");
                super::process_meter::Usage {
                    cpu_pct: 0.0,
                    rss_bytes: 0,
                }
            });
            Ok(DaemonStatusDto {
                version: info.config.daemon_version.clone(),
                api_version: API_VERSION,
                platform: std::env::consts::OS.to_owned(),
                pid: std::process::id(),
                started_at: info.started_at,
                uptime_secs: info.started.elapsed().as_secs(),
                root: info
                    .root
                    .as_ref()
                    .map(|r| r.to_posix_string())
                    .unwrap_or_default(),
                portable: info.root.as_ref().is_some_and(|r| r.is_portable()),
                last_event_seq: info.events.as_ref().map_or(0, |e| e.bus().last_seq()),
                active_jobs: info
                    .jobs
                    .as_ref()
                    .map_or(0, |j| u32::try_from(j.active().len()).unwrap_or(u32::MAX)),
                connections: u32::try_from(info.connections.load(Ordering::Relaxed))
                    .unwrap_or(u32::MAX),
                cpu_pct: usage.cpu_pct,
                rss_bytes: usage.rss_bytes,
            })
        }
    });

    router.handle::<logs_unsubscribe, _, _>(|params, ctx| async move { unsubscribe(params, ctx) });
    router
        .handle::<metrics_unsubscribe, _, _>(|params, ctx| async move { unsubscribe(params, ctx) });
    router.handle::<terminal_unsubscribe, _, _>(
        |params, ctx| async move { unsubscribe(params, ctx) },
    );

    if let Some(source) = info.events.clone() {
        let replay_limit = info.config.replay_limit;
        let subscribe_source = source.clone();
        router.handle::<events_subscribe, _, _>(move |params, ctx| {
            events::subscribe(subscribe_source.clone(), replay_limit, params, ctx)
        });
        router.handle::<events_unsubscribe, _, _>(
            |params, ctx| async move { unsubscribe(params, ctx) },
        );
        router.handle::<events_list, _, _>(move |params, _| events::list(source.clone(), params));
    }

    if let Some(jobs) = info.jobs.clone() {
        let list_jobs = jobs.clone();
        router.handle::<jobs_list, _, _>(move |params, _| {
            let jobs = if params.active {
                list_jobs.active()
            } else {
                list_jobs.list()
            };
            async move {
                Ok(JobListDto {
                    jobs: jobs.into_iter().map(JobDto::from).collect(),
                })
            }
        });
        let get_jobs = jobs.clone();
        router.handle::<jobs_get, _, _>(move |params, _| {
            let job = get_jobs.get(params.job_id);
            async move {
                job.map(JobDto::from)
                    .ok_or_else(|| ForgeError::NotFound(format!("job {}", params.job_id)))
            }
        });
        router.handle::<jobs_cancel, _, _>(move |params, _| {
            let outcome = jobs.cancel(params.job_id);
            async move {
                Ok(JobCancelDto {
                    job_id: params.job_id,
                    was_running: outcome?,
                })
            }
        });
    }

    if let Some(scope) = fs {
        let scope = Arc::new(scope);
        let s = Arc::clone(&scope);
        router.handle::<fs_read, _, _>(move |params, _| {
            let s = Arc::clone(&s);
            blocking(move || fs_ops::read(&s, &params))
        });
        let s = Arc::clone(&scope);
        router.handle::<fs_write, _, _>(move |params, _| {
            let s = Arc::clone(&s);
            blocking(move || fs_ops::write(&s, &params))
        });
        let s = Arc::clone(&scope);
        router.handle::<fs_tail, _, _>(move |params, _| {
            let s = Arc::clone(&s);
            blocking(move || fs_ops::tail(&s, &params))
        });
        let s = Arc::clone(&scope);
        router.handle::<fs_list, _, _>(move |params, _| {
            let s = Arc::clone(&s);
            blocking(move || fs_ops::list(&s, &params))
        });
        router
            .timeout::<fs_read>(FS_TIMEOUT)
            .timeout::<fs_write>(FS_TIMEOUT)
            .timeout::<fs_tail>(FS_TIMEOUT)
            .timeout::<fs_list>(FS_TIMEOUT);
    }
    router
}
