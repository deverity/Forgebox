//! PHP FastCGI pools of the daemon (docs/03 §2, docs/01 §5.1 step 7): one
//! [`forge_fcgi::PhpPool`] per installed version that serves sites, built from the
//! manifest's `service.pool` (no per-tool code: exe, args with `{{ port }}`, env,
//! scaling, recycle, port range, runtime deps).
//!
//! The table is keyed by [`PoolKey`] `{version, site}` (docs/06 §2.1, ADR-0044 §7): the
//! shared pool of a version (`php_8_4`, port owners `pool/<version id>#worker-<n>`) and
//! the dedicated pool of a site on it (`php_8_4__loja-test`, owners
//! `pool/<version id>~<site id>#worker-<n>`). Spawning and stopping take a turn (one
//! permit, with a timeout) so two callers never build the same pool twice. Rendering a
//! version's configuration takes another turn, so the quick settings of its series
//! (`tool_series_settings`, ADR-0042) are read and written by one render at a time. The
//! table also answers the update engine ([`forge_provider::update::PoolSwitcher`]:
//! blue/green of every pool of a version, [`PoolManager::switch_version`]) and the swap
//! ([`forge_fcgi::PoolDirectory`], shared pools).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forge_core::config::ForgeConfig;
use forge_core::error::{ForgeError, Result};
use forge_core::event::EventBus;
use forge_core::ids::{SiteId, ToolId, VersionId};
use forge_core::model::port::PortReservations;
use forge_core::paths::ForgeRoot;
use forge_core::platform::RuntimeDep;
use forge_core::store::{StoreReader, ToolSeriesSettingsRepo, ToolVersionRow};
use forge_core::sync::{LockLevel, OrderedMutex};
use forge_fcgi::{
    PhpPool, PoolDeps, PoolDirectory, PoolHandle, PoolSpec, PoolState, PoolStatus, Recycle,
    Scaling, SwapOptions, Switcher, UpstreamListener,
};
use forge_manifest::schema::tool::{ServiceKind, ToolManifest};
use forge_provider::layout::Layout;
use forge_provider::update::{PoolSwitch, PoolSwitcher};
use forge_registry::Registry;
use forge_render::Engine;
use forge_supervisor::Supervisor;
use futures::future::BoxFuture;
use tokio::sync::{Semaphore, watch};

use crate::catalog;
use crate::php_ext;
use crate::render;
use crate::settings;
use crate::vars::{PoolVars, RuntimeVars};

/// Budget of a pool start (`min` workers warm).
pub const POOL_START_TIMEOUT: Duration = Duration::from_secs(60);
/// Budget of a pool stop.
pub const POOL_STOP_TIMEOUT: Duration = Duration::from_secs(40);
/// Budget of a drain (the pool's own `drain_timeout`, 30 s, plus the workers' stop).
pub const POOL_DRAIN_TIMEOUT: Duration = Duration::from_secs(90);
/// Quick asks (status).
pub const POOL_QUICK: Duration = Duration::from_secs(1);
const TURN_TIMEOUT: Duration = Duration::from_secs(90);

/// Identity of a pool in the table (docs/06 §2.1, ADR-0044 §7): the installed version it
/// runs and, for a site's **dedicated** pool (`pool_mode: dedicated`), the site. The
/// shared pool of a version has no site. ADR-0029 keyed the table by version only; a
/// dedicated pool is a second pool of the same version with its own upstream
/// (`php_8_4__loja-test`) and its own port owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PoolKey {
    pub version: VersionId,
    pub site: Option<SiteId>,
}

impl PoolKey {
    /// The pool of `version` shared by every site on it.
    pub fn shared(version: VersionId) -> Self {
        Self {
            version,
            site: None,
        }
    }

    /// The pool of `version` that serves `site` alone.
    pub fn dedicated(version: VersionId, site: SiteId) -> Self {
        Self {
            version,
            site: Some(site),
        }
    }

    pub fn is_dedicated(&self) -> bool {
        self.site.is_some()
    }

    /// Owner id of the pool's port reservations (`pool/<owner id>#worker-<n>`): the
    /// version id for the shared pool (as before ADR-0044), `<version id>~<site id>` for a
    /// dedicated one, so starting or stopping one never releases the other's ports.
    pub fn owner_id(&self) -> String {
        match self.site {
            None => self.version.to_string(),
            Some(site) => format!("{}~{site}", self.version),
        }
    }
}

impl std::fmt::Display for PoolKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.owner_id())
    }
}

/// Budget for a pool of the same key, retired a moment ago, to finish draining before a
/// new one takes its port owner (a site switched back and forth).
const RETIRED_WAIT: Duration = Duration::from_secs(35);

#[derive(Default)]
struct Table {
    pools: BTreeMap<PoolKey, PoolHandle>,
    /// Pools taken out of the table that are still draining; `true` once stopped.
    retiring: BTreeMap<PoolKey, watch::Receiver<bool>>,
    /// Pools a switch is bringing up before the site row that needs them is written
    /// ([`PoolManager::hold`]): a reconcile running meanwhile must not retire them.
    held: BTreeMap<PoolKey, usize>,
}

/// Keeps the pool of a key out of [`PoolManager::retire`] while it lives (a blue/green
/// switch between "target pool warm" and "row written, servers re-rendered").
#[must_use = "the pool is only protected while the hold lives"]
pub struct PoolHold {
    inner: Arc<Inner>,
    key: PoolKey,
}

impl std::fmt::Debug for PoolHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoolHold").field("key", &self.key).finish()
    }
}

impl Drop for PoolHold {
    fn drop(&mut self) {
        let mut table = self.inner.table.lock();
        if let Some(count) = table.held.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                table.held.remove(&self.key);
            }
        }
    }
}

struct Inner {
    root: ForgeRoot,
    supervisor: Supervisor,
    ports: Arc<dyn PortReservations>,
    listener: Arc<dyn UpstreamListener>,
    registry: Registry,
    reader: StoreReader,
    layout: Layout,
    engine: Engine,
    bus: EventBus,
    config: Arc<dyn Fn() -> Arc<ForgeConfig> + Send + Sync>,
    prefs: Arc<dyn Fn() -> BTreeMap<String, String> + Send + Sync>,
    /// Lock `daemon.tables` (leaf).
    table: OrderedMutex<Table>,
    turn: Arc<Semaphore>,
    /// One configuration render at a time (settings read and files written together).
    render_turn: Arc<Semaphore>,
}

/// The pools. Cheap to clone.
#[derive(Clone)]
pub struct PoolManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for PoolManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoolManager")
            .field("pools", &self.inner.table.lock().pools.len())
            .finish()
    }
}

impl PoolManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        root: ForgeRoot,
        supervisor: Supervisor,
        ports: Arc<dyn PortReservations>,
        listener: Arc<dyn UpstreamListener>,
        registry: Registry,
        reader: StoreReader,
        layout: Layout,
        engine: Engine,
        bus: EventBus,
        config: Arc<dyn Fn() -> Arc<ForgeConfig> + Send + Sync>,
        prefs: Arc<dyn Fn() -> BTreeMap<String, String> + Send + Sync>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                root,
                supervisor,
                ports,
                listener,
                registry,
                reader,
                layout,
                engine,
                bus,
                config,
                prefs,
                table: OrderedMutex::new(LockLevel::DAEMON_TABLES, Table::default()),
                turn: Arc::new(Semaphore::new(1)),
                render_turn: Arc::new(Semaphore::new(1)),
            }),
        }
    }

    async fn turn(&self) -> Result<tokio::sync::OwnedSemaphorePermit> {
        tokio::time::timeout(TURN_TIMEOUT, Arc::clone(&self.inner.turn).acquire_owned())
            .await
            .map_err(|_| ForgeError::Timeout {
                operation: "waiting for another pool operation".into(),
                after: TURN_TIMEOUT,
            })?
            .map_err(|_| ForgeError::Internal("pool turn closed".into()))
    }

    /// Every pool handle (shared and dedicated).
    pub fn handles(&self) -> Vec<PoolHandle> {
        self.inner.table.lock().pools.values().cloned().collect()
    }

    /// Every pool with its key, ordered by key (a version's shared pool before its
    /// dedicated ones).
    pub fn entries(&self) -> Vec<(PoolKey, PoolHandle)> {
        self.inner
            .table
            .lock()
            .pools
            .iter()
            .map(|(k, h)| (*k, h.clone()))
            .collect()
    }

    /// The pools of `version`: its shared pool first, then the dedicated ones.
    pub fn of_version(&self, version: VersionId) -> Vec<(PoolKey, PoolHandle)> {
        self.inner
            .table
            .lock()
            .pools
            .iter()
            .filter(|(k, _)| k.version == version)
            .map(|(k, h)| (*k, h.clone()))
            .collect()
    }

    /// The shared pool of `version`.
    pub fn get(&self, version: VersionId) -> Option<PoolHandle> {
        self.get_key(&PoolKey::shared(version))
    }

    pub fn get_key(&self, key: &PoolKey) -> Option<PoolHandle> {
        self.inner.table.lock().pools.get(key).cloned()
    }

    /// Statuses of every pool (quick asks in parallel).
    pub async fn statuses(&self) -> Vec<PoolStatus> {
        self.keyed_statuses()
            .await
            .into_iter()
            .map(|(_, s)| s)
            .collect()
    }

    /// Statuses of every pool with its key (quick asks in parallel).
    pub async fn keyed_statuses(&self) -> Vec<(PoolKey, PoolStatus)> {
        let entries = self.entries();
        futures::future::join_all(
            entries
                .iter()
                .map(|(key, h)| async move { (*key, h.status(POOL_QUICK).await) }),
        )
        .await
        .into_iter()
        .filter_map(|(key, status)| status.ok().map(|s| (key, s)))
        .collect()
    }

    /// Pools of `versions` that run or are starting (shared and dedicated), the serving
    /// ones first.
    pub async fn running_of(&self, versions: &[VersionId]) -> Vec<PoolHandle> {
        let mut out: Vec<(bool, PoolHandle)> = Vec::new();
        for handle in self.handles() {
            if !versions.contains(&handle.version_id()) {
                continue;
            }
            if let Ok(status) = handle.status(POOL_QUICK).await {
                if matches!(status.state, PoolState::Running | PoolState::Starting) {
                    out.push((status.serving, handle));
                }
            }
        }
        out.sort_by_key(|(serving, _)| !*serving);
        out.into_iter().map(|(_, h)| h).collect()
    }

    /// [`render_version`] over the pools of `versions` (the versions of one series).
    pub async fn series_render_version(
        &self,
        versions: &[VersionId],
        fallback: VersionId,
    ) -> VersionId {
        render_version(&self.running_of(versions).await, fallback)
    }

    /// The spec of `row` (an installed version of a `service.kind: pool` tool).
    pub fn spec(&self, manifest: &ToolManifest, row: &ToolVersionRow) -> Result<PoolSpec> {
        pool_spec(
            &self.inner.root,
            manifest,
            row,
            &self.inner.layout,
            (self.inner.prefs)(),
        )
    }

    /// The shared pool of `version`, spawned (stopped) if it does not exist yet.
    pub async fn handle_for(&self, version: VersionId) -> Result<PoolHandle> {
        self.handle_for_key(PoolKey::shared(version), None).await
    }

    /// The pool of `key`, spawned (stopped) if it does not exist yet. `upstream` names
    /// the upstream of a dedicated pool (`php_8_4__loja-test`, required for one); `None`
    /// keeps the series name of the spec (`php_8_4`). A pool of the key whose upstream
    /// differs (the site's host, hence its slug, changed) is stopped and replaced.
    pub async fn handle_for_key(&self, key: PoolKey, upstream: Option<&str>) -> Result<PoolHandle> {
        let matches = |h: &PoolHandle| upstream.is_none_or(|u| h.upstream_name() == u);
        if let Some(handle) = self.get_key(&key).filter(matches) {
            return Ok(handle);
        }
        if key.is_dedicated() && upstream.is_none() {
            return Err(ForgeError::InvalidInput(format!(
                "the dedicated pool {key} needs its upstream name"
            )));
        }
        self.wait_retired(&key).await;
        let _turn = self.turn().await?;
        let stale = {
            let mut table = self.inner.table.lock();
            match table.pools.get(&key) {
                Some(handle) if matches(handle) => return Ok(handle.clone()),
                Some(_) => table.pools.remove(&key),
                None => None,
            }
        };
        if let Some(stale) = stale {
            tracing::info!(pool = %stale.upstream_name(), wanted = ?upstream, "pool renamed; replacing it");
            if let Err(err) = stale.stop(POOL_STOP_TIMEOUT).await {
                tracing::warn!(pool = %stale.upstream_name(), %err, "stopping a renamed pool");
            }
            stale.shutdown().await;
        }
        let row = catalog::version(&self.inner.reader, key.version).await?;
        let tool = catalog::require_tool(&self.inner.registry, &row.tool_id)?;
        if tool.manifest.spec.service.kind != ServiceKind::Pool {
            return Err(ForgeError::InvalidInput(format!(
                "{} does not run as a FastCGI pool",
                row.tool_id
            )));
        }
        let mut spec = self.spec(&tool.manifest, &row)?;
        if let (true, Some(name)) = (key.is_dedicated(), upstream) {
            spec.key = key.owner_id();
            spec.upstream = name.to_owned();
        }
        let handle = PhpPool::spawn(
            spec,
            PoolDeps {
                supervisor: self.inner.supervisor.clone(),
                ports: Arc::clone(&self.inner.ports),
                listener: Some(Arc::clone(&self.inner.listener)),
            },
        )?;
        self.inner.table.lock().pools.insert(key, handle.clone());
        Ok(handle)
    }

    /// A pool of `key` retired a moment ago keeps its port owner until it stopped: a new
    /// one waits for it (bounded), otherwise its start would release the draining
    /// workers' reservations.
    async fn wait_retired(&self, key: &PoolKey) {
        let retiring = self.inner.table.lock().retiring.get(key).cloned();
        let Some(mut done) = retiring else {
            return;
        };
        if tokio::time::timeout(RETIRED_WAIT, done.wait_for(|stopped| *stopped))
            .await
            .is_err()
        {
            tracing::warn!(pool = %key, "a retired pool of the same key is still draining; starting anyway");
        }
    }

    /// Renders the version's configuration target (php `conf.d` overlay) and starts its
    /// shared pool serving; answers once `min` workers are warm.
    pub async fn ensure_started(&self, version: VersionId) -> Result<PoolHandle> {
        self.ensure_started_key(PoolKey::shared(version), None)
            .await
    }

    /// [`PoolManager::ensure_started`] for the pool of `key` (`upstream`: see
    /// [`PoolManager::handle_for_key`]). A port range with no room left for its workers
    /// fails with `conflict` ("no free port left in range ...").
    pub async fn ensure_started_key(
        &self,
        key: PoolKey,
        upstream: Option<&str>,
    ) -> Result<PoolHandle> {
        let handle = self.handle_for_key(key, upstream).await?;
        let status = handle.status(POOL_QUICK).await?;
        if matches!(status.state, PoolState::Running) && status.serving {
            return Ok(handle);
        }
        self.render_config(key.version).await?;
        handle.start(POOL_START_TIMEOUT).await?;
        Ok(handle)
    }

    /// Renders the configuration of `version` (its `conf.d` overlay of the render target
    /// and the quick settings of its series, `conf.d/90-forgebox-settings.ini` for PHP)
    /// and publishes `config.rendered` for what changed. Does not touch the pool.
    ///
    /// The extension choices of the series (`ext_<name>`, M2-E2-T9) shape the bundled
    /// extension lines, and the debugger extension installed into `version` (Xdebug)
    /// gets its model (toggle line, its own series settings), so `20-xdebug.ini` follows
    /// what is stored. Renders run one at a time (the render queue): an extension toggle
    /// never interleaves with the render of a blue/green swap.
    pub async fn render_config(&self, version: VersionId) -> Result<render::Applied> {
        let _render = tokio::time::timeout(
            TURN_TIMEOUT,
            Arc::clone(&self.inner.render_turn).acquire_owned(),
        )
        .await
        .map_err(|_| ForgeError::Timeout {
            operation: "waiting for another pool configuration render".into(),
            after: TURN_TIMEOUT,
        })?
        .map_err(|_| ForgeError::Internal("pool render turn closed".into()))?;
        let row = catalog::version(&self.inner.reader, version).await?;
        let tool = catalog::require_tool(&self.inner.registry, &row.tool_id)?;
        let config = (self.inner.config)();
        let mut ctx = render::base_context(&self.inner.root, &config, &tool.manifest, &row);
        let series = catalog::series_of(&tool.manifest, &row.version);
        let stored = {
            let (tool_id, series) = (row.tool_id.clone(), series.clone());
            self.inner
                .reader
                .query(move |conn| ToolSeriesSettingsRepo::new(conn).get(&tool_id, &series))
                .await?
        };
        if ctx.php.is_some() {
            let linked = php_ext::linked(
                &self.inner.reader,
                &self.inner.registry,
                &tool.manifest,
                row.id,
            )
            .await?;
            let choices = php_ext::choices(&stored);
            let debugger = match linked
                .iter()
                .find(|l| php_ext::is_debugger(&l.entry.manifest))
            {
                Some(l) => {
                    let ext_stored =
                        php_ext::stored(&self.inner.reader, &l.entry.manifest.metadata.id, &series)
                            .await?;
                    let enabled = choices.get(&l.name) == Some(&true);
                    php_ext::debugger_model(&l.entry.manifest, enabled, &ext_stored)
                }
                None => None,
            };
            let registry: std::collections::BTreeSet<String> =
                linked.into_iter().map(|l| l.name).collect();
            if let Some(php) = ctx.php.as_mut() {
                php.settings = settings::context(&tool.manifest, &stored);
                php_ext::apply_choices(&choices, &registry, php);
                php.xdebug = debugger;
            }
        }
        let mut vars = RuntimeVars::new(self.inner.layout.vars_for(&tool.manifest, &row));
        vars.prefs = (self.inner.prefs)();
        // A broken plan never blocks a pool: it renders without the settings (the API
        // validated them before storing, so this is a manifest changed since).
        let plans = settings::plans(&tool.manifest, &stored, &vars).unwrap_or_else(|err| {
            tracing::warn!(tool = %row.tool_id, %series, %err, "quick settings not written");
            Vec::new()
        });
        let owner = format!("{}@{series}", row.tool_id);
        let (root, engine, bus) = (
            self.inner.root.clone(),
            self.inner.engine.clone(),
            self.inner.bus.clone(),
        );
        let manifest = Arc::clone(&tool.manifest);
        forge_core::runtime::spawn_blocking_timeout(
            "rendering the pool configuration",
            Duration::from_secs(30),
            move || {
                render::keep_present_extensions(&manifest, Path::new(&row.path), &mut ctx);
                render::apply_target(&root, &engine, &bus, &ctx, &owner, &plans)
            },
        )
        .await
    }

    /// Stops and forgets every pool of `version`, shared and dedicated (ports released by
    /// the pools).
    pub async fn stop(&self, version: VersionId) -> Result<()> {
        let keys: Vec<PoolKey> = self
            .of_version(version)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        let mut first = Ok(());
        for key in keys {
            if let Err(err) = self.stop_key(&key).await {
                if first.is_ok() {
                    first = Err(err);
                }
            }
        }
        first
    }

    /// Stops and forgets the pool of `key`.
    pub async fn stop_key(&self, key: &PoolKey) -> Result<()> {
        let _turn = self.turn().await?;
        let handle = self.inner.table.lock().pools.remove(key);
        if let Some(handle) = handle {
            let result = handle.stop(POOL_STOP_TIMEOUT).await;
            handle.shutdown().await;
            result?;
        }
        Ok(())
    }

    /// Protects the pool of `key` from [`PoolManager::retire`] until the returned hold is
    /// dropped: a switch takes it before starting its target pool, so a reconcile that
    /// runs before the site row is written (and so does not see the pool as needed yet)
    /// leaves it alone.
    pub fn hold(&self, key: PoolKey) -> PoolHold {
        *self.inner.table.lock().held.entry(key).or_default() += 1;
        PoolHold {
            inner: Arc::clone(&self.inner),
            key,
        }
    }

    /// Takes the pool of `key` out of service without cutting requests (docs/06 §2.2):
    /// it leaves the table at once and, in the background, drains (each worker stops once
    /// its connections end, at most the pool's `drain_timeout`) and stops. A new pool of
    /// the same key waits for that stop before it starts.
    pub fn retire(&self, key: PoolKey) {
        let (done_tx, done_rx) = watch::channel(false);
        let handle = {
            let mut table = self.inner.table.lock();
            if table.held.contains_key(&key) {
                tracing::debug!(pool = %key, "pool held by a switch in progress; not retired");
                return;
            }
            let Some(handle) = table.pools.remove(&key) else {
                return;
            };
            table.retiring.insert(key, done_rx.clone());
            handle
        };
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            match handle.drain(POOL_DRAIN_TIMEOUT).await {
                Ok(report) => {
                    tracing::info!(pool = %handle.upstream_name(), version = %handle.spec().version, workers = report.workers, forced = report.forced, "pool drained")
                }
                Err(err) => {
                    tracing::warn!(pool = %handle.upstream_name(), %err, "pool drain failed; stopping");
                    let _ = handle.stop(POOL_STOP_TIMEOUT).await;
                }
            }
            handle.shutdown().await;
            let _ = done_tx.send(true);
            let mut table = inner.table.lock();
            if table
                .retiring
                .get(&key)
                .is_some_and(|rx| rx.same_channel(&done_rx))
            {
                table.retiring.remove(&key);
            }
        });
    }

    /// Stops every pool in parallel within `timeout`, then their actors.
    pub async fn stop_all(&self, timeout: Duration) {
        let handles: Vec<PoolHandle> = {
            let mut table = self.inner.table.lock();
            std::mem::take(&mut table.pools).into_values().collect()
        };
        let stops = handles.iter().map(|handle| async move {
            if let Err(err) = handle.stop(POOL_STOP_TIMEOUT.min(timeout)).await {
                tracing::warn!(pool = %handle.upstream_name(), %err, "pool stop failed");
            }
            handle.shutdown().await;
        });
        if tokio::time::timeout(timeout, futures::future::join_all(stops))
            .await
            .is_err()
        {
            tracing::warn!(?timeout, "pools did not stop within the shutdown budget");
        }
    }

    /// Blue/green switcher over the shared pools and the web listener.
    pub fn switcher(&self) -> Switcher {
        Switcher::new(
            Arc::new(self.clone()),
            Arc::clone(&self.inner.listener),
            SwapOptions::default(),
        )
    }

    /// Blue/green of every pool of `from` to `to` (the update engine, docs/05 §4): the
    /// shared pool first, then each dedicated one, each to the pool of `to` with the same
    /// site and upstream name (`php_8_4__loja-test` stays the name across a patch). Pools
    /// already switched stay switched when a later one fails; the first error is
    /// returned (the engine rolls back by switching `to` → `from`).
    pub async fn switch_version(&self, from: VersionId, to: VersionId) -> Result<usize> {
        let sources = self.of_version(from);
        let options = SwapOptions::default();
        let mut switched = 0;
        for (key, source) in sources {
            let target_key = PoolKey {
                version: to,
                site: key.site,
            };
            let upstream = key.site.map(|_| source.upstream_name().to_owned());
            let target = self.handle_for_key(target_key, upstream.as_deref()).await?;
            let report =
                forge_fcgi::swap::swap(&source, &target, self.inner.listener.as_ref(), &options)
                    .await?;
            tracing::info!(pool = %target.upstream_name(), from = %source.spec().version, to = %target.spec().version, elapsed_ms = report.elapsed_ms, "pool switched");
            switched += 1;
        }
        Ok(switched)
    }
}

impl PoolDirectory for PoolManager {
    fn pool(&self, version: VersionId) -> forge_fcgi::swap::PoolLookup<'_> {
        Box::pin(self.handle_for(version))
    }
}

/// The update engine's view of the pools: blue/green of every pool of the source version
/// (shared and dedicated, [`PoolManager::switch_version`]), nothing to do when none runs.
#[derive(Clone)]
pub struct DaemonPoolSwitcher(pub PoolManager);

impl PoolSwitcher for DaemonPoolSwitcher {
    fn switch<'a>(&'a self, switch: &'a PoolSwitch) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.0
                .switch_version(switch.from, switch.to)
                .await
                .map(|_| ())
        })
    }

    fn serving<'a>(&'a self, _tool: &'a ToolId, version: VersionId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            for (_, handle) in self.0.of_version(version) {
                if handle
                    .status(POOL_QUICK)
                    .await
                    .is_ok_and(|s| s.serving && matches!(s.state, PoolState::Running))
                {
                    return Ok(true);
                }
            }
            Ok(false)
        })
    }
}

/// The version whose folder a render of the files shared by a series must use: the
/// series' `conf.d/10-forgebox.ini` holds `extension_dir = "<version folder>/ext"`, so it
/// is rendered with the version whose pool runs and serves (`running`, serving first, as
/// [`PoolManager::running_of`] answers), and with `fallback` only when no pool of the
/// series runs. Rendering with another patch would point the workers that start later
/// (autoscale, crash restart) at that patch's `ext/` folder.
pub fn render_version(running: &[PoolHandle], fallback: VersionId) -> VersionId {
    running.first().map_or(fallback, PoolHandle::version_id)
}

/// `service.pool` of `manifest` resolved for the installed `row` (pure).
pub fn pool_spec(
    root: &ForgeRoot,
    manifest: &ToolManifest,
    row: &ToolVersionRow,
    layout: &Layout,
    prefs: BTreeMap<String, String>,
) -> Result<PoolSpec> {
    let pool = manifest
        .spec
        .service
        .pool
        .as_ref()
        .filter(|_| manifest.spec.service.kind == ServiceKind::Pool)
        .ok_or_else(|| {
            ForgeError::ConfigInvalid(format!("{} declares no service.pool", row.tool_id))
        })?;
    let series = catalog::series_of(manifest, &row.version);
    let version_dir = PathBuf::from(&row.path);
    let ini_dir = root.etc().join(manifest.metadata.id.as_str()).join(&series);
    let mut spec = PoolSpec::new(
        row.id,
        row.version.clone(),
        series.clone(),
        version_dir.join(pool.exe.replace('/', "\\")),
        ini_dir,
    )?;
    spec.tool = manifest.metadata.id.clone();

    let mut vars = RuntimeVars::new(layout.vars_for(manifest, row));
    vars.prefs = prefs;
    vars.pool = Some(PoolVars {
        recycle: BTreeMap::from([
            (
                "max_requests".to_owned(),
                pool.recycle.max_requests.to_string(),
            ),
            ("max_rss_mb".to_owned(), pool.recycle.max_rss_mb.to_string()),
            (
                "max_age".to_owned(),
                pool.recycle.max_age.as_std().as_secs().to_string(),
            ),
        ]),
        scaling: BTreeMap::from([
            ("min".to_owned(), pool.scaling.min.to_string()),
            ("max".to_owned(), pool.scaling.max.to_string()),
            (
                "idle_timeout".to_owned(),
                pool.scaling.idle_timeout.as_std().as_secs().to_string(),
            ),
        ]),
    });
    if !pool.args.is_empty() {
        spec.args = vars.substitute_all(&pool.args)?;
    }
    for (name, value) in manifest.spec.config.env.iter().chain(&pool.env) {
        if name.eq_ignore_ascii_case("PATH") {
            continue;
        }
        spec.env.set(name.clone(), vars.substitute(value)?);
    }
    spec.path = {
        let mut out = vec![version_dir.clone()];
        for entry in &manifest.spec.install.path {
            let dir = if entry == "." {
                version_dir.clone()
            } else {
                version_dir.join(entry.replace('/', "\\"))
            };
            if !out.contains(&dir) {
                out.push(dir);
            }
        }
        out
    };
    spec.cwd = version_dir;
    spec.port_range = pool.port_range.clone();
    spec.scaling = Scaling {
        min: pool.scaling.min.max(1),
        max: pool.scaling.max.max(pool.scaling.min.max(1)),
        idle_timeout: pool.scaling.idle_timeout.as_std(),
    };
    spec.recycle = Recycle {
        max_requests: u64::from(pool.recycle.max_requests),
        max_rss_mb: u64::from(pool.recycle.max_rss_mb),
        max_age: (!pool.recycle.max_age.is_zero()).then(|| pool.recycle.max_age.as_std()),
    };
    spec.runtime_deps = manifest
        .spec
        .runtime_deps
        .iter()
        .filter_map(|d| d.parse::<RuntimeDep>().ok())
        .collect();
    // What the worker reports is `PHP_VERSION`: no semver build metadata (`8.4.27+1` is
    // the php-windows provider's second build of 8.4.27, which reports `8.4.27`).
    spec.expect_php_version = Some(
        row.version
            .split_once('+')
            .map_or(row.version.as_str(), |(v, _)| v)
            .to_owned(),
    );
    spec.log_dir = Some(
        root.logs()
            .join(manifest.metadata.id.as_str())
            .join(&series),
    );
    spec.validate()?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::store::{VersionSource, VersionState};

    #[test]
    fn php_pool_spec_from_the_manifest() {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/tools/php.yaml"),
        )
        .unwrap();
        let manifest = match forge_manifest::load_str(&text).unwrap() {
            forge_manifest::Manifest::Tool(t) => t,
            _ => unreachable!(),
        };
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let row = ToolVersionRow {
            id: VersionId::new(),
            tool_id: ToolId::new("php").unwrap(),
            version: "8.4.27".into(),
            canonical_name: "8.4.27-nts-vs17-x64".into(),
            variant: BTreeMap::from([("ts".into(), "nts".into())]),
            path: forge_core::paths::posix_string(
                &root.bin().join("php").join("8.4.27-nts-vs17-x64"),
            ),
            state: VersionState::Installed,
            source: VersionSource::Registry,
        };
        let store = forge_core::store::Store::open_in_memory().unwrap();
        let layout = Layout::new(
            root.clone(),
            forge_core::platform::current(),
            store.writer().clone(),
            store.reader().clone(),
            forge_core::jobs::JobRegistry::new(EventBus::new()),
            Arc::new(forge_provider::layout::NoTemplates),
        );
        let spec = pool_spec(&root, &manifest, &row, &layout, BTreeMap::new()).unwrap();
        assert_eq!(spec.upstream, "php_8_4");
        assert_eq!(spec.series, "8.4");
        assert!(spec.exe.ends_with("php-cgi.exe"));
        assert_eq!(spec.args[0], "-b");
        assert_eq!(spec.args[1], "127.0.0.1:{{ port }}");
        assert!(
            spec.args[3].ends_with("/etc/php/8.4/php.ini"),
            "{:?}",
            spec.args
        );
        assert_eq!(spec.scaling.min, 2);
        assert_eq!(spec.recycle.max_requests, 500);
        assert!(spec.env.get("PHPRC").unwrap().ends_with("/etc/php/8.4"));
        assert_eq!(spec.ini_dir, root.etc().join("php").join("8.4"));
        assert_eq!(spec.expect_php_version.as_deref(), Some("8.4.27"));
        assert_eq!(spec.runtime_deps.len(), 1);

        // A rebuilt release (`php-8.4.27-1-nts...zip` = 8.4.27+1) still reports 8.4.27.
        let rebuilt = ToolVersionRow {
            version: "8.4.27+1".into(),
            ..row.clone()
        };
        let spec = pool_spec(&root, &manifest, &rebuilt, &layout, BTreeMap::new()).unwrap();
        assert_eq!(spec.expect_php_version.as_deref(), Some("8.4.27"));
    }
}
