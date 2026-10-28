//! Service instances (docs/03 §1, docs/01 §5.1 step 6): one `forge_supervisor`
//! [`InstanceActor`](forge_supervisor::InstanceActor) per row of `instances`, built from
//! the tool's manifest (`service.daemon`) without any per-tool code:
//!
//! * ports: `service.daemon.ports[]` reserved through the allocator (the manifest
//!   `default` when free, else the first free port of its range), owner
//!   `instance/<id>/<port name>`;
//! * `instance.config.<id>`: `config.files[].path`; `instance.data_dir`:
//!   `service.daemon.data.dir`; every string substituted by [`RuntimeVars`];
//! * spawn, stop cascade (`stop.command`, then `ctrl-break`/`close`, then the job),
//!   health probe, restart policy, pre-checks (`check`, ports, `depends_on`, runtime deps)
//!   and the output log `logs/<tool>/<name>.out.log`.
//!
//! Before each start the tool's configuration target is rendered (when `forge-render`
//! has one; web servers get the sites and pools) and `data.init` runs once when the data
//! folder does not exist yet. The table of handles is a leaf lock (`daemon.tables`); every
//! wait on an actor has a timeout; starts and stops of different instances run in
//! parallel.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::ids::{InstanceId, ToolId};
use forge_core::model::port::{OwnerKind, ReservationOwner};
use forge_core::paths::ForgeRoot;
use forge_core::platform::{ProbeCommand, RuntimeDep};
use forge_core::store::{
    DesiredState, InstanceRepo, InstanceRow, StoreReader, StoreWriter, ToolVersionRow,
};
use forge_core::sync::{LockLevel, OrderedMutex};
use forge_manifest::schema::RestartOn;
use forge_manifest::schema::tool::{DaemonService, ServiceKind, StopSignal, ToolManifest};
use forge_net::ports::PortAllocator;
use forge_provider::layout::Layout;
use forge_registry::Registry;
use forge_supervisor::precheck::{ConfigCheck, DependencyRef, PrecheckSpec};
use forge_supervisor::{
    Backoff, DependencyFuture, DependencyLookup, HealthSpec, InstanceHandle, InstanceSpec,
    InstanceState, InstanceStatus, Probe, QUICK_ASK_TIMEOUT, RestartPolicy, SpawnSpec, StopPlan,
    StopReport, Supervisor,
};
use tokio::sync::Semaphore;

use crate::catalog::{self, ToolEntry};
use crate::vars::{InstanceVars, RuntimeVars, split_command};

/// Budget of a start (pre-checks, spawn, health) beyond the manifest's `start_timeout`.
const START_SLACK: Duration = Duration::from_secs(15);
/// Budget of `data.init` (database initialization can be slow on first run).
const DATA_INIT_TIMEOUT: Duration = Duration::from_secs(300);
/// Budget of `reload.command`.
const RELOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// Wait for the services turn (create/remove).
const TURN_TIMEOUT: Duration = Duration::from_secs(30);

/// The folders of the log files a manifest declares (`service.daemon.logs[].path`) for
/// one instance, keeping only those inside the root (no `..`): a manifest never makes the
/// daemon create folders elsewhere.
fn log_dirs_of(
    root: &ForgeRoot,
    manifest: &ToolManifest,
    vars: &RuntimeVars,
) -> Result<Vec<PathBuf>> {
    let Some(daemon) = manifest.spec.service.daemon.as_ref() else {
        return Ok(Vec::new());
    };
    let mut dirs = Vec::new();
    for log in &daemon.logs {
        let path = PathBuf::from(vars.substitute(&log.path)?);
        let inside = |p: &&std::path::Path| {
            p.starts_with(root.path())
                && !p
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
        };
        if let Some(parent) = path.parent().filter(inside) {
            if !dirs.iter().any(|d: &PathBuf| d == parent) {
                dirs.push(parent.to_path_buf());
            }
        }
    }
    Ok(dirs)
}

/// One instance: its row, its resolved runtime facts and its actor.
#[derive(Clone)]
pub struct Entry {
    pub row: InstanceRow,
    pub version: Option<ToolVersionRow>,
    pub manifest: Option<Arc<ToolManifest>>,
    pub ports: BTreeMap<String, u16>,
    pub config_files: BTreeMap<String, String>,
    pub vars: Option<RuntimeVars>,
    pub handle: Option<InstanceHandle>,
    /// Why there is no actor (manifest or version missing, invalid spec).
    pub error: Option<String>,
    pub start_timeout: Duration,
    pub stop_worst_case: Duration,
}

impl Entry {
    pub fn label(&self) -> String {
        format!("{}/{}", self.row.tool_id, self.row.name)
    }
}

/// What the rest of the daemon needs to render a tool's configuration before a start
/// (implemented by the web module: sites and pools for web servers).
pub trait ConfigRenderer: Send + Sync {
    fn render_instance<'a>(
        &'a self,
        entry: &'a Entry,
    ) -> futures::future::BoxFuture<'a, Result<()>>;
}

struct Inner {
    root: ForgeRoot,
    supervisor: Supervisor,
    ports: PortAllocator,
    layout: Layout,
    registry: Registry,
    reader: StoreReader,
    writer: StoreWriter,
    prefs: Arc<dyn Fn() -> BTreeMap<String, String> + Send + Sync>,
    renderer: std::sync::OnceLock<Arc<dyn ConfigRenderer>>,
    /// Lock `daemon.tables` (leaf).
    table: OrderedMutex<BTreeMap<InstanceId, Entry>>,
    turn: Arc<Semaphore>,
}

/// The instances of the daemon. Cheap to clone.
#[derive(Clone)]
pub struct ServiceManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ServiceManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceManager")
            .field("instances", &self.inner.table.lock().len())
            .finish()
    }
}

/// Dependency lookup over the manager (pre-check `depends_on`).
struct Lookup(std::sync::Weak<Inner>);

impl DependencyLookup for Lookup {
    fn state(&self, dependency: &DependencyRef) -> DependencyFuture {
        let inner = self.0.upgrade();
        let dependency = dependency.clone();
        Box::pin(async move {
            let inner = inner?;
            let handles: Vec<InstanceHandle> = inner
                .table
                .lock()
                .values()
                .filter(|e| {
                    e.row.tool_id == dependency.tool
                        && dependency
                            .instance
                            .as_deref()
                            .is_none_or(|name| name == e.row.name)
                })
                .filter_map(|e| e.handle.clone())
                .collect();
            let mut best: Option<InstanceState> = None;
            for handle in handles {
                if let Ok(status) = handle.status(QUICK_ASK_TIMEOUT).await {
                    if status.state == InstanceState::Running {
                        return Some(status.state);
                    }
                    best.get_or_insert(status.state);
                }
            }
            best
        })
    }
}

impl ServiceManager {
    /// Manager over `supervisor` (which gets the dependency lookup of this manager).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        root: ForgeRoot,
        supervisor: Supervisor,
        ports: PortAllocator,
        layout: Layout,
        registry: Registry,
        reader: StoreReader,
        writer: StoreWriter,
        prefs: Arc<dyn Fn() -> BTreeMap<String, String> + Send + Sync>,
    ) -> Self {
        let inner = Arc::new_cyclic(|weak: &std::sync::Weak<Inner>| Inner {
            root,
            supervisor: supervisor.with_dependencies(Arc::new(Lookup(weak.clone()))),
            ports,
            layout,
            registry,
            reader,
            writer,
            prefs,
            renderer: std::sync::OnceLock::new(),
            table: OrderedMutex::new(LockLevel::DAEMON_TABLES, BTreeMap::new()),
            turn: Arc::new(Semaphore::new(1)),
        });
        Self { inner }
    }

    /// Plugs the configuration renderer (once, at boot).
    pub fn set_renderer(&self, renderer: Arc<dyn ConfigRenderer>) {
        let _ = self.inner.renderer.set(renderer);
    }

    pub fn supervisor(&self) -> &Supervisor {
        &self.inner.supervisor
    }

    async fn turn(&self) -> Result<tokio::sync::OwnedSemaphorePermit> {
        tokio::time::timeout(TURN_TIMEOUT, Arc::clone(&self.inner.turn).acquire_owned())
            .await
            .map_err(|_| ForgeError::Timeout {
                operation: "waiting for another service operation".into(),
                after: TURN_TIMEOUT,
            })?
            .map_err(|_| ForgeError::Internal("services turn closed".into()))
    }

    /// Snapshot of every entry, by tool then name.
    pub fn entries(&self) -> Vec<Entry> {
        let mut out: Vec<Entry> = self.inner.table.lock().values().cloned().collect();
        out.sort_by(|a, b| {
            (a.row.tool_id.as_str(), a.row.name.as_str())
                .cmp(&(b.row.tool_id.as_str(), b.row.name.as_str()))
        });
        out
    }

    pub fn get(&self, id: InstanceId) -> Option<Entry> {
        self.inner.table.lock().get(&id).cloned()
    }

    /// Entries matching a target: `all`, a tool id, `tool/name` or an instance ULID.
    pub fn resolve(&self, target: &str) -> Result<Vec<Entry>> {
        let target = target.trim();
        let entries = self.entries();
        if target.is_empty() || target == "all" {
            return Ok(entries);
        }
        if let Ok(id) = target.parse::<InstanceId>() {
            return entries
                .into_iter()
                .find(|e| e.row.id == id)
                .map(|e| vec![e])
                .ok_or_else(|| ForgeError::NotFound(format!("instance {id}")));
        }
        let (tool, name) = match target.split_once('/') {
            Some((tool, name)) => (tool, Some(name)),
            None => (target, None),
        };
        let found: Vec<Entry> = entries
            .into_iter()
            .filter(|e| e.row.tool_id.as_str() == tool && name.is_none_or(|n| n == e.row.name))
            .collect();
        if found.is_empty() {
            return Err(ForgeError::NotFound(format!("service {target}")));
        }
        Ok(found)
    }

    /// Reads every row and spawns its actor (state `Stopped`). Rows whose manifest or
    /// version is missing stay listed with an error.
    pub async fn load_all(&self) -> Result<usize> {
        let rows = self
            .inner
            .reader
            .query(|conn| InstanceRepo::new(conn).list())
            .await?;
        let count = rows.len();
        for row in rows {
            let entry = self.build(row).await;
            if let Some(err) = &entry.error {
                tracing::warn!(instance = %entry.label(), %err, "instance not runnable");
            }
            self.inner.table.lock().insert(entry.row.id, entry);
        }
        Ok(count)
    }

    /// Creates the `default` instance of every installed tool with `service.kind:
    /// daemon` that has none yet (newest installed version, `autostart` from the
    /// manifest). Returns the instances created.
    pub async fn ensure_defaults(&self) -> Result<Vec<InstanceId>> {
        let _turn = self.turn().await?;
        let mut created = Vec::new();
        for tool in catalog::tools(&self.inner.registry) {
            if tool.manifest.spec.service.kind != ServiceKind::Daemon {
                continue;
            }
            let id = tool.manifest.metadata.id.clone();
            if self
                .inner
                .table
                .lock()
                .values()
                .any(|e| e.row.tool_id == id)
            {
                continue;
            }
            let rows = catalog::installed(&self.inner.reader, &id, Some(&tool.manifest)).await?;
            let Some(newest) = rows.first() else {
                continue;
            };
            let autostart = tool
                .manifest
                .spec
                .service
                .daemon
                .as_ref()
                .is_some_and(|d| d.autostart);
            match self
                .create_locked(&tool, newest, "default", None, autostart)
                .await
            {
                Ok(entry) => created.push(entry.row.id),
                Err(err) => tracing::warn!(tool = %id, %err, "default instance not created"),
            }
        }
        Ok(created)
    }

    /// Builds the entries of rows that someone else stored (the Laragon import) and the
    /// table does not know yet. Returns the ids adopted.
    pub async fn adopt_stored(&self) -> Result<Vec<InstanceId>> {
        let _turn = self.turn().await?;
        let rows = self
            .inner
            .reader
            .query(|conn| InstanceRepo::new(conn).list())
            .await?;
        let mut adopted = Vec::new();
        for row in rows {
            let known = self.inner.table.lock().contains_key(&row.id);
            if known {
                continue;
            }
            let entry = self.build(row).await;
            if let Some(err) = &entry.error {
                tracing::warn!(instance = %entry.label(), %err, "adopted instance not runnable");
            }
            adopted.push(entry.row.id);
            self.inner.table.lock().insert(entry.row.id, entry);
        }
        Ok(adopted)
    }

    /// `services.create`: a new instance of `version` of `tool`.
    pub async fn create(
        &self,
        tool: &ToolEntry,
        version: &ToolVersionRow,
        name: &str,
        port: Option<u16>,
        autostart: bool,
    ) -> Result<Entry> {
        let _turn = self.turn().await?;
        self.create_locked(tool, version, name, port, autostart)
            .await
    }

    async fn create_locked(
        &self,
        tool: &ToolEntry,
        version: &ToolVersionRow,
        name: &str,
        port: Option<u16>,
        autostart: bool,
    ) -> Result<Entry> {
        let daemon = tool
            .manifest
            .spec
            .service
            .daemon
            .as_ref()
            .filter(|_| tool.manifest.spec.service.kind == ServiceKind::Daemon)
            .ok_or_else(|| {
                ForgeError::InvalidInput(format!(
                    "{} has no long-running service (service.kind: {})",
                    tool.manifest.metadata.id,
                    tool.manifest.spec.service.kind.as_str()
                ))
            })?;
        forge_core::store::validate_instance_name(name)?;
        let existing = self
            .inner
            .table
            .lock()
            .values()
            .filter(|e| e.row.tool_id == tool.manifest.metadata.id)
            .count();
        if existing > 0 && !daemon.multi_instance {
            return Err(ForgeError::Conflict(format!(
                "{} allows a single instance (service.daemon.multi_instance: false)",
                tool.manifest.metadata.id
            )));
        }
        let mut row = InstanceRow::new(tool.manifest.metadata.id.clone(), version.id, name);
        row.autostart = autostart;
        let inserted = row.clone();
        self.inner
            .writer
            .execute(move |conn| InstanceRepo::new(conn).insert(&inserted))
            .await?;
        if let (Some(port), Some(first)) = (port, daemon.ports.first()) {
            // The explicit main port; on refusal the new row goes away again.
            let owner = instance_owner(row.id, &first.name)?;
            if let Err(err) = self
                .inner
                .ports
                .reserve(port, Some(&first.range), owner)
                .await
            {
                let id = row.id;
                let _ = self
                    .inner
                    .writer
                    .execute(move |conn| InstanceRepo::new(conn).delete(id))
                    .await;
                return Err(err);
            }
        }
        let mut entry = self.build(row).await;
        if let Some(vars) = &entry.vars {
            if let Some(dir) = vars.instance.as_ref().map(|i| i.data_dir.clone()) {
                if !dir.is_empty() {
                    entry.row.data_dir = Some(dir);
                    let row = entry.row.clone();
                    self.inner
                        .writer
                        .execute(move |conn| InstanceRepo::new(conn).update(&row))
                        .await?;
                }
            }
        }
        self.inner.table.lock().insert(entry.row.id, entry.clone());
        Ok(entry)
    }

    /// `services.remove`: stops, forgets the row and its ports (the data folder is
    /// deleted only with `delete_data`).
    pub async fn remove(&self, id: InstanceId, delete_data: bool) -> Result<()> {
        let _turn = self.turn().await?;
        let entry = self
            .get(id)
            .ok_or_else(|| ForgeError::NotFound(format!("instance {id}")))?;
        if let Some(handle) = &entry.handle {
            let _ = handle.stop(entry.stop_worst_case + START_SLACK).await;
            handle.shutdown().await;
        }
        self.inner
            .writer
            .execute(move |conn| InstanceRepo::new(conn).delete(id))
            .await?;
        self.inner
            .ports
            .release_owner(OwnerKind::Instance, &id.to_string())
            .await?;
        self.inner.table.lock().remove(&id);
        if delete_data {
            if let Some(dir) = entry.row.data_dir.clone() {
                let root = self.inner.root.clone();
                let path = PathBuf::from(&dir);
                if path.starts_with(root.data()) && path != root.data() {
                    forge_core::runtime::spawn_blocking_timeout(
                        "deleting the data folder",
                        Duration::from_secs(120),
                        move || {
                            std::fs::remove_dir_all(&path)
                                .or_else(|e| {
                                    if e.kind() == std::io::ErrorKind::NotFound {
                                        Ok(())
                                    } else {
                                        Err(e)
                                    }
                                })
                                .map_err(|e| ForgeError::io_at("deleting", &path, e))
                        },
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// Resolves the row into a runnable spec and spawns its actor.
    async fn build(&self, row: InstanceRow) -> Entry {
        let mut entry = Entry {
            row,
            version: None,
            manifest: None,
            ports: BTreeMap::new(),
            config_files: BTreeMap::new(),
            vars: None,
            handle: None,
            error: None,
            start_timeout: Duration::from_secs(30),
            stop_worst_case: StopPlan::default().worst_case(),
        };
        match self.resolve_spec(&mut entry).await {
            Ok(spec) => {
                entry.start_timeout = spec
                    .health
                    .as_ref()
                    .map_or(Duration::from_secs(30), |h| h.start_timeout);
                entry.stop_worst_case = spec.stop.worst_case();
                match self.inner.supervisor.spawn_instance(spec) {
                    Ok(handle) => entry.handle = Some(handle),
                    Err(err) => entry.error = Some(err.to_string()),
                }
            }
            Err(err) => entry.error = Some(err.to_string()),
        }
        entry
    }

    async fn resolve_spec(&self, entry: &mut Entry) -> Result<InstanceSpec> {
        let tool = catalog::require_tool(&self.inner.registry, &entry.row.tool_id)?;
        let version = catalog::version(&self.inner.reader, entry.row.version_id).await?;
        let daemon = tool.manifest.spec.service.daemon.clone().ok_or_else(|| {
            ForgeError::ConfigInvalid(format!("{} declares no service.daemon", entry.row.tool_id))
        })?;
        entry.manifest = Some(Arc::clone(&tool.manifest));
        entry.version = Some(version.clone());

        let mut ports = BTreeMap::new();
        for spec in &daemon.ports {
            let owner = instance_owner(entry.row.id, &spec.name)?;
            let reservation = match self.inner.ports.get(&owner).await? {
                Some(existing) => existing,
                None => {
                    // The manifest's default port, when free and still part of the
                    // range: a range reconfigured to leave it out (`[ports.ranges] web =
                    // [8080, 8443]`) means "not that port".
                    let preferred = match spec.default {
                        Some(port) if self.inner.ports.in_range(&spec.range, port) => self
                            .inner
                            .ports
                            .reserve(port, Some(&spec.range), owner.clone())
                            .await
                            .ok(),
                        _ => None,
                    };
                    match preferred {
                        Some(r) => r,
                        None => self.inner.ports.allocate(&spec.range, owner).await?,
                    }
                }
            };
            ports.insert(spec.name.clone(), reservation.port);
        }

        let base = self.inner.layout.vars_for(&tool.manifest, &version);
        let mut vars = RuntimeVars::new(base);
        vars.prefs = (self.inner.prefs)();
        vars.instance = Some(InstanceVars {
            name: entry.row.name.clone(),
            data_dir: String::new(),
            ports: ports.clone(),
            config: BTreeMap::new(),
        });
        // A data folder that is a reference (`forge import laragon`) is used where it is;
        // otherwise the manifest decides, so a moved root keeps working.
        let data_dir = match (entry.row.external_data_dir(), &daemon.data) {
            (Some(external), _) => external.to_owned(),
            (None, Some(data)) => vars.substitute(&data.dir)?,
            (None, None) => entry.row.data_dir.clone().unwrap_or_default(),
        };
        if let Some(instance) = vars.instance.as_mut() {
            instance.data_dir = data_dir.clone();
        }
        let mut config_files = BTreeMap::new();
        for file in &tool.manifest.spec.config.files {
            config_files.insert(file.id.clone(), vars.substitute(&file.path)?);
        }
        if let Some(instance) = vars.instance.as_mut() {
            instance.config = config_files.clone();
        }
        let spec = instance_spec(
            &self.inner.root,
            &tool.manifest,
            &daemon,
            &version,
            &entry.row,
            &vars,
            &ports,
        )?;
        entry.ports = ports;
        entry.config_files = config_files;
        if !data_dir.is_empty() {
            entry.row.data_dir = Some(data_dir);
        }
        entry.vars = Some(vars);
        Ok(spec)
    }

    fn handle_of(&self, id: InstanceId) -> Result<(Entry, InstanceHandle)> {
        let entry = self
            .get(id)
            .ok_or_else(|| ForgeError::NotFound(format!("instance {id}")))?;
        let handle = entry.handle.clone().ok_or_else(|| {
            ForgeError::ConfigInvalid(format!(
                "{} cannot run: {}",
                entry.label(),
                entry.error.clone().unwrap_or_default()
            ))
        })?;
        Ok((entry, handle))
    }

    async fn prepare(&self, entry: &Entry) -> Result<()> {
        if let Some(renderer) = self.inner.renderer.get().cloned() {
            renderer.render_instance(entry).await?;
        }
        self.log_dirs(entry).await?;
        self.data_init(entry).await
    }

    /// Creates the folders of the log files the manifest declares
    /// (`service.daemon.logs[].path`, usually per instance: `logs/mysql/<name>/`): the
    /// tool writes the file but most tools do not create its folder.
    async fn log_dirs(&self, entry: &Entry) -> Result<()> {
        let (Some(manifest), Some(vars)) = (&entry.manifest, &entry.vars) else {
            return Ok(());
        };
        let dirs = log_dirs_of(&self.inner.root, manifest, vars)?;
        if dirs.is_empty() {
            return Ok(());
        }
        forge_core::runtime::spawn_blocking_timeout(
            "creating the log folders",
            Duration::from_secs(10),
            move || {
                for dir in &dirs {
                    std::fs::create_dir_all(dir)
                        .map_err(|e| ForgeError::io_at("creating", dir, e))?;
                }
                Ok(())
            },
        )
        .await
    }

    /// Runs `service.daemon.data.init` once, when the data folder does not exist.
    async fn data_init(&self, entry: &Entry) -> Result<()> {
        let (Some(manifest), Some(version), Some(vars)) =
            (&entry.manifest, &entry.version, &entry.vars)
        else {
            return Ok(());
        };
        let Some(data) = manifest
            .spec
            .service
            .daemon
            .as_ref()
            .and_then(|d| d.data.clone())
        else {
            return Ok(());
        };
        let dir = PathBuf::from(vars.substitute(&data.dir)?);
        let exists = {
            let dir = dir.clone();
            tokio::task::spawn_blocking(move || dir.is_dir())
                .await
                .unwrap_or(false)
        };
        if exists {
            return Ok(());
        }
        let Some(init) = data.init else {
            let dir2 = dir.clone();
            return forge_core::runtime::spawn_blocking_timeout(
                "creating the data folder",
                Duration::from_secs(10),
                move || {
                    std::fs::create_dir_all(&dir2)
                        .map_err(|e| ForgeError::io_at("creating", &dir2, e))
                },
            )
            .await;
        };
        if let Some(parent) = dir.parent() {
            let parent = parent.to_path_buf();
            let _ = tokio::task::spawn_blocking(move || std::fs::create_dir_all(parent)).await;
        }
        let line = vars.substitute(&init.command)?;
        let mut parts = split_command(&line).into_iter();
        let program = parts
            .next()
            .ok_or_else(|| ForgeError::ConfigInvalid("empty data.init command".into()))?;
        let version_dir = PathBuf::from(&version.path);
        let exe = resolve_exe(&version_dir, &program);
        let command = ProbeCommand {
            exe,
            args: parts.map(Into::into).collect(),
            cwd: version_dir.clone(),
        };
        tracing::info!(instance = %entry.label(), dir = %dir.display(), "initializing the data folder");
        let output = forge_core::runtime::spawn_blocking_timeout(
            "data.init",
            DATA_INIT_TIMEOUT + Duration::from_secs(10),
            move || forge_core::platform::run_probe(&command, DATA_INIT_TIMEOUT),
        )
        .await?;
        if output.exit_code != Some(0) {
            return Err(ForgeError::ConfigInvalid(format!(
                "{}: data.init failed (exit {:?}): {}",
                entry.label(),
                output.exit_code,
                tail(&output.combined(), 2_000)
            )));
        }
        Ok(())
    }

    /// Starts one instance (renders its configuration first) and records `running` as
    /// its desired state. Answers when it is `Running`.
    pub async fn start(&self, id: InstanceId) -> Result<u32> {
        let (entry, handle) = self.handle_of(id)?;
        self.prepare(&entry).await?;
        let pid = handle.start(entry.start_timeout + START_SLACK).await?;
        self.set_desired(id, DesiredState::Running).await;
        Ok(pid)
    }

    /// Stops one instance with the cascade.
    pub async fn stop(&self, id: InstanceId) -> Result<StopReport> {
        let (entry, handle) = self.handle_of(id)?;
        let report = handle.stop(entry.stop_worst_case + START_SLACK).await?;
        self.set_desired(id, DesiredState::Stopped).await;
        Ok(report)
    }

    /// Restarts one instance (validated first when running, docs/03 §1.5).
    pub async fn restart(&self, id: InstanceId) -> Result<u32> {
        let (entry, handle) = self.handle_of(id)?;
        self.prepare(&entry).await?;
        let pid = handle
            .restart(entry.stop_worst_case + entry.start_timeout + START_SLACK)
            .await?;
        self.set_desired(id, DesiredState::Running).await;
        Ok(pid)
    }

    /// Re-renders the configuration and applies it to a running instance
    /// ([`ServiceManager::apply_reload`]).
    pub async fn reload(&self, id: InstanceId) -> Result<()> {
        let (entry, _) = self.handle_of(id)?;
        if let Some(renderer) = self.inner.renderer.get().cloned() {
            renderer.render_instance(&entry).await?;
        }
        self.apply_reload(id).await.map(|_| ())
    }

    /// Applies already rendered configuration to a running instance:
    /// `service.daemon.reload.command` when declared, otherwise a restart. Returns
    /// whether the instance was running (and therefore reloaded).
    pub async fn apply_reload(&self, id: InstanceId) -> Result<bool> {
        let (entry, handle) = self.handle_of(id)?;
        let status = handle.status(QUICK_ASK_TIMEOUT).await?;
        if status.state != InstanceState::Running {
            return Ok(false);
        }
        let command = entry
            .manifest
            .as_ref()
            .and_then(|m| m.spec.service.daemon.as_ref())
            .and_then(|d| d.reload.as_ref())
            .and_then(|r| r.command.clone());
        match (command, &entry.vars, &entry.version) {
            (Some(command), Some(vars), Some(version)) => {
                let line = vars.substitute(&command)?;
                let mut parts = split_command(&line).into_iter();
                let program = parts
                    .next()
                    .ok_or_else(|| ForgeError::ConfigInvalid("empty reload command".into()))?;
                let version_dir = PathBuf::from(&version.path);
                let probe = ProbeCommand {
                    exe: resolve_exe(&version_dir, &program),
                    args: parts.map(Into::into).collect(),
                    cwd: version_dir,
                };
                let output = forge_core::runtime::spawn_blocking_timeout(
                    "reload command",
                    RELOAD_TIMEOUT + Duration::from_secs(5),
                    move || forge_core::platform::run_probe(&probe, RELOAD_TIMEOUT),
                )
                .await?;
                if output.exit_code == Some(0) {
                    Ok(true)
                } else {
                    Err(ForgeError::ConfigInvalid(format!(
                        "{}: reload failed (exit {:?}): {}",
                        entry.label(),
                        output.exit_code,
                        tail(&output.combined(), 2_000)
                    )))
                }
            }
            _ => handle
                .restart(entry.stop_worst_case + entry.start_timeout + START_SLACK)
                .await
                .map(|_| true),
        }
    }

    /// Records one daemon-owned key of `instances.settings` (`forgebox.*`) in the store and
    /// in the cached entry, so the next render sees it.
    pub async fn record_setting(&self, id: InstanceId, key: &str, value: &str) -> Result<()> {
        let (k, v) = (key.to_owned(), value.to_owned());
        self.inner
            .writer
            .execute(move |conn| InstanceRepo::new(conn).set_setting(id, &k, &v))
            .await?;
        if let Some(entry) = self.inner.table.lock().get_mut(&id) {
            entry.row.settings.insert(key.to_owned(), value.to_owned());
        }
        Ok(())
    }

    async fn set_desired(&self, id: InstanceId, state: DesiredState) {
        let result = self
            .inner
            .writer
            .execute(move |conn| InstanceRepo::new(conn).set_desired_state(id, state))
            .await;
        if let Err(err) = result {
            tracing::warn!(%id, %err, "desired state not recorded");
        }
        if let Some(entry) = self.inner.table.lock().get_mut(&id) {
            entry.row.desired_state = state;
        }
    }

    /// Status of every instance with an actor, asked in parallel (each with the quick
    /// timeout); entries without an actor are reported with their error.
    pub async fn statuses(&self) -> Vec<(Entry, Option<InstanceStatus>)> {
        let entries = self.entries();
        let futures = entries.into_iter().map(|entry| async move {
            let status = match &entry.handle {
                Some(handle) => handle.status(QUICK_ASK_TIMEOUT).await.ok(),
                None => None,
            };
            (entry, status)
        });
        futures::future::join_all(futures).await
    }

    /// Instances to start at boot: `autostart` or desired state `running`, in waves
    /// following `depends_on` (a wave starts in parallel; a cycle starts the rest
    /// together). Failures are logged; the boot goes on.
    pub async fn autostart(
        &self,
        filter: &(dyn Fn(&Entry) -> bool + Sync),
    ) -> Vec<(String, Result<u32>)> {
        let wanted: Vec<Entry> = self
            .entries()
            .into_iter()
            .filter(|e| e.handle.is_some() && filter(e))
            .filter(|e| e.row.autostart || e.row.desired_state == DesiredState::Running)
            .collect();
        let mut results = Vec::new();
        for wave in topological_waves(&wanted) {
            let starts = wave.into_iter().map(|entry| async move {
                let label = entry.label();
                (label, self.start(entry.row.id).await)
            });
            for (label, result) in futures::future::join_all(starts).await {
                match &result {
                    Ok(pid) => tracing::info!(instance = %label, pid, "autostarted"),
                    Err(err) => tracing::warn!(instance = %label, %err, "autostart failed"),
                }
                results.push((label, result));
            }
        }
        results
    }

    /// Stops every running instance in parallel (bounded by `timeout` overall), then
    /// stops the actors. Desired states are kept (they are restored at the next boot).
    pub async fn stop_all(&self, timeout: Duration) {
        let entries = self.entries();
        let stops = entries.iter().filter_map(|e| {
            let handle = e.handle.clone()?;
            let budget = e.stop_worst_case + Duration::from_secs(1);
            let label = e.label();
            Some(async move {
                let status = handle.status(QUICK_ASK_TIMEOUT).await;
                let active = status.as_ref().is_ok_and(|s| s.state.is_active() || matches!(s.state, InstanceState::Crashed { .. }));
                if active {
                    match handle.stop(budget).await {
                        Ok(report) => tracing::info!(instance = %label, elapsed_ms = report.elapsed_ms, "stopped"),
                        Err(err) => tracing::warn!(instance = %label, %err, "stop failed"),
                    }
                }
                handle.shutdown().await;
            })
        });
        if tokio::time::timeout(timeout, futures::future::join_all(stops))
            .await
            .is_err()
        {
            tracing::warn!(
                ?timeout,
                "instances did not stop within the shutdown budget"
            );
        }
    }
}

/// The last `max` bytes of `text` (on a character boundary).
pub fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.trim_end().to_owned();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].trim_end().to_owned()
}

/// Owner of the port `name` of instance `id`.
pub fn instance_owner(id: InstanceId, name: &str) -> Result<ReservationOwner> {
    ReservationOwner::new(OwnerKind::Instance, id.to_string(), name)
}

/// A program of a manifest command: relative paths are inside the version folder.
fn resolve_exe(version_dir: &std::path::Path, program: &str) -> PathBuf {
    let path = PathBuf::from(program.replace('/', "\\"));
    if path.is_absolute() {
        path
    } else {
        version_dir.join(path)
    }
}

/// `PATH` entries of a version: the folder plus the manifest's `install.path` subfolders.
fn path_entries(manifest: &ToolManifest, version_dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = vec![version_dir.to_path_buf()];
    for entry in &manifest.spec.install.path {
        let dir = if entry == "." {
            version_dir.to_path_buf()
        } else {
            version_dir.join(entry.replace('/', "\\"))
        };
        if !out.contains(&dir) {
            out.push(dir);
        }
    }
    out
}

fn command_spec(
    line: &str,
    version_dir: &std::path::Path,
    env: &forge_supervisor::process::EnvMap,
    path: &[PathBuf],
) -> Result<SpawnSpec> {
    let mut parts = split_command(line).into_iter();
    let program = parts
        .next()
        .ok_or_else(|| ForgeError::ConfigInvalid("empty command".into()))?;
    let mut spec = SpawnSpec::new(resolve_exe(version_dir, &program), version_dir).args(parts);
    spec.env = env.clone();
    spec.path = path.to_vec();
    spec.stdio = forge_supervisor::process::Stdio::Discard;
    Ok(spec)
}

/// The supervisor spec of an instance (pure: everything already substituted).
pub fn instance_spec(
    root: &ForgeRoot,
    manifest: &ToolManifest,
    daemon: &DaemonService,
    version: &ToolVersionRow,
    row: &InstanceRow,
    vars: &RuntimeVars,
    ports: &BTreeMap<String, u16>,
) -> Result<InstanceSpec> {
    let version_dir = PathBuf::from(&version.path);
    let exe = resolve_exe(&version_dir, &vars.substitute(&daemon.exe)?);
    let cwd = match &daemon.cwd {
        Some(cwd) => PathBuf::from(vars.substitute(cwd)?),
        None => version_dir.clone(),
    };
    let path = path_entries(manifest, &version_dir);
    let mut spawn = SpawnSpec::new(exe, cwd).args(vars.substitute_all(&daemon.args)?);
    for (name, value) in manifest.spec.config.env.iter().chain(&daemon.env) {
        if name.eq_ignore_ascii_case("PATH") {
            continue;
        }
        spawn.env.set(name.clone(), vars.substitute(value)?);
    }
    for spec in &daemon.ports {
        if let (Some(env), Some(port)) = (&spec.env, ports.get(&spec.name)) {
            spawn.env.set(env.clone(), port.to_string());
        }
    }
    spawn.path = path.clone();
    spawn.new_process_group = daemon.console_group;

    let mut instance =
        InstanceSpec::new(row.id, row.tool_id.clone(), row.name.clone(), spawn.clone())
            .log_to(root)?;

    // Stop cascade: stop.command, then the manifest's next step; the job always last.
    let mut stop = StopPlan {
        timeout: daemon.stop.timeout.as_std(),
        ctrl_break: matches!(daemon.stop.then, StopSignal::CtrlBreak | StopSignal::CtrlC)
            && daemon.console_group,
        wm_close: matches!(daemon.stop.then, StopSignal::Close),
        ..StopPlan::default()
    };
    if let Some(command) = &daemon.stop.command {
        stop.command = Some(command_spec(
            &vars.substitute(command)?,
            &version_dir,
            &spawn.env,
            &path,
        )?);
    }
    instance.stop = stop;

    if let Some(health) = &daemon.health {
        let probe = if let Some(tcp) = &health.tcp {
            Probe::Tcp {
                addr: vars.substitute(tcp)?,
            }
        } else if let Some(http) = &health.http {
            Probe::Http {
                url: vars.substitute(&http.url)?,
                expect_status: http.expect_status,
            }
        } else if let Some(command) = &health.command {
            Probe::Command {
                command: command_spec(
                    &vars.substitute(&command.cmd)?,
                    &version_dir,
                    &spawn.env,
                    &path,
                )?,
                exit_code: command.exit_code,
            }
        } else {
            Probe::Pid
        };
        let mut spec = HealthSpec::new(probe);
        spec.interval = health.interval.as_std();
        spec.success_threshold = health.success_threshold.max(1);
        spec.failure_threshold = health.failure_threshold.max(1);
        spec.start_timeout = daemon.start_timeout.as_std();
        instance.health = Some(spec);
    }

    instance.restart = match daemon.restart_policy.on {
        RestartOn::Never => RestartPolicy::Never,
        RestartOn::Always => RestartPolicy::Always {
            backoff: Backoff::default(),
        },
        RestartOn::Failure => RestartPolicy::OnFailure {
            max: daemon.restart_policy.max,
            window: daemon.restart_policy.window.as_std(),
            backoff: Backoff::default(),
        },
    };

    let mut precheck = PrecheckSpec::default().with_ports(ports.values().copied());
    if let Some(check) = &daemon.check {
        precheck = precheck.with_check(ConfigCheck::new(command_spec(
            &vars.substitute(&check.command)?,
            &version_dir,
            &spawn.env,
            &path,
        )?));
    }
    let mut deps = BTreeSet::new();
    for dep in &daemon.depends_on {
        if deps.insert(dep.clone()) {
            precheck = precheck.with_dependency(DependencyRef::tool(dep.clone()));
        }
    }
    for dep in &manifest.spec.runtime_deps {
        match dep.parse::<RuntimeDep>() {
            Ok(dep) => precheck = precheck.with_runtime(dep),
            Err(err) => tracing::debug!(%dep, %err, "runtime dependency not checked"),
        }
    }
    instance.precheck = precheck;
    Ok(instance)
}

/// Waves of entries such that each one's `depends_on` tools come in earlier waves.
pub fn topological_waves(entries: &[Entry]) -> Vec<Vec<Entry>> {
    let deps_of = |e: &Entry| -> BTreeSet<ToolId> {
        e.manifest
            .as_ref()
            .and_then(|m| m.spec.service.daemon.as_ref())
            .map(|d| d.depends_on.iter().cloned().collect())
            .unwrap_or_default()
    };
    let present: BTreeSet<ToolId> = entries.iter().map(|e| e.row.tool_id.clone()).collect();
    let mut remaining: Vec<Entry> = entries.to_vec();
    let mut done: BTreeSet<ToolId> = BTreeSet::new();
    let mut waves = Vec::new();
    while !remaining.is_empty() {
        let (ready, blocked): (Vec<Entry>, Vec<Entry>) = remaining.into_iter().partition(|e| {
            deps_of(e)
                .iter()
                .all(|d| done.contains(d) || !present.contains(d))
        });
        if ready.is_empty() {
            // Cycle: start what is left together; the pre-checks report the problem.
            waves.push(blocked);
            break;
        }
        for e in &ready {
            done.insert(e.row.tool_id.clone());
        }
        waves.push(ready);
        remaining = blocked;
    }
    waves
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::ids::VersionId;
    use forge_core::store::{VersionSource, VersionState};
    use forge_provider::layout::{PathVars, TemplateVars};

    fn manifest(name: &str) -> ToolManifest {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../registry/tools/{name}.yaml")),
        )
        .unwrap();
        match forge_manifest::load_str(&text).unwrap() {
            forge_manifest::Manifest::Tool(t) => t,
            _ => unreachable!(),
        }
    }

    fn setup(name: &str, version: &str) -> (ForgeRoot, ToolManifest, ToolVersionRow, InstanceRow) {
        let root = ForgeRoot::new(r"C:\fb").unwrap();
        let manifest = manifest(name);
        let row = ToolVersionRow {
            id: VersionId::new(),
            tool_id: manifest.metadata.id.clone(),
            version: version.into(),
            canonical_name: version.into(),
            variant: BTreeMap::new(),
            path: format!("C:/fb/bin/{name}/{version}"),
            state: VersionState::Installed,
            source: VersionSource::Registry,
        };
        let instance = InstanceRow::new(manifest.metadata.id.clone(), row.id, "default");
        (root, manifest, row, instance)
    }

    fn vars_for(
        root: &ForgeRoot,
        manifest: &ToolManifest,
        row: &ToolVersionRow,
        instance: &InstanceRow,
        ports: &BTreeMap<String, u16>,
    ) -> RuntimeVars {
        let base = TemplateVars {
            tool: row.tool_id.to_string(),
            version: row.version.clone(),
            series: catalog::series_of(manifest, &row.version),
            canonical_name: row.canonical_name.clone(),
            variant: row.variant.clone(),
            paths: PathVars::new(root, std::path::Path::new(&row.path)),
        };
        let mut vars = RuntimeVars::new(base);
        let mut instance_vars = InstanceVars {
            name: instance.name.clone(),
            data_dir: String::new(),
            ports: ports.clone(),
            config: BTreeMap::new(),
        };
        vars.instance = Some(instance_vars.clone());
        if let Some(data) = manifest
            .spec
            .service
            .daemon
            .as_ref()
            .and_then(|d| d.data.as_ref())
        {
            instance_vars.data_dir = vars.substitute(&data.dir).unwrap();
        }
        for file in &manifest.spec.config.files {
            let path = vars.substitute(&file.path).unwrap();
            instance_vars.config.insert(file.id.clone(), path);
        }
        vars.instance = Some(instance_vars);
        vars
    }

    #[test]
    fn mysql_spec_comes_from_the_manifest() {
        let (root, manifest, row, instance) = setup("mysql", "8.4.6");
        let ports = BTreeMap::from([("main".to_owned(), 3306)]);
        let vars = vars_for(&root, &manifest, &row, &instance, &ports);
        let daemon = manifest.spec.service.daemon.clone().unwrap();
        let spec =
            instance_spec(&root, &manifest, &daemon, &row, &instance, &vars, &ports).unwrap();
        assert_eq!(
            spec.spawn.exe,
            PathBuf::from(r"C:/fb/bin/mysql/8.4.6\bin\mysqld.exe")
        );
        assert_eq!(
            spec.spawn.args,
            [
                "--defaults-file=C:/fb/etc/mysql/default/my.ini",
                "--console"
            ]
        );
        let stop = spec.stop.command.as_ref().unwrap();
        assert!(stop.exe.ends_with(r"bin\mysqladmin.exe"));
        assert!(stop.args.contains(&"--port=3306".to_owned()));
        match &spec.health.as_ref().unwrap().probe {
            Probe::Tcp { addr } => assert_eq!(addr, "127.0.0.1:3306"),
            other => panic!("{other:?}"),
        }
        assert_eq!(spec.precheck.ports, [3306]);
        assert!(spec.precheck.check.is_some());
        assert_eq!(
            spec.logs.file.as_deref(),
            Some(root.logs().join("mysql").join("default.out.log").as_path())
        );
    }

    #[test]
    fn nginx_spec_uses_the_web_ports_and_its_config_file() {
        let (root, manifest, row, instance) = setup("nginx", "1.30.6");
        let ports = BTreeMap::from([("http".to_owned(), 8080), ("https".to_owned(), 8443)]);
        let vars = vars_for(&root, &manifest, &row, &instance, &ports);
        let daemon = manifest.spec.service.daemon.clone().unwrap();
        let spec =
            instance_spec(&root, &manifest, &daemon, &row, &instance, &vars, &ports).unwrap();
        assert_eq!(
            spec.spawn.args,
            [
                "-p",
                "C:/fb/bin/nginx/1.30.6",
                "-c",
                "C:/fb/etc/nginx/nginx.conf"
            ]
        );
        let mut precheck_ports = spec.precheck.ports.clone();
        precheck_ports.sort_unstable();
        assert_eq!(precheck_ports, [8080, 8443]);
        assert!(spec.stop.ctrl_break);
    }

    #[test]
    fn log_folders_come_from_the_manifest_and_stay_inside_the_root() {
        let (root, manifest, row, instance) = setup("mysql", "8.4.6");
        let ports = BTreeMap::from([("main".to_owned(), 3306)]);
        let vars = vars_for(&root, &manifest, &row, &instance, &ports);
        // M1 gate: mysqld does not create `logs/mysql/<instance>/` and refused to start.
        assert_eq!(
            log_dirs_of(&root, &manifest, &vars).unwrap(),
            [PathBuf::from("C:/fb/logs/mysql/default")]
        );

        let mut outside = manifest.clone();
        let logs = &mut outside.spec.service.daemon.as_mut().unwrap().logs;
        let template = logs[0].clone();
        logs.clear();
        for path in [
            "C:/elsewhere/mysql/error.log",
            "{{ paths.logs }}/../../escape/error.log",
            "{{ paths.logs }}/mysql/{{ instance.name }}/slow.log",
            "{{ paths.logs }}/mysql/{{ instance.name }}/general.log",
        ] {
            let mut log = template.clone();
            log.path = path.into();
            logs.push(log);
        }
        assert_eq!(
            log_dirs_of(&root, &outside, &vars).unwrap(),
            [PathBuf::from("C:/fb/logs/mysql/default")],
            "outside the root and `..` are ignored, duplicates once"
        );
    }

    #[test]
    fn waves_follow_depends_on() {
        let (_, mysql, mrow, minst) = setup("mysql", "8.4.6");
        let (_, nginx, nrow, ninst) = setup("nginx", "1.30.6");
        let entry = |m: ToolManifest, v: ToolVersionRow, r: InstanceRow| Entry {
            row: r,
            version: Some(v),
            manifest: Some(Arc::new(m)),
            ports: BTreeMap::new(),
            config_files: BTreeMap::new(),
            vars: None,
            handle: None,
            error: None,
            start_timeout: Duration::from_secs(1),
            stop_worst_case: Duration::from_secs(1),
        };
        let mut nginx_dep = nginx.clone();
        nginx_dep
            .spec
            .service
            .daemon
            .as_mut()
            .unwrap()
            .depends_on
            .push(ToolId::new("mysql").unwrap());
        let entries = vec![entry(nginx_dep, nrow, ninst), entry(mysql, mrow, minst)];
        let waves = topological_waves(&entries);
        assert_eq!(waves.len(), 2);
        assert_eq!(waves[0][0].row.tool_id.as_str(), "mysql");
        assert_eq!(waves[1][0].row.tool_id.as_str(), "nginx");
    }
}
