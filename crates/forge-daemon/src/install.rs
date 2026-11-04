//! `tools.install` (docs/03 §5.1): provider → download → verify/extract/layout, as one
//! daemon job per request. The pieces are `forge-provider`'s: releases resolved by the
//! manifest's providers (`ProviderRegistry`), checksum completed when it lives in a
//! second document, the archive fetched through the download queue with its sha256 and
//! size enforced, then `Layout::install_from_archive` (its own job, awaited here with the
//! progress forwarded). A release without a checksum is refused by the layout engine.
//!
//! Extensions (`kind: Extension`, ADR-0043) go through [`install_extension`]: the
//! releases of the extension are resolved like a tool's
//! (`ExtensionManifest::release_view`), matched against the installed versions of the
//! tool it extends ([`forge_provider::layout::extension::plan`]) and installed into each
//! target by `Layout::install_extension`, one layout job per target.
//!
//! One resolver, three users (M2-E2-T5): `tools.install`, `tools.plan` and `tools.releases`
//! all look releases up through [`ReleaseResolver`] (the providers, or a fake in tests),
//! pick them with [`pick_release`] (series → newest patch, default variant), expand
//! `depends_on` and stacks with [`expand`], and decide "already installed" with
//! [`installed_row`]. A plan therefore cannot promise what an install would not do.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use forge_core::Timestamp;
use forge_core::error::{ForgeError, Result};
use forge_core::ids::{ToolId, VersionId};
use forge_core::jobs::{JobContext, JobProgress, JobState};
use forge_core::store::{ToolVersionRow, VersionSource, VersionState};
use forge_ipc::api::{
    CatalogKind, ReleaseVariantDto, ToolPlanDto, ToolPlanItemDto, ToolPlanReason, ToolReleaseDto,
    ToolReleasesDto, ToolSpec,
};
use forge_manifest::schema::ExtensionManifest;
use forge_manifest::schema::tool::{ServiceKind, ToolManifest};
use forge_manifest::version::Constraint;
use forge_provider::download::Priority;
use forge_provider::layout::{
    ExtensionInstallRequest, ExtensionReport, ExtensionTarget, InstallOptions, InstallReport,
    InstallRequest,
};
use forge_provider::provider::{ProviderRegistry, Release, ResolveRequest};
use forge_provider::update::{ArchiveFetcher, DownloadFetcher, FetchArchive, ReleaseSource};
use futures::future::BoxFuture;
use serde::de::DeserializeOwned;

use crate::catalog::{self, ExtensionEntry, Installable};
use crate::daemon::Daemon;

/// Longest wait for one download.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// Longest wait for the layout job (extract, post_install, move).
const INSTALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Longest wait for one provider while an install resolves its release.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(60);
/// Longest wait for one provider in `tools.releases` / `tools.plan`: a cold cache without
/// network answers (io or timeout) well inside 15 s.
pub const LISTING_TIMEOUT: Duration = Duration::from_secs(12);

/// `(tool, version)` of a spec (`php@8.3`).
pub fn parse_spec(spec: &str) -> Result<(ToolId, Option<String>)> {
    let (tool, version) = match spec.split_once('@') {
        Some((tool, version)) => (tool, Some(version.trim().to_owned())),
        None => (spec, None),
    };
    let tool = ToolId::new(tool.trim().to_ascii_lowercase())
        .map_err(|e| ForgeError::InvalidInput(format!("tool {tool:?}: {e}")))?;
    Ok((tool, version.filter(|v| !v.is_empty())))
}

/// The constraint of a user version: an exact version when it has three components
/// (`8.3.36`), otherwise the series (`8.3` → `~8.3`, `22` → `~22`).
pub fn constraint_for(manifest: &ToolManifest, version: &str) -> Result<Constraint> {
    let scheme = manifest.spec.versioning.scheme;
    let version = version.trim_start_matches('v');
    let text = if version.split('.').count() >= 3
        || version.starts_with(['<', '>', '=', '~', '^'])
        || version.contains('*')
    {
        version.to_owned()
    } else {
        format!("~{version}")
    };
    Constraint::parse(scheme, &text)
        .map_err(|e| ForgeError::InvalidInput(format!("version {version:?}: {e}")))
}

/// `{key: value}` for a variant value given by the user (`nts` → `{ts: nts}`).
pub fn variant_filter(
    manifest: &ToolManifest,
    value: Option<&str>,
) -> Result<BTreeMap<String, String>> {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(BTreeMap::new());
    };
    for variant in &manifest.spec.versioning.variants {
        if variant.values.iter().any(|v| v == value) {
            return Ok(BTreeMap::from([(variant.key.clone(), value.to_owned())]));
        }
    }
    Err(ForgeError::InvalidInput(format!(
        "{} has no variant {value:?}",
        manifest.metadata.id
    )))
}

async fn wait_layout_job<T: DeserializeOwned>(
    daemon: &Daemon,
    job: &JobContext,
    id: forge_core::ids::JobId,
) -> Result<T> {
    let handle = daemon
        .jobs
        .handle(id)
        .ok_or_else(|| ForgeError::Internal(format!("install job {id} vanished")))?;
    let info = tokio::select! {
        info = handle.wait(INSTALL_TIMEOUT) => info?,
        () = job.cancelled() => {
            let _ = daemon.jobs.cancel(id);
            return Err(ForgeError::Cancelled(format!("installation job {id}")));
        }
    };
    match info.state {
        JobState::Done => serde_json::from_value(info.result.unwrap_or_default())
            .map_err(|e| ForgeError::Internal(format!("install report: {e}"))),
        JobState::Cancelled => Err(ForgeError::Cancelled(format!("installation job {id}"))),
        _ => {
            let error = info
                .error
                .unwrap_or_else(|| forge_core::jobs::JobErrorInfo {
                    code: "internal".into(),
                    message: "installation failed".into(),
                    details: None,
                });
            Err(match error.code.as_str() {
                "verification_failed" => ForgeError::VerificationFailed(error.message),
                "not_found" => ForgeError::NotFound(error.message),
                "conflict" => ForgeError::Conflict(error.message),
                "invalid_input" => ForgeError::InvalidInput(error.message),
                _ => ForgeError::Internal(error.message),
            })
        }
    }
}

// ------------------------------------------------------------------------ resolver

/// Releases as a [`ReleaseResolver`] answers them.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// Preferred first (newest version, then the manifest's variant order).
    pub releases: Vec<Release>,
    /// Served from an expired cache because the providers could not be reached.
    pub stale: bool,
    /// When the data was fetched from the providers (the cache's age when `stale`).
    pub fetched_at: Option<Timestamp>,
}

/// Where the install pipeline asks "which releases exist?": the providers of the
/// manifests in the daemon ([`ProviderResolver`]), a fake with canned releases in tests
/// ([`BootOptions::release_resolver`](crate::boot::BootOptions::release_resolver)).
pub trait ReleaseResolver: Send + Sync {
    /// Resolves `req`; every provider gets at most `per_provider`.
    fn resolve<'a>(
        &'a self,
        req: ResolveRequest<'a>,
        per_provider: Duration,
    ) -> BoxFuture<'a, Result<Resolved>>;

    /// Fills the checksum of a release whose hash lives in a second document.
    fn complete<'a>(&'a self, release: &'a mut Release) -> BoxFuture<'a, Result<()>>;
}

/// The real thing: [`ProviderRegistry`] with its metadata cache (TTL, `ETag`, offline).
pub struct ProviderResolver {
    providers: Arc<ProviderRegistry>,
}

impl ProviderResolver {
    pub fn new(providers: Arc<ProviderRegistry>) -> Self {
        Self { providers }
    }
}

impl ReleaseResolver for ProviderResolver {
    fn resolve<'a>(
        &'a self,
        req: ResolveRequest<'a>,
        per_provider: Duration,
    ) -> BoxFuture<'a, Result<Resolved>> {
        Box::pin(async move {
            let (result, trace) = self.providers.resolve_traced(req, per_provider).await;
            Ok(Resolved {
                releases: result?,
                stale: trace.stale,
                fetched_at: Some(trace.fetched_at),
            })
        })
    }

    fn complete<'a>(&'a self, release: &'a mut Release) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { ReleaseSource::complete(&*self.providers, release).await })
    }
}

/// Runs `fut`, giving up the moment the job is cancelled (so `jobs.cancel` ends a job
/// stuck behind a slow provider at once, not when the provider answers).
async fn cancellable<T>(
    job: &JobContext,
    fut: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {
        out = fut => out,
        () = job.cancelled() => Err(job
            .check_cancelled()
            .err()
            .unwrap_or_else(|| ForgeError::Cancelled("job".into()))),
    }
}

/// The release a spec resolves to.
#[derive(Debug, Clone)]
pub struct Picked {
    pub release: Release,
    pub stale: bool,
}

/// THE resolver of `tool[@version] [--variant]`, used by `tools.install` and `tools.plan`
/// alike: the series (`8.4`) or exact version (`8.4.27`) -> the newest matching release,
/// with the preferred (default) variant unless `variant` narrows it. `not_found` when the
/// providers have nothing for this platform.
pub async fn pick_release(
    resolver: &dyn ReleaseResolver,
    manifest: &ToolManifest,
    version: Option<&str>,
    variant: Option<&str>,
    per_provider: Duration,
) -> Result<Picked> {
    let constraint = version.map(|v| constraint_for(manifest, v)).transpose()?;
    let variants = variant_filter(manifest, variant)?;
    let resolved = resolver
        .resolve(
            ResolveRequest {
                tool: manifest,
                constraint: constraint.as_ref(),
                variants: &variants,
                channel: None,
            },
            per_provider,
        )
        .await?;
    let release = resolved.releases.into_iter().next().ok_or_else(|| {
        let id = &manifest.metadata.id;
        let spec = version.map_or_else(|| id.to_string(), |v| format!("{id}@{v}"));
        ForgeError::NotFound(format!("no release of {spec} for this platform"))
    })?;
    Ok(Picked {
        release,
        stale: resolved.stale,
    })
}

/// The installed row that already is `release` (same version, same variant values).
pub fn installed_row<'a>(
    rows: &'a [ToolVersionRow],
    release: &Release,
) -> Option<&'a ToolVersionRow> {
    rows.iter().find(|r| {
        r.version == release.version
            && release
                .variants
                .iter()
                .all(|(k, v)| r.variant.get(k) == Some(v))
    })
}

// ------------------------------------------------------------------------ expansion

/// What a client asked to install (or plan): one tool/extension, maybe with a version.
#[derive(Debug, Clone)]
pub struct Request {
    pub tool: ToolId,
    /// Series or exact version (`8.4`, `8.4.27`).
    pub version: Option<String>,
    /// [`ToolPlanReason::Requested`] (a spec) or [`ToolPlanReason::Stack`].
    pub reason: ToolPlanReason,
    /// Extensions installed into this tool right after it (stacks).
    pub extensions: Vec<ToolId>,
}

impl Request {
    pub fn requested(tool: ToolId, version: Option<String>) -> Self {
        Self {
            tool,
            version,
            reason: ToolPlanReason::Requested,
            extensions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Tool,
    Extension,
}

/// One thing to install, in install order.
#[derive(Debug, Clone)]
pub struct Step {
    pub tool: ToolId,
    pub version: Option<String>,
    pub kind: StepKind,
    pub reason: ToolPlanReason,
    /// Extensions of a stack: the version of the extended tool it goes into (`php@8.4`).
    pub target: Option<String>,
}

impl Step {
    /// `php@8.4`, `nginx`: what `tools.install` takes for this step.
    pub fn spec(&self) -> String {
        match &self.version {
            Some(v) => format!("{}@{v}", self.tool),
            None => self.tool.to_string(),
        }
    }
}

struct Expander<'a> {
    registry: &'a forge_registry::Registry,
    installed: &'a BTreeSet<ToolId>,
    wanted: BTreeMap<&'a ToolId, &'a Request>,
    steps: Vec<Step>,
    done: BTreeSet<ToolId>,
    visiting: BTreeSet<ToolId>,
}

impl Expander<'_> {
    /// Adds the tool after its not-yet-installed `depends_on` (recursively).
    fn tool(&mut self, id: &ToolId, request: Option<&Request>) -> Result<()> {
        if self.done.contains(id) || !self.visiting.insert(id.clone()) {
            return Ok(());
        }
        let entry = catalog::require_tool(self.registry, id)?;
        for dep in &entry.manifest.spec.depends_on {
            if let Some(wanted) = self.wanted.get(dep).copied() {
                self.tool(dep, Some(wanted))?;
            } else if !self.installed.contains(dep) {
                catalog::require_tool(self.registry, dep).map_err(|_| {
                    ForgeError::NotFound(format!("{id} needs {dep}, which no loaded registry has"))
                })?;
                self.tool(dep, None)?;
            }
        }
        self.visiting.remove(id);
        self.done.insert(id.clone());
        let (version, reason, extensions) = match request {
            Some(r) => (r.version.clone(), r.reason, r.extensions.clone()),
            None => (None, ToolPlanReason::Dependency, Vec::new()),
        };
        let target = Some(match &version {
            Some(v) => format!("{id}@{v}"),
            None => id.to_string(),
        });
        self.steps.push(Step {
            tool: id.clone(),
            version,
            kind: StepKind::Tool,
            reason,
            target: None,
        });
        for ext in extensions {
            self.steps.push(Step {
                tool: ext,
                version: None,
                kind: StepKind::Extension,
                reason: ToolPlanReason::Extension,
                target: target.clone(),
            });
        }
        Ok(())
    }
}

/// The install order of `requests`: every tool after the tools it `depends_on` that are
/// not installed (those are added, as [`ToolPlanReason::Dependency`]), extensions where
/// the request puts them. Shared by `tools.install` and `tools.plan`, so the plan lists
/// exactly what the install does. `installed` are the tool ids with some installed version.
pub fn expand(
    registry: &forge_registry::Registry,
    installed: &BTreeSet<ToolId>,
    requests: &[Request],
) -> Result<Vec<Step>> {
    let mut kinds = Vec::new();
    for request in requests {
        kinds.push(catalog::require_installable(registry, &request.tool)?);
    }
    let mut expander = Expander {
        registry,
        installed,
        wanted: BTreeMap::new(),
        steps: Vec::new(),
        done: BTreeSet::new(),
        visiting: BTreeSet::new(),
    };
    for (request, kind) in requests.iter().zip(&kinds) {
        if matches!(kind, Installable::Tool(_)) {
            expander.wanted.entry(&request.tool).or_insert(request);
        }
    }
    for (request, kind) in requests.iter().zip(&kinds) {
        match kind {
            Installable::Tool(_) => expander.tool(&request.tool, Some(request))?,
            Installable::Extension(_) => {
                if expander.done.insert(request.tool.clone()) {
                    expander.steps.push(Step {
                        tool: request.tool.clone(),
                        version: request.version.clone(),
                        kind: StepKind::Extension,
                        reason: request.reason,
                        target: None,
                    });
                }
            }
        }
    }
    Ok(expander.steps)
}

/// Tool ids with some installed version.
pub async fn installed_ids(daemon: &Daemon) -> Result<BTreeSet<ToolId>> {
    Ok(catalog::all_installed(&daemon.reader)
        .await?
        .into_iter()
        .map(|r| r.tool_id)
        .collect())
}

// ------------------------------------------------------------------------ tools.plan

fn size_of(release: &Release, manifest: &ToolManifest) -> (Option<u64>, bool) {
    match release.size {
        Some(bytes) => (Some(bytes), false),
        None => match manifest.spec.install.size_estimate_mb {
            Some(mb) => (Some(u64::from(mb) * 1024 * 1024), true),
            None => (None, false),
        },
    }
}

/// The build `release` would install to, as a row that is not in the store.
fn planned_row(tool: &ToolId, release: &Release, canonical: &str) -> ToolVersionRow {
    ToolVersionRow {
        id: VersionId::new(),
        tool_id: tool.clone(),
        version: release.version.clone(),
        canonical_name: canonical.to_owned(),
        variant: release.variants.clone(),
        path: String::new(),
        state: VersionState::Installed,
        source: VersionSource::Registry,
    }
}

/// The variant a step is resolved with: a requested tool must have it; a stack tool takes
/// it only when it declares it; a dependency never.
pub fn step_variant<'a>(
    step: &Step,
    manifest: &ToolManifest,
    variant: Option<&'a str>,
) -> Option<&'a str> {
    match step.reason {
        ToolPlanReason::Requested => variant,
        ToolPlanReason::Stack => variant.filter(|v| variant_filter(manifest, Some(v)).is_ok()),
        _ => None,
    }
}

/// What `tools.install` would do for `steps`, without doing it: the same
/// [`pick_release`] per tool, the same extension matching, sizes from the release (else the
/// manifest estimate, flagged), what is already installed marked. `target` is the
/// `--for` of the request. Cancellable between and during provider calls.
pub async fn plan(
    daemon: &Daemon,
    job: &JobContext,
    steps: &[Step],
    variant: Option<&str>,
    target: Option<&str>,
) -> Result<ToolPlanDto> {
    let installed_all = catalog::all_installed(&daemon.reader).await?;
    let mut items: Vec<ToolPlanItemDto> = Vec::new();
    // Per tool: the build the plan installs (the installed row, else a planned one), so an
    // extension of the same plan has a target.
    let mut planned: BTreeMap<ToolId, ToolVersionRow> = BTreeMap::new();
    let total = steps.len().max(1) as f32;
    for (index, step) in steps.iter().enumerate() {
        job.check_cancelled()?;
        job.progress(
            JobProgress::new()
                .pct(100.0 * index as f32 / total)
                .message(format!("resolving {}", step.spec())),
        );
        match step.kind {
            StepKind::Tool => {
                let tool = catalog::require_tool(&daemon.registry, &step.tool)?;
                let manifest = &tool.manifest;
                let picked = cancellable(
                    job,
                    pick_release(
                        &*daemon.resolver,
                        manifest,
                        step.version.as_deref(),
                        step_variant(step, manifest, variant),
                        LISTING_TIMEOUT,
                    ),
                )
                .await?;
                let release = picked.release;
                let rows: Vec<ToolVersionRow> = installed_all
                    .iter()
                    .filter(|r| r.tool_id == step.tool)
                    .cloned()
                    .collect();
                let existing = installed_row(&rows, &release).cloned();
                let canonical = forge_manifest::version::canonical_name(
                    &release.version,
                    &release.variants,
                    &manifest.spec.versioning.variant_order(),
                );
                let (size_bytes, size_estimated) = size_of(&release, manifest);
                planned.insert(
                    step.tool.clone(),
                    existing
                        .clone()
                        .unwrap_or_else(|| planned_row(&step.tool, &release, &canonical)),
                );
                items.push(ToolPlanItemDto {
                    tool: step.tool.clone(),
                    kind: CatalogKind::Tool,
                    spec: ToolSpec::from(step.spec().as_str()),
                    version: release.version,
                    variant: release.variants,
                    canonical_name: Some(canonical),
                    size_bytes,
                    size_estimated,
                    already_installed: existing.is_some(),
                    reason: step.reason,
                    target: None,
                });
            }
            StepKind::Extension => {
                plan_extension(
                    daemon,
                    job,
                    step,
                    target,
                    &installed_all,
                    &planned,
                    &mut items,
                )
                .await?;
            }
        }
    }
    let (total_bytes, unknown_size) = totals(&items);
    job.progress(JobProgress::new().pct(100.0).message("plan ready"));
    Ok(ToolPlanDto {
        items,
        total_bytes,
        unknown_size,
    })
}

/// `(total_bytes, unknown_size)` of a plan: what the items still to download add up to
/// (estimates included), and how many of them have no size at all.
pub fn totals(items: &[ToolPlanItemDto]) -> (u64, u32) {
    let mut total_bytes = 0u64;
    let mut unknown_size = 0u32;
    for item in items.iter().filter(|i| !i.already_installed) {
        match item.size_bytes {
            Some(bytes) => total_bytes += bytes,
            None => unknown_size += 1,
        }
    }
    (total_bytes, unknown_size)
}

/// The items of one extension step: one per version of the extended tool it goes into
/// (installed ones, and the one the same plan installs).
async fn plan_extension(
    daemon: &Daemon,
    job: &JobContext,
    step: &Step,
    param_target: Option<&str>,
    installed_all: &[ToolVersionRow],
    planned: &BTreeMap<ToolId, ToolVersionRow>,
    items: &mut Vec<ToolPlanItemDto>,
) -> Result<()> {
    let ext = catalog::extension(&daemon.registry, &step.tool).ok_or_else(|| {
        ForgeError::NotFound(format!(
            "no extension {} in the loaded registries",
            step.tool
        ))
    })?;
    let manifest = Arc::clone(&ext.manifest);
    let extends = manifest.spec.extends.clone();
    let target_tool = catalog::require_tool(&daemon.registry, &extends)?;
    let mut rows: Vec<ToolVersionRow> = installed_all
        .iter()
        .filter(|r| r.tool_id == extends)
        .cloned()
        .collect();
    if let Some(row) = planned.get(&extends) {
        if !rows.iter().any(|r| r.canonical_name == row.canonical_name) {
            rows.push(row.clone());
        }
    }
    catalog::sort_newest_first(&target_tool.manifest, &mut rows);
    if rows.is_empty() {
        return Err(ForgeError::NotFound(format!(
            "{} installs into {extends}, and no version of {extends} is installed \
             (forge tool install {extends})",
            manifest.metadata.id
        )));
    }
    let target = step.target.as_deref().or(param_target);
    let (targets, explicit) = extension_targets(&manifest, &target_tool.manifest, &rows, target)?;
    let view = manifest.release_view();
    let constraint = step
        .version
        .as_deref()
        .map(|v| constraint_for(&view, v))
        .transpose()?;
    let resolved = cancellable(
        job,
        daemon.resolver.resolve(
            ResolveRequest {
                tool: &view,
                constraint: constraint.as_ref(),
                variants: &BTreeMap::new(),
                channel: None,
            },
            LISTING_TIMEOUT,
        ),
    )
    .await?;
    let installed_ext: Vec<ToolVersionRow> = installed_all
        .iter()
        .filter(|r| r.tool_id == step.tool)
        .cloned()
        .collect();
    let matched = forge_provider::layout::extension::plan(
        &manifest,
        &target_tool.manifest,
        &targets,
        explicit,
        &resolved.releases,
        &installed_ext,
    )?;
    for item in matched.items {
        let up_to_date = item.up_to_date();
        let (size_bytes, size_estimated) = size_of(&item.release, &view);
        items.push(ToolPlanItemDto {
            tool: step.tool.clone(),
            kind: CatalogKind::Extension,
            spec: ToolSpec::from(step.spec().as_str()),
            version: item.release.version.clone(),
            variant: item.release.variants.clone(),
            canonical_name: None,
            size_bytes,
            size_estimated,
            already_installed: up_to_date,
            reason: step.reason,
            target: Some(ToolSpec::from(
                format!("{extends}@{}", item.target.version).as_str(),
            )),
        });
    }
    Ok(())
}

// -------------------------------------------------------------------- tools.releases

/// The channel a version belongs to: the first declared channel it matches (pre-releases
/// only match channels that take them, and the other way round), else `stable`/`preview`.
fn channel_of(manifest: &ToolManifest, version: &str) -> String {
    let versioning = &manifest.spec.versioning;
    let Ok(parsed) = forge_manifest::version::Version::parse(versioning.scheme, version) else {
        return "stable".to_owned();
    };
    let pre = parsed.is_prerelease();
    for (name, channel) in &versioning.channels {
        if channel.prerelease == pre
            && forge_manifest::version::channel_matches(channel, versioning.scheme, &parsed)
                .unwrap_or(false)
        {
            return name.clone();
        }
    }
    if pre { "preview" } else { "stable" }.to_owned()
}

/// The releases the providers publish for `tool` (a tool or an extension), grouped by
/// version, newest first, with the installed flag. Nothing is downloaded. `channel`
/// `None`: stable releases plus those of every channel that takes pre-releases.
/// Offline or with the providers failing, the cached list comes back with `stale`.
pub async fn releases(
    daemon: &Daemon,
    job: &JobContext,
    tool: &ToolId,
    channel: Option<&str>,
) -> Result<ToolReleasesDto> {
    let manifest: Arc<ToolManifest> = match catalog::require_installable(&daemon.registry, tool)? {
        Installable::Tool(t) => t.manifest,
        Installable::Extension(e) => Arc::new(e.manifest.release_view()),
    };
    let versioning = &manifest.spec.versioning;
    let channels: Vec<Option<String>> = match channel {
        // A tool without channels has only "stable".
        Some("stable") if !versioning.channels.contains_key("stable") => vec![None],
        Some(name) => vec![Some(name.to_owned())],
        None => std::iter::once(None)
            .chain(
                versioning
                    .channels
                    .iter()
                    .filter(|(_, c)| c.prerelease)
                    .map(|(name, _)| Some(name.clone())),
            )
            .collect(),
    };
    let mut merged: Vec<Release> = Vec::new();
    let mut seen: BTreeSet<(String, Vec<(String, String)>)> = BTreeSet::new();
    let mut stale = false;
    let mut fetched_at: Option<Timestamp> = None;
    let no_variants = BTreeMap::new();
    for (index, name) in channels.iter().enumerate() {
        job.check_cancelled()?;
        job.progress(
            JobProgress::new()
                .pct(100.0 * index as f32 / channels.len() as f32)
                .message(format!("listing the releases of {tool}")),
        );
        let resolved = cancellable(
            job,
            daemon.resolver.resolve(
                ResolveRequest {
                    tool: &manifest,
                    constraint: None,
                    variants: &no_variants,
                    channel: name.as_deref(),
                },
                LISTING_TIMEOUT,
            ),
        )
        .await;
        let resolved = match resolved {
            Ok(r) => r,
            // The first list is the answer; a failing extra channel only shortens it.
            Err(err) if index > 0 && !matches!(err, ForgeError::Cancelled(_)) => {
                tracing::warn!(%tool, ?name, %err, "channel not listed");
                continue;
            }
            Err(err) => return Err(err),
        };
        stale |= resolved.stale;
        fetched_at = match (fetched_at, resolved.fetched_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        for release in resolved.releases {
            let key = (
                release.version.clone(),
                release
                    .variants
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            );
            if seen.insert(key) {
                merged.push(release);
            }
        }
    }
    // Newest first; builds of one version keep the provider's preferred-first order.
    let scheme = versioning.scheme;
    merged.sort_by(|a, b| {
        let parse = |r: &Release| forge_manifest::version::Version::parse(scheme, &r.version).ok();
        match (parse(a), parse(b)) {
            (Some(x), Some(y)) => y.cmp(&x),
            _ => std::cmp::Ordering::Equal,
        }
    });
    let installed = catalog::installed(&daemon.reader, tool, None).await?;
    let order = versioning.variant_order();
    let mut out: Vec<ToolReleaseDto> = Vec::new();
    for release in merged {
        let build_installed = installed_row(&installed, &release).is_some();
        let variant = ReleaseVariantDto {
            canonical_name: forge_manifest::version::canonical_name(
                &release.version,
                &release.variants,
                &order,
            ),
            variant: release.variants.clone(),
            size_bytes: release.size,
            installed: build_installed,
        };
        match out.iter_mut().find(|r| r.version == release.version) {
            Some(existing) => {
                existing.installed |= build_installed;
                existing.published_at = existing.published_at.or(release.published_at);
                existing.variants.push(variant);
            }
            None => out.push(ToolReleaseDto {
                series: catalog::series_of(&manifest, &release.version),
                channel: Some(channel_of(&manifest, &release.version)),
                published_at: release.published_at,
                size_bytes: release.size,
                installed: build_installed,
                variants: vec![variant],
                version: release.version,
            }),
        }
    }
    Ok(ToolReleasesDto {
        tool: tool.clone(),
        releases: out,
        stale,
        fetched_at,
    })
}

/// Installs one spec; returns the installed (or already installed) version row.
pub async fn install_one(
    daemon: &Daemon,
    job: &JobContext,
    spec: &str,
    variant: Option<&str>,
) -> Result<ToolVersionRow> {
    let (tool_id, version) = parse_spec(spec)?;
    let tool = catalog::require_tool(&daemon.registry, &tool_id)?;
    let manifest: Arc<ToolManifest> = Arc::clone(&tool.manifest);
    job.progress(
        JobProgress::new()
            .pct(2.0)
            .message(format!("resolving {spec}")),
    );
    let mut release = cancellable(
        job,
        pick_release(
            &*daemon.resolver,
            &manifest,
            version.as_deref(),
            variant,
            RESOLVE_TIMEOUT,
        ),
    )
    .await?
    .release;
    // Already installed? Same version and variants → nothing to download.
    let installed = catalog::installed(&daemon.reader, &tool_id, Some(&manifest)).await?;
    if let Some(row) = installed_row(&installed, &release) {
        job.progress(
            JobProgress::new()
                .pct(100.0)
                .message(format!("{} {} already installed", tool_id, row.version)),
        );
        return Ok(row.clone());
    }
    job.check_cancelled()?;
    daemon.resolver.complete(&mut release).await?;
    job.progress(
        JobProgress::new()
            .pct(5.0)
            .message(format!("downloading {} {}", tool_id, release.version)),
    );
    let fetcher = DownloadFetcher::new(
        daemon.downloads.clone(),
        daemon.root.tmp_downloads(),
        DOWNLOAD_TIMEOUT,
    );
    let archive = fetcher
        .fetch(FetchArchive {
            job,
            manifest: &manifest,
            release: &release,
            priority: Priority::UserAction,
        })
        .await?;
    job.check_cancelled()?;
    job.progress(
        JobProgress::new()
            .pct(60.0)
            .message(format!("installing {} {}", tool_id, release.version)),
    );
    let install_job = daemon.layout.install_from_archive(InstallRequest {
        manifest: Arc::clone(&manifest),
        release,
        archive,
        options: InstallOptions {
            allow_run: tool.official,
            ..InstallOptions::default()
        },
    })?;
    let report: InstallReport = wait_layout_job(daemon, job, install_job).await?;
    let row = catalog::version(&daemon.reader, report.version_id).await?;
    // Daemon tools get their `default` instance right away (docs/08 §2 `data/<tool>/default`).
    if let Err(err) = daemon.services.ensure_defaults().await {
        tracing::warn!(%err, "default instance after install");
    }
    job.progress(
        JobProgress::new()
            .pct(100.0)
            .message(format!("{} {} installed", row.tool_id, row.version)),
    );
    Ok(row)
}

/// The versions of the extended tool an extension goes into (`rows`, newest first): the
/// one `target` names (`php@8.4` = newest installed 8.4, `php@8.4.27`; explicit), or all
/// of them (`None`, `php`). A version that is not installed is `version_not_installed`.
pub fn extension_targets(
    ext: &ExtensionManifest,
    target_manifest: &ToolManifest,
    rows: &[ToolVersionRow],
    target: Option<&str>,
) -> Result<(Vec<ToolVersionRow>, bool)> {
    let extends = &ext.spec.extends;
    let Some(spec) = target else {
        return Ok((rows.to_vec(), false));
    };
    let (tool, version) = parse_spec(spec)?;
    if tool != *extends {
        return Err(ForgeError::InvalidInput(format!(
            "{} goes into {extends}: --for takes {extends}@<version>, not {spec}",
            ext.metadata.id
        )));
    }
    let Some(version) = version else {
        return Ok((rows.to_vec(), false));
    };
    match catalog::pick(target_manifest, rows, &version) {
        Some(row) => Ok((vec![row.clone()], true)),
        None => Err(ForgeError::VersionNotInstalled {
            tool: extends.to_string(),
            requested: version.clone(),
            installed: rows.iter().map(|r| r.version.clone()).collect(),
            fix: format!("forge tool install {extends}@{version}"),
        }),
    }
}

/// Installs the extension `ext` (`version`: `3.5`, `3.5.3` or `None` = newest that fits)
/// into the version of the extended tool `target` names, or into every compatible
/// installed version (ADR-0043). Returns the extension's rows of those targets (already
/// installed ones included). Errors: `not_found` when the extended tool has no version
/// or no target has a compatible build (naming the compatible series).
pub async fn install_extension(
    daemon: &Daemon,
    job: &JobContext,
    ext: &ExtensionEntry,
    version: Option<&str>,
    target: Option<&str>,
) -> Result<Vec<ToolVersionRow>> {
    let manifest = Arc::clone(&ext.manifest);
    let id = manifest.metadata.id.clone();
    let extends = manifest.spec.extends.clone();
    let target_tool = catalog::require_tool(&daemon.registry, &extends)?;
    let rows = catalog::installed(&daemon.reader, &extends, Some(&target_tool.manifest)).await?;
    let (targets, explicit) = extension_targets(&manifest, &target_tool.manifest, &rows, target)?;
    let view = manifest.release_view();
    let constraint = version.map(|v| constraint_for(&view, v)).transpose()?;
    job.progress(
        JobProgress::new()
            .pct(2.0)
            .message(format!("resolving {id} for {extends}")),
    );
    let releases = cancellable(
        job,
        daemon.resolver.resolve(
            ResolveRequest {
                tool: &view,
                constraint: constraint.as_ref(),
                variants: &BTreeMap::new(),
                channel: None,
            },
            RESOLVE_TIMEOUT,
        ),
    )
    .await?
    .releases;
    let installed = catalog::installed(&daemon.reader, &id, None).await?;
    let planned = forge_provider::layout::extension::plan(
        &manifest,
        &target_tool.manifest,
        &targets,
        explicit,
        &releases,
        &installed,
    )?;
    for (folder, why) in &planned.skipped {
        tracing::info!(extension = %id, target = %folder, "skipped: {why}");
        job.progress(JobProgress::new().message(format!("{folder}: {why}")));
    }
    let fetcher = DownloadFetcher::new(
        daemon.downloads.clone(),
        daemon.root.tmp_downloads(),
        DOWNLOAD_TIMEOUT,
    );
    let total = planned.items.len().max(1) as f32;
    let mut out = Vec::new();
    // Series whose files the targets share, each with its first (newest) target.
    let mut touched: BTreeMap<String, VersionId> = BTreeMap::new();
    for (index, item) in planned.items.into_iter().enumerate() {
        job.check_cancelled()?;
        let base = 5.0 + 90.0 * index as f32 / total;
        let folder = item.target.canonical_name.clone();
        touched
            .entry(catalog::series_of(
                &target_tool.manifest,
                &item.target.version,
            ))
            .or_insert(item.target.id);
        // Same version registered and its file still there: nothing to do (a file deleted
        // by hand is installed again).
        let present = match item.installed.as_ref().filter(|_| item.up_to_date()) {
            Some(row) => tokio::fs::try_exists(&row.path).await.unwrap_or(false),
            None => false,
        };
        if present {
            job.progress(JobProgress::new().pct(base).message(format!(
                "{id} {} already installed into {extends} {folder}",
                item.release.version
            )));
            if let Some(row) = item.installed {
                out.push(row);
            }
            continue;
        }
        let mut release = item.release;
        daemon.resolver.complete(&mut release).await?;
        job.progress(JobProgress::new().pct(base).message(format!(
            "downloading {id} {} for {extends} {folder}",
            release.version
        )));
        let archive = fetcher
            .fetch(FetchArchive {
                job,
                manifest: &view,
                release: &release,
                priority: Priority::UserAction,
            })
            .await?;
        job.check_cancelled()?;
        job.progress(JobProgress::new().pct(base + 45.0 / total).message(format!(
            "installing {id} {} into {extends} {folder}",
            release.version
        )));
        let layout_job = daemon.layout.install_extension(ExtensionInstallRequest {
            manifest: Arc::clone(&manifest),
            target: ExtensionTarget {
                manifest: Arc::clone(&target_tool.manifest),
                row: item.target,
            },
            release,
            archive,
            options: InstallOptions::default(),
        })?;
        let report: ExtensionReport = wait_layout_job(daemon, job, layout_job).await?;
        out.push(catalog::version(&daemon.reader, report.version_id).await?);
    }
    if target_tool.manifest.spec.service.kind == ServiceKind::Pool {
        render_series(daemon, &target_tool.manifest, &rows, &touched).await;
    }
    job.progress(JobProgress::new().pct(100.0).message(format!(
        "{id} installed into {} version(s) of {extends}",
        out.len()
    )));
    Ok(out)
}

/// Renders the files of every series in `touched` (series → its first target) once, after
/// an extension install: the post_install render wrote the overlay off, and the series has
/// stored choices (M2-E2-T9: an extension the user turned on stays on). An already
/// installed target converges too (same files, nothing written when they match).
///
/// The series files are shared by its versions and `10-forgebox.ini` holds the
/// `extension_dir` of the version rendered, so each series renders with the version whose
/// pool runs and serves ([`crate::pools::PoolManager::series_render_version`], like
/// `php.ext_*` and `services.set`); the target only when no pool of the series runs.
/// Rendering with the target would point later workers of the serving patch (autoscale,
/// crash restart) at the target's `ext/` folder. A failed render is logged: the
/// extension is installed and the next render (pool start, toggle) writes the files.
async fn render_series(
    daemon: &Daemon,
    manifest: &ToolManifest,
    rows: &[ToolVersionRow],
    touched: &BTreeMap<String, VersionId>,
) {
    for (series, target) in touched {
        let versions: Vec<VersionId> = rows
            .iter()
            .filter(|row| catalog::series_of(manifest, &row.version) == *series)
            .map(|row| row.id)
            .collect();
        let version = daemon.pools.series_render_version(&versions, *target).await;
        if let Err(err) = daemon.pools.render_config(version).await {
            tracing::warn!(tool = %manifest.metadata.id, %series, %err, "configuration not rendered after the extension install");
        }
    }
}

/// A [`ReleaseResolver`] with canned releases and no network, for tests of the install
/// pipeline (`BootOptions::release_resolver`): per tool id, filtered and ordered like the
/// real providers (constraint, variants, stable-only without a channel, preferred first).
pub mod testing {
    use std::sync::Mutex;

    use forge_manifest::version::{Build, Version, sort_builds};

    use super::*;

    #[derive(Default)]
    struct State {
        releases: BTreeMap<String, Vec<Release>>,
        stale: bool,
        fetched_at: Option<Timestamp>,
        delay: Duration,
        calls: Vec<String>,
    }

    #[derive(Default)]
    pub struct FakeResolver {
        state: Mutex<State>,
    }

    /// A build of a tool at a made-up URL (tests that install point `url` at a local server).
    pub fn release(version: &str, variants: &[(&str, &str)], size: Option<u64>) -> Release {
        Release {
            version: version.to_owned(),
            url: format!("https://fake.invalid/{version}.zip"),
            sha256: Some("0".repeat(64)),
            sha512: None,
            size,
            variants: variants
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            published_at: None,
            signature: None,
            file_name: format!("{version}.zip"),
            checksum_url: None,
            tags: Vec::new(),
        }
    }

    impl FakeResolver {
        pub fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn state(&self) -> std::sync::MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(|e| e.into_inner())
        }

        /// The releases of `tool` (any order; builds of one version in any order).
        pub fn set(&self, tool: &str, releases: Vec<Release>) {
            self.state().releases.insert(tool.to_owned(), releases);
        }

        /// Answers with `stale: true` and this `fetched_at` (the providers were not reached).
        pub fn set_stale(&self, stale: bool, fetched_at: Option<Timestamp>) {
            let mut state = self.state();
            state.stale = stale;
            state.fetched_at = fetched_at;
        }

        /// Every answer takes this long (a slow provider).
        pub fn set_delay(&self, delay: Duration) {
            self.state().delay = delay;
        }

        /// The tools asked about so far, in order.
        pub fn calls(&self) -> Vec<String> {
            self.state().calls.clone()
        }
    }

    impl ReleaseResolver for FakeResolver {
        fn resolve<'a>(
            &'a self,
            req: ResolveRequest<'a>,
            _per_provider: Duration,
        ) -> BoxFuture<'a, Result<Resolved>> {
            Box::pin(async move {
                let id = req.tool.metadata.id.to_string();
                let (all, stale, fetched_at, delay) = {
                    let mut state = self.state();
                    state.calls.push(id.clone());
                    (
                        state.releases.get(&id).cloned().unwrap_or_default(),
                        state.stale,
                        state.fetched_at,
                        state.delay,
                    )
                };
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let versioning = &req.tool.spec.versioning;
                let mut kept: Vec<(Version, Release)> = all
                    .into_iter()
                    .filter_map(|r| {
                        let v = Version::parse(versioning.scheme, &r.version).ok()?;
                        let channel = req.channel.and_then(|c| versioning.channels.get(c));
                        let allow_pre = channel.is_some_and(|c| c.prerelease);
                        if req.channel.is_none() && v.is_prerelease() {
                            return None;
                        }
                        if let Some(c) = channel {
                            if !forge_manifest::version::channel_matches(c, versioning.scheme, &v)
                                .unwrap_or(false)
                            {
                                return None;
                            }
                        }
                        if let Some(constraint) = req.constraint {
                            if !constraint.matches_with(&v, allow_pre) {
                                return None;
                            }
                        }
                        req.variants
                            .iter()
                            .all(|(k, want)| r.variants.get(k) == Some(want))
                            .then_some((v, r))
                    })
                    .collect();
                sort_builds(&mut kept, &versioning.variants, |(v, r)| Build {
                    version: v,
                    variants: &r.variants,
                });
                Ok(Resolved {
                    releases: kept.into_iter().map(|(_, r)| r).collect(),
                    stale,
                    fetched_at,
                })
            })
        }

        fn complete<'a>(&'a self, _release: &'a mut Release) -> BoxFuture<'a, Result<()>> {
            Box::pin(async { Ok(()) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn php() -> ToolManifest {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/tools/php.yaml"),
        )
        .unwrap();
        match forge_manifest::load_str(&text).unwrap() {
            forge_manifest::Manifest::Tool(t) => t,
            _ => unreachable!(),
        }
    }

    #[test]
    fn specs_split_tool_and_version() {
        let (tool, version) = parse_spec("PHP@8.3").unwrap();
        assert_eq!((tool.as_str(), version.as_deref()), ("php", Some("8.3")));
        let (tool, version) = parse_spec("nginx").unwrap();
        assert_eq!((tool.as_str(), version), ("nginx", None));
        assert!(parse_spec("bad tool@1").is_err());
    }

    #[test]
    fn series_become_tilde_constraints() {
        let manifest = php();
        let series = constraint_for(&manifest, "8.3").unwrap();
        let scheme = manifest.spec.versioning.scheme;
        let v = |s: &str| forge_manifest::version::Version::parse(scheme, s).unwrap();
        assert!(series.matches(&v("8.3.36")));
        assert!(!series.matches(&v("8.4.1")));
        let exact = constraint_for(&manifest, "8.3.36").unwrap();
        assert!(exact.matches(&v("8.3.36")));
        assert!(!exact.matches(&v("8.3.37")));
    }

    #[test]
    fn variant_values_map_to_their_key() {
        let manifest = php();
        assert_eq!(
            variant_filter(&manifest, Some("ts")).unwrap(),
            BTreeMap::from([("ts".to_owned(), "ts".to_owned())])
        );
        assert!(variant_filter(&manifest, None).unwrap().is_empty());
        assert!(variant_filter(&manifest, Some("arm64")).is_err());
    }

    fn xdebug() -> ExtensionManifest {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../registry/tools/xdebug.yaml"),
        )
        .unwrap();
        match forge_manifest::load_str(&text).unwrap() {
            forge_manifest::Manifest::Extension(e) => e,
            _ => unreachable!(),
        }
    }

    fn php_row(version: &str) -> ToolVersionRow {
        ToolVersionRow {
            id: forge_core::ids::VersionId::new(),
            tool_id: ToolId::new("php").unwrap(),
            version: version.into(),
            canonical_name: format!("{version}-nts-vs17-x64"),
            variant: BTreeMap::new(),
            path: format!("C:/fb/bin/php/{version}-nts-vs17-x64"),
            state: forge_core::store::VersionState::Installed,
            source: forge_core::store::VersionSource::Registry,
        }
    }

    #[test]
    fn extension_targets_follow_the_for_spec() {
        let (ext, php) = (xdebug(), php());
        let rows = [php_row("8.4.27"), php_row("8.4.26"), php_row("8.3.36")];
        let (all, explicit) = extension_targets(&ext, &php, &rows, None).unwrap();
        assert_eq!((all.len(), explicit), (3, false));
        let (all, explicit) = extension_targets(&ext, &php, &rows, Some("php")).unwrap();
        assert_eq!((all.len(), explicit), (3, false));
        // A series is its newest installed version; an exact version is that one.
        let (one, explicit) = extension_targets(&ext, &php, &rows, Some("php@8.4")).unwrap();
        assert!(explicit);
        assert_eq!(one[0].version, "8.4.27");
        let (one, _) = extension_targets(&ext, &php, &rows, Some("php@8.4.26")).unwrap();
        assert_eq!(one[0].version, "8.4.26");
        let err = extension_targets(&ext, &php, &rows, Some("php@8.2")).unwrap_err();
        assert_eq!(err.code(), "not_found");
        assert_eq!(err.details().unwrap()["reason"], "version_not_installed");
        let err = extension_targets(&ext, &php, &rows, Some("nginx@1.30")).unwrap_err();
        assert_eq!(err.code(), "invalid_input");
    }

    fn item(size: Option<u64>, installed: bool) -> ToolPlanItemDto {
        ToolPlanItemDto {
            tool: ToolId::new("php").unwrap(),
            kind: CatalogKind::Tool,
            spec: ToolSpec::from("php"),
            version: "8.4.27".into(),
            variant: BTreeMap::new(),
            canonical_name: None,
            size_bytes: size,
            size_estimated: false,
            already_installed: installed,
            reason: ToolPlanReason::Requested,
            target: None,
        }
    }

    #[test]
    fn plan_totals_sum_known_sizes_and_count_the_unknown() {
        let items = [
            item(Some(100), false),
            item(Some(50), false),
            item(None, false),
            item(None, false),
            // Installed: nothing to download, whatever its size.
            item(Some(9_000), true),
            item(None, true),
        ];
        assert_eq!(totals(&items), (150, 2));
        assert_eq!(totals(&[]), (0, 0));
    }

    #[test]
    fn a_release_is_installed_when_version_and_variant_values_match() {
        let mut row = php_row("8.4.27");
        row.variant = BTreeMap::from([("ts".to_owned(), "nts".to_owned())]);
        let rows = [row];
        let nts = testing::release("8.4.27", &[("ts", "nts"), ("arch", "x64")], None);
        let ts = testing::release("8.4.27", &[("ts", "ts")], None);
        let newer = testing::release("8.4.28", &[("ts", "nts")], None);
        // The row records only some keys: the release's keys the row has must agree.
        assert!(
            installed_row(&rows, &testing::release("8.4.27", &[("ts", "nts")], None)).is_some()
        );
        assert!(installed_row(&rows, &ts).is_none());
        assert!(installed_row(&rows, &newer).is_none());
        assert!(
            installed_row(&rows, &nts).is_none(),
            "a key the row lacks is a different build"
        );
    }
}
