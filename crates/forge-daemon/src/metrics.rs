//! The daemon side of the per-site metrics (M2-E5-T8, docs/14 §3, ADR-0044, ADR-0029).
//!
//! The metrics service itself (tailers, aggregator, attribution, persistence) is the
//! `forge_metrics::service` actor; this module only feeds it what the daemon knows and
//! `forge-metrics` must not depend on:
//!
//! * **sources**: the `logs[].access` blocks of the manifests of the instances that run
//!   right now (web servers), the path substituted with the instance's variables and the
//!   mapping converted to an `AccessFormat`. Nothing here knows a tool by id;
//! * **routes**: every site with its host, aliases and `metrics_enabled`;
//! * **pools**: every 10 s the latest sample of each PHP pool (its `subscribe_metrics`
//!   watch, read without holding it across an `.await`) and the sites it serves;
//! * **switch**: `[metrics] enabled`, off by default in a portable root unless the user set
//!   it ([`enabled_for`]).
//!
//! [`initial_sync`] is a boot step (after the web servers start); [`feed_loop`] is a
//! scheduler task that reacts to `site.*`, `service.*` and `config.changed` (debounced) and
//! refreshes the pool samples every 10 s. Every set replaces the whole state, so a message
//! lost to a full mailbox is repaired by the next one.

use std::path::Path;
use std::time::Duration;

use forge_core::config::ForgeConfig;
use forge_core::event::{EventKind, StreamItem};
use forge_core::paths::ForgeRoot;
use forge_ipc::api::ConfigChangedPayload;
use forge_manifest::schema::tool::{AccessMulti, AccessSpec, AccessUnit};
use forge_metrics::{
    AccessFormat, FieldMap, LogSource, Multi, PoolFeed, PoolMode, PoolSample, SiteRoute, Unit,
    UpstreamMap,
};
use forge_supervisor::InstanceState;
use tokio_util::sync::CancellationToken;

use crate::daemon::Daemon;

/// Pool samples are refreshed (and the sources and routes checked again) this often.
pub const FEED_INTERVAL: Duration = Duration::from_secs(10);
/// Debounce of the event reactions.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// Budget of the boot step.
const BOOT_SYNC: Duration = Duration::from_secs(5);

/// Whether metrics are collected with `config` in `root`: the explicit `[metrics] enabled`,
/// else on, except in a portable root (docs/14 §3, docs/08 §2.2).
pub fn enabled_for(config: &ForgeConfig, root: &ForgeRoot) -> bool {
    config.metrics.effective(root.is_portable())
}

fn unit(unit: AccessUnit) -> Unit {
    match unit {
        AccessUnit::S => Unit::Seconds,
        AccessUnit::Ms => Unit::Millis,
        AccessUnit::Us => Unit::Micros,
        AccessUnit::Ns => Unit::Nanos,
    }
}

/// The manifest's `logs[].access` mapping as the parser's format (docs/04 §1.2).
pub fn access_format(spec: &AccessSpec) -> AccessFormat {
    AccessFormat {
        ts: FieldMap::new(spec.ts.pointer.clone(), unit(spec.ts.unit)),
        ts_is_start: spec.ts.start,
        host: spec.host.clone(),
        status: spec.status.clone(),
        duration: FieldMap::new(spec.duration.pointer.clone(), unit(spec.duration.unit)),
        upstream_duration: spec.upstream_duration.as_ref().map(|u| UpstreamMap {
            field: FieldMap::new(u.pointer.clone(), unit(u.unit)),
            multi: match u.multi {
                AccessMulti::Sum => Multi::Sum,
                AccessMulti::Last => Multi::Last,
            },
        }),
        bytes: spec.bytes.clone(),
        method: spec.method.clone(),
        // The parser always drops the query string (`strip_query` is implied).
        uri: spec.uri.as_ref().map(|u| u.pointer.clone()),
        lenient_escapes: spec.lenient_escapes,
    }
}

/// Stable key of a followed file (the persisted checkpoint's `path_key`): the path relative
/// to the root with `/` when it is inside it, else the absolute path with `/`.
pub fn source_key(root: &ForgeRoot, path: &Path) -> String {
    let rel = path.strip_prefix(root.path()).unwrap_or(path);
    forge_core::paths::posix_string(rel)
}

/// The access logs of the instances that run now.
pub async fn sources(daemon: &Daemon) -> Vec<LogSource> {
    let mut out: Vec<LogSource> = Vec::new();
    for (entry, status) in daemon.services.statuses().await {
        let running = status
            .as_ref()
            .is_some_and(|s| matches!(s.state, InstanceState::Running | InstanceState::Starting));
        if !running {
            continue;
        }
        let (Some(manifest), Some(vars)) = (&entry.manifest, &entry.vars) else {
            continue;
        };
        let Some(spec) = manifest.spec.service.daemon.as_ref() else {
            continue;
        };
        for log in &spec.logs {
            let Some(access) = &log.access else {
                continue;
            };
            let path = match vars.substitute(&log.path) {
                Ok(path) => std::path::PathBuf::from(path),
                Err(err) => {
                    tracing::warn!(instance = %entry.label(), %err, "metrics: access log path");
                    continue;
                }
            };
            let key = source_key(&daemon.root, &path);
            // Two instances writing the same file are one source.
            if out.iter().any(|s| s.key == key) {
                continue;
            }
            out.push(LogSource {
                key,
                path,
                format: access_format(access),
            });
        }
    }
    out
}

/// Every site with its names and switch.
pub async fn routes(daemon: &Daemon) -> forge_core::error::Result<Vec<SiteRoute>> {
    Ok(daemon
        .sites
        .list()
        .await?
        .into_iter()
        .map(|site| SiteRoute {
            id: site.id,
            host: site.host,
            aliases: site.aliases,
            metrics_enabled: site.metrics_enabled,
        })
        .collect())
}

/// The latest sample of every pool and the sites it serves (shared pools: the dedicated
/// mode arrives with `sites.pool_mode`, M2-E5-T11).
pub async fn pools(daemon: &Daemon) -> Vec<PoolFeed> {
    let handles = daemon.pools.handles();
    if handles.is_empty() {
        return Vec::new();
    }
    let sites = match daemon.sites.list().await {
        Ok(sites) => sites,
        Err(err) => {
            tracing::debug!(%err, "metrics: sites for the pool feed");
            return Vec::new();
        }
    };
    let plan = match daemon.web.php_plan(&sites).await {
        Ok(plan) => plan,
        Err(err) => {
            tracing::debug!(%err, "metrics: php plan for the pool feed");
            return Vec::new();
        }
    };
    handles
        .into_iter()
        .map(|handle| {
            let version = handle.version_id();
            // A copy of the watch value: the borrow never crosses an `.await`.
            let metrics = handle.subscribe_metrics().borrow().clone();
            PoolFeed {
                sample: PoolSample {
                    pool: version.to_string(),
                    mode: PoolMode::Shared,
                    cpu_pct: metrics.cpu_pct.unwrap_or(0.0),
                    mem_bytes: metrics.rss_bytes,
                    workers_active: metrics.workers_active,
                    from_ms: 0,
                    to_ms: 0,
                },
                sites: plan
                    .iter()
                    .filter(|(_, php)| php.version.id == version)
                    .map(|(site, _)| *site)
                    .collect(),
            }
        })
        .collect()
}

/// What the feed last sent (a set is only sent again when it changed).
#[derive(Default)]
struct Sent {
    sources: Option<Vec<LogSource>>,
    routes: Option<Vec<SiteRoute>>,
    enabled: Option<bool>,
}

async fn sync_sources(daemon: &Daemon, sent: &mut Sent) {
    let sources = sources(daemon).await;
    if sent.sources.as_ref() != Some(&sources) && daemon.metrics.set_sources(sources.clone()) {
        sent.sources = Some(sources);
    }
}

async fn sync_routes(daemon: &Daemon, sent: &mut Sent) {
    match routes(daemon).await {
        Ok(routes) => {
            if sent.routes.as_ref() != Some(&routes) && daemon.metrics.set_routes(routes.clone()) {
                sent.routes = Some(routes);
            }
        }
        Err(err) => tracing::warn!(%err, "metrics: sites not read"),
    }
}

fn sync_enabled(daemon: &Daemon, sent: &mut Sent) {
    let enabled = enabled_for(&daemon.config(), &daemon.root);
    if sent.enabled != Some(enabled) && daemon.metrics.set_enabled(enabled) {
        sent.enabled = Some(enabled);
    }
}

async fn sync_all(daemon: &Daemon, sent: &mut Sent) {
    sync_enabled(daemon, sent);
    sync_routes(daemon, sent).await;
    sync_sources(daemon, sent).await;
}

/// Boot step (after the web servers start): the first routes and sources, bounded.
pub async fn initial_sync(daemon: &Daemon) {
    let mut sent = Sent::default();
    if tokio::time::timeout(BOOT_SYNC, sync_all(daemon, &mut sent))
        .await
        .is_err()
    {
        tracing::warn!("metrics: first sync did not finish in time; the feed retries");
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Dirty {
    routes: bool,
    sources: bool,
    enabled: bool,
}

impl Dirty {
    fn any(self) -> bool {
        self.routes || self.sources || self.enabled
    }

    fn of(kind: EventKind, payload: &serde_json::Value) -> Self {
        let mut dirty = Self::default();
        match kind {
            EventKind::SiteCreated
            | EventKind::SiteUpdated
            | EventKind::SiteRemoved
            | EventKind::SitePhpChanged => dirty.routes = true,
            EventKind::ServiceRunning
            | EventKind::ServiceStarting
            | EventKind::ServiceStopped
            | EventKind::ServiceCrashed
            | EventKind::ServiceFailed => dirty.sources = true,
            EventKind::ConfigChanged => {
                dirty.enabled = serde_json::from_value::<ConfigChangedPayload>(payload.clone())
                    .is_ok_and(|p| p.keys.iter().any(|k| k.starts_with("metrics")));
            }
            _ => {}
        }
        dirty
    }

    fn merge(&mut self, other: Self) {
        self.routes |= other.routes;
        self.sources |= other.sources;
        self.enabled |= other.enabled;
    }
}

/// Scheduler task: keeps the service in step with the sites, the running instances, the
/// preferences and the pools until `stop`.
pub async fn feed_loop(daemon: Daemon, stop: CancellationToken) {
    let mut events = daemon.bus.subscribe();
    let mut sent = Sent::default();
    sync_all(&daemon, &mut sent).await;
    let mut feed = tokio::time::interval(FEED_INTERVAL);
    feed.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let mut dirty = tokio::select! {
            () = stop.cancelled() => return,
            _ = feed.tick() => {
                daemon.metrics.set_pools(pools(&daemon).await);
                // What changed without an event, or a set lost to a full mailbox (`sent`
                // only records what the service accepted), is caught up here.
                sync_all(&daemon, &mut sent).await;
                Dirty::default()
            }
            item = events.recv_item() => match item {
                Some(StreamItem::Event(event)) => Dirty::of(event.kind, &event.payload),
                Some(StreamItem::Lagged { .. }) => Dirty { routes: true, sources: true, enabled: true },
                None => return,
            },
        };
        if !dirty.any() {
            continue;
        }
        // Debounce: wait for the burst to end.
        loop {
            let more = tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(DEBOUNCE) => None,
                item = events.recv_item() => Some(item),
            };
            match more {
                None => break,
                Some(Some(StreamItem::Event(event))) => {
                    dirty.merge(Dirty::of(event.kind, &event.payload));
                }
                Some(Some(StreamItem::Lagged { .. })) => dirty.merge(Dirty {
                    routes: true,
                    sources: true,
                    enabled: true,
                }),
                Some(None) => return,
            }
        }
        if dirty.enabled {
            sync_enabled(&daemon, &mut sent);
        }
        if dirty.routes {
            sync_routes(&daemon, &mut sent).await;
        }
        if dirty.sources {
            sync_sources(&daemon, &mut sent).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nginx_mapping_of_the_registry_becomes_a_parser_format() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/tools/nginx.yaml");
        let forge_manifest::schema::Manifest::Tool(manifest) =
            forge_manifest::parse::load_file(&path).unwrap()
        else {
            panic!("nginx.yaml is a tool manifest");
        };
        let access = manifest.spec.service.daemon.as_ref().unwrap().logs[1]
            .access
            .clone()
            .unwrap();
        let format = access_format(&access);
        assert_eq!(format.ts, FieldMap::new("/t", Unit::Seconds));
        assert!(!format.ts_is_start);
        assert_eq!(format.host, "/host");
        assert_eq!(
            format.upstream_duration,
            Some(UpstreamMap {
                field: FieldMap::new("/ut", Unit::Seconds),
                multi: Multi::Sum,
            })
        );
        assert_eq!(format.uri.as_deref(), Some("/u"));
        let parser = forge_metrics::AccessParser::new(&format).unwrap();
        let rec = parser
            .parse(br#"{"t":"1761652800.250","host":"loja.test:80","s":503,"rt":0.120,"ut":"0.100","b":12,"m":"GET","u":"/p/12","up":"127.0.0.1:19000"}"#)
            .unwrap();
        assert_eq!(
            (rec.status, rec.duration_us, rec.upstream_us),
            (503, 120_000, Some(100_000))
        );
    }

    #[test]
    fn source_keys_are_relative_to_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let inside = root.logs().join("nginx").join("access.json");
        assert_eq!(source_key(&root, &inside), "logs/nginx/access.json");
        let outside = Path::new("D:/elsewhere/access.json");
        assert_eq!(source_key(&root, outside), "D:/elsewhere/access.json");
    }

    #[test]
    fn portable_roots_default_to_off_unless_set() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let mut config = ForgeConfig::default();
        assert!(enabled_for(&config, &root));
        std::fs::write(root.portable_flag(), "").unwrap();
        assert!(!enabled_for(&config, &root));
        config.metrics.enabled = Some(true);
        assert!(enabled_for(&config, &root));
        config.metrics.enabled = Some(false);
        std::fs::remove_file(root.portable_flag()).unwrap();
        assert!(!enabled_for(&config, &root));
    }

    #[test]
    fn events_mark_what_to_resync() {
        let none = serde_json::Value::Null;
        assert!(Dirty::of(EventKind::SiteCreated, &none).routes);
        assert!(Dirty::of(EventKind::ServiceRunning, &none).sources);
        assert!(!Dirty::of(EventKind::DaemonReady, &none).any());
        let keys = |k: &str| serde_json::json!({"v": 1, "keys": [k]});
        assert!(Dirty::of(EventKind::ConfigChanged, &keys("metrics.enabled")).enabled);
        assert!(!Dirty::of(EventKind::ConfigChanged, &keys("tld")).any());
    }
}
