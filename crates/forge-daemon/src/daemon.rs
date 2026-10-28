//! The composed daemon: every component, shared by the API handlers, the scheduler and
//! the shutdown. Built by [`crate::boot`]; cheap to clone (one `Arc`).

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use forge_core::Timestamp;
use forge_core::actor::ActorSupervisor;
use forge_core::config::ForgeConfig;
use forge_core::error::{ForgeError, Result};
use forge_core::event::EventBus;
use forge_core::jobs::{JobContext, JobRegistry, JobSpec};
use forge_core::paths::ForgeRoot;
use forge_core::platform::{NetworkCost, Platform};
use forge_core::store::{StoreReader, StoreWriter};
use forge_core::sync::{LockLevel, OrderedMutex};
use forge_ipc::ServerHandle;
use forge_ipc::api::JobDto;
use forge_ipc::server::Ctx;
use forge_net::dns::DnsServer;
use forge_net::policy::DnsPolicyManager;
use forge_net::ports::PortAllocator;
use forge_provider::download::DownloadManager;
use forge_provider::layout::Layout;
use forge_provider::provider::ProviderRegistry;
use forge_provider::update::UpdateEngine;
use forge_registry::Registry;
use forge_render::Engine;
use forge_sites::{AppManager, SiteService};
use forge_supervisor::{JobTree, Supervisor};
use forge_tls::TlsManager;
use serde::Serialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::args::Args;
use crate::pools::PoolManager;
use crate::services::ServiceManager;
use crate::web::WebManager;

/// Why the daemon is going down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownRequest {
    /// `user`, `restart`, `update`, `signal` (`daemon.shutdown` payload).
    pub reason: String,
    /// Start a new daemon once this one is down (`daemon.restart`).
    pub restart: bool,
    /// Leave instances and pools running (`daemon.stop --keep-services`): their jobs are
    /// not terminated by the daemon. They still die with the root Job Object when the
    /// process exits (`KILL_ON_JOB_CLOSE`), so this only skips the graceful cascade.
    pub keep_services: bool,
}

/// One-shot shutdown request shared by Ctrl+C, `daemon.stop` and `daemon.restart`.
#[derive(Debug)]
pub struct Shutdown {
    token: CancellationToken,
    /// Lock `daemon.tables` (leaf): the first request wins.
    request: OrderedMutex<Option<ShutdownRequest>>,
}

impl Default for Shutdown {
    fn default() -> Self {
        Self {
            token: CancellationToken::new(),
            request: OrderedMutex::new(LockLevel::DAEMON_TABLES, None),
        }
    }
}

impl Shutdown {
    /// Asks for a shutdown (the first request is kept).
    pub fn request(&self, request: ShutdownRequest) {
        {
            let mut slot = self.request.lock();
            if slot.is_none() {
                *slot = Some(request);
            }
        }
        self.token.cancel();
    }

    pub fn requested(&self) -> Option<ShutdownRequest> {
        self.request.lock().clone()
    }

    pub fn is_requested(&self) -> bool {
        self.token.is_cancelled()
    }

    pub async fn wait(&self) {
        self.token.cancelled().await;
    }

    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

/// Every component of the daemon.
pub struct Inner {
    pub root: ForgeRoot,
    pub args: Args,
    pub platform: Platform,
    pub config: watch::Sender<Arc<ForgeConfig>>,
    pub writer: StoreWriter,
    pub reader: StoreReader,
    pub bus: EventBus,
    pub jobs: JobRegistry,
    pub actors: ActorSupervisor,
    pub registry: Registry,
    pub providers: Arc<ProviderRegistry>,
    pub downloads: DownloadManager,
    pub layout: Layout,
    pub updates: UpdateEngine,
    pub job_tree: JobTree,
    pub supervisor: Supervisor,
    pub ports: PortAllocator,
    pub services: ServiceManager,
    pub pools: PoolManager,
    pub web: WebManager,
    pub sites: SiteService,
    pub apps: AppManager,
    pub tls: TlsManager,
    pub dns: DnsServer,
    pub dns_policy: DnsPolicyManager,
    /// The OS side of the DNS policy (diagnostics read it directly).
    pub dns_os_policy: Arc<dyn forge_core::platform::DnsPolicy>,
    pub engine: Engine,
    pub network_cost: Arc<dyn NetworkCost>,
    /// Case-sensitivity conversions (one table of folders in conversion, docs/15).
    pub case: forge_core::fs::case::CaseConversions,
    pub shutdown: Shutdown,
    pub started: Instant,
    pub started_at: Timestamp,
    pub server: OnceLock<ServerHandle>,
    pub endpoint: String,
}

/// Handle on the composed daemon. Cheap to clone.
#[derive(Clone)]
pub struct Daemon {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Daemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon")
            .field("root", &self.inner.root)
            .finish_non_exhaustive()
    }
}

impl std::ops::Deref for Daemon {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.inner
    }
}

impl Daemon {
    pub fn new(inner: Inner) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Current `forgebox.toml`.
    pub fn config(&self) -> Arc<ForgeConfig> {
        Arc::clone(&self.inner.config.borrow())
    }

    /// Replaces the in-memory preferences (after `forge config set` saved them).
    /// The parts read by other components are handed to them too (the port ranges of the
    /// allocator), so a preference changed through the API applies without a restart.
    pub fn set_config(&self, config: ForgeConfig) {
        if config.ports != self.config().ports {
            self.inner.ports.set_config(config.ports.clone());
        }
        self.inner.config.send_replace(Arc::new(config));
    }

    /// `prefs.*` of manifest strings (docs/04 §1.4): the scalar preferences.
    pub fn prefs(&self) -> BTreeMap<String, String> {
        prefs_of(&self.config())
    }

    /// Starts a daemon job for request `ctx` (idempotent by `request_id`) and answers with
    /// its snapshot.
    pub fn job<F, Fut, T>(&self, ctx: &Ctx, name: impl Into<String>, factory: F) -> Result<JobDto>
    where
        F: FnOnce(JobContext) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
        T: Serialize + Send + 'static,
    {
        if self.shutdown.is_requested() {
            return Err(ForgeError::Conflict("the daemon is shutting down".into()));
        }
        let handle = self
            .inner
            .jobs
            .spawn(JobSpec::new(name).request_id(ctx.request_id()), factory);
        Ok(JobDto::from(handle.info()))
    }
}

/// Scalar preferences of `config` by key (`tld` without the dot, `default_server`...).
pub fn prefs_of(config: &ForgeConfig) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "tld".to_owned(),
            config.tld.trim_start_matches('.').to_owned(),
        ),
        (
            "default_server".to_owned(),
            config.default_server.to_string(),
        ),
        (
            "case_sensitive".to_owned(),
            config.case_sensitive.to_string(),
        ),
    ])
}
