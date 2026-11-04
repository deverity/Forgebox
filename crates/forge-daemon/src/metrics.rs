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
//!   watch, read without holding it across an `.await`) and the sites it serves; a
//!   site's dedicated pool (`pool_mode: dedicated`) goes as `PoolMode::Dedicated`, so its
//!   CPU/RAM are exact (`approx = false`);
//! * **switch**: `[metrics] enabled`, off by default in a portable root unless the user set
//!   it ([`enabled_for`]);
//! * **rotation** (M2-E5-T12, docs/14 section 3.8): [`rotate_loop`] applies the manifest's
//!   `logs[].access.rotate` of the running instances ([`rotation_targets`], [`run_rotation`]).
//!
//! [`initial_sync`] is a boot step (after the web servers start); [`feed_loop`] is a
//! scheduler task that reacts to `site.*`, `service.*` and `config.changed` (debounced) and
//! refreshes the pool samples every 10 s. Every set replaces the whole state, so a message
//! lost to a full mailbox is repaired by the next one.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use forge_core::config::ForgeConfig;
use forge_core::event::{EventKind, StreamItem};
use forge_core::ids::{InstanceId, SiteId};
use forge_core::paths::ForgeRoot;
use forge_fcgi::PoolMetrics;
use forge_ipc::api::ConfigChangedPayload;
use forge_manifest::schema::tool::{
    AccessMulti, AccessRotate, AccessSpec, AccessUnit, Reload, RotateStrategy,
};
use forge_metrics::rotate::{self, Check, RotatePolicy, Rotator, Strategy};
use forge_metrics::{
    AccessFormat, FieldMap, LogSource, Multi, PoolFeed, PoolMode, PoolSample, SiteRoute, Unit,
    UpstreamMap,
};
use forge_supervisor::InstanceState;
use tokio_util::sync::CancellationToken;

use crate::daemon::Daemon;
use crate::pools::PoolKey;

/// Pool samples are refreshed (and the sources and routes checked again) this often.
pub const FEED_INTERVAL: Duration = Duration::from_secs(10);
/// Debounce of the event reactions.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// Budget of the boot step.
const BOOT_SYNC: Duration = Duration::from_secs(5);
/// How often the sizes of the access logs are looked at.
pub const ROTATE_INTERVAL: Duration = Duration::from_secs(5);
/// Budget of one rotation step (the renames, or the reopen command with its own timeouts).
const ROTATE_STEP: Duration = Duration::from_secs(45);

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

/// An access log of an instance that runs now.
struct AccessLog {
    instance: InstanceId,
    key: String,
    path: PathBuf,
    access: AccessSpec,
}

/// The `logs[].access` blocks of the running instances, the path substituted with the
/// instance's variables. Two instances writing the same file are one log.
async fn running_access_logs(daemon: &Daemon) -> Vec<AccessLog> {
    let mut out: Vec<AccessLog> = Vec::new();
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
            if out.iter().any(|l| l.key == key) {
                continue;
            }
            out.push(AccessLog {
                instance: entry.row.id,
                key,
                path,
                access: access.clone(),
            });
        }
    }
    out
}

/// The access logs of the instances that run now.
pub async fn sources(daemon: &Daemon) -> Vec<LogSource> {
    running_access_logs(daemon)
        .await
        .into_iter()
        .map(|log| LogSource {
            key: log.key,
            path: log.path,
            format: access_format(&log.access),
        })
        .collect()
}

/// An access log the daemon keeps from growing (docs/14 section 3.8).
#[derive(Debug, Clone)]
pub struct RotationTarget {
    /// Same key as the metrics source (path relative to the root).
    pub key: String,
    pub path: PathBuf,
    pub policy: RotatePolicy,
    /// The instance that writes the file, told to reopen it after the rename.
    pub instance: InstanceId,
    /// `access.rotate.reopen`; only used by `rename-reopen`.
    pub reopen: Option<Reload>,
}

/// The manifest's `access.rotate` as a policy (absent: `none`, with the default size for
/// the warning). An unreadable `max_size` falls back to the default; the manifest linter
/// already rejects it.
pub fn rotation_policy(rotate: Option<&AccessRotate>) -> RotatePolicy {
    let Some(rotate) = rotate else {
        return RotatePolicy::new(Strategy::None, None, None);
    };
    let strategy = match rotate.strategy {
        RotateStrategy::RenameReopen => Strategy::RenameReopen,
        RotateStrategy::SelfRotating => Strategy::SelfRotating,
        RotateStrategy::None => Strategy::None,
    };
    let max_size = rotate
        .max_size
        .as_deref()
        .and_then(forge_manifest::parse::parse_size);
    RotatePolicy::new(strategy, max_size, Some(rotate.keep))
}

/// The rotation of every access log of the running instances (including `self` and `none`
/// ones: the first is left alone, the second is watched).
pub async fn rotation_targets(daemon: &Daemon) -> Vec<RotationTarget> {
    running_access_logs(daemon)
        .await
        .into_iter()
        .map(|log| RotationTarget {
            key: log.key,
            path: log.path,
            policy: rotation_policy(log.access.rotate.as_ref()),
            instance: log.instance,
            reopen: log.access.rotate.as_ref().and_then(|r| r.reopen.clone()),
        })
        .collect()
}

/// What the blocking half of one rotation step decided.
enum Step {
    Idle,
    Warn {
        size: u64,
        limit: u64,
    },
    /// Renamed (or a reopen is still owed): tell the server.
    Reopen {
        renamed_size: Option<u64>,
    },
    /// The rename failed; the next try is after the given wait.
    RenameFailed {
        error: String,
        wait: Duration,
    },
}

/// The reopen of a [`RotationTarget`]: `Ok(true)` when a running instance was told,
/// `Ok(false)` when it is not running.
pub type ReopenFuture = futures::future::BoxFuture<'static, forge_core::error::Result<bool>>;

/// One pass over `targets`: renames the files that reached their `max_size` (in
/// `spawn_blocking`), runs `reopen` for them (the daemon passes the instance's reload path,
/// [`crate::services::ServiceManager::apply_reopen`]) and warns about the `none` ones that
/// grew past 4x `max_size`. A failed rename or reopen backs off (5 s doubling to 10 min) and
/// is tried again; `reopen` returning `Ok(false)` (instance not running) counts as done: the
/// next start opens a new file.
pub async fn run_rotation<F>(rotator: &Arc<Mutex<Rotator>>, targets: &[RotationTarget], reopen: F)
where
    F: Fn(RotationTarget) -> ReopenFuture,
{
    {
        let paths: Vec<&Path> = targets.iter().map(|t| t.path.as_path()).collect();
        if let Ok(mut r) = rotator.lock() {
            r.retain(&paths);
        }
    }
    for target in targets {
        let step = {
            let rotator = Arc::clone(rotator);
            let (path, policy) = (target.path.clone(), target.policy);
            forge_core::runtime::spawn_blocking_timeout(
                "access log rotation",
                ROTATE_STEP,
                move || {
                    // The lock lives inside the blocking closure: never across an `.await`.
                    let mut r = rotator.lock().map_err(|_| {
                        forge_core::error::ForgeError::Internal("rotator lock".into())
                    })?;
                    let now = Instant::now();
                    Ok(match r.check(&path, &policy, now) {
                        Check::Idle => Step::Idle,
                        Check::Warn { size, limit } => Step::Warn { size, limit },
                        Check::Reopen => Step::Reopen { renamed_size: None },
                        Check::Rotate { size } => match rotate::rotate_files(&path, policy.keep) {
                            Ok(()) => {
                                r.rotated(&path, now);
                                Step::Reopen {
                                    renamed_size: Some(size),
                                }
                            }
                            Err(e) => Step::RenameFailed {
                                error: e.to_string(),
                                wait: r.failed(&path, now),
                            },
                        },
                    })
                },
            )
            .await
        };
        match step {
            Ok(Step::Idle) => {}
            Ok(Step::Warn { size, limit }) => tracing::warn!(
                log = %target.key, size, limit,
                "metrics: access log is not rotated (strategy none) and grew past 4x max_size"
            ),
            Ok(Step::RenameFailed { error, wait }) => tracing::warn!(
                log = %target.key, %error, retry_in = ?wait,
                "metrics: access log could not be renamed for rotation"
            ),
            Ok(Step::Reopen { renamed_size }) => {
                if let Some(size) = renamed_size {
                    tracing::info!(log = %target.key, size, keep = target.policy.keep, "metrics: access log rotated");
                }
                let result = tokio::time::timeout(ROTATE_STEP, reopen(target.clone())).await;
                let Ok(mut r) = rotator.lock() else {
                    continue;
                };
                match result {
                    Ok(Ok(_)) => r.reopened(&target.path),
                    Ok(Err(err)) => {
                        let wait = r.failed(&target.path, Instant::now());
                        tracing::warn!(log = %target.key, %err, retry_in = ?wait, "metrics: the server did not reopen its access log");
                    }
                    Err(_) => {
                        let wait = r.failed(&target.path, Instant::now());
                        tracing::warn!(log = %target.key, retry_in = ?wait, "metrics: reopening the access log timed out");
                    }
                }
            }
            Err(err) => tracing::warn!(log = %target.key, %err, "metrics: rotation step failed"),
        }
    }
}

/// Scheduler task: every [`ROTATE_INTERVAL`], rotates the access logs of the running
/// instances by their manifests (no per-tool code) until `stop`.
pub async fn rotate_loop(daemon: Daemon, stop: CancellationToken) {
    let rotator = Arc::new(Mutex::new(Rotator::default()));
    let mut tick = tokio::time::interval(ROTATE_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = tick.tick() => {}
        }
        let targets = rotation_targets(&daemon).await;
        if targets.is_empty() {
            continue;
        }
        let processes = Arc::clone(daemon.platform.processes());
        let reopen = |target: RotationTarget| -> ReopenFuture {
            let (services, processes) = (daemon.services.clone(), Arc::clone(&processes));
            Box::pin(async move {
                match &target.reopen {
                    Some(reopen) => {
                        services
                            .apply_reopen(target.instance, reopen, &processes)
                            .await
                    }
                    None => Ok(true),
                }
            })
        };
        tokio::select! {
            () = stop.cancelled() => return,
            () = run_rotation(&rotator, &targets, reopen) => {}
        }
    }
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

/// The latest sample of every pool and the sites it serves: a shared pool serves the
/// sites of its version in shared mode (CPU/RAM apportioned, `approx`), a dedicated pool
/// its own site (exact, ADR-0044 §5).
pub async fn pools(daemon: &Daemon) -> Vec<PoolFeed> {
    let entries = daemon.pools.entries();
    if entries.is_empty() {
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
    let samples: Vec<(PoolKey, PoolMetrics)> = entries
        .into_iter()
        // A copy of the watch value: the borrow never crosses an `.await`.
        .map(|(key, handle)| (key, handle.subscribe_metrics().borrow().clone()))
        .collect();
    let served: Vec<(SiteId, PoolKey)> = plan.iter().map(|(site, php)| (*site, php.pool)).collect();
    pool_feeds(&samples, &served)
}

/// [`pools`] without the daemon: each pool's sample (`pool` = its port owner id, mode
/// from its key) and the sites the plan routes to it (`served`: site → pool).
pub fn pool_feeds(
    samples: &[(PoolKey, PoolMetrics)],
    served: &[(SiteId, PoolKey)],
) -> Vec<PoolFeed> {
    samples
        .iter()
        .map(|(key, metrics)| PoolFeed {
            sample: PoolSample {
                pool: key.owner_id(),
                mode: match key.site {
                    None => PoolMode::Shared,
                    Some(site) => PoolMode::Dedicated(site.to_string()),
                },
                cpu_pct: metrics.cpu_pct.unwrap_or(0.0),
                mem_bytes: metrics.rss_bytes,
                workers_active: metrics.workers_active,
                from_ms: 0,
                to_ms: 0,
            },
            sites: served
                .iter()
                .filter(|(_, pool)| pool == key)
                .map(|(site, _)| *site)
                .collect(),
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
    fn a_dedicated_pool_is_attributed_exactly_to_its_site() {
        use forge_core::ids::VersionId;
        use forge_metrics::attribution::{Bucket, PoolRoute, SiteLoad, attribute};

        let version = VersionId::new();
        let (loja, blog, news) = (SiteId::new(), SiteId::new(), SiteId::new());
        let shared = PoolKey::shared(version);
        let dedicated = PoolKey::dedicated(version, loja);
        let sample = |cpu: f64, mem: u64, workers: u32| PoolMetrics {
            cpu_pct: Some(cpu),
            rss_bytes: mem,
            workers_active: workers,
            ..PoolMetrics::default()
        };
        let feeds = pool_feeds(
            &[
                (shared, sample(4.0, 200, 2)),
                (dedicated, sample(3.0, 100, 2)),
            ],
            &[(loja, dedicated), (blog, shared), (news, shared)],
        );
        assert_eq!(feeds.len(), 2);
        assert_eq!(feeds[0].sample.mode, PoolMode::Shared);
        assert_eq!(feeds[0].sample.pool, version.to_string());
        assert_eq!(feeds[0].sites, vec![blog, news]);
        assert_eq!(feeds[1].sample.mode, PoolMode::Dedicated(loja.to_string()));
        assert_eq!(feeds[1].sample.pool, format!("{version}~{loja}"));
        assert_eq!(feeds[1].sites, vec![loja]);

        // Through the attribution of the metrics service (one 10 s bucket, as its tick).
        let length_ms = 10_000;
        let mut routes = Vec::new();
        let mut pools = Vec::new();
        for feed in &feeds {
            let mut sample = feed.sample.clone();
            sample.to_ms = length_ms;
            for site in &feed.sites {
                routes.push(PoolRoute {
                    site: site.to_string(),
                    pool: sample.pool.clone(),
                    from_ms: 0,
                    to_ms: length_ms,
                });
            }
            pools.push(sample);
        }
        let load = |ut: u64| SiteLoad {
            ut_sum_us: ut,
            ..SiteLoad::default()
        };
        let bucket = Bucket {
            length_ms,
            pools,
            apps: Vec::new(),
            routes,
            loads: [
                (loja.to_string(), load(1_000)),
                (blog.to_string(), load(3_000)),
                (news.to_string(), load(1_000)),
            ]
            .into_iter()
            .collect(),
        };
        let out = attribute(&bucket);
        let exact = &out.sites[&loja.to_string()];
        assert!(!exact.approx, "a dedicated site is exact");
        assert!(
            (exact.cpu_pct - 3.0).abs() < 1e-9,
            "the pool's CPU: {exact:?}"
        );
        assert_eq!(exact.mem_bytes, 100);
        let apportioned = &out.sites[&blog.to_string()];
        assert!(apportioned.approx, "a shared site is apportioned");
        assert!(
            (apportioned.cpu_pct - 3.0).abs() < 1e-9,
            "3/4 of 4 %: {apportioned:?}"
        );
    }

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
