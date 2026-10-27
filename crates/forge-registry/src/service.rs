//! The registry service: configured registries, refresh jobs and the in-memory catalog.
//!
//! Concurrency (docs/03 §7, ADR-0011): [`Registry`] is a cheap-to-clone handle over shared
//! state that is a *snapshot swap*, not a mailbox actor: readers (`find`, `search`, `list`)
//! clone an `Arc<Snapshot>` under a `parking_lot` lock that is never held across an `.await`,
//! and all slow work (network, `git`, file I/O, hashing, parsing) runs in a job of the
//! `JobRegistry` or in `spawn_blocking`. [`Registry::refresh`] returns a `JobId` at once and
//! progress/outcome arrive as `job.*` events. At most one refresh runs per registry: asking
//! again returns the running job. Every network call and every `git` run has a timeout.
//!
//! Loading never leaves a registry half-updated: the new catalog replaces the old one only
//! after the signature, every hash and the cache write succeeded; on any failure the previous
//! (verified) catalog stays in service and the error is recorded in `registries.last_error`.
//!
//! Cache and TTL: `state/registry/<name>/` is re-verified on every read. [`Registry::load`]
//! uses the cache without touching the source while `last_refresh_at + ttl` is in the future;
//! [`Registry::refresh`] always asks the source (HTTP uses `If-None-Match`; git compares the
//! commit id) and only rewrites the cache when something changed.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forge_core::Timestamp;
use forge_core::error::{ForgeError, Result};
use forge_core::ids::{JobId, ToolId};
use forge_core::jobs::{JobContext, JobProgress, JobRegistry};
use forge_core::paths::ForgeRoot;
use forge_core::store::{Store, StoreReader, StoreWriter};
use forge_manifest::schema::Category;
use parking_lot::RwLock;
use rusqlite::params;
use serde::Serialize;
use url::Url;

use crate::cache;
use crate::index::{
    self, INDEX_FILE, LoadOptions, LoadedRegistry, MAX_INDEX_BYTES, MAX_MANIFEST_BYTES, Section,
    SkippedEntry, Verification, VerifiedIndex, VerifyOptions,
};
use crate::search::{self, CatalogEntry, SearchHit};
use crate::source::{GIT_TIMEOUT, RegistrySource, git_sync};

/// Default cache lifetime of a registry.
pub const DEFAULT_TTL: Duration = Duration::from_secs(3600);

/// Configuration of one registry (a row of the `registries` table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryConfig {
    /// `[a-z0-9-]`, unique; also the folder name under `state/registry/`.
    pub name: String,
    pub source: RegistrySource,
    /// The official registry may declare `post_install.run`; others may not (docs/11 §2).
    pub official: bool,
    pub enabled: bool,
    /// Lower first when the same id exists in several registries.
    pub priority: i64,
    /// Development only: an unsigned index may load, shown as "not verified".
    pub trusted_unsigned: bool,
    /// Minisign public key (base64 `RW...` or `.pub` text) that must have signed the index.
    pub public_key: Option<String>,
    /// How long the cache is used without asking the source.
    pub ttl: Duration,
}

impl RegistryConfig {
    /// A third-party, enabled registry with priority 100, no key and the default TTL.
    pub fn new(name: impl Into<String>, source: RegistrySource) -> Self {
        Self {
            name: name.into(),
            source,
            official: false,
            enabled: true,
            priority: 100,
            trusted_unsigned: false,
            public_key: None,
            ttl: DEFAULT_TTL,
        }
    }

    /// An official registry verified with `public_key`, priority 10.
    pub fn official(
        name: impl Into<String>,
        source: RegistrySource,
        public_key: impl Into<String>,
    ) -> Self {
        Self {
            official: true,
            priority: 10,
            public_key: Some(public_key.into()),
            ..Self::new(name, source)
        }
    }

    fn load_options(&self) -> LoadOptions {
        LoadOptions {
            verify: VerifyOptions {
                public_key: self.public_key.clone(),
                trusted_unsigned: self.trusted_unsigned,
            },
            official: self.official,
        }
    }

    fn validate(&self) -> Result<()> {
        ToolId::new(self.name.clone())
            .map_err(|e| ForgeError::InvalidInput(format!("registry name `{}`: {e}", self.name)))?;
        Ok(())
    }
}

/// Tuning of the network and git calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceOptions {
    pub connect_timeout: Duration,
    /// Whole request, body included.
    pub http_timeout: Duration,
    pub git_timeout: Duration,
    /// Manifests downloaded at the same time.
    pub concurrency: usize,
    pub user_agent: String,
}

impl Default for ServiceOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            http_timeout: Duration::from_secs(30),
            git_timeout: GIT_TIMEOUT,
            concurrency: 8,
            user_agent: format!("forgebox-registry/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// Where the catalog of a [`LoadReport`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadSource {
    /// Already in memory and within the TTL: nothing was read.
    Memory,
    /// Read from `state/registry/<name>/` within the TTL; the source was not contacted.
    Cache,
    /// The source said nothing changed (HTTP 304, same git commit); cache re-verified.
    NotModified,
    /// Downloaded or read from the source and written to the cache.
    Source,
    /// The source failed; the last verified catalog is served (see `warning`).
    StaleCache,
}

/// Outcome of a load or refresh (also the `result` of the `registry.refresh` job).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoadReport {
    pub registry: String,
    pub source: LoadSource,
    pub verification: Verification,
    pub tools: usize,
    pub stacks: usize,
    /// Manifests that were refused or unusable (the registry still loaded).
    pub skipped: Vec<SkippedEntry>,
    pub etag: Option<String>,
    pub warning: Option<String>,
}

/// One registry as `list()` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryInfo {
    pub name: String,
    pub kind: String,
    pub location: String,
    pub official: bool,
    pub enabled: bool,
    pub priority: i64,
    pub trusted_unsigned: bool,
    /// `None` until the first successful load in this process or a previous run.
    pub verification: Option<Verification>,
    pub last_refresh_at: Option<Timestamp>,
    pub last_error: Option<String>,
    pub tools: usize,
    pub stacks: usize,
    pub skipped: Vec<SkippedEntry>,
    /// A refresh job is running.
    pub refreshing: Option<JobId>,
}

#[derive(Debug)]
struct Snapshot {
    verification: Verification,
    entries: Vec<CatalogEntry>,
    skipped: Vec<SkippedEntry>,
    tools: usize,
    stacks: usize,
}

#[derive(Debug)]
struct Slot {
    config: RegistryConfig,
    snapshot: Option<Arc<Snapshot>>,
    etag: Option<String>,
    last_refresh_at: Option<Timestamp>,
    last_error: Option<String>,
    verification: Option<Verification>,
}

#[derive(Default)]
struct State {
    slots: BTreeMap<String, Slot>,
    refreshing: HashMap<String, JobId>,
}

struct Inner {
    root: ForgeRoot,
    writer: StoreWriter,
    reader: StoreReader,
    jobs: JobRegistry,
    http: reqwest::Client,
    options: ServiceOptions,
    // Never held across `.await` (clippy::await_holding_lock = deny).
    state: RwLock<State>,
}

/// Handle of the registry service. Cloning is cheap.
#[derive(Clone)]
pub struct Registry {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("registries", &self.inner.state.read().slots.len())
            .finish()
    }
}

/// Removes the "refresh running" mark when the job ends, however it ends (done, failed,
/// cancelled, dropped before it started).
struct RefreshGuard {
    registry: Registry,
    name: String,
}

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        self.registry
            .inner
            .state
            .write()
            .refreshing
            .remove(&self.name);
    }
}

#[allow(clippy::large_enum_variant)] // short-lived, moved once into `finish`
enum Fetched {
    NotModified,
    Fresh {
        verified: VerifiedIndex,
        files: BTreeMap<String, Vec<u8>>,
        etag: Option<String>,
    },
}

fn blocking_failed(e: tokio::task::JoinError) -> ForgeError {
    ForgeError::Internal(format!("registry task failed: {e}"))
}

impl Registry {
    /// Creates the service over the daemon's store and job registry. Configured registries
    /// are added with [`Registry::add`] or restored from the database with
    /// [`Registry::restore`].
    pub fn new(
        root: ForgeRoot,
        store: &Store,
        jobs: JobRegistry,
        options: ServiceOptions,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(options.user_agent.clone())
            .connect_timeout(options.connect_timeout)
            .build()
            .map_err(|e| ForgeError::Internal(format!("building the HTTP client: {e}")))?;
        Ok(Self {
            inner: Arc::new(Inner {
                root,
                writer: store.writer().clone(),
                reader: store.reader().clone(),
                jobs,
                http,
                options,
                state: RwLock::new(State::default()),
            }),
        })
    }

    // ---- configuration -------------------------------------------------------------

    /// Adds a registry or updates the configuration of an existing one (same name),
    /// persisting it in the `registries` table. A changed source, key or origin drops the
    /// in-memory catalog so it is verified again under the new configuration.
    pub async fn add(&self, config: RegistryConfig) -> Result<()> {
        config.validate()?;
        let row = config.clone();
        let now = Timestamp::now().to_string();
        let new_id = ulid::Ulid::generate().to_string();
        self.inner
            .writer
            .execute(move |conn| {
                conn.execute(
                    "INSERT INTO registries (id, name, url, kind, official, enabled, priority, \
                     trusted_unsigned, public_key, ttl_secs, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11) \
                     ON CONFLICT(name) DO UPDATE SET url = excluded.url, kind = excluded.kind, \
                     official = excluded.official, enabled = excluded.enabled, \
                     priority = excluded.priority, trusted_unsigned = excluded.trusted_unsigned, \
                     public_key = excluded.public_key, ttl_secs = excluded.ttl_secs, \
                     updated_at = excluded.updated_at",
                    params![
                        new_id,
                        row.name,
                        row.source.location(),
                        row.source.kind(),
                        row.official,
                        row.enabled,
                        row.priority,
                        row.trusted_unsigned,
                        row.public_key,
                        i64::try_from(row.ttl.as_secs()).unwrap_or(i64::MAX),
                        now,
                    ],
                )?;
                Ok(())
            })
            .await?;
        let mut state = self.inner.state.write();
        match state.slots.get_mut(&config.name) {
            Some(slot) => {
                let same_trust = slot.config.source == config.source
                    && slot.config.public_key == config.public_key
                    && slot.config.official == config.official
                    && slot.config.trusted_unsigned == config.trusted_unsigned;
                if !same_trust {
                    slot.snapshot = None;
                    slot.etag = None;
                    slot.verification = None;
                }
                slot.config = config;
            }
            None => {
                state.slots.insert(
                    config.name.clone(),
                    Slot {
                        config,
                        snapshot: None,
                        etag: None,
                        last_refresh_at: None,
                        last_error: None,
                        verification: None,
                    },
                );
            }
        }
        Ok(())
    }

    /// Forgets a registry: row, in-memory catalog and cache folder.
    pub async fn remove(&self, name: &str) -> Result<()> {
        if !self.inner.state.read().slots.contains_key(name) {
            return Err(ForgeError::NotFound(format!("registry {name}")));
        }
        let owned = name.to_owned();
        self.inner
            .writer
            .execute(move |conn| {
                conn.execute("DELETE FROM registries WHERE name = ?1", [owned])?;
                Ok(())
            })
            .await?;
        self.inner.state.write().slots.remove(name);
        let base = self.inner.root.state_registry();
        let owned = name.to_owned();
        tokio::task::spawn_blocking(move || cache::remove(&base, &owned))
            .await
            .map_err(blocking_failed)?
    }

    /// Reads the configured registries from the database (no catalog is loaded yet).
    pub async fn restore(&self) -> Result<usize> {
        let rows = self
            .inner
            .reader
            .query(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT name, url, kind, official, enabled, priority, trusted_unsigned, \
                     public_key, ttl_secs, etag, last_refresh_at, last_error, verification \
                     FROM registries ORDER BY priority, name",
                )?;
                let rows = stmt
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, bool>(3)?,
                            r.get::<_, bool>(4)?,
                            r.get::<_, i64>(5)?,
                            r.get::<_, bool>(6)?,
                            r.get::<_, Option<String>>(7)?,
                            r.get::<_, i64>(8)?,
                            r.get::<_, Option<String>>(9)?,
                            r.get::<_, Option<String>>(10)?,
                            r.get::<_, Option<String>>(11)?,
                            r.get::<_, Option<String>>(12)?,
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await?;
        let mut restored = 0;
        let mut state = self.inner.state.write();
        for (
            name,
            url,
            kind,
            official,
            enabled,
            priority,
            trusted_unsigned,
            public_key,
            ttl_secs,
            etag,
            last_refresh_at,
            last_error,
            verification,
        ) in rows
        {
            let source = match RegistrySource::from_parts(&kind, &url) {
                Ok(source) => source,
                Err(e) => {
                    tracing::warn!(registry = %name, "ignoring registry row: {e}");
                    continue;
                }
            };
            let config = RegistryConfig {
                name: name.clone(),
                source,
                official,
                enabled,
                priority,
                trusted_unsigned,
                public_key,
                ttl: Duration::from_secs(u64::try_from(ttl_secs).unwrap_or(0)),
            };
            state.slots.entry(name).or_insert(Slot {
                config,
                snapshot: None,
                etag,
                last_refresh_at: last_refresh_at.and_then(|t| t.parse().ok()),
                last_error,
                verification: verification.map(|v| {
                    if v == "verified" {
                        Verification::Verified {
                            key_id: String::new(),
                        }
                    } else {
                        Verification::Unverified
                    }
                }),
            });
            restored += 1;
        }
        Ok(restored)
    }

    // ---- loading -------------------------------------------------------------------

    /// Starts a refresh of `name` and returns its job at once. The job always asks the source
    /// (conditionally) and rewrites the cache when something changed. If a refresh of this
    /// registry is already running, that job's id is returned.
    ///
    /// Must be called inside a tokio runtime.
    pub fn refresh(&self, name: &str) -> Result<JobId> {
        let mut state = self.inner.state.write();
        let Some(slot) = state.slots.get(name) else {
            return Err(ForgeError::NotFound(format!("registry {name}")));
        };
        if !slot.config.enabled {
            return Err(ForgeError::Conflict(format!("registry {name} is disabled")));
        }
        if let Some(running) = state.refreshing.get(name) {
            return Ok(*running);
        }
        let guard = RefreshGuard {
            registry: self.clone(),
            name: name.to_owned(),
        };
        let this = self.clone();
        let owned = name.to_owned();
        let id = self
            .inner
            .jobs
            .start(format!("registry.refresh:{name}"), move |ctx| async move {
                let _guard = guard;
                this.run(&owned, true, Some(&ctx)).await
            });
        state.refreshing.insert(name.to_owned(), id);
        Ok(id)
    }

    /// Makes the catalog of `name` available, using the cache while it is within its TTL and
    /// falling back to the last verified catalog if the source is unreachable. Awaits the work
    /// (unlike [`Registry::refresh`]); use it at daemon start.
    pub async fn load(&self, name: &str) -> Result<LoadReport> {
        self.run(name, false, None).await
    }

    /// [`Registry::load`] for every enabled registry; one failure does not stop the others.
    pub async fn load_all(&self) -> Vec<(String, Result<LoadReport>)> {
        let names: Vec<String> = self
            .inner
            .state
            .read()
            .slots
            .values()
            .filter(|s| s.config.enabled)
            .map(|s| s.config.name.clone())
            .collect();
        let mut out = Vec::new();
        for name in names {
            let report = self.load(&name).await;
            out.push((name, report));
        }
        out
    }

    fn report(
        name: &str,
        source: LoadSource,
        snapshot: &Snapshot,
        etag: Option<String>,
        warning: Option<String>,
    ) -> LoadReport {
        LoadReport {
            registry: name.to_owned(),
            source,
            verification: snapshot.verification.clone(),
            tools: snapshot.tools,
            stacks: snapshot.stacks,
            skipped: snapshot.skipped.clone(),
            etag,
            warning,
        }
    }

    fn progress(ctx: Option<&JobContext>, message: &str, pct: f32) {
        if let Some(ctx) = ctx {
            ctx.progress(JobProgress {
                pct: Some(pct),
                message: Some(message.to_owned()),
                ..JobProgress::default()
            });
        }
    }

    async fn run(&self, name: &str, force: bool, ctx: Option<&JobContext>) -> Result<LoadReport> {
        let (config, snapshot, etag, last_refresh_at) = {
            let state = self.inner.state.read();
            let slot = state
                .slots
                .get(name)
                .ok_or_else(|| ForgeError::NotFound(format!("registry {name}")))?;
            (
                slot.config.clone(),
                slot.snapshot.clone(),
                slot.etag.clone(),
                slot.last_refresh_at,
            )
        };
        if !config.enabled {
            return Err(ForgeError::Conflict(format!("registry {name} is disabled")));
        }
        let base = self.inner.root.state_registry();
        let cached = {
            let base = base.clone();
            let owned = name.to_owned();
            tokio::task::spawn_blocking(move || cache::exists(&base, &owned))
                .await
                .map_err(blocking_failed)?
        };
        let within_ttl = last_refresh_at.is_some_and(|at| {
            let age = Timestamp::now().as_second() - at.as_second();
            age >= 0 && (age as u64) < config.ttl.as_secs()
        });

        if !force && within_ttl {
            if let Some(snapshot) = &snapshot {
                return Ok(Self::report(name, LoadSource::Memory, snapshot, etag, None));
            }
            if cached {
                match self.read_cache(&config).await {
                    Ok(loaded) => {
                        let snapshot = self.install(&config, loaded);
                        return Ok(Self::report(name, LoadSource::Cache, &snapshot, etag, None));
                    }
                    Err(e) => {
                        tracing::warn!(registry = %name, "cache is unusable, refreshing: {e}");
                    }
                }
            }
        }

        Self::progress(ctx, "fetching", 10.0);
        // A conditional request is only valid when the cache it refers to is still there.
        let conditional = if cached { etag.clone() } else { None };
        match self.fetch(&config, conditional.as_deref(), ctx).await {
            Ok(Fetched::NotModified) => {
                let loaded = self.read_cache(&config).await;
                match loaded {
                    Ok(loaded) => {
                        let snapshot = self.install(&config, loaded);
                        self.record_success(&config, etag.clone(), &snapshot, None)
                            .await?;
                        Ok(Self::report(
                            name,
                            LoadSource::NotModified,
                            &snapshot,
                            etag,
                            None,
                        ))
                    }
                    Err(e) => {
                        // The cache vanished or was tampered with: start over unconditionally.
                        tracing::warn!(registry = %name, "cache is unusable after 304: {e}");
                        self.fetch_and_install(&config, None, ctx).await
                    }
                }
            }
            Ok(Fetched::Fresh {
                verified,
                files,
                etag,
            }) => match self.finish(&config, verified, files, etag, ctx).await {
                Ok(report) => Ok(report),
                Err(e) => self.failed(&config, snapshot, e, force).await,
            },
            Err(e) => self.failed(&config, snapshot, e, force).await,
        }
    }

    async fn fetch_and_install(
        &self,
        config: &RegistryConfig,
        etag: Option<&str>,
        ctx: Option<&JobContext>,
    ) -> Result<LoadReport> {
        match self.fetch(config, etag, ctx).await? {
            Fetched::Fresh {
                verified,
                files,
                etag,
            } => self.finish(config, verified, files, etag, ctx).await,
            Fetched::NotModified => Err(ForgeError::Internal(
                "unconditional fetch answered not modified".to_owned(),
            )),
        }
    }

    async fn finish(
        &self,
        config: &RegistryConfig,
        verified: VerifiedIndex,
        files: BTreeMap<String, Vec<u8>>,
        etag: Option<String>,
        ctx: Option<&JobContext>,
    ) -> Result<LoadReport> {
        Self::progress(ctx, "verifying", 70.0);
        let options = config.load_options();
        let loaded =
            tokio::task::spawn_blocking(move || index::build_loaded(verified, files, &options))
                .await
                .map_err(blocking_failed)??;
        Self::progress(ctx, "caching", 85.0);
        let base = self.inner.root.state_registry();
        let name = config.name.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            cache::write(&base, &name, &loaded).map(|()| loaded)
        })
        .await
        .map_err(blocking_failed)??;
        let index_sha = loaded.index_sha256.clone();
        let snapshot = self.install(config, loaded);
        self.record_success(config, etag.clone(), &snapshot, Some(index_sha))
            .await?;
        Self::progress(ctx, "done", 100.0);
        Ok(Self::report(
            &config.name,
            LoadSource::Source,
            &snapshot,
            etag,
            None,
        ))
    }

    /// Source failure: keep the previous catalog; `load` may serve it, `refresh` reports.
    async fn failed(
        &self,
        config: &RegistryConfig,
        snapshot: Option<Arc<Snapshot>>,
        error: ForgeError,
        force: bool,
    ) -> Result<LoadReport> {
        tracing::warn!(registry = %config.name, "refresh failed: {error}");
        self.record_error(config, &error.to_string()).await;
        let security = matches!(error, ForgeError::VerificationFailed(_));
        if force || security {
            return Err(error);
        }
        let stale = match snapshot {
            Some(snapshot) => Some(snapshot),
            None => match self.read_cache(config).await {
                Ok(loaded) => Some(self.install(config, loaded)),
                Err(_) => None,
            },
        };
        match stale {
            Some(snapshot) => Ok(Self::report(
                &config.name,
                LoadSource::StaleCache,
                &snapshot,
                None,
                Some(error.to_string()),
            )),
            None => Err(error),
        }
    }

    async fn read_cache(&self, config: &RegistryConfig) -> Result<LoadedRegistry> {
        let dir = cache::cache_dir(&self.inner.root.state_registry(), &config.name);
        let options = config.load_options();
        tokio::task::spawn_blocking(move || index::load_tree(&dir, &options))
            .await
            .map_err(blocking_failed)?
    }

    /// Replaces the in-memory catalog of the registry.
    fn install(&self, config: &RegistryConfig, loaded: LoadedRegistry) -> Arc<Snapshot> {
        let entries: Vec<CatalogEntry> = loaded
            .manifests
            .iter()
            .map(|m| CatalogEntry {
                registry: config.name.clone(),
                official: config.official,
                verification: loaded.verification.clone(),
                section: m.section,
                id: m.id.clone(),
                path: m.path.clone(),
                sha256: m.sha256.clone(),
                latest: m.latest.clone(),
                manifest: Arc::clone(&m.manifest),
            })
            .collect();
        let tools = entries
            .iter()
            .filter(|e| e.section == Section::Tools)
            .count();
        let stacks = entries.len() - tools;
        let snapshot = Arc::new(Snapshot {
            verification: loaded.verification,
            entries,
            skipped: loaded.skipped,
            tools,
            stacks,
        });
        if let Some(slot) = self.inner.state.write().slots.get_mut(&config.name) {
            slot.snapshot = Some(Arc::clone(&snapshot));
            slot.verification = Some(snapshot.verification.clone());
        }
        snapshot
    }

    async fn record_success(
        &self,
        config: &RegistryConfig,
        etag: Option<String>,
        snapshot: &Snapshot,
        index_sha256: Option<String>,
    ) -> Result<()> {
        let now = Timestamp::now();
        if let Some(slot) = self.inner.state.write().slots.get_mut(&config.name) {
            slot.etag = etag.clone();
            slot.last_refresh_at = Some(now);
            slot.last_error = None;
        }
        let name = config.name.clone();
        let verification = if snapshot.verification.is_verified() {
            "verified"
        } else {
            "unverified"
        };
        self.inner
            .writer
            .execute(move |conn| {
                conn.execute(
                    "UPDATE registries SET etag = ?2, last_refresh_at = ?3, verification = ?4, \
                     index_sha256 = COALESCE(?5, index_sha256), last_error = NULL, \
                     updated_at = ?3 WHERE name = ?1",
                    params![name, etag, now.to_string(), verification, index_sha256],
                )?;
                Ok(())
            })
            .await
    }

    async fn record_error(&self, config: &RegistryConfig, message: &str) {
        if let Some(slot) = self.inner.state.write().slots.get_mut(&config.name) {
            slot.last_error = Some(message.to_owned());
        }
        let name = config.name.clone();
        let message = message.to_owned();
        let now = Timestamp::now().to_string();
        let result = self
            .inner
            .writer
            .execute(move |conn| {
                conn.execute(
                    "UPDATE registries SET last_error = ?2, updated_at = ?3 WHERE name = ?1",
                    params![name, message, now],
                )?;
                Ok(())
            })
            .await;
        if let Err(e) = result {
            tracing::warn!(registry = %config.name, "cannot record the refresh error: {e}");
        }
    }

    // ---- sources -------------------------------------------------------------------

    async fn fetch(
        &self,
        config: &RegistryConfig,
        etag: Option<&str>,
        ctx: Option<&JobContext>,
    ) -> Result<Fetched> {
        let options = config.load_options();
        match &config.source {
            RegistrySource::Dir(path) => {
                let path = path.clone();
                let (verified, files) =
                    tokio::task::spawn_blocking(move || index::read_tree(&path, &options))
                        .await
                        .map_err(blocking_failed)??;
                Ok(Fetched::Fresh {
                    verified,
                    files,
                    etag: None,
                })
            }
            RegistrySource::Git(_) => self.fetch_git(config, etag).await,
            RegistrySource::Http(url) => self.fetch_http(config, url, etag, ctx).await,
        }
    }

    async fn fetch_git(&self, config: &RegistryConfig, etag: Option<&str>) -> Result<Fetched> {
        let RegistrySource::Git(url) = &config.source else {
            return Err(ForgeError::Internal("not a git source".to_owned()));
        };
        let url: Url = url.clone();
        let checkout = cache::git_checkout_dir(&self.inner.root.state_registry(), &config.name);
        let timeout = self.inner.options.git_timeout;
        let previous = etag.map(str::to_owned);
        let options = config.load_options();
        let task = tokio::task::spawn_blocking(move || {
            let commit = git_sync(&url, &checkout, timeout)?;
            if previous.as_deref() == Some(commit.as_str()) {
                return Ok::<_, ForgeError>(Fetched::NotModified);
            }
            let (verified, files) = index::read_tree(&checkout, &options)?;
            Ok(Fetched::Fresh {
                verified,
                files,
                etag: Some(commit),
            })
        });
        // Leave room for the two git invocations (fetch + reset) plus bookkeeping.
        tokio::time::timeout(timeout * 3 + Duration::from_secs(5), task)
            .await
            .map_err(|_| ForgeError::Timeout {
                operation: format!("syncing git registry {}", config.name),
                after: timeout * 3,
            })?
            .map_err(blocking_failed)?
    }

    async fn fetch_http(
        &self,
        config: &RegistryConfig,
        index_url: &Url,
        etag: Option<&str>,
        ctx: Option<&JobContext>,
    ) -> Result<Fetched> {
        let (index_bytes, new_etag) = match self.http_get(index_url, MAX_INDEX_BYTES, etag).await? {
            HttpBody::NotModified => return Ok(Fetched::NotModified),
            HttpBody::Body { bytes, etag } => (bytes, etag),
        };
        // The signature is checked before any manifest is requested.
        let options = config.load_options();
        let verified = {
            let verify = options.verify.clone();
            tokio::task::spawn_blocking(move || index::verify_index(index_bytes, &verify))
                .await
                .map_err(blocking_failed)??
        };
        Self::progress(ctx, "downloading manifests", 30.0);
        let wanted: Vec<String> = index::entries(&verified.index)?
            .into_iter()
            .map(|(_, entry)| entry.path.clone())
            .collect();
        let total = wanted.len().max(1);
        let mut urls = Vec::with_capacity(wanted.len());
        for path in wanted {
            let url = index_url.join(&path).map_err(|e| {
                ForgeError::InvalidInput(format!(
                    "cannot resolve `{path}` against the index URL: {e}"
                ))
            })?;
            urls.push((path, url));
        }
        use futures::StreamExt as _;
        let client = self.clone();
        let mut downloads = futures::stream::iter(urls)
            .map(|(path, url)| {
                let client = client.clone();
                async move {
                    match client.http_get(&url, MAX_MANIFEST_BYTES, None).await? {
                        HttpBody::Body { bytes, .. } => Ok((path, bytes)),
                        HttpBody::NotModified => Err(ForgeError::Internal(
                            "unexpected 304 for an unconditional request".to_owned(),
                        )),
                    }
                }
            })
            .buffer_unordered(self.inner.options.concurrency.max(1));
        let mut files = BTreeMap::new();
        while let Some(next) = downloads.next().await {
            let (path, bytes) = next?;
            files.insert(path, bytes);
            Self::progress(
                ctx,
                "downloading manifests",
                30.0 + 40.0 * files.len() as f32 / total as f32,
            );
        }
        Ok(Fetched::Fresh {
            verified,
            files,
            etag: new_etag,
        })
    }

    async fn http_get(&self, url: &Url, max: u64, etag: Option<&str>) -> Result<HttpBody> {
        let timeout = self.inner.options.http_timeout;
        let mut request = self.inner.http.get(url.clone());
        if let Some(etag) = etag {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let work = async {
            let mut response = request.send().await.map_err(|e| http_error(url, &e))?;
            let status = response.status();
            if status == reqwest::StatusCode::NOT_MODIFIED && etag.is_some() {
                return Ok(HttpBody::NotModified);
            }
            if status == reqwest::StatusCode::NOT_FOUND {
                return Err(ForgeError::NotFound(url.to_string()));
            }
            if !status.is_success() {
                return Err(ForgeError::Io {
                    context: format!("GET {url}"),
                    source: std::io::Error::other(format!("HTTP {status}")),
                });
            }
            if response.content_length().is_some_and(|len| len > max) {
                return Err(ForgeError::InvalidInput(format!(
                    "{url} is larger than {max} bytes"
                )));
            }
            let new_etag = response
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|e| http_error(url, &e))? {
                if bytes.len() as u64 + chunk.len() as u64 > max {
                    return Err(ForgeError::InvalidInput(format!(
                        "{url} is larger than {max} bytes"
                    )));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(HttpBody::Body {
                bytes,
                etag: new_etag,
            })
        };
        tokio::time::timeout(timeout, work)
            .await
            .map_err(|_| ForgeError::Timeout {
                operation: format!("GET {url}"),
                after: timeout,
            })?
    }

    // ---- queries -------------------------------------------------------------------

    /// The registries, ordered by priority then name.
    pub fn list(&self) -> Vec<RegistryInfo> {
        let state = self.inner.state.read();
        let mut slots: Vec<&Slot> = state.slots.values().collect();
        slots.sort_by(|a, b| {
            (a.config.priority, &a.config.name).cmp(&(b.config.priority, &b.config.name))
        });
        slots
            .into_iter()
            .map(|slot| RegistryInfo {
                name: slot.config.name.clone(),
                kind: slot.config.source.kind().to_owned(),
                location: slot.config.source.location(),
                official: slot.config.official,
                enabled: slot.config.enabled,
                priority: slot.config.priority,
                trusted_unsigned: slot.config.trusted_unsigned,
                verification: slot
                    .snapshot
                    .as_ref()
                    .map(|s| s.verification.clone())
                    .or_else(|| slot.verification.clone()),
                last_refresh_at: slot.last_refresh_at,
                last_error: slot.last_error.clone(),
                tools: slot.snapshot.as_ref().map_or(0, |s| s.tools),
                stacks: slot.snapshot.as_ref().map_or(0, |s| s.stacks),
                skipped: slot
                    .snapshot
                    .as_ref()
                    .map(|s| s.skipped.clone())
                    .unwrap_or_default(),
                refreshing: state.refreshing.get(&slot.config.name).copied(),
            })
            .collect()
    }

    /// Every entry of every enabled, loaded registry, ordered by registry priority then
    /// section then id.
    pub fn entries(&self) -> Vec<CatalogEntry> {
        let state = self.inner.state.read();
        let mut slots: Vec<&Slot> = state
            .slots
            .values()
            .filter(|s| s.config.enabled && s.snapshot.is_some())
            .collect();
        slots.sort_by(|a, b| {
            (a.config.priority, &a.config.name).cmp(&(b.config.priority, &b.config.name))
        });
        slots
            .into_iter()
            .filter_map(|s| s.snapshot.as_ref())
            .flat_map(|s| s.entries.iter().cloned())
            .collect()
    }

    /// The manifest with this id: tools and extensions first, then stacks; when several
    /// registries have it, the one with the lowest priority number wins.
    pub fn find(&self, id: &str) -> Option<CatalogEntry> {
        let entries = self.entries();
        [Section::Tools, Section::Stacks]
            .into_iter()
            .find_map(|section| {
                entries
                    .iter()
                    .find(|e| e.section == section && e.id.as_str() == id)
                    .cloned()
            })
    }

    /// Searches by id, name and tag (see [`crate::search`] for the ranking), optionally
    /// restricted to a category.
    pub fn search(&self, query: &str, category: Option<Category>) -> Vec<SearchHit> {
        search::search(&self.entries(), query, category)
    }

    /// Folder with the cache of `name` (diagnostics, tests).
    pub fn cache_path(&self, name: &str) -> PathBuf {
        cache::cache_dir(&self.inner.root.state_registry(), name)
    }

    /// Name of the index file inside a registry.
    pub const INDEX_FILE: &'static str = INDEX_FILE;
}

enum HttpBody {
    NotModified,
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
    },
}

fn http_error(url: &Url, e: &reqwest::Error) -> ForgeError {
    if e.is_timeout() {
        ForgeError::Timeout {
            operation: format!("GET {url}"),
            after: Duration::ZERO,
        }
    } else {
        ForgeError::Io {
            context: format!("GET {url}"),
            source: std::io::Error::other(e.to_string()),
        }
    }
}
