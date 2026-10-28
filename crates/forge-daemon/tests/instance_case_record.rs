//! `instance.case_sensitive` of an instance with a data folder (docs/15 §5) through the
//! daemon's real `ServiceManager` and `WebManager`, wired as `boot.rs` wires them but with
//! an injected `CaseSensitiveFs`:
//!
//! * the value chosen for a data folder that does not exist yet is written to
//!   `instances.settings["forgebox.case_sensitive"]` (checked in the store, not only in
//!   the cached entry) by `start`, before `data.init` runs: `data.init` fails here (no
//!   `mysqld.exe`) and the value is already there;
//! * once the folder exists, a flag that cannot be read keeps the recorded value
//!   (`lower_case_table_names` in `my.ini` does not change) and the row is not touched;
//! * the render reads the record from the live table, not from the entry it was handed
//!   (a clone taken before the value was recorded).
//!
//! The mysql manifest comes from the repository's `registry/` (signed with the
//! development key), offline.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use forge_core::actor::ActorSupervisor;
use forge_core::config::ForgeConfig;
use forge_core::error::{ForgeError, Result};
use forge_core::event::EventBus;
use forge_core::ids::InstanceId;
use forge_core::jobs::JobRegistry;
use forge_core::paths::{ForgeRoot, InitOptions};
use forge_core::platform::{self, CaseSensitiveFs, CaseSensitivitySupport};
use forge_core::store::{
    CASE_SENSITIVE_KEY, InstanceRepo, NewToolVersion, Store, ToolVersionRepo, VersionSource,
    VersionState,
};
use forge_daemon::catalog;
use forge_daemon::render::DaemonRenderer;
use forge_daemon::services::{ConfigRenderer, ServiceManager};
use forge_daemon::web::WebManager;
use forge_net::ports::PortAllocator;
use forge_provider::layout::Layout;
use forge_registry::{Registry, RegistryConfig, RegistrySource, ServiceOptions};
use forge_render::Engine;
use forge_sites::service::SiteService;
use forge_sites::site::SiteRules;
use forge_supervisor::{JobTree, Supervisor};
use forge_tls::{TlsConfig, TlsManager};

const SENSITIVE: u8 = 0;
const INSENSITIVE: u8 = 1;
const FAILING: u8 = 2;

/// Flag of every folder, switchable during the test.
#[derive(Default)]
struct SwitchCs(AtomicU8);

impl SwitchCs {
    fn set_mode(&self, mode: u8) {
        self.0.store(mode, Ordering::SeqCst);
    }
}

impl CaseSensitiveFs for SwitchCs {
    fn support(&self, _dir: &Path) -> CaseSensitivitySupport {
        CaseSensitivitySupport::Supported
    }
    fn get(&self, dir: &Path) -> Result<bool> {
        match self.0.load(Ordering::SeqCst) {
            SENSITIVE => Ok(true),
            INSENSITIVE => Ok(false),
            _ => Err(ForgeError::Internal(format!(
                "NtQueryInformationFile {}: access denied",
                dir.display()
            ))),
        }
    }
    fn set(&self, _dir: &Path, _enabled: bool) -> Result<()> {
        Ok(())
    }
}

fn repo_registry() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../registry")
        .canonicalize()
        .map(|p| forge_core::paths::plain_path(&p))
        .unwrap()
}

struct Harness {
    _dir: tempfile::TempDir,
    _actors: ActorSupervisor,
    root: ForgeRoot,
    store: Store,
    services: ServiceManager,
    web: WebManager,
    cs: Arc<SwitchCs>,
    id: InstanceId,
}

impl Harness {
    /// A root with an installed (empty) mysql version and its `default` instance; the
    /// web manager reads the flag through `cs`.
    async fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("fbcase-")
            .tempdir()
            .unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let cs = Arc::new(SwitchCs::default());
        root.init_with(InitOptions::default(), cs.as_ref()).unwrap();
        let config = ForgeConfig::default();
        assert!(config.case_sensitive, "forgebox.toml asks for parity");
        let config_fn: Arc<dyn Fn() -> Arc<ForgeConfig> + Send + Sync> = {
            let config = Arc::new(config.clone());
            Arc::new(move || Arc::clone(&config))
        };

        let store = Store::open(&root.db()).unwrap();
        let bus = EventBus::new();
        let jobs = JobRegistry::new(bus.clone());
        let actors = ActorSupervisor::new(bus.clone());
        let platform = platform::current();

        let registry = Registry::new(
            root.clone(),
            &store,
            jobs.clone(),
            ServiceOptions::default(),
        )
        .unwrap();
        registry
            .add(RegistryConfig::official(
                "official",
                RegistrySource::parse(&repo_registry().to_string_lossy()).unwrap(),
                forge_registry::index::DEV_PUBLIC_KEY,
            ))
            .await
            .unwrap();
        for (name, result) in registry.load_all().await {
            result.unwrap_or_else(|e| panic!("registry {name}: {e}"));
        }
        let tool = catalog::require_tool(&registry, &"mysql".parse().unwrap()).unwrap();

        // An installed version without binaries: `data.init` (bin/mysqld.exe) fails.
        let bin = root.bin().join("mysql").join("8.4.6");
        std::fs::create_dir_all(&bin).unwrap();
        let new = NewToolVersion {
            tool_id: tool.manifest.metadata.id.clone(),
            tool_name: "MySQL".into(),
            version: "8.4.6".into(),
            canonical_name: "8.4.6".into(),
            variant: Default::default(),
            path: forge_core::paths::posix_string(&bin),
            state: VersionState::Installed,
            source: VersionSource::Manual,
        };
        store
            .writer()
            .execute(move |conn| ToolVersionRepo::new(conn).register_if_absent(&new))
            .await
            .unwrap()
            .expect("version registered");
        let version = catalog::installed(store.reader(), &tool.manifest.metadata.id, None)
            .await
            .unwrap()
            .remove(0);

        let job_tree = JobTree::new(Arc::clone(platform.processes())).unwrap();
        let supervisor = Supervisor::new(job_tree, actors.clone())
            .with_port_table(Arc::clone(platform.port_table()));
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
        let prefs_fn: Arc<dyn Fn() -> std::collections::BTreeMap<String, String> + Send + Sync> =
            Arc::new(Default::default);
        let services = ServiceManager::new(
            root.clone(),
            supervisor,
            ports,
            layout,
            registry.clone(),
            store.reader().clone(),
            store.writer().clone(),
            prefs_fn,
        );
        let tls = TlsManager::spawn(
            &actors,
            &root,
            TlsConfig::for_root(&root, &config),
            platform.shared_file_acl(),
            platform::trust_store(None),
        );
        let sites = SiteService::new(
            store.writer().clone(),
            store.reader().clone(),
            bus.clone(),
            SiteRules::new(config.tlds()),
        );
        let case_fs: Arc<dyn CaseSensitiveFs> = cs.clone();
        let web = WebManager::new(
            root.clone(),
            engine,
            bus,
            registry,
            store.reader().clone(),
            sites,
            services.clone(),
            tls,
            config_fn,
            case_fs,
        );
        services.set_renderer(Arc::new(web.clone()));
        let entry = services
            .create(&tool, &version, "default", None, false)
            .await
            .unwrap();
        Self {
            _dir: dir,
            _actors: actors,
            root,
            store,
            services,
            web,
            cs,
            id: entry.row.id,
        }
    }

    fn data_dir(&self) -> PathBuf {
        self.root.data().join("mysql").join("default")
    }

    /// `forgebox.case_sensitive` of the row, read from the store.
    async fn stored(&self) -> Option<String> {
        let id = self.id;
        self.store
            .reader()
            .query(move |conn| InstanceRepo::new(conn).get(id))
            .await
            .unwrap()
            .expect("instance row")
            .settings
            .get(CASE_SENSITIVE_KEY)
            .cloned()
    }

    /// `lower_case_table_names` of the rendered `my.ini`.
    fn lower_case_table_names(&self) -> String {
        let ini =
            std::fs::read_to_string(self.root.etc().join("mysql").join("default").join("my.ini"))
                .unwrap();
        ini.lines()
            .find_map(|l| {
                let (key, value) = l.split_once('=')?;
                (key.trim() == "lower_case_table_names").then(|| value.trim().to_owned())
            })
            .unwrap_or_else(|| panic!("no lower_case_table_names in my.ini:\n{ini}"))
    }

    /// `services.start`: renders (recording the value), creates the log folders, then
    /// runs `data.init`, which fails (no `mysqld.exe`).
    async fn start_until_data_init(&self) {
        let err = self.services.start(self.id).await.unwrap_err();
        assert!(
            err.to_string().contains("mysqld") || err.to_string().contains("data.init"),
            "start stops at data.init: {err}"
        );
        assert!(!self.data_dir().exists(), "data.init did not run");
    }
}

/// Initialized case-sensitive (`=0`): a flag that cannot be read later keeps `=0`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_value_recorded_before_data_init_survives_a_failing_read() {
    let h = Harness::new().await;
    let stale = h.services.get(h.id).unwrap();
    assert_eq!(stale.row.recorded_case_sensitive(), None);

    // (1) Fresh data folder on a case-sensitive parent: recorded in the store before
    // data.init ran.
    h.cs.set_mode(SENSITIVE);
    h.start_until_data_init().await;
    assert_eq!(h.stored().await.as_deref(), Some("true"));
    assert_eq!(
        h.services.get(h.id).unwrap().row.recorded_case_sensitive(),
        Some(true),
        "the cached entry sees the value too"
    );
    assert_eq!(h.lower_case_table_names(), "0");

    // (2) data.init succeeded (the folder now exists).
    std::fs::create_dir_all(h.data_dir()).unwrap();

    // (3) The flag cannot be read: still `=0`, row unchanged, also when rendering an
    // entry cloned before the value was recorded.
    h.cs.set_mode(FAILING);
    for entry in [h.services.get(h.id).unwrap(), stale] {
        h.web.render_instance(&entry).await.unwrap();
        assert_eq!(h.lower_case_table_names(), "0");
        assert_eq!(h.stored().await.as_deref(), Some("true"));
    }
}

/// Initialized insensitive (`=2`): a later failing read keeps `=2`, although
/// `forgebox.toml` asks for parity (what an unrecorded, initialized folder would get).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_insensitive_record_survives_a_failing_read() {
    let h = Harness::new().await;
    let stale = h.services.get(h.id).unwrap();

    h.cs.set_mode(INSENSITIVE);
    h.start_until_data_init().await;
    assert_eq!(h.stored().await.as_deref(), Some("false"));
    assert_eq!(
        h.services.get(h.id).unwrap().row.recorded_case_sensitive(),
        Some(false)
    );
    assert_eq!(h.lower_case_table_names(), "2");

    std::fs::create_dir_all(h.data_dir()).unwrap();

    h.cs.set_mode(FAILING);
    for entry in [h.services.get(h.id).unwrap(), stale] {
        h.web.render_instance(&entry).await.unwrap();
        assert_eq!(
            h.lower_case_table_names(),
            "2",
            "the recorded value wins over forgebox.toml"
        );
        assert_eq!(h.stored().await.as_deref(), Some("false"));
    }

    // A case-sensitive read later does not flip it either.
    h.cs.set_mode(SENSITIVE);
    h.web
        .render_instance(&h.services.get(h.id).unwrap())
        .await
        .unwrap();
    assert_eq!(h.lower_case_table_names(), "2");
    assert_eq!(h.stored().await.as_deref(), Some("false"));
}
