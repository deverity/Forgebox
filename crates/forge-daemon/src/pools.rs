//! PHP FastCGI pools of the daemon (docs/03 §2, docs/01 §5.1 step 7): one
//! [`forge_fcgi::PhpPool`] per installed version that serves sites, built from the
//! manifest's `service.pool` (no per-tool code: exe, args with `{{ port }}`, env,
//! scaling, recycle, port range, runtime deps).
//!
//! The table is keyed by version id (port owners are `pool/<version id>#worker-<n>`);
//! spawning and stopping take a turn (one permit, with a timeout) so two callers never
//! build the same pool twice. Rendering a version's configuration takes another turn, so
//! the quick settings of its series (`tool_series_settings`, ADR-0042) are read and
//! written by one render at a time. The table also answers the update engine
//! ([`forge_provider::update::PoolSwitcher`], blue/green through
//! [`forge_fcgi::Switcher`]) and the swap ([`forge_fcgi::PoolDirectory`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forge_core::config::ForgeConfig;
use forge_core::error::{ForgeError, Result};
use forge_core::event::EventBus;
use forge_core::ids::{ToolId, VersionId};
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
use tokio::sync::Semaphore;

use crate::catalog;
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
    table: OrderedMutex<BTreeMap<VersionId, PoolHandle>>,
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
            .field("pools", &self.inner.table.lock().len())
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
                table: OrderedMutex::new(LockLevel::DAEMON_TABLES, BTreeMap::new()),
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

    /// Every pool handle, by version id.
    pub fn handles(&self) -> Vec<PoolHandle> {
        self.inner.table.lock().values().cloned().collect()
    }

    pub fn get(&self, version: VersionId) -> Option<PoolHandle> {
        self.inner.table.lock().get(&version).cloned()
    }

    /// Statuses of every pool (quick asks in parallel).
    pub async fn statuses(&self) -> Vec<PoolStatus> {
        let handles = self.handles();
        futures::future::join_all(handles.iter().map(|h| h.status(POOL_QUICK)))
            .await
            .into_iter()
            .filter_map(Result::ok)
            .collect()
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

    /// The pool of `version`, spawned (stopped) if it does not exist yet.
    pub async fn handle_for(&self, version: VersionId) -> Result<PoolHandle> {
        if let Some(handle) = self.get(version) {
            return Ok(handle);
        }
        let _turn = self.turn().await?;
        if let Some(handle) = self.get(version) {
            return Ok(handle);
        }
        let row = catalog::version(&self.inner.reader, version).await?;
        let tool = catalog::require_tool(&self.inner.registry, &row.tool_id)?;
        if tool.manifest.spec.service.kind != ServiceKind::Pool {
            return Err(ForgeError::InvalidInput(format!(
                "{} does not run as a FastCGI pool",
                row.tool_id
            )));
        }
        let spec = self.spec(&tool.manifest, &row)?;
        let handle = PhpPool::spawn(
            spec,
            PoolDeps {
                supervisor: self.inner.supervisor.clone(),
                ports: Arc::clone(&self.inner.ports),
                listener: Some(Arc::clone(&self.inner.listener)),
            },
        )?;
        self.inner.table.lock().insert(version, handle.clone());
        Ok(handle)
    }

    /// Renders the version's configuration target (php `conf.d` overlay) and starts its
    /// pool serving; answers once `min` workers are warm.
    pub async fn ensure_started(&self, version: VersionId) -> Result<PoolHandle> {
        let handle = self.handle_for(version).await?;
        let status = handle.status(POOL_QUICK).await?;
        if matches!(status.state, PoolState::Running) && status.serving {
            return Ok(handle);
        }
        self.render_config(version).await?;
        handle.start(POOL_START_TIMEOUT).await?;
        Ok(handle)
    }

    /// Renders the configuration of `version` (its `conf.d` overlay of the render target
    /// and the quick settings of its series, `conf.d/90-forgebox-settings.ini` for PHP)
    /// and publishes `config.rendered` for what changed. Does not touch the pool.
    pub async fn render_config(&self, version: VersionId) -> Result<()> {
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
        if let Some(php) = ctx.php.as_mut() {
            php.settings = settings::context(&tool.manifest, &stored);
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
                render::apply_target(&root, &engine, &bus, &ctx, &owner, &plans).map(|_| ())
            },
        )
        .await
    }

    /// Stops and forgets the pool of `version` (ports released by the pool).
    pub async fn stop(&self, version: VersionId) -> Result<()> {
        let _turn = self.turn().await?;
        let handle = self.inner.table.lock().remove(&version);
        if let Some(handle) = handle {
            let result = handle.stop(POOL_STOP_TIMEOUT).await;
            handle.shutdown().await;
            result?;
        }
        Ok(())
    }

    /// Takes the pool of `version` out of service without cutting requests (docs/06
    /// §2.2): it leaves the table at once and, in the background, drains (each worker
    /// stops once its connections end, at most the pool's `drain_timeout`) and stops.
    pub fn retire(&self, version: VersionId) {
        let Some(handle) = self.inner.table.lock().remove(&version) else {
            return;
        };
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
        });
    }

    /// Stops every pool in parallel within `timeout`, then their actors.
    pub async fn stop_all(&self, timeout: Duration) {
        let handles: Vec<PoolHandle> = {
            let mut table = self.inner.table.lock();
            std::mem::take(&mut *table).into_values().collect()
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

    /// Blue/green switcher over this table and the web listener.
    pub fn switcher(&self) -> Switcher {
        Switcher::new(
            Arc::new(self.clone()),
            Arc::clone(&self.inner.listener),
            SwapOptions::default(),
        )
    }
}

impl PoolDirectory for PoolManager {
    fn pool(&self, version: VersionId) -> forge_fcgi::swap::PoolLookup<'_> {
        Box::pin(self.handle_for(version))
    }
}

/// The update engine's view of the pools: blue/green by version id when the source pool
/// runs, nothing to do otherwise.
#[derive(Clone)]
pub struct DaemonPoolSwitcher(pub PoolManager);

impl PoolSwitcher for DaemonPoolSwitcher {
    fn switch<'a>(&'a self, switch: &'a PoolSwitch) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.0.get(switch.from).is_none() {
                return Ok(());
            }
            self.0
                .switcher()
                .switch(switch.from, switch.to)
                .await
                .map(|_| ())
        })
    }

    fn serving<'a>(&'a self, _tool: &'a ToolId, version: VersionId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            match self.0.get(version) {
                Some(handle) => Ok(handle
                    .status(POOL_QUICK)
                    .await
                    .is_ok_and(|s| s.serving && matches!(s.state, PoolState::Running))),
                None => Ok(false),
            }
        })
    }
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
