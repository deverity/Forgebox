//! `AppManager`: the processes of every site, supervised (docs/06 §4.2, docs/03 §7).
//!
//! **Structure.** One actor (`apps`, mailbox + timeouts on every ask) owns the registry:
//! which sites have processes, the [`InstanceHandle`] of each (one `InstanceActor` per
//! process, all under the **Job Object of the site**, itself nested in the daemon's root
//! job) and two flags per process that decide `autostart: on-visit`. The actor only
//! decides and answers at once; everything slow runs outside it, in the caller's task:
//! planning (folders, `spawn_blocking`), port allocation (the store's writer thread),
//! starting and stopping (the instance actors).
//!
//! **Life of a site.** [`AppManager::up`] plans the processes ([`AppPlan::build`]),
//! allocates the `$PORT`s from the `apps` range (idempotent per process: the same port
//! after a restart, so the rendered vhost stays valid), stops what the site already ran,
//! registers one instance per process and starts the `autostart: true` ones, waiting until
//! they listen. [`AppManager::down`] stops them (graceful cascade, then the Job) and
//! forgets the site; [`AppManager::forget`] also frees its ports (site removed).
//!
//! **`autostart: on-visit`** (docs/06 §4.2, §8). The vhost of such a site is rendered with
//! `start_on_visit`: when the app does not answer, the web server returns 502 and the
//! internal location `/_forgebox/starting?host=<host>` is asked. That HTTP endpoint belongs
//! to the daemon (M1-E5); this crate gives it everything behind the request:
//!
//! * [`AppManager::ensure_started`] — idempotent: the **first** call per registration starts
//!   the site's on-visit processes (without waiting; the browser polls through the page),
//!   later calls start nothing (`already`). A process the user stopped is never started by
//!   a visit;
//! * [`AppManager::starting_page`] — calls it and renders `pages/starting.html.j2` (auto
//!   refresh every 2 s; the refresh asks the original URL again, which is proxied normally
//!   once the app listens).
//!
//! The first 502 usually arrives before the process listens, so the page is shown for a
//! few seconds; nothing is retried by Forgebox itself.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use forge_core::actor::{Actor, ActorHandle, ActorRef, ActorSpec, Context, Reply, StopOutcome};
use forge_core::error::{ForgeError, Result};
use forge_core::ids::SiteId;
use forge_core::model::port::{OwnerKind, PortReservations};
use forge_core::paths::ForgeRoot;
use forge_core::runtime::spawn_blocking_timeout;
use forge_manifest::schema::project::ProcessAutostart;
use forge_render::Engine;
use forge_supervisor::{
    InstanceHandle, InstanceState, InstanceStatus, JobTree, QUICK_ASK_TIMEOUT, Supervisor,
};
use futures::future::join_all;
use serde::Serialize;

use super::runtime::RuntimeCatalog;
use super::spec::{
    APP_START_TIMEOUT, APPS_RANGE, AppEnv, AppPlan, AppSite, AppSpec, PortRequest,
    port_request_for, validate_processes,
};
use crate::messages;

/// Budgets of the manager's operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppTimeouts {
    /// A request to the manager's actor (it only touches memory).
    pub ask: Duration,
    /// Planning the processes of a site (folders, executables).
    pub plan: Duration,
    /// One port allocation or reservation.
    pub port: Duration,
    /// A process listening after its start.
    pub start: Duration,
    /// A process gone after its stop (cascade worst case included).
    pub stop: Duration,
}

impl Default for AppTimeouts {
    fn default() -> Self {
        Self {
            ask: Duration::from_secs(5),
            plan: Duration::from_secs(30),
            port: Duration::from_secs(10),
            start: APP_START_TIMEOUT + Duration::from_secs(10),
            stop: Duration::from_secs(30),
        }
    }
}

/// What the actor knows about one process.
#[derive(Debug, Clone)]
pub struct ProcView {
    pub host: String,
    pub name: String,
    pub spec: AppSpec,
    pub handle: InstanceHandle,
    /// The user stopped it: a visit does not start it again.
    pub manually_stopped: bool,
    /// An on-visit start was already dispatched (by a visit or by the user).
    pub visit_started: bool,
}

/// Status of one process.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProcessStatus {
    pub name: String,
    pub port: Option<u16>,
    pub proxy: bool,
    /// `true`, `false` or `on-visit`.
    pub autostart: String,
    /// `nodejs 22.23.4`, when the process runs on a runtime.
    pub runtime: Option<String>,
    pub command: String,
    pub log_file: std::path::PathBuf,
    pub manually_stopped: bool,
    pub visit_started: bool,
    /// State, pid, restarts and health from the instance actor.
    pub instance: InstanceStatus,
}

impl ProcessStatus {
    /// Whether the process is meant to be up: autostart on (or already visited) and not
    /// stopped by the user.
    pub fn wanted(&self) -> bool {
        !self.manually_stopped && (self.autostart == "true" || self.visit_started)
    }
}

/// Status of the processes of one site.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SiteAppsStatus {
    pub site_id: SiteId,
    pub host: String,
    pub processes: Vec<ProcessStatus>,
}

impl SiteAppsStatus {
    /// Every wanted process is `Running`.
    pub fn ready(&self) -> bool {
        self.processes
            .iter()
            .filter(|p| p.wanted())
            .all(|p| p.instance.state == InstanceState::Running)
    }

    /// A wanted process ended in `Failed`.
    pub fn failed(&self) -> Option<&ProcessStatus> {
        self.processes
            .iter()
            .filter(|p| p.wanted())
            .find(|p| matches!(p.instance.state, InstanceState::Failed { .. }))
    }

    /// The status of process `name`.
    pub fn process(&self, name: &str) -> Option<&ProcessStatus> {
        self.processes.iter().find(|p| p.name == name)
    }
}

/// Answer of [`AppManager::ensure_started`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnsureOutcome {
    pub host: String,
    /// Processes this call started (the first call of a registration).
    pub started: Vec<String>,
    /// On-visit processes that were started before (or are stopped by the user).
    pub already: Vec<String>,
}

/// What the daemon returns for `/_forgebox/starting?host=...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartingPage {
    /// 503 while starting, 500 when a process failed.
    pub status: u16,
    /// `Retry-After` seconds (0 = none).
    pub retry_after_secs: u32,
    pub html: String,
    /// Every wanted process listens: the original URL works now.
    pub ready: bool,
    /// Failure text when a wanted process ended in `Failed`.
    pub failure: Option<String>,
}

/// Seconds between refreshes of the starting page.
pub const STARTING_REFRESH_SECS: u32 = 2;

/// Marks set by the user's commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Start,
    Stop,
}

enum AppMsg {
    Register {
        host: String,
        aliases: Vec<String>,
        specs: Vec<AppSpec>,
        reply: Reply<Result<Vec<ProcView>>>,
    },
    Take {
        host: String,
        reply: Reply<Vec<ProcView>>,
    },
    View {
        host: Option<String>,
        reply: Reply<Vec<ProcView>>,
    },
    Mark {
        host: String,
        process: Option<String>,
        mark: Mark,
        reply: Reply<Result<Vec<ProcView>>>,
    },
    Ensure {
        host: String,
        reply: Reply<Result<(Vec<ProcView>, Vec<ProcView>)>>,
    },
}

struct Entry {
    spec: AppSpec,
    handle: InstanceHandle,
    manually_stopped: bool,
    visit_started: bool,
}

struct SiteEntry {
    aliases: Vec<String>,
    /// The site's Job Object (instance jobs are nested in it).
    _jobs: JobTree,
    processes: BTreeMap<String, Entry>,
}

struct AppActor {
    supervisor: Supervisor,
    sites: BTreeMap<String, SiteEntry>,
    /// Alias → primary host.
    aliases: HashMap<String, String>,
}

impl AppActor {
    fn new(supervisor: Supervisor) -> Self {
        Self {
            supervisor,
            sites: BTreeMap::new(),
            aliases: HashMap::new(),
        }
    }

    fn primary<'a>(&'a self, host: &'a str) -> &'a str {
        self.aliases.get(host).map_or(host, String::as_str)
    }

    fn views(&self, host: &str, entry: &SiteEntry) -> Vec<ProcView> {
        entry
            .processes
            .iter()
            .map(|(name, p)| ProcView {
                host: host.to_owned(),
                name: name.clone(),
                spec: p.spec.clone(),
                handle: p.handle.clone(),
                manually_stopped: p.manually_stopped,
                visit_started: p.visit_started,
            })
            .collect()
    }

    fn register(
        &mut self,
        host: String,
        aliases: Vec<String>,
        specs: Vec<AppSpec>,
    ) -> Result<Vec<ProcView>> {
        for name in std::iter::once(&host).chain(&aliases) {
            let taken = self.sites.contains_key(name) || self.aliases.contains_key(name);
            if taken {
                return Err(ForgeError::Conflict(format!(
                    "{name} already has processes registered; bring the site down first"
                )));
            }
        }
        let jobs = self.supervisor.jobs().nested()?;
        let supervisor = self.supervisor.clone().with_jobs(jobs.clone());
        let mut processes = BTreeMap::new();
        for spec in specs {
            let handle = supervisor.spawn_instance(spec.instance_spec()?)?;
            processes.insert(
                spec.name.clone(),
                Entry {
                    spec,
                    handle,
                    manually_stopped: false,
                    visit_started: false,
                },
            );
        }
        let entry = SiteEntry {
            aliases: aliases.clone(),
            _jobs: jobs,
            processes,
        };
        let views = self.views(&host, &entry);
        for alias in aliases {
            self.aliases.insert(alias, host.clone());
        }
        self.sites.insert(host, entry);
        Ok(views)
    }

    fn take(&mut self, host: &str) -> Vec<ProcView> {
        let primary = self.primary(host).to_owned();
        let Some(entry) = self.sites.remove(&primary) else {
            return Vec::new();
        };
        for alias in &entry.aliases {
            self.aliases.remove(alias);
        }
        self.views(&primary, &entry)
    }

    fn view(&self, host: Option<&str>) -> Vec<ProcView> {
        match host {
            Some(host) => {
                let primary = self.primary(host);
                self.sites
                    .get(primary)
                    .map(|entry| self.views(primary, entry))
                    .unwrap_or_default()
            }
            None => self
                .sites
                .iter()
                .flat_map(|(host, entry)| self.views(host, entry))
                .collect(),
        }
    }

    fn mark(&mut self, host: &str, process: Option<&str>, mark: Mark) -> Result<Vec<ProcView>> {
        let primary = self.primary(host).to_owned();
        let entry = self
            .sites
            .get_mut(&primary)
            .ok_or_else(|| ForgeError::NotFound(format!("no processes registered for {host}")))?;
        if let Some(name) = process {
            if !entry.processes.contains_key(name) {
                return Err(ForgeError::NotFound(format!(
                    "process '{name}' of {primary}"
                )));
            }
        }
        for (name, p) in &mut entry.processes {
            if process.is_some_and(|wanted| wanted != name) {
                continue;
            }
            match mark {
                Mark::Start => {
                    p.manually_stopped = false;
                    p.visit_started = true;
                }
                Mark::Stop => p.manually_stopped = true,
            }
        }
        let entry = &self.sites[&primary];
        let mut views = self.views(&primary, entry);
        views.retain(|v| process.is_none_or(|wanted| wanted == v.name));
        Ok(views)
    }

    /// The first call per registration takes every on-visit process that was not started
    /// and is not stopped by the user; the others are reported as already handled.
    fn ensure(&mut self, host: &str) -> Result<(Vec<ProcView>, Vec<ProcView>)> {
        let primary = self.primary(host).to_owned();
        let entry = self
            .sites
            .get_mut(&primary)
            .ok_or_else(|| ForgeError::NotFound(format!("no processes registered for {host}")))?;
        let mut started = Vec::new();
        let mut already = Vec::new();
        for (name, p) in &mut entry.processes {
            if p.spec.autostart != ProcessAutostart::OnVisit {
                continue;
            }
            let view = ProcView {
                host: primary.clone(),
                name: name.clone(),
                spec: p.spec.clone(),
                handle: p.handle.clone(),
                manually_stopped: p.manually_stopped,
                visit_started: p.visit_started,
            };
            if p.visit_started || p.manually_stopped {
                already.push(view);
            } else {
                p.visit_started = true;
                started.push(ProcView {
                    visit_started: true,
                    ..view
                });
            }
        }
        Ok((started, already))
    }
}

impl Actor for AppActor {
    type Msg = AppMsg;

    async fn handle(&mut self, msg: AppMsg, _ctx: &mut Context<Self>) {
        // A dropped reply means the caller gave up; nothing to do.
        match msg {
            AppMsg::Register {
                host,
                aliases,
                specs,
                reply,
            } => {
                let _ = reply.send(self.register(host, aliases, specs));
            }
            AppMsg::Take { host, reply } => {
                let _ = reply.send(self.take(&host));
            }
            AppMsg::View { host, reply } => {
                let _ = reply.send(self.view(host.as_deref()));
            }
            AppMsg::Mark {
                host,
                process,
                mark,
                reply,
            } => {
                let _ = reply.send(self.mark(&host, process.as_deref(), mark));
            }
            AppMsg::Ensure { host, reply } => {
                let _ = reply.send(self.ensure(&host));
            }
        }
    }
}

struct Inner {
    actor: ActorHandle<AppMsg>,
    env: AppEnv,
    ports: Arc<dyn PortReservations>,
    engine: Engine,
    timeouts: AppTimeouts,
}

/// The processes of the sites, supervised. Cheap to clone.
#[derive(Clone)]
pub struct AppManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for AppManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppManager")
            .field("actor", &self.inner.actor)
            .finish_non_exhaustive()
    }
}

impl AppManager {
    /// Manager running its actor and the instance actors under `supervisor`, resolving
    /// runtimes with `catalog`, allocating `$PORT`s from `ports` and rendering the
    /// starting page with `engine`.
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn new(
        supervisor: Supervisor,
        root: ForgeRoot,
        catalog: RuntimeCatalog,
        ports: Arc<dyn PortReservations>,
        engine: Engine,
    ) -> Self {
        let env = AppEnv {
            root,
            catalog,
            system_path: supervisor.jobs().processes().system_path(),
        };
        let actors = supervisor.actors().clone();
        let actor = actors.spawn(ActorSpec::new("apps"), {
            let supervisor = supervisor.clone();
            move || AppActor::new(supervisor.clone())
        });
        Self {
            inner: Arc::new(Inner {
                actor,
                env,
                ports,
                engine,
                timeouts: AppTimeouts::default(),
            }),
        }
    }

    /// Replaces the budgets (tests shorten them).
    ///
    /// # Panics
    /// If the manager was already cloned.
    pub fn with_timeouts(mut self, timeouts: AppTimeouts) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("with_timeouts is called before the manager is shared")
            .timeouts = timeouts;
        self
    }

    fn actor(&self) -> &ActorRef<AppMsg> {
        self.inner.actor.actor_ref()
    }

    async fn ask<R>(&self, make: impl FnOnce(Reply<R>) -> AppMsg) -> Result<R> {
        Ok(self.actor().ask(make, self.inner.timeouts.ask).await?)
    }

    // ---------------------------------------------------------------- ports

    /// `$PORT` of every process of `site` that listens, allocated (or read back, the
    /// allocation is idempotent per process) from the `apps` range. This is what the
    /// vhost needs ([`crate::drivers::VhostContext::app_ports`]), available before the
    /// processes exist.
    pub async fn allocate_ports(&self, site: &AppSite) -> Result<BTreeMap<String, u16>> {
        validate_processes(&site.host, &site.processes)?;
        let mut ports = BTreeMap::new();
        for (name, spec) in &site.processes {
            let request = port_request_for(&site.env, spec);
            if let Some(port) = self.reserve(site.id, name, request).await? {
                ports.insert(name.clone(), port);
            }
        }
        Ok(ports)
    }

    async fn reserve(&self, site: SiteId, name: &str, request: PortRequest) -> Result<Option<u16>> {
        let owner =
            forge_core::model::port::ReservationOwner::new(OwnerKind::App, site.to_string(), name)?;
        let budget = self.inner.timeouts.port;
        let timed = |what: &'static str| ForgeError::Timeout {
            operation: what.to_owned(),
            after: budget,
        };
        match request {
            PortRequest::None => Ok(None),
            PortRequest::Auto => {
                let reservation =
                    tokio::time::timeout(budget, self.inner.ports.allocate(APPS_RANGE, owner))
                        .await
                        .map_err(|_| timed("allocating the port of a process"))??;
                Ok(Some(reservation.port))
            }
            PortRequest::Fixed(port) => {
                let reservation = tokio::time::timeout(
                    budget,
                    self.inner.ports.reserve(port, Some(APPS_RANGE), owner),
                )
                .await
                .map_err(|_| timed("reserving the port of a process"))??;
                Ok(Some(reservation.port))
            }
        }
    }

    /// Plans every process (blocking work off the async workers) and gives each its port.
    async fn prepare(&self, site: &AppSite) -> Result<Vec<AppSpec>> {
        validate_processes(&site.host, &site.processes)?;
        let env = self.inner.env.clone();
        let planned = site.clone();
        let plans = spawn_blocking_timeout(
            "planning the processes of a site",
            self.inner.timeouts.plan,
            move || {
                planned
                    .processes
                    .iter()
                    .map(|(name, spec)| AppPlan::build(&planned, name, spec, &env))
                    .collect::<Result<Vec<_>>>()
            },
        )
        .await?;
        let mut specs = Vec::with_capacity(plans.len());
        for plan in plans {
            let port = self
                .reserve(plan.site_id, &plan.name, plan.port_request())
                .await?;
            specs.push(plan.finish(port)?);
        }
        Ok(specs)
    }

    // ------------------------------------------------------------ life cycle

    /// Brings the processes of `site` up: replaces what the site ran, registers one
    /// instance per process and starts the `autostart: true` ones, waiting (at most
    /// [`AppTimeouts::start`]) until they listen. A process that fails to start does not
    /// fail the call: its `Failed(reason)` is in the returned status.
    pub async fn up(&self, site: &AppSite) -> Result<SiteAppsStatus> {
        let specs = self.prepare(site).await?;
        self.down(&site.host).await?;
        let views = self
            .ask(|reply| AppMsg::Register {
                host: site.host.clone(),
                aliases: site.aliases.clone(),
                specs,
                reply,
            })
            .await??;
        let starts = views
            .iter()
            .filter(|v| v.spec.autostart == ProcessAutostart::Yes)
            .map(|v| self.start_instance(v));
        for result in join_all(starts).await {
            if let Err((name, error)) = result {
                tracing::warn!(host = %site.host, process = %name, %error, "process did not start");
            }
        }
        self.status(&site.host).await
    }

    /// Stops every process of the site (graceful cascade, then the Job) and forgets it.
    /// Returns the names that were registered; unknown hosts are not an error.
    pub async fn down(&self, host: &str) -> Result<Vec<String>> {
        let views = self
            .ask(|reply| AppMsg::Take {
                host: host.to_owned(),
                reply,
            })
            .await?;
        let names = views.iter().map(|v| v.name.clone()).collect();
        let stops = views.into_iter().map(|v| {
            let timeout = self.inner.timeouts.stop;
            async move {
                if let Err(error) = v.handle.stop(timeout).await {
                    tracing::warn!(host = %v.host, process = %v.name, %error, "stopping a process");
                }
                let _ = v.handle.shutdown().await;
            }
        });
        join_all(stops).await;
        Ok(names)
    }

    /// [`AppManager::down`] plus freeing the ports of the site (the site was removed).
    pub async fn forget(&self, site_id: SiteId, host: &str) -> Result<()> {
        self.down(host).await?;
        let id = site_id.to_string();
        tokio::time::timeout(
            self.inner.timeouts.port,
            self.inner.ports.release_owner(OwnerKind::App, &id),
        )
        .await
        .map_err(|_| ForgeError::Timeout {
            operation: "releasing the ports of a site".to_owned(),
            after: self.inner.timeouts.port,
        })??;
        Ok(())
    }

    /// Stops every site (daemon shutdown) and the manager's own actor.
    pub async fn shutdown(&self) -> StopOutcome {
        let views = self
            .ask(|reply| AppMsg::View { host: None, reply })
            .await
            .unwrap_or_default();
        let mut hosts: Vec<String> = views.into_iter().map(|v| v.host).collect();
        hosts.dedup();
        for host in hosts {
            let _ = self.down(&host).await;
        }
        self.inner.actor.stop().await
    }

    // ------------------------------------------------- per-process commands

    async fn start_instance(
        &self,
        view: &ProcView,
    ) -> std::result::Result<u32, (String, ForgeError)> {
        view.handle
            .start(self.inner.timeouts.start)
            .await
            .map_err(|e| (view.name.clone(), e))
    }

    /// Starts one process (`None`: all of the site) and waits until it listens.
    pub async fn start_process(&self, host: &str, process: Option<&str>) -> Result<SiteAppsStatus> {
        let views = self.mark(host, process, Mark::Start).await?;
        let mut first_error = None;
        for result in join_all(views.iter().map(|v| self.start_instance(v))).await {
            if let Err((_, error)) = result {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => self.status(host).await,
        }
    }

    /// Stops one process (`None`: all of the site). A visit does not start it again.
    pub async fn stop_process(&self, host: &str, process: Option<&str>) -> Result<SiteAppsStatus> {
        let views = self.mark(host, process, Mark::Stop).await?;
        let timeout = self.inner.timeouts.stop;
        let results = join_all(views.iter().map(|v| v.handle.stop(timeout))).await;
        if let Some(error) = results.into_iter().find_map(std::result::Result::err) {
            return Err(error);
        }
        self.status(host).await
    }

    /// Restarts one process (`None`: all of the site) and waits until it listens.
    pub async fn restart_process(
        &self,
        host: &str,
        process: Option<&str>,
    ) -> Result<SiteAppsStatus> {
        let views = self.mark(host, process, Mark::Start).await?;
        let timeout = self.inner.timeouts.start;
        let results = join_all(views.iter().map(|v| v.handle.restart(timeout))).await;
        if let Some(error) = results.into_iter().find_map(std::result::Result::err) {
            return Err(error);
        }
        self.status(host).await
    }

    async fn mark(&self, host: &str, process: Option<&str>, mark: Mark) -> Result<Vec<ProcView>> {
        self.ask(|reply| AppMsg::Mark {
            host: host.to_owned(),
            process: process.map(str::to_owned),
            mark,
            reply,
        })
        .await?
    }

    // ----------------------------------------------------------------- visits

    /// The hook behind `autostart: on-visit`: called when the web server answered 502 for
    /// `host`. The first call of a registration starts the site's on-visit processes —
    /// **without waiting** for them — and later calls start nothing. `NotFound` when the
    /// host has no processes registered. The HTTP trigger (`/_forgebox/starting`) is the
    /// daemon's; see [`AppManager::starting_page`].
    pub async fn ensure_started(&self, host: &str) -> Result<EnsureOutcome> {
        let (started, already) = self
            .ask(|reply| AppMsg::Ensure {
                host: host.to_owned(),
                reply,
            })
            .await??;
        let primary = started
            .first()
            .or(already.first())
            .map_or_else(|| host.to_owned(), |v| v.host.clone());
        for view in &started {
            let manager = self.clone();
            let view = view.clone();
            tokio::spawn(async move {
                if let Err((name, error)) = manager.start_instance(&view).await {
                    tracing::warn!(host = %view.host, process = %name, %error, "on-visit start failed");
                }
            });
        }
        Ok(EnsureOutcome {
            host: primary,
            started: started.into_iter().map(|v| v.name).collect(),
            already: already.into_iter().map(|v| v.name).collect(),
        })
    }

    /// Answer of `/_forgebox/starting?host=<host>`: triggers [`AppManager::ensure_started`]
    /// and renders the page in `lang` (`pt-BR`/`en-US`, from `Accept-Language`).
    pub async fn starting_page(&self, host: &str, lang: &str) -> Result<StartingPage> {
        self.ensure_started(host).await?;
        let status = self.status(host).await?;
        let failure = status.failed().map(|p| {
            let reason = match &p.instance.state {
                InstanceState::Failed { reason } => {
                    serde_json::to_string(reason).unwrap_or_default()
                }
                _ => String::new(),
            };
            messages::starting_failed(&p.name, &reason).1
        });
        let ready = status.ready();
        let state = status
            .processes
            .iter()
            .filter(|p| p.wanted())
            .map(|p| p.instance.state.name())
            .next()
            .unwrap_or("starting");
        let refresh = (failure.is_none()).then_some(STARTING_REFRESH_SECS);
        let texts = messages::starting_texts(lang);
        let html = self.inner.engine.render(
            "pages/starting.html.j2",
            &serde_json::json!({
                "host": host,
                "state": state,
                "detail": failure,
                "refresh_secs": refresh,
                "t": texts,
            }),
        )?;
        Ok(StartingPage {
            status: if failure.is_some() { 500 } else { 503 },
            retry_after_secs: refresh.unwrap_or(0),
            html,
            ready,
            failure,
        })
    }

    // ----------------------------------------------------------------- status

    async fn statuses(&self, views: Vec<ProcView>) -> Result<Vec<(ProcView, InstanceStatus)>> {
        let asks = views.into_iter().map(|view| async move {
            let status = view.handle.status(QUICK_ASK_TIMEOUT).await?;
            Ok::<_, ForgeError>((view, status))
        });
        join_all(asks).await.into_iter().collect()
    }

    fn process_status(view: ProcView, instance: InstanceStatus) -> ProcessStatus {
        ProcessStatus {
            name: view.name,
            port: view.spec.port,
            proxy: view.spec.proxy,
            autostart: match view.spec.autostart {
                ProcessAutostart::Yes => "true",
                ProcessAutostart::No => "false",
                ProcessAutostart::OnVisit => "on-visit",
            }
            .to_owned(),
            runtime: view.spec.runtime.as_ref().map(|r| r.label()),
            command: view.spec.command,
            log_file: view.spec.log_file,
            manually_stopped: view.manually_stopped,
            visit_started: view.visit_started,
            instance,
        }
    }

    /// Status of the processes of `host` (`NotFound` when none are registered).
    pub async fn status(&self, host: &str) -> Result<SiteAppsStatus> {
        let views = self
            .ask(|reply| AppMsg::View {
                host: Some(host.to_owned()),
                reply,
            })
            .await?;
        let Some(first) = views.first() else {
            return Err(ForgeError::NotFound(format!(
                "no processes registered for {host}"
            )));
        };
        let (site_id, primary) = (first.spec.site_id, first.host.clone());
        let mut processes: Vec<ProcessStatus> = self
            .statuses(views)
            .await?
            .into_iter()
            .map(|(view, instance)| Self::process_status(view, instance))
            .collect();
        processes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(SiteAppsStatus {
            site_id,
            host: primary,
            processes,
        })
    }

    /// Hosts that have processes registered, sorted.
    pub async fn hosts(&self) -> Result<Vec<String>> {
        let views = self.ask(|reply| AppMsg::View { host: None, reply }).await?;
        let mut hosts: Vec<String> = views.into_iter().map(|v| v.host).collect();
        hosts.sort();
        hosts.dedup();
        Ok(hosts)
    }

    /// Waits until every wanted process of `host` is `Running`, one failed, or `timeout`
    /// passes; returns the last status (check [`SiteAppsStatus::ready`]).
    pub async fn wait_ready(&self, host: &str, timeout: Duration) -> Result<SiteAppsStatus> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let status = self.status(host).await?;
            let done = status.ready() && status.processes.iter().any(|p| p.wanted())
                || status.failed().is_some();
            if done || tokio::time::Instant::now() >= deadline {
                return Ok(status);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
