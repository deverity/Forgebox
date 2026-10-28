//! Boot sequence of `forgeboxd` (docs/01 §5.1), in order:
//!
//! 1. mutex (taken by `main` before anything else) → `ForgeRoot::init` (case-sensitive
//!    flag on `www/ etc/ data/ logs/ tmp/ backups/`) → `state/forgebox.db` opened and
//!    migrated → event bus seeded with the stored `seq` and its persistence task;
//! 2. `forgebox.toml` and the manifest index (local cache, bounded wait; refreshes run
//!    as jobs in the background);
//! 3. `bin/` reconciled with the database (`discover` + `import_discovered`, bounded);
//! 4. root Job Object (`JobTree`), supervisor, port allocator, layout/download/update
//!    engines, `tls` actor, sites;
//! 5. DNS server when `[dns] enabled`, plus a read-only check of the NRPT/`hosts` policy;
//! 6. instances with `autostart` (or left running) in `depends_on` order, web servers
//!    excepted;
//! 7. PHP pools for the versions referenced by enabled sites;
//! 8. web server configuration rendered (managed blocks, drift kept) and web servers
//!    started or reloaded;
//! 9. API opened on the local transport and `daemon.ready` published.
//!
//! Every step that can wait on the outside world is bounded; a step that fails without
//! making the daemon useless (registry offline, DNS port taken, a service that does not
//! start) is logged and the boot goes on.

use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::Timestamp;
use forge_core::actor::ActorSupervisor;
use forge_core::config::ForgeConfig;
use forge_core::error::{ForgeError, Result};
use forge_core::event::{EventBus, EventBusOptions, EventKind};
use forge_core::helper::{HelperClient, HelperPolicy};
use forge_core::jobs::JobRegistry;
use forge_core::paths::{ForgeRoot, InitReport, posix_string};
use forge_core::platform::{self, Platform};
use forge_core::store::Store;
use forge_ipc::api::{API_VERSION, DaemonReadyPayload, FsCaseSensitivityPayload};
use forge_ipc::auth::{IpcToken, TokenAuthenticator};
use forge_ipc::server::{EventSource, FsScope};
use forge_ipc::{Server, ServerConfig};
use forge_net::dns::{DnsServer, DnsServerConfig, ZoneSet};
use forge_net::policy::DnsPolicyManager;
use forge_net::ports::PortAllocator;
use forge_provider::download::{DownloadConfig, DownloadManager};
use forge_provider::layout::Layout;
use forge_provider::provider::{ProviderConfig, ProviderRegistry};
use forge_provider::update::{DownloadFetcher, UpdateConfig, UpdateDeps, UpdateEngine};
use forge_registry::{Registry, RegistryConfig, RegistrySource, ServiceOptions};
use forge_render::Engine;
use forge_sites::apps::RuntimeCatalog;
use forge_sites::{
    AppManager, ParkedConfig, ParkedDir, ParkedSync, ParkedWatcher, SiteRules, SiteService,
};
use forge_supervisor::{JobTree, Supervisor};
use forge_tls::{TlsConfig, TlsManager};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::args::Args;
use crate::catalog;
use crate::daemon::{Daemon, Inner, Shutdown, prefs_of};
use crate::event_log::EventLog;
use crate::instance_lock::InstanceLock;
use crate::pools::{DaemonPoolSwitcher, PoolManager};
use crate::render::DaemonRenderer;
use crate::scheduler::Scheduler;
use crate::services::ServiceManager;
use crate::update_feed::UpdateSource;
use crate::web::{LateListener, WebManager};

/// Name of the registry added on first boot.
pub const OFFICIAL_REGISTRY: &str = "official";
/// Where the official registry lives (raw `index.yaml` of the repository).
pub const OFFICIAL_REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/deverity/Forgebox/main/registry/index.yaml";
/// How long the boot waits for the cached catalog before going on without it.
const CATALOG_WAIT: Duration = Duration::from_secs(10);
/// How long the boot waits for the `bin/` scan.
const DISCOVER_WAIT: Duration = Duration::from_secs(20);
/// Budget of the read-only DNS policy check.
const POLICY_CHECK: Duration = Duration::from_secs(5);

/// Everything `main` needs to run and stop the daemon.
pub struct Booted {
    pub daemon: Daemon,
    pub store: Store,
    pub event_log: EventLog,
    pub server_task: JoinHandle<()>,
    pub scheduler: Scheduler,
    pub parked: Option<ParkedWatcher>,
    pub boot_ms: u128,
}

fn blocking_err(e: tokio::task::JoinError) -> ForgeError {
    ForgeError::Internal(format!("boot task failed: {e}"))
}

/// Step 1 (after the mutex and the root folders): store and the stored `seq`.
async fn open_store(root: &ForgeRoot) -> Result<(Store, u64)> {
    let root = root.clone();
    tokio::task::spawn_blocking(move || {
        let store = Store::open(&root.db())?;
        let max_seq = store.events().max_seq()?;
        let schema = store.schema_version()?;
        tracing::info!(root = %root, schema, last_seq = max_seq, "store opened");
        Ok((store, max_seq))
    })
    .await
    .map_err(blocking_err)?
}

fn publish_case_report(bus: &EventBus, report: &InitReport) {
    if let Some(reason) = &report.case_sensitive_unavailable {
        let payload = FsCaseSensitivityPayload {
            v: 1,
            dir: String::new(),
            site_id: None,
            reason: Some(reason.clone()),
            reason_code: Some("unsupported".into()),
        };
        bus.publish(
            EventKind::FsCaseSensitivityUnavailable,
            serde_json::to_value(payload).unwrap_or_default(),
        );
    }
    for (dir, reason) in &report.case_sensitive_failed {
        let payload = FsCaseSensitivityPayload {
            v: 1,
            dir: posix_string(dir),
            site_id: None,
            reason: Some(reason.clone()),
            reason_code: Some("error".into()),
        };
        bus.publish(
            EventKind::FsCaseSensitivityUnavailable,
            serde_json::to_value(payload).unwrap_or_default(),
        );
    }
    for dir in report
        .created
        .iter()
        .filter(|d| report.case_sensitive_applied.contains(d))
    {
        let payload = FsCaseSensitivityPayload {
            v: 1,
            dir: posix_string(dir),
            site_id: None,
            reason: None,
            reason_code: None,
        };
        bus.publish(
            EventKind::FsCaseSensitivityApplied,
            serde_json::to_value(payload).unwrap_or_default(),
        );
    }
}

/// The official registry source: `--registry` / `FORGEBOX_OFFICIAL_REGISTRY` (folder or
/// URL), else [`OFFICIAL_REGISTRY_URL`].
fn official_source(args: &Args) -> Result<RegistrySource> {
    match &args.registry {
        Some(source) => RegistrySource::parse(source),
        None => RegistrySource::parse(OFFICIAL_REGISTRY_URL),
    }
}

/// Step 2: configured registries (the official one added on first boot), catalog
/// loaded from the cache within [`CATALOG_WAIT`].
async fn load_catalog(registry: &Registry, args: &Args) -> Result<()> {
    let restored = registry.restore().await?;
    if restored == 0 {
        let config = RegistryConfig::official(
            OFFICIAL_REGISTRY,
            official_source(args)?,
            forge_registry::index::DEV_PUBLIC_KEY,
        );
        registry.add(config).await?;
        tracing::info!(source = %official_source(args)?.location(), "official registry added");
    }
    let loader = registry.clone();
    let task = tokio::spawn(async move { loader.load_all().await });
    match tokio::time::timeout(CATALOG_WAIT, task).await {
        Ok(Ok(results)) => {
            for (name, result) in results {
                match result {
                    Ok(report) => {
                        tracing::info!(registry = %name, tools = report.tools, source = ?report.source, "registry loaded")
                    }
                    Err(err) => tracing::warn!(registry = %name, %err, "registry not loaded"),
                }
            }
        }
        Ok(Err(e)) => tracing::warn!(%e, "registry load task failed"),
        Err(_) => tracing::warn!(
            ?CATALOG_WAIT,
            "registry still loading; booting without the full catalog"
        ),
    }
    Ok(())
}

/// Step 3: loose folders of `bin/` registered (only versions confirmed by the tool).
async fn reconcile_bin(root: &ForgeRoot, registry: &Registry, store: &Store) {
    let manifests: Vec<forge_manifest::schema::tool::ToolManifest> = catalog::tools(registry)
        .into_iter()
        .map(|t| (*t.manifest).clone())
        .collect();
    if manifests.is_empty() {
        return;
    }
    let (root, writer) = (root.clone(), store.writer().clone());
    let task = tokio::spawn(async move {
        let found = forge_registry::discover(&root, &manifests).await;
        forge_registry::import_discovered(&writer, &found).await
    });
    match tokio::time::timeout(DISCOVER_WAIT, task).await {
        Ok(Ok(Ok(report))) => tracing::info!(?report, "bin/ reconciled"),
        Ok(Ok(Err(err))) => tracing::warn!(%err, "bin/ scan failed"),
        Ok(Err(e)) => tracing::warn!(%e, "bin/ scan task failed"),
        Err(_) => tracing::warn!(?DISCOVER_WAIT, "bin/ scan still running; continuing"),
    }
}

/// Step 5: the resolver (when enabled) and a read-only look at the OS policy.
async fn start_dns(dns: &DnsServer, policy: &DnsPolicyManager, config: &ForgeConfig) {
    if !config.dns.enabled {
        tracing::info!("dns: disabled in forgebox.toml");
        return;
    }
    let nameserver = match dns.start().await {
        Ok(addr) => {
            tracing::info!(%addr, "dns: resolver listening");
            (addr.port() == 53).then_some(addr.ip())
        }
        Err(err) => {
            tracing::warn!(%err, "dns: resolver did not start");
            None
        }
    };
    // The hosts fallback chosen by an earlier `forge dns enable` (ADR-0033): armed again
    // before the first status, so site changes keep reaching hosts even when no block
    // was written yet. The scheduler syncs the block with the sites once at start.
    if let Err(err) = policy.restore().await {
        tracing::warn!(%err, "dns: saved hosts mode not readable");
    }
    match tokio::time::timeout(POLICY_CHECK, policy.status(&config.tlds(), nameserver)).await {
        Ok(Ok(states)) => {
            for state in states {
                tracing::info!(tld = %state.tld, policy = ?state.state, mode = ?state.state.mode(), reason = ?state.reason, probe_ok = ?state.probe.as_ref().map(|p| p.ok), "dns: policy");
            }
        }
        Ok(Err(err)) => tracing::warn!(%err, "dns: policy not readable"),
        Err(_) => tracing::warn!("dns: policy check timed out"),
    }
}

/// Collaborators a test replaces; `main` boots with the defaults (the real OS, the real
/// update engine).
#[derive(Default)]
pub struct BootOptions {
    /// OS facts and actions (fakes for the port table, the shell opener...).
    pub platform: Option<Platform>,
    /// Answers the scheduler's update questions (a fake with no network).
    pub update_source: Option<Arc<dyn UpdateSource>>,
}

/// Runs the whole sequence. `lock` is the instance mutex taken by `main`, which also
/// prepared the root folders (`ForgeRoot::init`, reported in `init_report`) and read
/// `config`.
pub async fn boot(
    args: Args,
    root: ForgeRoot,
    lock: &InstanceLock,
    config: ForgeConfig,
    init_report: InitReport,
) -> Result<Booted> {
    boot_with(
        args,
        root,
        lock,
        config,
        init_report,
        BootOptions::default(),
    )
    .await
}

/// [`boot`] with some collaborators replaced ([`BootOptions`]).
pub async fn boot_with(
    args: Args,
    root: ForgeRoot,
    lock: &InstanceLock,
    config: ForgeConfig,
    init_report: InitReport,
    options: BootOptions,
) -> Result<Booted> {
    let started = Instant::now();
    let started_at = Timestamp::now();

    // 1. Store, events.
    let (store, max_seq) = open_store(&root).await?;
    let bus = EventBus::with_options(EventBusOptions {
        next_seq: max_seq + 1,
        ..EventBusOptions::default()
    });
    let event_log = EventLog::spawn(&bus, store.writer().clone());
    publish_case_report(&bus, &init_report);
    let jobs = JobRegistry::new(bus.clone());
    let actors = ActorSupervisor::new(bus.clone());
    let platform: Platform = options.platform.unwrap_or_else(platform::current);
    let (config_tx, config_rx) = watch::channel(Arc::new(config.clone()));
    let config_fn: Arc<dyn Fn() -> Arc<ForgeConfig> + Send + Sync> = {
        let rx = config_rx.clone();
        Arc::new(move || Arc::clone(&rx.borrow()))
    };
    let prefs_fn: Arc<dyn Fn() -> std::collections::BTreeMap<String, String> + Send + Sync> = {
        let rx = config_rx;
        Arc::new(move || prefs_of(&rx.borrow()))
    };

    // 2. Catalog.
    let registry = Registry::new(
        root.clone(),
        &store,
        jobs.clone(),
        ServiceOptions::default(),
    )?;
    load_catalog(&registry, &args).await?;

    // 3. bin/.
    reconcile_bin(&root, &registry, &store).await;

    // 4. Job Object, engines, actors.
    let job_tree = JobTree::new(Arc::clone(platform.processes()))?;
    let supervisor = Supervisor::new(job_tree.clone(), actors.clone())
        .with_port_table(Arc::clone(platform.port_table()))
        .with_runtime_deps(Arc::clone(platform.runtime_deps()))
        .with_process_stats(Arc::clone(platform.process_stats()));
    let ports = PortAllocator::new(
        &store,
        Arc::clone(platform.port_table()),
        config.ports.clone(),
    );
    let engine = Engine::for_root(&root);
    let renderer = Arc::new(DaemonRenderer::new(
        root.clone(),
        engine.clone(),
        registry.clone(),
        Arc::clone(&config_fn),
    ));
    let layout = Layout::new(
        root.clone(),
        platform.clone(),
        store.writer().clone(),
        store.reader().clone(),
        jobs.clone(),
        renderer,
    );
    let downloads = DownloadManager::spawn(
        &actors,
        jobs.clone(),
        DownloadConfig::new(root.tmp_downloads()),
    )?;
    let providers = Arc::new(ProviderRegistry::new(ProviderConfig::new(
        root.state_registry().join("cache"),
    ))?);
    let helper_policy = HelperPolicy::from_config(&root, &config)?;
    let helper = HelperClient::for_root(&root, helper_policy);
    let tls = TlsManager::spawn(
        &actors,
        &root,
        TlsConfig::for_root(&root, &config),
        platform.shared_file_acl(),
        platform::trust_store(Some(helper.clone())),
    );
    let sites = SiteService::new(
        store.writer().clone(),
        store.reader().clone(),
        bus.clone(),
        SiteRules::new(config.tlds()),
    );
    let network_cost = platform::network_cost();
    let case = forge_core::fs::case::CaseConversions::new(
        jobs.clone(),
        platform.shared_case_sensitive_fs(),
    );
    let services = ServiceManager::new(
        root.clone(),
        supervisor.clone(),
        ports.clone(),
        layout.clone(),
        registry.clone(),
        store.reader().clone(),
        store.writer().clone(),
        Arc::clone(&prefs_fn),
    );
    let web = WebManager::new(
        root.clone(),
        engine.clone(),
        bus.clone(),
        registry.clone(),
        store.reader().clone(),
        sites.clone(),
        services.clone(),
        tls.clone(),
        Arc::clone(&config_fn),
        platform.shared_case_sensitive_fs(),
    );
    let late = LateListener::default();
    let pools = PoolManager::new(
        root.clone(),
        supervisor.clone(),
        Arc::new(ports.clone()),
        Arc::new(late.clone()),
        registry.clone(),
        store.reader().clone(),
        layout.clone(),
        engine.clone(),
        bus.clone(),
        Arc::clone(&config_fn),
        Arc::clone(&prefs_fn),
    );
    web.set_pools(pools.clone());
    late.set(web.clone());
    services.set_renderer(Arc::new(web.clone()));
    let metered: forge_provider::update::MeteredProbe = {
        let cost = Arc::clone(&network_cost);
        Arc::new(move || cost.is_metered().unwrap_or(false))
    };
    let updates = UpdateEngine::spawn(
        &actors,
        UpdateDeps {
            layout: layout.clone(),
            reader: store.reader().clone(),
            writer: store.writer().clone(),
            catalog: Arc::new(registry.clone()),
            releases: providers.clone(),
            fetcher: Arc::new(DownloadFetcher::new(
                downloads.clone(),
                root.tmp_downloads(),
                Duration::from_secs(30 * 60),
            )),
            pools: Arc::new(DaemonPoolSwitcher(pools.clone())),
            metered,
        },
        UpdateConfig::new(config.updates.clone()),
    );
    let runtime_catalog = RuntimeCatalog::from_manifests(
        root.clone(),
        catalog::tools(&registry)
            .iter()
            .map(|t| t.manifest.as_ref())
            .collect::<Vec<_>>(),
    );
    let apps = AppManager::new(
        supervisor.clone(),
        root.clone(),
        runtime_catalog,
        Arc::new(ports.clone()),
        engine.clone(),
    );

    // 5. DNS.
    let dns = DnsServer::spawn(
        &actors,
        DnsServerConfig::default(),
        ZoneSet::from_config(&config)?,
    );
    let dns_os_policy = platform::dns_policy(helper);
    let dns_policy = DnsPolicyManager::new(Arc::clone(&dns_os_policy))
        .with_events(bus.clone())
        .with_state_file(root.state_dns_policy());
    start_dns(&dns, &dns_policy, &config).await;

    let endpoint = forge_ipc::transport::local_transport(&root).endpoint();
    let daemon = Daemon::new(Inner {
        root: root.clone(),
        args: args.clone(),
        platform,
        config: config_tx,
        writer: store.writer().clone(),
        reader: store.reader().clone(),
        bus: bus.clone(),
        jobs: jobs.clone(),
        actors,
        registry,
        providers,
        downloads,
        layout,
        updates,
        job_tree,
        supervisor,
        ports,
        services: services.clone(),
        pools,
        web: web.clone(),
        sites: sites.clone(),
        apps,
        tls,
        dns,
        dns_policy,
        dns_os_policy,
        engine,
        network_cost,
        case,
        shutdown: Shutdown::default(),
        started,
        started_at,
        server: std::sync::OnceLock::new(),
        endpoint: endpoint.clone(),
    });

    // Internal endpoint (`/_forgebox/starting` of on-visit processes), before any web
    // server is rendered: its port goes into their configuration.
    start_internal(&daemon).await;

    // 6. Instances (web servers wait for step 8).
    let loaded = services.load_all().await?;
    let created = services.ensure_defaults().await?;
    tracing::info!(
        instances = loaded + created.len(),
        created = created.len(),
        "instances loaded"
    );
    let results = services
        .autostart(&|entry| !web.is_web_server_entry(entry))
        .await;
    tracing::info!(
        started = results.iter().filter(|r| r.1.is_ok()).count(),
        failed = results.iter().filter(|r| r.1.is_err()).count(),
        "services autostarted"
    );

    // Parked folders → implicit sites (before pools: they may add php sites).
    let parked = start_parked(&daemon).await;

    // 7-8. Pools, web configuration, web servers.
    if let Err(err) = web.reconcile("boot").await {
        tracing::warn!(%err, "web reconcile failed at boot");
    }
    let results = services
        .autostart(&|entry| web.is_web_server_entry(entry))
        .await;
    if !results.is_empty() {
        tracing::info!(
            started = results.iter().filter(|r| r.1.is_ok()).count(),
            "web servers autostarted"
        );
    }

    // 9. API.
    let token = {
        let path = root.ipc_token();
        let acl = daemon.platform.shared_file_acl();
        tokio::task::spawn_blocking(move || IpcToken::load_or_create(&path, acl.as_ref()))
            .await
            .map_err(blocking_err)??
    };
    let server = Server::builder(Arc::new(TokenAuthenticator::new(token)))
        .config(ServerConfig {
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            ..ServerConfig::default()
        })
        .router(crate::api::router(&daemon))
        .process_stats(Arc::clone(daemon.platform.process_stats()))
        .not_implemented(crate::api::not_implemented::methods())
        .root(root.clone())
        .events(EventSource::new(bus.clone()).with_store(store.reader().clone()))
        .jobs(jobs.clone())
        .fs(FsScope::new(&root))
        .build();
    let transport = forge_ipc::transport::local_transport(&root);
    let (server_handle, server_task) =
        server
            .bind_and_spawn(transport.as_ref())
            .await
            .map_err(|e| match e {
                forge_ipc::transport::TransportError::AddrInUse(name) => {
                    ForgeError::Conflict(format!("{name} is already served by another daemon"))
                }
                other => ForgeError::Internal(format!("opening the API: {other}")),
            })?;
    let _ = daemon.server.set(server_handle);
    match tokio::task::spawn_blocking(lock.describe_task(endpoint.clone())).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::warn!(%err, "lock file not written"),
        Err(e) => tracing::warn!(%e, "lock file task failed"),
    }
    let scheduler = match options.update_source {
        Some(source) => Scheduler::start_with(&daemon, source),
        None => Scheduler::start(&daemon),
    };
    let boot_ms = started.elapsed().as_millis();
    bus.publish(
        EventKind::DaemonReady,
        serde_json::to_value(DaemonReadyPayload {
            v: 1,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            api_version: API_VERSION,
        })
        .unwrap_or_default(),
    );
    tracing::info!(%endpoint, boot_ms, pid = std::process::id(), "daemon ready");
    Ok(Booted {
        daemon,
        store,
        event_log,
        server_task,
        scheduler,
        parked,
        boot_ms,
    })
}

async fn start_internal(daemon: &Daemon) {
    let owner = match crate::internal::owner() {
        Ok(owner) => owner,
        Err(err) => {
            tracing::warn!(%err, "internal endpoint owner");
            return;
        }
    };
    let port = match daemon
        .ports
        .allocate(crate::internal::INTERNAL_RANGE, owner)
        .await
    {
        Ok(reservation) => reservation.port,
        Err(err) => {
            tracing::warn!(%err, "internal endpoint: no port; on-visit pages unavailable");
            return;
        }
    };
    match crate::internal::serve(port, daemon.apps.clone(), daemon.shutdown.token()).await {
        Ok(port) => daemon.web.set_internal_port(port),
        Err(err) => tracing::warn!(%err, "internal endpoint did not start"),
    }
}

async fn start_parked(daemon: &Daemon) -> Option<ParkedWatcher> {
    let config = daemon.config();
    let dirs: Vec<ParkedDir> = config
        .parked_dirs
        .iter()
        .map(|d| {
            let path = std::path::Path::new(d);
            ParkedDir::new(if path.is_absolute() {
                path.to_path_buf()
            } else {
                daemon.root.path().join(path)
            })
        })
        .collect();
    if dirs.is_empty() {
        return None;
    }
    let mut parked = ParkedConfig::new(dirs);
    parked.default_tld = config.tld.trim_start_matches('.').to_owned();
    let sync = ParkedSync::new(daemon.sites.clone(), parked);
    match tokio::time::timeout(Duration::from_secs(15), sync.reconcile()).await {
        Ok(Ok(report)) => tracing::info!(?report, "parked folders reconciled"),
        Ok(Err(err)) => tracing::warn!(%err, "parked folders not reconciled"),
        Err(_) => tracing::warn!("parked reconcile timed out"),
    }
    match ParkedWatcher::start(sync) {
        Ok(watcher) => Some(watcher),
        Err(err) => {
            tracing::warn!(%err, "parked folders are not watched");
            None
        }
    }
}
