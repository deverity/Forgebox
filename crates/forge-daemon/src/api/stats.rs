//! `stats.get` and `metrics.subscribe` (docs/14 §3.6, ADR-0044 §8-§9), answered by the
//! metrics service ([`crate::metrics`], `forge_metrics::service`).
//!
//! * `stats.get`: without `from`, the last `window` up to `to` (default now); with `from`,
//!   the exact range `[from, to)` (the window only labels it). Ranges older than the
//!   in-memory retention are merged with the stored minutes and hours.
//! * `stats.series|routes|export|top`: [`super::stats_series`].
//! * `metrics.subscribe`: answers `last_seq` = `resume_from` (0 without it), then a snapshot
//!   at once (one sample per matched site, metrics off included), then one sample per
//!   matched site with metrics on every `StatsWindow::subscribe_cadence_ms`. `seq` is the
//!   subscription's own. A resumed subscription gets a fresh snapshot and nothing of the
//!   gap. The connection's drop-oldest queue reports drops as `metrics.lagged`
//!   (ADR-0015): a client that stops reading never slows the service down.

use std::collections::BTreeMap;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::ids::SiteId;
use forge_core::store::Site;
use forge_ipc::api::methods::{
    metrics_subscribe, stats_export, stats_get, stats_routes, stats_series, stats_top,
};
use forge_ipc::api::{
    MetricsEnvelope, MetricsSubscribeParams, SiteRef, SiteStatsDto, StatsDto, StatsParams,
    StatsWindow, SubscriptionDto,
};
use forge_ipc::server::{Ctx, Router, StreamSender};
use forge_metrics::{SiteRoute, Sites, Snapshot, Span};

use super::{convert, route, stats_series as views};
use crate::daemon::Daemon;

/// Answer deadline of both methods: a range beyond the in-memory retention reads up to
/// 7 days of stored minutes (below the client's 10 s call timeout).
const ANSWER_TIMEOUT: Duration = Duration::from_secs(8);
/// Ask timeout of the metrics actor (it answers from memory).
const ASK_TIMEOUT: Duration = Duration::from_secs(2);

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, stats_get, get);
    route!(router, daemon, metrics_subscribe, subscribe);
    route!(router, daemon, stats_series, views::series);
    route!(router, daemon, stats_routes, views::routes);
    route!(router, daemon, stats_export, views::export);
    route!(router, daemon, stats_top, views::top);
    router
        .timeout::<stats_get>(ANSWER_TIMEOUT)
        .timeout::<stats_series>(views::ANSWER_TIMEOUT)
        .timeout::<metrics_subscribe>(ANSWER_TIMEOUT);
}

fn window_span(window: StatsWindow) -> Duration {
    Duration::from_secs(u64::from(window.seconds()))
}

fn route_of(site: Site) -> SiteRoute {
    SiteRoute {
        id: site.id,
        host: site.host,
        aliases: site.aliases,
        metrics_enabled: site.metrics_enabled,
    }
}

/// The sites to report, from the daemon's own view (a site created a moment ago is there
/// before the feed hands it to the service): `site` (`not_found` when unknown) or all.
async fn sites_of(daemon: &Daemon, site: Option<&SiteRef>) -> Result<Vec<SiteRoute>> {
    match site {
        Some(site) => Ok(vec![route_of(super::sites::find(daemon, site).await?)]),
        None => crate::metrics::routes(daemon).await,
    }
}

/// What `stats.get` aggregates.
pub fn span_of(params: &StatsParams) -> Result<Span> {
    let window = jiff::SignedDuration::from_secs(i64::from(params.window.seconds()));
    match (params.from, params.to) {
        (Some(from), to) => {
            if to.is_some_and(|to| from >= to) {
                return Err(ForgeError::InvalidInput(
                    "from must be earlier than to".into(),
                ));
            }
            Ok(Span::Range { from, to })
        }
        (None, Some(to)) => Ok(Span::Range {
            from: to - window,
            to: Some(to),
        }),
        (None, None) => Ok(Span::Window(window_span(params.window))),
    }
}

/// Workers of each site's dedicated pool, charged to the site exactly
/// (`SiteStatsDto.processes`, M2-E5-T11). Read from the pools' metric watches: no ask.
fn dedicated_workers(daemon: &Daemon) -> BTreeMap<SiteId, u32> {
    daemon
        .pools
        .entries()
        .into_iter()
        .filter_map(|(key, handle)| {
            let workers = handle.subscribe_metrics().borrow().workers_active;
            key.site.map(|site| (site, workers))
        })
        .collect()
}

/// [`convert::site_stats`] plus the site's exact processes.
fn site_stats(
    sample: &forge_metrics::SiteSample,
    window: StatsWindow,
    exact: &BTreeMap<SiteId, u32>,
) -> SiteStatsDto {
    let mut dto = convert::site_stats(sample, window);
    if let Some(workers) = exact.get(&sample.site) {
        dto.processes = Some(dto.processes.unwrap_or(0) + workers);
    }
    dto
}

async fn get(daemon: Daemon, params: StatsParams, _ctx: Ctx) -> Result<StatsDto> {
    let span = span_of(&params)?;
    let sites = sites_of(&daemon, params.site.as_ref()).await?;
    let snapshot = daemon
        .metrics
        .query(span, Sites::These(sites), ASK_TIMEOUT)
        .await?;
    let exact = dedicated_workers(&daemon);
    Ok(StatsDto {
        window: params.window,
        sites: snapshot
            .samples
            .iter()
            .map(|s| site_stats(s, params.window, &exact))
            .collect(),
    })
}

/// Sends the samples of `snapshot` for `site` (or all); `only_enabled` skips the sites whose
/// metrics are off (periodic samples). `false` once the stream is closed.
fn send_samples(
    sender: &StreamSender,
    seq: &mut u64,
    snapshot: &Snapshot,
    site: Option<SiteId>,
    window: StatsWindow,
    only_enabled: bool,
    exact: &BTreeMap<SiteId, u32>,
) -> bool {
    for sample in &snapshot.samples {
        if site.is_some_and(|id| id != sample.site) || (only_enabled && !sample.metrics_enabled) {
            continue;
        }
        *seq += 1;
        let envelope = MetricsEnvelope {
            subscription_id: sender.id(),
            seq: *seq,
            sample: site_stats(sample, window, exact),
        };
        if !sender.send(&envelope) {
            return false;
        }
    }
    !sender.is_closed()
}

async fn subscribe(
    daemon: Daemon,
    params: MetricsSubscribeParams,
    ctx: Ctx,
) -> Result<SubscriptionDto> {
    let sites = sites_of(&daemon, params.site.as_ref()).await?;
    let site = params
        .site
        .as_ref()
        .and_then(|_| sites.first().map(|s| s.id));
    let window = params.window;
    let span = window_span(window);
    let cadence = Duration::from_millis(u64::from(window.subscribe_cadence_ms()));
    // The watch first, then the snapshot: no publication falls between them unseen.
    let mut updates = daemon.metrics.subscribe(span, cadence, ASK_TIMEOUT).await?;
    let snapshot = daemon
        .metrics
        .query(Span::Window(span), Sites::These(sites), ASK_TIMEOUT)
        .await?;
    let sender = ctx.open_stream(None)?;
    let id = sender.id();
    let last_seq = params.resume_from.unwrap_or(0);
    let mut seq = last_seq;
    // Queued now, delivered right after the answer (the stream starts inactive).
    let exact = dedicated_workers(&daemon);
    if send_samples(&sender, &mut seq, &snapshot, site, window, false, &exact) {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = sender.closed() => return,
                    changed = updates.changed() => {
                        if changed.is_err() {
                            // The service stopped (daemon shutdown).
                            sender.close();
                            return;
                        }
                    }
                }
                // A clone of the `Arc`: the watch's read guard never crosses an `.await`.
                let latest = updates.borrow_and_update().clone();
                if let Some(snapshot) = latest {
                    let exact = dedicated_workers(&daemon);
                    if !send_samples(&sender, &mut seq, &snapshot, site, window, true, &exact) {
                        return;
                    }
                }
            }
        });
    }
    Ok(SubscriptionDto {
        subscription_id: id,
        stream: "metrics".into(),
        last_seq,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> forge_core::Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn spans_follow_the_params() {
        let window = |w| StatsParams {
            window: w,
            ..StatsParams::default()
        };
        assert_eq!(
            span_of(&window(StatsWindow::OneMinute)).unwrap(),
            Span::Window(Duration::from_secs(60))
        );
        let to = ts("2026-11-01T11:00:00Z");
        assert_eq!(
            span_of(&StatsParams {
                to: Some(to),
                ..window(StatsWindow::OneHour)
            })
            .unwrap(),
            Span::Range {
                from: ts("2026-11-01T10:00:00Z"),
                to: Some(to)
            }
        );
        let from = ts("2026-11-01T10:30:00Z");
        assert_eq!(
            span_of(&StatsParams {
                from: Some(from),
                ..window(StatsWindow::SevenDays)
            })
            .unwrap(),
            Span::Range { from, to: None }
        );
        let bad = StatsParams {
            from: Some(to),
            to: Some(from),
            ..StatsParams::default()
        };
        assert_eq!(span_of(&bad).unwrap_err().code(), "invalid_input");
    }
}
