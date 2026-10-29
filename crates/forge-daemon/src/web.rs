//! Sites → pools → web servers (docs/01 §5.1 steps 7-8, docs/06 §2, docs/03 §2.2):
//!
//! * [`WebManager::reconcile`]: resolves the PHP version of every enabled `php` site
//!   (exact pin, series, or the newest installed series), makes sure each needed pool
//!   runs, renders the web servers' configuration and reloads the running ones, then
//!   stops pools no site uses any more.
//! * [`WebManager::apply_web`]: only the render + reload part. It is also the
//!   [`UpstreamListener`] of every pool (scaling, recycle, blue/green), so the web
//!   server always lists the warm workers.
//! * [`ConfigRenderer`] for the service manager: before an instance starts, its
//!   configuration target is rendered (web servers with their sites and pools).
//!
//! Renders are serialized by one turn (a semaphore permit with a timeout): files under
//! `etc/` are never written by two tasks at once. Nothing here starts a pool while
//! holding the turn, so a pool calling the listener during its start cannot deadlock.
//! Secure sites get their certificate from the `tls` actor before rendering; when that
//! fails the site is served over plain HTTP and the failure is logged.
//!
//! `instance.case_sensitive` comes from [`crate::instance_case`] (docs/15 §5): for a data
//! folder not initialized yet, the **real** flag (where `ForgeRoot::init` could not set it,
//! e.g. no `FILE_DELETE_CHILD` under `C:\` on managed machines, the database gets
//! `lower_case_table_names=2` and `fs.case_sensitivity_unavailable` is published, instead
//! of a server that refuses to initialize); for an initialized one, the value recorded
//! when it was initialized, whatever the flag reads now.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use forge_core::config::ForgeConfig;
use forge_core::error::{ForgeError, Result};
use forge_core::event::{EventBus, EventKind};
use forge_core::ids::{SiteId, VersionId};
use forge_core::paths::ForgeRoot;
use forge_core::platform::CaseSensitiveFs;
use forge_core::store::{CASE_SENSITIVE_KEY, Site, SiteKind, StoreReader, ToolVersionRow};
use forge_fcgi::{PoolState, UpstreamChange, UpstreamFuture, UpstreamListener};
use forge_registry::Registry;
use forge_render::{
    Engine, InstanceModel, RenderContext, UpstreamModel, UpstreamServer, VhostModel,
};
use forge_sites::{SiteService, VhostContext};
use forge_tls::{LeafRequest, TlsManager};
use futures::future::BoxFuture;
use tokio::sync::Semaphore;

use crate::catalog;
use crate::instance_case::{self, DataDirProbe};
use crate::pools::PoolManager;
use crate::render;
use crate::services::{ConfigRenderer, Entry, ServiceManager};

const TURN_TIMEOUT: Duration = Duration::from_secs(60);
const RENDER_TIMEOUT: Duration = Duration::from_secs(30);
const TLS_TIMEOUT: Duration = Duration::from_secs(30);

/// PHP resolution of one site.
#[derive(Debug, Clone)]
pub struct SitePhp {
    pub version: ToolVersionRow,
    /// `php_8_3` (series) or `php_8_3_36` (exact pin).
    pub upstream: String,
}

struct Inner {
    root: ForgeRoot,
    engine: Engine,
    bus: EventBus,
    registry: Registry,
    reader: StoreReader,
    sites: SiteService,
    pools: std::sync::OnceLock<PoolManager>,
    services: ServiceManager,
    tls: TlsManager,
    config: Arc<dyn Fn() -> Arc<ForgeConfig> + Send + Sync>,
    /// Reads the flag of the instances' data folders.
    case_fs: Arc<dyn CaseSensitiveFs>,
    turn: Arc<Semaphore>,
    /// Port of the daemon's internal endpoint (`prefs.internal_port`); 0 = not running.
    internal_port: std::sync::atomic::AtomicU16,
}

/// Rendering of sites and pools into the web servers. Cheap to clone.
#[derive(Clone)]
pub struct WebManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for WebManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebManager").finish_non_exhaustive()
    }
}

/// What a render pass did.
#[derive(Debug, Clone, Default)]
pub struct WebReport {
    pub servers: Vec<String>,
    pub written: Vec<String>,
    pub reloaded: Vec<String>,
    pub skipped_sites: Vec<String>,
    /// Why a site's vhost did not build, by host.
    pub site_errors: BTreeMap<String, String>,
    /// Why a PHP site was left out of the render before building its vhost, by host: its
    /// version is not installed or its pool is not running.
    pub unserved: BTreeMap<String, String>,
    /// Web servers whose render or reload failed: `(tool id, error)`.
    pub server_errors: Vec<(String, String)>,
}

impl WebManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        root: ForgeRoot,
        engine: Engine,
        bus: EventBus,
        registry: Registry,
        reader: StoreReader,
        sites: SiteService,
        services: ServiceManager,
        tls: TlsManager,
        config: Arc<dyn Fn() -> Arc<ForgeConfig> + Send + Sync>,
        case_fs: Arc<dyn CaseSensitiveFs>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                root,
                engine,
                bus,
                registry,
                reader,
                sites,
                pools: std::sync::OnceLock::new(),
                services,
                tls,
                config,
                case_fs,
                turn: Arc::new(Semaphore::new(1)),
                internal_port: std::sync::atomic::AtomicU16::new(0),
            }),
        }
    }

    /// The port of the internal endpoint the web servers send `/_forgebox/*` to.
    pub fn set_internal_port(&self, port: u16) {
        self.inner
            .internal_port
            .store(port, std::sync::atomic::Ordering::Relaxed);
    }

    /// Plugs the pool table (created after this manager: pools need it as listener).
    pub fn set_pools(&self, pools: PoolManager) {
        let _ = self.inner.pools.set(pools);
    }

    fn pools(&self) -> Option<&PoolManager> {
        self.inner.pools.get()
    }

    async fn turn(&self) -> Result<tokio::sync::OwnedSemaphorePermit> {
        tokio::time::timeout(TURN_TIMEOUT, Arc::clone(&self.inner.turn).acquire_owned())
            .await
            .map_err(|_| ForgeError::Timeout {
                operation: "waiting for another configuration render".into(),
                after: TURN_TIMEOUT,
            })?
            .map_err(|_| ForgeError::Internal("render turn closed".into()))
    }

    /// The PHP version and upstream of every enabled `php` site that has one.
    pub async fn php_plan(&self, sites: &[Site]) -> Result<BTreeMap<SiteId, SitePhp>> {
        let mut out = BTreeMap::new();
        let Some(tool) = catalog::pool_tools(&self.inner.registry).into_iter().next() else {
            return Ok(out);
        };
        let rows = catalog::installed(
            &self.inner.reader,
            &tool.manifest.metadata.id,
            Some(&tool.manifest),
        )
        .await?;
        if rows.is_empty() {
            return Ok(out);
        }
        // A series is served by its *active* version (the update engine's view: after a
        // rollback the older, retained build serves even though a newer one is
        // installed; docs/05 §4.1), not simply by the newest installed one.
        let history = {
            let id = tool.manifest.metadata.id.clone();
            self.inner
                .reader
                .query(move |conn| {
                    forge_core::store::UpdateHistoryRepo::new(conn).list(Some(&id), 200)
                })
                .await
                .unwrap_or_default()
        };
        let mut active: Vec<ToolVersionRow> =
            forge_provider::update::policy::active_versions(&tool.manifest, &rows, &history)
                .into_iter()
                .map(|a| a.row.clone())
                .collect();
        catalog::sort_newest_first(&tool.manifest, &mut active);
        let Some(newest) = active.first().or(rows.first()) else {
            return Ok(out);
        };
        let default_series = catalog::series_of(&tool.manifest, &newest.version);
        for site in sites
            .iter()
            .filter(|s| s.enabled && s.kind == SiteKind::Php)
        {
            let wanted = site
                .php
                .exact
                .as_deref()
                .or(site.php.series.as_deref())
                .unwrap_or(&default_series);
            let chosen = if site.php.exact.is_some() {
                catalog::pick(&tool.manifest, &rows, wanted)
            } else {
                catalog::pick(&tool.manifest, &active, wanted)
                    .or_else(|| catalog::pick(&tool.manifest, &rows, wanted))
            };
            match chosen {
                Some(row) => {
                    out.insert(
                        site.id,
                        SitePhp {
                            version: row.clone(),
                            upstream: forge_sites::vhost::php_upstream_name(
                                &site.php,
                                &default_series,
                            ),
                        },
                    );
                }
                None => {
                    tracing::warn!(site = %site.host, php = wanted, "PHP version not installed; site not served")
                }
            }
        }
        Ok(out)
    }

    /// The installed PHP version a user spec (`8.3` or `8.3.36`) designates, or
    /// [`ForgeError::VersionNotInstalled`] listing what is installed (`sites.php`).
    pub async fn require_php(&self, requested: &str) -> Result<ToolVersionRow> {
        let tool = catalog::pool_tools(&self.inner.registry)
            .into_iter()
            .next()
            .ok_or_else(|| {
                ForgeError::NotFound("no PHP tool in the loaded registries".to_owned())
            })?;
        let id = tool.manifest.metadata.id.clone();
        let rows = catalog::installed(&self.inner.reader, &id, Some(&tool.manifest)).await?;
        catalog::pick(&tool.manifest, &rows, requested)
            .cloned()
            .ok_or_else(|| ForgeError::VersionNotInstalled {
                tool: id.to_string(),
                requested: requested.to_owned(),
                installed: rows.iter().map(|r| r.version.clone()).collect(),
                fix: format!("forge tool install {id}@{requested}"),
            })
    }

    /// Version a `php` site is served by right now (`8.3.36`), `None` when nothing
    /// installed satisfies it.
    pub async fn resolved_php(&self, site: &Site) -> Option<String> {
        let plan = self.php_plan(std::slice::from_ref(site)).await.ok()?;
        plan.get(&site.id).map(|p| p.version.version.clone())
    }

    /// Steps 7-8 of the boot, and the reaction to site changes: pools for the sites,
    /// web servers rendered and reloaded, unused pools drained (blue/green of docs/06
    /// §2.2: target up, vhost re-rendered, reload, source drained).
    pub async fn reconcile(&self, reason: &str) -> Result<WebReport> {
        self.reconcile_scoped(reason, None).await
    }

    /// [`WebManager::reconcile`] for a change to `target`: the same work, but the result
    /// answers for that site only. A vhost that does not build or a web server that fails
    /// to render/reload for it is an error, and so is a pool that cannot start for its
    /// PHP version when `check_pool` (a PHP switch; a driver change does not depend on
    /// the pool). Failures of other sites and pools stay in the log (a bad neighbour must
    /// not fail someone else's job).
    pub async fn reconcile_for(
        &self,
        reason: &str,
        target: &Site,
        check_pool: bool,
    ) -> Result<WebReport> {
        self.reconcile_scoped(reason, Some((target, check_pool)))
            .await
    }

    async fn reconcile_scoped(
        &self,
        reason: &str,
        scope: Option<(&Site, bool)>,
    ) -> Result<WebReport> {
        let target = scope.map(|(site, _)| site);
        let check_pool = scope.is_some_and(|(_, check)| check);
        let sites = self.inner.sites.list().await?;
        let plan = self.php_plan(&sites).await?;
        let needed: BTreeSet<VersionId> = plan.values().map(|p| p.version.id).collect();
        let target_version = target.and_then(|t| plan.get(&t.id)).map(|p| p.version.id);
        let mut target_failure: Option<ForgeError> = None;
        if let Some(t) = target.filter(|_| check_pool) {
            if t.enabled && t.kind == SiteKind::Php && target_version.is_none() {
                target_failure = Some(ForgeError::NotFound(format!(
                    "no installed PHP serves {} (php {})",
                    t.host,
                    t.php
                        .exact
                        .as_deref()
                        .or(t.php.series.as_deref())
                        .unwrap_or("default")
                )));
            }
        }
        if let Some(pools) = self.pools() {
            let starts = needed
                .iter()
                .map(|id| async move { (*id, pools.ensure_started(*id).await) });
            for (id, result) in futures::future::join_all(starts).await {
                if let Err(err) = result {
                    tracing::warn!(version = %id, %err, "PHP pool did not start");
                    if target_version == Some(id) && target_failure.is_none() {
                        target_failure = Some(err);
                    }
                }
            }
        }
        let report = self.apply_web(reason).await?;
        if let Some(pools) = self.pools() {
            for handle in pools.handles() {
                if !needed.contains(&handle.version_id()) {
                    // The web servers no longer route to it (rendered above): drain.
                    tracing::info!(pool = %handle.upstream_name(), version = %handle.spec().version, "no site uses this pool; draining");
                    pools.retire(handle.version_id());
                }
            }
        }
        if let Some(err) = target_failure.filter(|_| check_pool) {
            return Err(err);
        }
        if let Some(t) = target {
            if let Some(why) = report
                .unserved
                .get(&t.host)
                .filter(|_| check_pool)
                .or_else(|| report.site_errors.get(&t.host))
            {
                return Err(ForgeError::ConfigInvalid(format!(
                    "{} was not rendered: {why}",
                    t.host
                )));
            }
            let served_by = t
                .server
                .clone()
                .unwrap_or_else(|| (self.inner.config)().default_server.to_string());
            if let Some((_, why)) = report
                .server_errors
                .iter()
                .find(|(tool, _)| *tool == served_by)
            {
                return Err(ForgeError::ConfigInvalid(format!(
                    "{served_by} could not apply the configuration of {}: {why}",
                    t.host
                )));
            }
        }
        Ok(report)
    }

    /// Upstreams of the running pools, plus an alias per exact pin (`php_8_3_36` →
    /// the servers of its version's pool). Two pools of one series share its name
    /// during a blue/green switch: the one serving wins, and `pinned` (the model a pool
    /// just handed to the listener) wins over both.
    async fn upstreams(
        &self,
        plan: &BTreeMap<SiteId, SitePhp>,
        pinned: Option<&forge_fcgi::UpstreamModel>,
    ) -> Vec<UpstreamModel> {
        let Some(pools) = self.pools() else {
            return Vec::new();
        };
        let mut by_version: BTreeMap<VersionId, Vec<UpstreamServer>> = BTreeMap::new();
        let mut names: BTreeMap<String, Vec<UpstreamServer>> = BTreeMap::new();
        let mut statuses = pools.statuses().await;
        statuses.sort_by_key(|s| s.serving);
        for status in statuses {
            if !matches!(status.state, PoolState::Running | PoolState::Starting) {
                continue;
            }
            let servers: Vec<UpstreamServer> = status
                .upstream
                .servers()
                .into_iter()
                .map(|(host, port)| UpstreamServer { host, port })
                .collect();
            if servers.is_empty() {
                continue;
            }
            by_version.insert(status.version_id, servers.clone());
            names.insert(status.upstream.name.clone(), servers);
        }
        if let Some(model) = pinned.filter(|m| !m.is_empty()) {
            let servers: Vec<UpstreamServer> = model
                .servers()
                .into_iter()
                .map(|(host, port)| UpstreamServer { host, port })
                .collect();
            names.insert(model.name.clone(), servers);
        }
        for php in plan.values() {
            if let Some(servers) = by_version.get(&php.version.id) {
                names
                    .entry(php.upstream.clone())
                    .or_insert_with(|| servers.clone());
            }
        }
        names
            .into_iter()
            .map(|(name, servers)| UpstreamModel::new(name, servers))
            .collect()
    }

    async fn ensure_cert(&self, site: &Site) -> bool {
        let crt = self
            .inner
            .root
            .certs_sites()
            .join(format!("{}.crt", site.host));
        let key = self
            .inner
            .root
            .certs_sites()
            .join(format!("{}.key", site.host));
        let exists = tokio::task::spawn_blocking(move || crt.is_file() && key.is_file())
            .await
            .unwrap_or(false);
        if exists {
            return true;
        }
        let request = LeafRequest {
            host: site.host.clone(),
            aliases: site.aliases.clone(),
            site_id: Some(site.id),
        };
        match tokio::time::timeout(TLS_TIMEOUT, self.inner.tls.issue(request)).await {
            Ok(Ok(_)) => true,
            Ok(Err(err)) => {
                tracing::warn!(site = %site.host, %err, "certificate not issued; serving over HTTP");
                false
            }
            Err(_) => {
                tracing::warn!(site = %site.host, "certificate issue timed out; serving over HTTP");
                false
            }
        }
    }

    /// Vhosts of the sites served by `server` (a tool id).
    async fn vhosts(
        &self,
        server: &str,
        sites: &[Site],
        plan: &BTreeMap<SiteId, SitePhp>,
        upstreams: &[UpstreamModel],
        report: &mut WebReport,
    ) -> Vec<VhostModel> {
        let config = (self.inner.config)();
        let default_series = plan
            .values()
            .next()
            .map(|p| p.upstream.trim_start_matches("php_").replace('_', "."))
            .unwrap_or_else(|| "8.4".to_owned());
        let ctx = VhostContext {
            certs_sites: self.inner.root.certs_sites(),
            default_php_series: default_series,
            extra: forge_render::ExtraModel::default(),
            app_ports: BTreeMap::new(),
            proxy_options: BTreeMap::new(),
        };
        // Document roots spelled with long names (servers refuse 8.3 names, docs/03 §3.1).
        let sites: Vec<Site> = {
            let sites = sites.to_vec();
            tokio::task::spawn_blocking(move || {
                sites
                    .into_iter()
                    .map(|mut s| {
                        if let Some(root) = s.root.as_deref().filter(|r| r.contains('~')) {
                            s.root = Some(forge_core::paths::posix_string(
                                &forge_core::platform::long_path(std::path::Path::new(root)),
                            ));
                        }
                        s
                    })
                    .collect()
            })
            .await
            .unwrap_or_default()
        };
        let mut out = Vec::new();
        for site in sites.iter().filter(|s| s.enabled) {
            let served_by = site
                .server
                .clone()
                .unwrap_or_else(|| config.default_server.to_string());
            if served_by != server {
                continue;
            }
            let mut site = site.clone();
            if site.kind == SiteKind::Php {
                let Some(php) = plan.get(&site.id) else {
                    report.skipped_sites.push(site.host.clone());
                    report
                        .unserved
                        .insert(site.host.clone(), "its PHP version is not installed".into());
                    continue;
                };
                if !upstreams.iter().any(|u| u.name == php.upstream) {
                    tracing::warn!(site = %site.host, upstream = %php.upstream, "PHP pool not running; site not rendered");
                    report.skipped_sites.push(site.host.clone());
                    report.unserved.insert(
                        site.host.clone(),
                        format!("PHP pool {} is not running", php.upstream),
                    );
                    continue;
                }
                // Render against the exact upstream the plan resolved.
                if site.php.exact.is_none() && site.php.series.is_none() {
                    site.php.series =
                        Some(php.upstream.trim_start_matches("php_").replace('_', "."));
                }
            }
            if site.secure && !self.ensure_cert(&site).await {
                site.secure = false;
                site.http_redirect = false;
            }
            match forge_sites::vhost::build(&site, &ctx) {
                Ok(model) => out.push(model),
                Err(err) => {
                    tracing::warn!(site = %site.host, %err, "site not rendered");
                    report.skipped_sites.push(site.host.clone());
                    report
                        .site_errors
                        .insert(site.host.clone(), err.to_string());
                }
            }
        }
        out
    }

    /// Whether `entry` runs a web server: its tool's render target has per-site files.
    pub fn is_web_server_entry(&self, entry: &Entry) -> bool {
        serves_sites(entry.row.tool_id.as_str())
    }

    /// Context of an instance: tool, instance model, prefs from its `http`/`https`
    /// ports and, for web servers, sites and pools.
    async fn instance_context(
        &self,
        entry: &Entry,
        report: &mut WebReport,
        pinned: Option<&forge_fcgi::UpstreamModel>,
    ) -> Result<Option<RenderContext>> {
        let (Some(manifest), Some(version)) = (&entry.manifest, &entry.version) else {
            return Ok(None);
        };
        if forge_render::targets::by_id(entry.row.tool_id.as_str()).is_none() {
            return Ok(None);
        }
        let config = (self.inner.config)();
        let mut ctx = render::base_context(&self.inner.root, &config, manifest, version);
        let data_dir = entry.row.data_dir.clone().unwrap_or_default();
        let case_sensitive = self
            .instance_case_sensitive(entry, config.case_sensitive, &data_dir)
            .await?;
        ctx.instance = InstanceModel {
            name: entry.row.name.clone(),
            ports: entry.ports.clone(),
            data_dir,
            case_sensitive,
            settings: entry.row.settings.clone(),
        };
        if let Some(port) = entry.ports.get("http") {
            ctx.prefs.http_port = *port;
        }
        if let Some(port) = entry.ports.get("https") {
            ctx.prefs.https_port = *port;
        }
        let internal = self
            .inner
            .internal_port
            .load(std::sync::atomic::Ordering::Relaxed);
        if internal != 0 {
            ctx.prefs.internal_port = internal;
        }
        if self.is_web_server_entry(entry) {
            let sites = self.inner.sites.list().await?;
            let plan = self.php_plan(&sites).await?;
            let upstreams = self.upstreams(&plan, pinned).await;
            ctx.sites = self
                .vhosts(
                    entry.row.tool_id.as_str(),
                    &sites,
                    &plan,
                    &upstreams,
                    report,
                )
                .await;
            ctx.pools = upstreams;
        }
        Ok(Some(ctx))
    }

    /// `instance.case_sensitive` of an instance ([`crate::instance_case`]): for a data
    /// folder not initialized yet, on only when `forgebox.toml` asks for it **and** the
    /// folder (or the closest existing parent, whose flag the folder created on the first
    /// start inherits) carries the flag, and the choice is recorded before `data.init`
    /// uses it; for an initialized folder, the recorded value. Publishes
    /// `fs.case_sensitivity_unavailable` when there is something to say.
    async fn instance_case_sensitive(
        &self,
        entry: &Entry,
        wanted: bool,
        data_dir: &str,
    ) -> Result<bool> {
        if data_dir.is_empty() {
            // No data folder: nothing in the configuration depends on it.
            return Ok(wanted);
        }
        let owner = entry.label();
        // The table may already hold a value recorded after `entry` was cloned.
        let recorded = self.inner.services.get(entry.row.id).map_or_else(
            || entry.row.recorded_case_sensitive(),
            |e| e.row.recorded_case_sensitive(),
        );
        let path = Path::new(data_dir);
        let probe = DataDirProbe::run(
            Arc::clone(&self.inner.case_fs),
            path,
            forge_core::fs::case::STATUS_TIMEOUT,
        )
        .await;
        let outcome = instance_case::decide(wanted, recorded, probe, path).inspect_err(|err| {
            tracing::warn!(instance = %owner, %data_dir, %err, "data folder state unknown and no recorded case sensitivity; not rendering");
        })?;
        if let Some(value) = outcome.record {
            self.inner
                .services
                .record_setting(entry.row.id, CASE_SENSITIVE_KEY, &value.to_string())
                .await?;
            tracing::info!(instance = %owner, %data_dir, case_sensitive = value, "case sensitivity of the data folder recorded");
        }
        if let Some(note) = &outcome.note {
            tracing::warn!(instance = %owner, %data_dir, case_sensitive = outcome.sensitive, "{note}");
        }
        if let Some(payload) = outcome.event {
            tracing::warn!(instance = %owner, %data_dir, case_sensitive = outcome.sensitive, reason = %payload["reason"], "data folder case sensitivity unavailable");
            self.inner
                .bus
                .publish(EventKind::FsCaseSensitivityUnavailable, payload);
        }
        Ok(outcome.sensitive)
    }

    async fn render_entry(
        &self,
        entry: &Entry,
        report: &mut WebReport,
        pinned: Option<&forge_fcgi::UpstreamModel>,
    ) -> Result<bool> {
        let Some(ctx) = self.instance_context(entry, report, pinned).await? else {
            return Ok(false);
        };
        let (root, engine, bus, owner) = (
            self.inner.root.clone(),
            self.inner.engine.clone(),
            self.inner.bus.clone(),
            entry.label(),
        );
        let applied = forge_core::runtime::spawn_blocking_timeout(
            "rendering configuration",
            RENDER_TIMEOUT,
            move || render::apply_target(&root, &engine, &bus, &ctx, &owner),
        )
        .await?;
        report.written.extend(applied.written.iter().cloned());
        Ok(applied.changed())
    }

    /// Renders every web server's configuration and reloads the running ones whose
    /// files changed.
    pub async fn apply_web(&self, reason: &str) -> Result<WebReport> {
        self.apply_web_with(reason, None).await
    }

    /// [`WebManager::apply_web`] where `pinned` (the model a pool hands to its listener)
    /// decides the servers of its upstream name.
    pub async fn apply_web_with(
        &self,
        reason: &str,
        pinned: Option<&forge_fcgi::UpstreamModel>,
    ) -> Result<WebReport> {
        let _turn = self.turn().await?;
        let mut report = WebReport::default();
        for entry in self.inner.services.entries() {
            if entry.handle.is_none() || !self.is_web_server_entry(&entry) {
                continue;
            }
            report.servers.push(entry.label());
            match self.render_entry(&entry, &mut report, pinned).await {
                Ok(true) => match self.inner.services.apply_reload(entry.row.id).await {
                    Ok(true) => report.reloaded.push(entry.label()),
                    Ok(false) => {}
                    Err(err) => {
                        tracing::warn!(server = %entry.label(), %err, "reload failed");
                        report
                            .server_errors
                            .push((entry.row.tool_id.to_string(), err.to_string()));
                    }
                },
                Ok(false) => {}
                Err(err) => {
                    tracing::warn!(server = %entry.label(), %err, "render failed");
                    report
                        .server_errors
                        .push((entry.row.tool_id.to_string(), err.to_string()));
                }
            }
        }
        tracing::debug!(reason, servers = ?report.servers, written = report.written.len(), reloaded = ?report.reloaded, "web configuration applied");
        Ok(report)
    }
}

impl ConfigRenderer for WebManager {
    fn render_instance<'a>(&'a self, entry: &'a Entry) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let _turn = self.turn().await?;
            let mut report = WebReport::default();
            self.render_entry(entry, &mut report, None)
                .await
                .map(|_| ())
        })
    }
}

impl UpstreamListener for WebManager {
    fn upstream_changed(&self, change: UpstreamChange) -> UpstreamFuture {
        let this = self.clone();
        Box::pin(async move {
            let (reason, pinned) = match &change {
                UpstreamChange::Scaled { model, reason } => {
                    (format!("pool {} {}", model.name, reason.as_str()), model)
                }
                // The target takes over the name of the source (both are `php_8_3` in a
                // patch update): render it before the update engine records the new
                // active version.
                UpstreamChange::Swapped { from, to } => {
                    (format!("swap {} -> {}", from.name, to.name), to)
                }
            };
            this.apply_web_with(&reason, Some(pinned)).await.map(|_| ())
        })
    }
}

/// Whether the render target of `tool` writes per-site files (a web server).
pub fn serves_sites(tool: &str) -> bool {
    forge_render::targets::by_id(tool).is_some_and(|target| {
        target
            .files
            .iter()
            .any(|f| matches!(f.scope, forge_render::targets::Scope::Site))
    })
}

/// Listener placeholder plugged into the pools before the web manager exists.
#[derive(Clone, Default)]
pub struct LateListener(Arc<std::sync::OnceLock<WebManager>>);

impl LateListener {
    pub fn set(&self, web: WebManager) {
        let _ = self.0.set(web);
    }
}

impl UpstreamListener for LateListener {
    fn upstream_changed(&self, change: UpstreamChange) -> UpstreamFuture {
        match self.0.get() {
            Some(web) => web.upstream_changed(change),
            None => Box::pin(async { Ok(()) }),
        }
    }
}
