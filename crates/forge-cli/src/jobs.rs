//! Long operations: a job method answers at once with a `JobDto`; the CLI follows it
//! through `events.subscribe` (`job.*` and `tool.install.*`) with a poll of `jobs.get` as
//! a safety net, draws progress bars on a terminal, and returns the job's result or its
//! error (with the job's stable `code`, so exit codes work the same as for plain calls).
//!
//! The subscription is opened *before* the method is called, so a job that finishes
//! before the answer reaches us is still seen; its events are buffered by the client.

use std::collections::HashMap;
use std::time::Duration;

use forge_ipc::api::methods::{jobs_cancel, jobs_get, jobs_list};
use forge_ipc::api::{
    ApiMethod, EventEnvelope, EventFilter, JobCancelledPayload, JobDonePayload, JobDto,
    JobFailedPayload, JobIdParams, JobListParams, JobProgress, JobProgressPayload, JobState,
    ToolInstallPayload,
};
use forge_ipc::base::ids::JobId;
use forge_ipc::client::{Subscription, SubscriptionItem};
use forge_ipc::{Client, ClientError};
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::context::Context;
use crate::error::{CliError, CliResult};
use crate::messages::Msg;
use crate::output::{Output, format_bytes};

/// Poll interval of `jobs.get` while following a job.
const POLL: Duration = Duration::from_secs(1);
/// How long to wait for a cancelled job to end.
const CANCEL_WAIT: Duration = Duration::from_secs(5);

/// Starts `M` and waits for the job; returns its result as `R`.
pub async fn run<M, R>(ctx: &Context, params: M::Params) -> CliResult<R>
where
    M: ApiMethod<Result = JobDto>,
    R: DeserializeOwned + Serialize,
{
    let client = ctx.client().await?;
    let mut events = open_events(client).await;
    let job = client.call::<M>(params).await?;
    let value = wait(&ctx.out, client, job, events.as_mut()).await?;
    serde_json::from_value(value).map_err(|e| {
        CliError::new(
            "internal",
            format!("resultado de {} inesperado: {e}", M::INFO.name),
        )
    })
}

/// The `job.*` / `tool.install.*` stream; `None` when it cannot be opened (the poll
/// still works).
pub async fn open_events(client: &Client) -> Option<Subscription<EventEnvelope>> {
    client
        .subscribe_events(EventFilter::kinds(["job.*", "tool.install.*"]), None)
        .await
        .ok()
}

/// Follows `job` until it ends. Returns `job.done`'s result.
pub async fn wait(
    out: &Output,
    client: &Client,
    job: JobDto,
    mut events: Option<&mut Subscription<EventEnvelope>>,
) -> CliResult<Value> {
    let id = job.id;
    let mut ui = JobUi::new(out, &job.name);
    ui.progress(&job.progress);
    let mut current = job;
    let mut poll = tokio::time::interval(POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    poll.tick().await;
    let outcome = loop {
        if current.state.is_terminal() {
            break finish(current);
        }
        tokio::select! {
            item = next_event(&mut events) => match item {
                Some(envelope) => {
                    if let Some(end) = apply_event(&mut ui, &id, &envelope) {
                        break end;
                    }
                }
                None => events = None,
            },
            _ = poll.tick() => {
                match client.call::<jobs_get>(JobIdParams { job_id: id }).await {
                    Ok(snapshot) => {
                        ui.progress(&snapshot.progress);
                        current = snapshot;
                    }
                    Err(err) => break Err(err.into()),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                ui.finish(false);
                return Err(cancel(out, client, id).await);
            }
        }
    };
    ui.finish(outcome.is_ok());
    outcome
}

/// Next `job.*` / `tool.install.*` event, pending forever when there is no stream.
async fn next_event(
    events: &mut Option<&mut Subscription<EventEnvelope>>,
) -> Option<EventEnvelope> {
    let Some(sub) = events.as_mut() else {
        return std::future::pending().await;
    };
    loop {
        match sub.next_item().await? {
            SubscriptionItem::Item(envelope) => return Some(envelope),
            SubscriptionItem::Lagged { .. } => {}
        }
    }
}

/// Updates the UI with an event of job `id`; `Some` when the event ends the job.
fn apply_event(ui: &mut JobUi, id: &JobId, envelope: &EventEnvelope) -> Option<CliResult<Value>> {
    let event = &envelope.event;
    let payload = &event.payload;
    match event.kind.as_str() {
        "job.progress" => {
            let p: JobProgressPayload = serde_json::from_value(payload.clone()).ok()?;
            if p.job_id == *id {
                ui.progress(&p.progress);
            }
            None
        }
        "job.done" => {
            let p: JobDonePayload = serde_json::from_value(payload.clone()).ok()?;
            (p.job_id == *id).then_some(Ok(p.result))
        }
        "job.failed" => {
            let p: JobFailedPayload = serde_json::from_value(payload.clone()).ok()?;
            (p.job_id == *id).then(|| Err(CliError::new(p.error.code, p.error.message)))
        }
        "job.cancelled" => {
            let p: JobCancelledPayload = serde_json::from_value(payload.clone()).ok()?;
            (p.job_id == *id).then(|| Err(CliError::new("cancelled", "operação cancelada")))
        }
        kind if kind.starts_with("tool.install.") => {
            let p: ToolInstallPayload = serde_json::from_value(payload.clone()).ok()?;
            if p.job_id == Some(*id) {
                ui.tool(kind, &p);
            }
            None
        }
        _ => None,
    }
}

/// The outcome of a job that is already terminal.
fn finish(job: JobDto) -> CliResult<Value> {
    match job.state {
        JobState::Done => Ok(job.result.unwrap_or(Value::Null)),
        JobState::Cancelled => Err(CliError::new("cancelled", "operação cancelada")),
        _ => {
            let error = job.error.unwrap_or(forge_ipc::api::JobErrorInfo {
                code: "internal".into(),
                message: format!("{} falhou", job.name),
            });
            Err(CliError::new(error.code, error.message))
        }
    }
}

/// Ctrl+C: asks the daemon to cancel and waits a little for it.
async fn cancel(out: &Output, client: &Client, id: JobId) -> CliError {
    out.note(out.t(Msg::Cancelling));
    let _ = client.call::<jobs_cancel>(JobIdParams { job_id: id }).await;
    let deadline = tokio::time::Instant::now() + CANCEL_WAIT;
    while tokio::time::Instant::now() < deadline {
        match client.call::<jobs_get>(JobIdParams { job_id: id }).await {
            Ok(job) if job.state.is_terminal() => break,
            Err(ClientError::Disconnected) => break,
            _ => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
    CliError::new("cancelled", out.t(Msg::Cancelled))
}

/// `forge jobs [--active]` data.
pub async fn list(client: &Client, active: bool) -> CliResult<Vec<JobDto>> {
    Ok(client
        .call::<jobs_list>(JobListParams { active })
        .await?
        .jobs)
}

// --- progress bars -------------------------------------------------------------------

/// Progress display of one job: a main bar plus one per tool being installed. Drawn only
/// on an interactive stderr and never with `--json` or `--quiet`.
struct JobUi {
    multi: MultiProgress,
    main: ProgressBar,
    items: HashMap<String, ProgressBar>,
    enabled: bool,
}

fn bar_style(with_bar: bool) -> ProgressStyle {
    let template = if with_bar {
        "{spinner:.green} {prefix:.bold} [{bar:30.cyan/blue}] {pos:>3}% {msg}"
    } else {
        "{spinner:.green} {prefix:.bold} {msg}"
    };
    ProgressStyle::with_template(template)
        .unwrap_or_else(|_| ProgressStyle::default_bar())
        .progress_chars("=> ")
}

impl JobUi {
    fn new(out: &Output, name: &str) -> Self {
        let enabled = out.interactive();
        let multi = MultiProgress::with_draw_target(if enabled {
            ProgressDrawTarget::stderr()
        } else {
            ProgressDrawTarget::hidden()
        });
        let main = multi.add(ProgressBar::new(100));
        main.set_style(bar_style(false));
        main.set_prefix(name.to_owned());
        if enabled {
            main.enable_steady_tick(Duration::from_millis(120));
        }
        Self {
            multi,
            main,
            items: HashMap::new(),
            enabled,
        }
    }

    fn progress(&mut self, progress: &JobProgress) {
        if !self.enabled {
            return;
        }
        if let Some(pct) = progress.pct {
            self.main.set_style(bar_style(true));
            self.main.set_position(pct.round() as u64);
        }
        self.main.set_message(describe(progress));
    }

    fn tool(&mut self, kind: &str, payload: &ToolInstallPayload) {
        if !self.enabled {
            return;
        }
        let key = format!("{}@{}", payload.tool, payload.version);
        let bar = self.items.entry(key.clone()).or_insert_with(|| {
            let bar = self.multi.add(ProgressBar::new(100));
            bar.set_style(bar_style(true));
            bar.set_prefix(key);
            bar.enable_steady_tick(Duration::from_millis(120));
            bar
        });
        if let Some(progress) = &payload.progress {
            if let Some(pct) = progress.pct {
                bar.set_position(pct.round() as u64);
            }
            let mut message = describe(progress);
            if let Some(step) = &payload.step {
                message = format!("{step} {message}");
            }
            bar.set_message(message);
        }
        match kind {
            "tool.install.completed" => {
                bar.set_position(100);
                bar.finish_with_message("ok");
            }
            "tool.install.failed" => {
                let why = payload
                    .error
                    .as_ref()
                    .map_or_else(|| "falhou".to_owned(), |e| e.message.clone());
                bar.abandon_with_message(why);
            }
            _ => {}
        }
    }

    fn finish(&mut self, ok: bool) {
        if !self.enabled {
            return;
        }
        for bar in self.items.values() {
            if !bar.is_finished() {
                bar.finish_and_clear();
            }
        }
        if ok {
            self.main.finish_and_clear();
        } else {
            self.main.abandon();
        }
    }
}

/// `12.3 MiB/45.0 MiB  3.2 MiB/s  ETA 10s  message`.
fn describe(progress: &JobProgress) -> String {
    let mut parts = Vec::new();
    if let Some(done) = progress.bytes_done {
        parts.push(match progress.bytes_total {
            Some(total) => format!("{}/{}", format_bytes(done), format_bytes(total)),
            None => format_bytes(done),
        });
    }
    if let Some(speed) = progress.speed_bps {
        parts.push(format!("{}/s", format_bytes(speed)));
    }
    if let Some(eta) = progress.eta_secs {
        parts.push(format!("ETA {}", crate::output::format_duration(eta)));
    }
    if let Some(message) = &progress.message {
        parts.push(message.clone());
    }
    parts.join("  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_described_with_speed_and_eta() {
        let progress = JobProgress::new()
            .pct(27.0)
            .bytes(12 * 1024 * 1024, Some(45 * 1024 * 1024))
            .rate(3 * 1024 * 1024, Some(10))
            .message("php@8.3");
        assert_eq!(
            describe(&progress),
            "12.0 MiB/45.0 MiB  3.0 MiB/s  ETA 10s  php@8.3"
        );
        assert_eq!(describe(&JobProgress::new()), "");
    }

    #[test]
    fn finished_jobs_map_to_results_and_codes() {
        let mut job = sample(JobState::Done);
        job.result = Some(serde_json::json!({"ok": true}));
        assert_eq!(finish(job).unwrap(), serde_json::json!({"ok": true}));
        assert_eq!(finish(sample(JobState::Done)).unwrap(), Value::Null);
        assert_eq!(
            finish(sample(JobState::Cancelled)).unwrap_err().code,
            "cancelled"
        );
        let mut failed = sample(JobState::Failed);
        failed.error = Some(forge_ipc::api::JobErrorInfo {
            code: "not_found".into(),
            message: "no such tool".into(),
        });
        let err = finish(failed).unwrap_err();
        assert_eq!((err.code.as_str(), err.exit), ("not_found", 3));
    }

    fn envelope(kind: &str, payload: Value) -> EventEnvelope {
        EventEnvelope {
            subscription_id: forge_ipc::base::ids::SubscriptionId::new(),
            seq: 1,
            event: forge_ipc::api::EventDto {
                id: forge_ipc::base::ids::EventId::new(),
                seq: 1,
                ts: "2026-10-27T12:00:00Z".parse().unwrap(),
                kind: kind.into(),
                payload,
            },
        }
    }

    fn ui() -> JobUi {
        // Tests run without a terminal: nothing is drawn.
        JobUi::new(
            &Output::new(false, false, crate::messages::Lang::PtBr),
            "test",
        )
    }

    #[test]
    fn events_of_the_job_end_it_and_events_of_others_are_ignored() {
        let id = JobId::new();
        let other = JobId::new();
        let mut ui = ui();
        let done = |job: JobId| serde_json::json!({"v": 1, "job_id": job, "name": "x", "duration_ms": 5, "result": {"n": 1}});
        assert!(apply_event(&mut ui, &id, &envelope("job.done", done(other))).is_none());
        let end = apply_event(&mut ui, &id, &envelope("job.done", done(id))).unwrap();
        assert_eq!(end.unwrap(), serde_json::json!({"n": 1}));

        let failed = serde_json::json!({
            "v": 1, "job_id": id, "name": "x", "duration_ms": 5,
            "error": {"code": "conflict", "message": "host taken"}
        });
        let err = apply_event(&mut ui, &id, &envelope("job.failed", failed))
            .unwrap()
            .unwrap_err();
        assert_eq!((err.code.as_str(), err.exit), ("conflict", 4));

        let cancelled = serde_json::json!({"v": 1, "job_id": id, "name": "x", "duration_ms": 5, "forced": false});
        let err = apply_event(&mut ui, &id, &envelope("job.cancelled", cancelled))
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "cancelled");
    }

    #[test]
    fn progress_and_install_events_never_end_the_job() {
        let id = JobId::new();
        let mut ui = ui();
        let progress =
            serde_json::json!({"v": 1, "job_id": id, "pct": 40.0, "message": "downloading"});
        assert!(apply_event(&mut ui, &id, &envelope("job.progress", progress)).is_none());
        let install = serde_json::json!({
            "v": 1, "job_id": id, "tool": "php", "version": "8.3.1",
            "progress": {"pct": 10.0}, "step": "extract"
        });
        assert!(apply_event(&mut ui, &id, &envelope("tool.install.progress", install)).is_none());
        // Kinds this client does not know are ignored (newer daemons).
        assert!(
            apply_event(
                &mut ui,
                &id,
                &envelope("job.future_kind", serde_json::json!({}))
            )
            .is_none()
        );
        // A payload that does not parse is ignored too.
        assert!(
            apply_event(
                &mut ui,
                &id,
                &envelope("job.done", serde_json::json!({"x": 1}))
            )
            .is_none()
        );
    }

    fn sample(state: JobState) -> JobDto {
        JobDto {
            id: JobId::new(),
            name: "x".into(),
            request_id: None,
            state,
            progress: JobProgress::new(),
            cancel_requested: false,
            started_at: "2026-10-27T12:00:00Z".parse().unwrap(),
            finished_at: None,
            result: None,
            error: None,
        }
    }
}
