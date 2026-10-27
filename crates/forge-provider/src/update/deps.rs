//! What the update engine gets injected (ADR-0011: every collaborator is a trait object,
//! so tests plug fakes and the daemon plugs the real subsystems):
//!
//! * [`ToolCatalog`] — the manifest of a tool (the daemon passes the registry service;
//!   [`StaticCatalog`] for tests and offline tools);
//! * [`ReleaseSource`] — releases of a tool ([`ProviderRegistry`] in production);
//! * [`ArchiveFetcher`] — downloads a release ([`DownloadFetcher`] over the
//!   [`DownloadManager`] queue);
//! * [`PoolSwitcher`] — blue/green between the pools of two versions of a series
//!   (`forge_fcgi::Switcher` behind the daemon; [`NoPools`] when there are none);
//! * [`MeteredProbe`] — whether the connection is metered right now.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::ids::{ToolId, VersionId};
use forge_core::jobs::{JobContext, JobState};
use forge_manifest::Manifest;
use forge_manifest::schema::ToolManifest;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::download::{DownloadManager, DownloadOutcome, DownloadRequest, Priority};
use crate::provider::{ProviderRegistry, Release, ResolveRequest, resolve_checksum};

/// A tool's manifest and whether its `post_install.run` may execute (official registry
/// or the user's own manifest, docs/11 §2).
#[derive(Debug, Clone)]
pub struct CatalogTool {
    pub manifest: Arc<ToolManifest>,
    pub allow_run: bool,
}

/// Where the engine finds manifests.
pub trait ToolCatalog: Send + Sync {
    fn tool(&self, id: &ToolId) -> Option<CatalogTool>;
}

/// A fixed set of manifests.
#[derive(Debug, Clone, Default)]
pub struct StaticCatalog {
    tools: BTreeMap<ToolId, CatalogTool>,
}

impl StaticCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, manifest: Arc<ToolManifest>, allow_run: bool) -> Self {
        self.tools.insert(
            manifest.metadata.id.clone(),
            CatalogTool {
                manifest,
                allow_run,
            },
        );
        self
    }
}

impl ToolCatalog for StaticCatalog {
    fn tool(&self, id: &ToolId) -> Option<CatalogTool> {
        self.tools.get(id).cloned()
    }
}

impl ToolCatalog for forge_registry::Registry {
    fn tool(&self, id: &ToolId) -> Option<CatalogTool> {
        let entry = self.find(id.as_str())?;
        match entry.manifest.as_ref() {
            Manifest::Tool(tool) => Some(CatalogTool {
                manifest: Arc::new(tool.clone()),
                allow_run: entry.official,
            }),
            _ => None,
        }
    }
}

/// Releases of a tool.
pub trait ReleaseSource: Send + Sync {
    /// Every stable release of the tool, every variant, preferred first.
    fn releases<'a>(&'a self, manifest: &'a ToolManifest) -> BoxFuture<'a, Result<Vec<Release>>>;

    /// Fills the checksum of a release whose hash lives in a second document
    /// (`checksum_url`, docs/05 §1.1) before it is downloaded.
    fn complete<'a>(&'a self, _release: &'a mut Release) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl ReleaseSource for ProviderRegistry {
    fn releases<'a>(&'a self, manifest: &'a ToolManifest) -> BoxFuture<'a, Result<Vec<Release>>> {
        Box::pin(async move {
            let variants = BTreeMap::new();
            self.resolve(ResolveRequest {
                tool: manifest,
                constraint: None,
                variants: &variants,
                channel: None,
            })
            .await
        })
    }

    fn complete<'a>(&'a self, release: &'a mut Release) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if release.sha256.is_none() && release.sha512.is_none() {
                resolve_checksum(self.fetcher(), "checksums", release).await?;
            }
            Ok(())
        })
    }
}

/// What to download for an update.
#[derive(Debug, Clone, Copy)]
pub struct FetchArchive<'a> {
    /// The update job (progress, cancellation).
    pub job: &'a JobContext,
    pub manifest: &'a ToolManifest,
    pub release: &'a Release,
    pub priority: Priority,
}

/// Downloads the archive of a release and returns its path.
pub trait ArchiveFetcher: Send + Sync {
    fn fetch<'a>(&'a self, req: FetchArchive<'a>) -> BoxFuture<'a, Result<PathBuf>>;
}

/// [`ArchiveFetcher`] over the download queue: `tmp/downloads/<file name>`, the
/// release's sha256 and size checked by the download engine (and again by the
/// installer), cancelled with the update job.
#[derive(Debug, Clone)]
pub struct DownloadFetcher {
    manager: DownloadManager,
    dir: PathBuf,
    timeout: Duration,
}

impl DownloadFetcher {
    /// Downloads into `dir` (`tmp/downloads/`), waiting at most `timeout` per file.
    pub fn new(manager: DownloadManager, dir: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            manager,
            dir: dir.into(),
            timeout,
        }
    }
}

impl ArchiveFetcher for DownloadFetcher {
    fn fetch<'a>(&'a self, req: FetchArchive<'a>) -> BoxFuture<'a, Result<PathBuf>> {
        Box::pin(async move {
            let release = req.release;
            let url = Url::parse(&release.url)
                .map_err(|e| ForgeError::InvalidInput(format!("{}: {e}", release.url)))?;
            let name =
                if release.file_name.is_empty() || release.file_name.contains(['/', '\\', ':']) {
                    format!("{}-{}", req.manifest.metadata.id, release.version)
                } else {
                    release.file_name.clone()
                };
            let mut download =
                DownloadRequest::new(vec![url], self.dir.join(name)).priority(req.priority);
            if let Some(sha256) = &release.sha256 {
                download = download.expected_sha256(sha256.clone());
            }
            if let Some(size) = release.size {
                download = download.expected_size(size);
            }
            let id = self.manager.download(download).await?;
            let handle = self
                .manager
                .jobs()
                .handle(id)
                .ok_or_else(|| ForgeError::Internal(format!("download job {id} vanished")))?;
            let info = tokio::select! {
                info = handle.wait(self.timeout) => info?,
                () = req.job.cancelled() => {
                    let _ = self.manager.cancel(id);
                    return Err(ForgeError::Cancelled(format!("download of {}", release.url)));
                }
            };
            match info.state {
                JobState::Done => {
                    let outcome: DownloadOutcome =
                        serde_json::from_value(info.result.unwrap_or_default())
                            .map_err(|e| ForgeError::Internal(format!("download result: {e}")))?;
                    Ok(outcome.path)
                }
                _ => Err(super::error_from_job(&info)),
            }
        })
    }
}

/// One blue/green switch between two versions of a series (docs/03 §2.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolSwitch {
    pub tool: ToolId,
    /// Series whose sites move (`8.3`).
    pub series: String,
    /// Version that serves now and the one that takes over.
    pub from: VersionId,
    pub to: VersionId,
    pub from_version: String,
    pub to_version: String,
}

/// Moves the traffic of the pools of a series from one version to another. Implemented by
/// the daemon over `forge_fcgi::Switcher::switch(from, to)` (which starts the target pool,
/// warms it up, swaps the upstream with a reload and drains the source); when no pool of
/// `from` runs, there is nothing to switch and it returns `Ok`.
pub trait PoolSwitcher: Send + Sync {
    fn switch<'a>(&'a self, switch: &'a PoolSwitch) -> BoxFuture<'a, Result<()>>;

    /// Whether a pool of `version` serves sites right now (a site pinned to an exact
    /// version); such a version is never removed by retention.
    fn serving<'a>(
        &'a self,
        _tool: &'a ToolId,
        _version: VersionId,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }
}

/// No pools (tools without `service.kind: pool`, or a daemon without the pool manager).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPools;

impl PoolSwitcher for NoPools {
    fn switch<'a>(&'a self, _switch: &'a PoolSwitch) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

/// Whether the connection is metered right now (Windows `NetworkCostManager` in the
/// daemon). Called from a blocking thread.
pub type MeteredProbe = Arc<dyn Fn() -> bool + Send + Sync>;

/// A probe that always answers `metered`.
pub fn fixed_metered(metered: bool) -> MeteredProbe {
    Arc::new(move || metered)
}
