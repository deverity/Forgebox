//! Update engine (docs/05 §4, docs/01 §5.3, docs/03 §6): policies, checks, side-by-side
//! updates with blue/green pool switching, retention, history and rollback.
//!
//! [`UpdateEngine`] is an actor (`update-engine`, ADR-0011) that owns the scheduling
//! state (last check per tool) and the *in-flight* table: at most one update or rollback
//! per `(tool, series)` at a time (asking again for the same target returns the running
//! job). Everything slow runs outside its mailbox: checks in tasks that answer the caller
//! directly, updates and rollbacks as jobs ([`forge_core::jobs`]) that report back when
//! they end. Every message has a timeout.
//!
//! * [`UpdateEngine::check`] resolves releases ([`ReleaseSource`]), groups installed
//!   versions into update lines and picks the best release in reach of the effective
//!   policy ([`policy`]): `manual | notify | auto-patch | auto-minor | auto`, databases
//!   capped at `notify`. A line an update moved to another series is *superseded* and
//!   skipped, so the retained old version is not offered again.
//! * [`UpdateEngine::apply`] returns a `JobId` at once. The job: records a `started`
//!   history row → downloads ([`ArchiveFetcher`]) and installs the new version **side by
//!   side** ([`Layout::install_from_archive`], `current` untouched) unless it is already
//!   installed → `update.migrate` (`copy_dir` of `ext/`, `only_missing`) → re-renders the
//!   series configuration for the new version ([`Layout::rerender`], the injected
//!   `TemplateRenderer`) → blue/green ([`PoolSwitcher`], tools with `service.kind: pool`)
//!   → moves `current` when the CLI version was in the same line → marks the history row
//!   `completed` → applies `retention` (versions of the line older than the new one,
//!   beyond `update.retention`, are removed; never before the switch succeeded, never
//!   `current`, a version a pool still serves or one used by instances) → `tool.updated`.
//!   A failure marks the row `failed`; a failed switch re-renders the configuration back
//!   for the old version, which keeps serving, and removes the version the job had just
//!   installed.
//! * [`UpdateEngine::rollback`] undoes the newest `completed` update of a series:
//!   re-renders for the retained version, switches the pools back, moves `current` back,
//!   records a `rollback` row (`rollback_of`) and marks the update `rolled_back`;
//!   `tool.rollback`. A rolled back line is *pinned*: nothing is applied automatically
//!   until the user updates it.
//! * Scheduler hooks: [`UpdateEngine::next_check_at`] (interval + jitter, quiet hours,
//!   metered connection through the injected [`MeteredProbe`]) and
//!   [`UpdateEngine::run_scheduled`] (re-checks quiet hours/metered at run time, checks
//!   and applies the automatic updates with download priority `AutoUpdate`).

mod deps;
mod migrate;
pub mod policy;
pub mod schedule;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forge_core::Timestamp;
use forge_core::actor::{
    Actor, ActorHandle, ActorRef, ActorSpec, ActorSupervisor, Context, Reply, WeakActorRef,
};
use forge_core::config::UpdatesConfig;
use forge_core::error::{ForgeError, Result};
use forge_core::event::EventKind;
use forge_core::ids::{JobId, RequestId, ToolId, UpdateId, VersionId};
use forge_core::jobs::{JobContext, JobInfo, JobProgress, JobSpec, JobState};
use forge_core::paths::posix_string;
use forge_core::store::{
    NewUpdateHistory, StoreReader, StoreWriter, ToolVersionRepo, ToolVersionRow, UpdateHistoryRepo,
    UpdateHistoryRow, UpdateStatus, UpdateTrigger, VersionState,
};
use forge_manifest::schema::ToolManifest;
use forge_manifest::schema::tool::{Layout as InstallLayout, ServiceKind};
use forge_manifest::version::Version;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

pub use self::deps::{
    ArchiveFetcher, CatalogTool, DownloadFetcher, FetchArchive, MeteredProbe, NoPools, PoolSwitch,
    PoolSwitcher, ReleaseSource, StaticCatalog, ToolCatalog, fixed_metered,
};
pub use self::policy::{AvailableUpdate, GroupKey, UpdateKind};
pub use self::schedule::{DeferReason, NextCheck};
use crate::download::Priority;
use crate::layout::{CurrentPolicy, InstallOptions, InstallReport, InstallRequest, Layout};
use crate::messages;

/// Version of [`ToolUpdateEventPayload`].
pub const TOOL_UPDATE_EVENT_VERSION: u32 = 1;
/// Name of the actor.
pub const UPDATE_ENGINE_ACTOR: &str = "update-engine";
const MAILBOX_CAPACITY: usize = 64;

/// Timeouts and scheduling knobs. [`UpdateConfig::new`] gives the defaults.
#[derive(Debug, Clone)]
pub struct UpdateConfig {
    /// `[updates]` of `forgebox.toml`: quiet hours and metered policy.
    pub updates: UpdatesConfig,
    /// Time zone of `quiet_hours` (the system's by default).
    pub time_zone: TimeZone,
    /// ± fraction of `check_interval` (0.1).
    pub jitter: f64,
    /// First check of a tool never checked before (60 s after the engine asks).
    pub first_check_delay: Duration,
    /// How long a metered connection postpones a check (30 min).
    pub metered_retry: Duration,
    /// Quick messages to the actor (5 s).
    pub ask_timeout: Duration,
    /// Resolving the releases of one tool (120 s).
    pub check_timeout: Duration,
    /// Download + installation of the new version (30 min).
    pub install_timeout: Duration,
    /// One blue/green switch (5 min).
    pub switch_timeout: Duration,
    /// History rows read per tool (200).
    pub history_scan: usize,
}

impl UpdateConfig {
    pub fn new(updates: UpdatesConfig) -> Self {
        Self {
            updates,
            time_zone: TimeZone::system(),
            jitter: 0.1,
            first_check_delay: Duration::from_secs(60),
            metered_retry: Duration::from_secs(30 * 60),
            ask_timeout: Duration::from_secs(5),
            check_timeout: Duration::from_secs(120),
            install_timeout: Duration::from_secs(30 * 60),
            switch_timeout: Duration::from_secs(5 * 60),
            history_scan: 200,
        }
    }
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self::new(UpdatesConfig::default())
    }
}

/// Collaborators of the engine (see [`deps`](self::ToolCatalog)).
#[derive(Clone)]
pub struct UpdateDeps {
    /// Installs, re-renders, moves `current`, removes versions; its job registry runs the
    /// update jobs and its event bus gets `tool.updated|rollback`.
    pub layout: Layout,
    pub reader: StoreReader,
    pub writer: StoreWriter,
    pub catalog: Arc<dyn ToolCatalog>,
    pub releases: Arc<dyn ReleaseSource>,
    pub fetcher: Arc<dyn ArchiveFetcher>,
    pub pools: Arc<dyn PoolSwitcher>,
    pub metered: MeteredProbe,
}

/// An update to apply.
#[derive(Debug, Clone)]
pub struct ApplyRequest {
    pub update: AvailableUpdate,
    /// `Auto` (scheduler) or `User`.
    pub trigger: UpdateTrigger,
    /// Idempotency key of the job (ADR-0009).
    pub request_id: Option<RequestId>,
}

impl ApplyRequest {
    /// Asked for by the user.
    pub fn user(update: AvailableUpdate) -> Self {
        Self {
            update,
            trigger: UpdateTrigger::User,
            request_id: None,
        }
    }

    /// Applied by the scheduler.
    pub fn auto(update: AvailableUpdate) -> Self {
        Self {
            update,
            trigger: UpdateTrigger::Auto,
            request_id: None,
        }
    }
}

/// A rollback to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackRequest {
    pub tool: ToolId,
    /// Series to roll back (`8.3`).
    pub series: String,
    pub request_id: Option<RequestId>,
}

impl RollbackRequest {
    pub fn new(tool: ToolId, series: impl Into<String>) -> Self {
        Self {
            tool,
            series: series.into(),
            request_id: None,
        }
    }
}

/// Result of an update job (`job.done`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateReport {
    pub tool: ToolId,
    pub series: String,
    pub from: String,
    pub to: String,
    pub from_id: VersionId,
    pub to_id: VersionId,
    pub to_canonical: String,
    pub update_id: UpdateId,
    /// The version was downloaded and installed by this job (not already there).
    pub installed: bool,
    pub pools_switched: bool,
    /// `current` was moved to the new version.
    pub current_moved: bool,
    /// Files carried over by `update.migrate` (relative to the new version).
    pub migrated: Vec<String>,
    /// Configuration re-rendered for the new version.
    pub rendered: Vec<PathBuf>,
    /// Folders removed by retention.
    pub removed: Vec<String>,
    pub warnings: Vec<String>,
}

/// Result of a rollback job (`job.done`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackReport {
    pub tool: ToolId,
    pub series: String,
    /// Version that served before the rollback and the retained one serving now.
    pub from: String,
    pub to: String,
    pub from_id: VersionId,
    pub to_id: VersionId,
    pub update_id: UpdateId,
    /// The update this rollback undid.
    pub rollback_of: UpdateId,
    pub pools_switched: bool,
    pub current_moved: bool,
    pub rendered: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

/// Payload of `tool.updated` / `tool.rollback`: the fields of `forge_ipc`'s
/// `ToolVersionChangePayload` (docs/03 §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolUpdateEventPayload {
    pub v: u32,
    pub tool: ToolId,
    pub from: String,
    pub to: String,
    /// Version serving now.
    pub version_id: VersionId,
    pub series: Option<String>,
    pub job_id: Option<JobId>,
    pub update_id: Option<UpdateId>,
    pub rollback_of: Option<UpdateId>,
    /// Folders removed by retention.
    pub removed: Vec<String>,
}

/// Outcome of a scheduled run of one tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledRun {
    pub tool: ToolId,
    /// Nothing was done (quiet hours, metered connection).
    pub deferred: Option<DeferReason>,
    pub updates: Vec<AvailableUpdate>,
    /// Jobs of the automatic updates started.
    pub started: Vec<JobId>,
}

/// Snapshot of the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateEngineStatus {
    /// `(tool, series, job)` of the running updates and rollbacks.
    pub in_flight: Vec<(ToolId, String, JobId)>,
    pub last_checks: Vec<(ToolId, Timestamp)>,
}

/// Rebuilds an error from a failed or cancelled job snapshot.
pub(crate) fn error_from_job(info: &JobInfo) -> ForgeError {
    if info.state == JobState::Cancelled {
        return ForgeError::Cancelled(format!("job {}", info.id));
    }
    let Some(error) = &info.error else {
        return ForgeError::Internal(format!("job {} ended as {:?}", info.id, info.state));
    };
    let message = error.message.clone();
    match error.code.as_str() {
        "not_found" => ForgeError::NotFound(message),
        "conflict" => ForgeError::Conflict(message),
        "verification_failed" => ForgeError::VerificationFailed(message),
        "config_invalid" => ForgeError::ConfigInvalid(message),
        "invalid_input" => ForgeError::InvalidInput(message),
        "not_implemented" => ForgeError::NotImplemented(message),
        "cancelled" => ForgeError::Cancelled(message),
        "forbidden" => ForgeError::Forbidden(message),
        "needs_elevation" => ForgeError::NeedsElevation(message),
        "unsupported_platform" => ForgeError::UnsupportedPlatform(message),
        _ => ForgeError::Internal(message),
    }
}

/// Same folder, compared as lowercase POSIX strings without a trailing `/`.
fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        posix_string(p)
            .trim_start_matches("//?/")
            .trim_end_matches('/')
            .to_lowercase()
    };
    norm(a) == norm(b)
}

fn parse_version(manifest: &ToolManifest, text: &str) -> Option<Version> {
    Version::parse(manifest.spec.versioning.scheme, text).ok()
}

async fn blocking<T, F>(what: &'static str, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ForgeError::Internal(format!("{what} task failed: {e}")))?
}

// ------------------------------------------------------------------------- core

/// Shared, immutable part: the work of checks and jobs.
struct Core {
    deps: UpdateDeps,
    config: UpdateConfig,
}

/// What the actor knows of a tool to schedule it.
struct ToolSnapshot {
    manifest: Arc<ToolManifest>,
    policy: forge_manifest::schema::tool::UpdatePolicy,
}

impl Core {
    fn publish<T: Serialize>(&self, kind: EventKind, payload: &T) {
        match serde_json::to_value(payload) {
            Ok(value) => {
                self.deps.layout.jobs().events().publish(kind, value);
            }
            Err(e) => tracing::error!(%kind, "event payload not serializable: {e}"),
        }
    }

    fn catalog_tool(&self, tool: &ToolId) -> Result<CatalogTool> {
        self.deps
            .catalog
            .tool(tool)
            .ok_or_else(|| ForgeError::NotFound(format!("manifest of {tool}")))
    }

    async fn snapshot(&self, tool: &ToolId) -> Result<ToolSnapshot> {
        let cat = self.catalog_tool(tool)?;
        let t = tool.clone();
        let override_policy = self
            .deps
            .reader
            .query(move |conn| ToolVersionRepo::new(conn).update_policy(&t))
            .await?;
        Ok(ToolSnapshot {
            policy: policy::effective_policy(&cat.manifest, override_policy.as_deref()),
            manifest: cat.manifest,
        })
    }

    async fn row(&self, id: VersionId) -> Result<Option<ToolVersionRow>> {
        self.deps
            .reader
            .query(move |conn| ToolVersionRepo::new(conn).get(id))
            .await
    }

    async fn rows(&self, tool: &ToolId) -> Result<Vec<ToolVersionRow>> {
        let tool = tool.clone();
        self.deps
            .reader
            .query(move |conn| ToolVersionRepo::new(conn).list(Some(&tool)))
            .await
    }

    async fn history(&self, tool: &ToolId) -> Result<Vec<UpdateHistoryRow>> {
        let (tool, limit) = (tool.clone(), self.config.history_scan);
        self.deps
            .reader
            .query(move |conn| UpdateHistoryRepo::new(conn).list(Some(&tool), limit))
            .await
    }

    /// Updates of one tool (module docs).
    async fn check_tool(&self, tool: &ToolId) -> Result<Vec<AvailableUpdate>> {
        let snapshot = self.snapshot(tool).await?;
        let t = tool.clone();
        let limit = self.config.history_scan;
        let (rows, history) = self
            .deps
            .reader
            .query(move |conn| {
                Ok((
                    ToolVersionRepo::new(conn).list(Some(&t))?,
                    UpdateHistoryRepo::new(conn).list(Some(&t), limit)?,
                ))
            })
            .await?;
        if !rows.iter().any(|r| r.state == VersionState::Installed) {
            return Ok(Vec::new());
        }
        let manifest = &snapshot.manifest;
        let releases = tokio::time::timeout(
            self.config.check_timeout,
            self.deps.releases.releases(manifest),
        )
        .await
        .map_err(|_| ForgeError::Timeout {
            operation: format!("resolving the releases of {tool}"),
            after: self.config.check_timeout,
        })??;
        Ok(policy::plan(
            manifest,
            snapshot.policy,
            &rows,
            &history,
            &releases,
        ))
    }

    /// `tool` or every installed tool with a manifest. With every tool, a tool that fails
    /// is logged and skipped.
    async fn check(&self, tool: Option<ToolId>) -> Result<(Vec<ToolId>, Vec<AvailableUpdate>)> {
        if let Some(tool) = tool {
            let found = self.check_tool(&tool).await?;
            return Ok((vec![tool], found));
        }
        let rows = self
            .deps
            .reader
            .query(|conn| ToolVersionRepo::new(conn).list(None))
            .await?;
        let tools: BTreeSet<ToolId> = rows
            .into_iter()
            .filter(|r| r.state == VersionState::Installed)
            .map(|r| r.tool_id)
            .collect();
        let mut checked = Vec::new();
        let mut out = Vec::new();
        for tool in tools {
            if self.deps.catalog.tool(&tool).is_none() {
                tracing::debug!(%tool, "update check: no manifest, skipped");
                continue;
            }
            match self.check_tool(&tool).await {
                Ok(found) => {
                    checked.push(tool);
                    out.extend(found);
                }
                Err(e) => tracing::warn!(%tool, "update check failed: {e}"),
            }
        }
        Ok((checked, out))
    }

    async fn wait_install(&self, ctx: &JobContext, id: JobId) -> Result<InstallReport> {
        let handle = self
            .deps
            .layout
            .jobs()
            .handle(id)
            .ok_or_else(|| ForgeError::Internal(format!("install job {id} vanished")))?;
        let info = tokio::select! {
            info = handle.wait(self.config.install_timeout) => info?,
            () = ctx.cancelled() => {
                handle.cancel();
                return Err(ForgeError::Cancelled(format!("install job {id}")));
            }
        };
        if info.state != JobState::Done {
            return Err(error_from_job(&info));
        }
        serde_json::from_value(info.result.unwrap_or_default())
            .map_err(|e| ForgeError::Internal(format!("install result: {e}")))
    }

    async fn switch(&self, manifest: &ToolManifest, switch: PoolSwitch) -> Result<bool> {
        if manifest.spec.service.kind != ServiceKind::Pool {
            return Ok(false);
        }
        tokio::time::timeout(self.config.switch_timeout, self.deps.pools.switch(&switch))
            .await
            .map_err(|_| ForgeError::Timeout {
                operation: format!(
                    "switching the {} {} pools {} -> {}",
                    switch.tool, switch.series, switch.from_version, switch.to_version
                ),
                after: self.config.switch_timeout,
            })??;
        Ok(true)
    }

    /// Points `current` at `to` when it pointed at a version of `from`'s line.
    async fn follow_current(
        &self,
        manifest: &Arc<ToolManifest>,
        from: &ToolVersionRow,
        to: &ToolVersionRow,
    ) -> Result<bool> {
        let install = &manifest.spec.install;
        if !install.current_link || install.layout != InstallLayout::Versioned {
            return Ok(false);
        }
        let layout = &self.deps.layout;
        let Some(current) = layout.current_version(&from.tool_id).await? else {
            return Ok(false);
        };
        if same_dir(&current.path, Path::new(&to.path)) {
            return Ok(false);
        }
        let rows = self.rows(&from.tool_id).await?;
        let line = GroupKey::of_row(manifest, from);
        let in_line = rows.iter().any(|r| {
            same_dir(&current.path, Path::new(&r.path)) && GroupKey::of_row(manifest, r) == line
        });
        if !in_line {
            return Ok(false);
        }
        let report = layout.use_version(manifest, &to.canonical_name).await?;
        Ok(!report.unchanged)
    }

    /// Removes versions of the line beyond `update.retention` (module docs).
    async fn retention(
        &self,
        manifest: &Arc<ToolManifest>,
        from: &ToolVersionRow,
        to: &ToolVersionRow,
    ) -> (Vec<String>, Vec<String>) {
        let (mut removed, mut warnings) = (Vec::new(), Vec::new());
        let rows = match self.rows(&to.tool_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warnings.push(format!("retention: {e}"));
                return (removed, warnings);
            }
        };
        let Some(to_version) = parse_version(manifest, &to.version) else {
            return (removed, warnings);
        };
        let series = [
            policy::series_of(manifest, &from.version),
            policy::series_of(manifest, &to.version),
        ];
        let line = GroupKey::of_row(manifest, to);
        let mut older: Vec<&ToolVersionRow> = rows
            .iter()
            .filter(|r| {
                r.id != to.id
                    && r.state == VersionState::Installed
                    && line.same_line(manifest, &r.variant)
                    && series.contains(&policy::series_of(manifest, &r.version))
                    && parse_version(manifest, &r.version).is_some_and(|v| v < to_version)
            })
            .collect();
        older.sort_by(|a, b| policy::cmp_rows(manifest, a, b));
        let keep = usize::try_from(manifest.spec.update.retention).unwrap_or(usize::MAX);
        for row in older.into_iter().skip(keep) {
            match self.deps.pools.serving(&row.tool_id, row.id).await {
                Ok(false) => {}
                Ok(true) => {
                    warnings.push(messages::retention_kept_serving(&row.canonical_name));
                    continue;
                }
                Err(e) => {
                    warnings.push(format!("retention: {}: {e}", row.canonical_name));
                    continue;
                }
            }
            match self
                .deps
                .layout
                .remove_version(Arc::clone(manifest), row.id)
                .await
            {
                Ok(report) => {
                    removed.push(report.canonical_name);
                    warnings.extend(report.warnings);
                }
                Err(e) => warnings.push(format!("retention: {}: {e}", row.canonical_name)),
            }
        }
        (removed, warnings)
    }

    // ------------------------------------------------------------------ apply

    async fn run_apply(&self, ctx: JobContext, req: ApplyRequest) -> Result<UpdateReport> {
        let update = &req.update;
        let cat = self.catalog_tool(&update.tool)?;
        let from = self
            .row(update.from_id)
            .await?
            .filter(|r| r.state == VersionState::Installed)
            .ok_or_else(|| ForgeError::Conflict(messages::update_from_changed(&update.from)))?;
        let new = NewUpdateHistory {
            tool_id: update.tool.clone(),
            series: Some(update.series.clone()),
            from_version_id: Some(from.id),
            to_version_id: None,
            from_version: Some(from.version.clone()),
            to_version: update.to.clone(),
            policy: Some(policy::policy_str(update.policy).to_owned()),
            triggered_by: req.trigger,
            status: UpdateStatus::Started,
            job_id: Some(ctx.id()),
            rollback_of: None,
        };
        let update_id = self
            .deps
            .writer
            .execute(move |conn| UpdateHistoryRepo::new(conn).insert(&new))
            .await?;
        let outcome = self.apply_steps(&ctx, &req, &cat, &from, update_id).await;
        let (status, to_id, error) = match &outcome {
            Ok(report) => (UpdateStatus::Completed, Some(report.to_id), None),
            Err((to_id, err)) => (UpdateStatus::Failed, *to_id, Some(err.to_string())),
        };
        let finished = self
            .deps
            .writer
            .execute(move |conn| {
                UpdateHistoryRepo::new(conn).finish(update_id, status, to_id, error.as_deref())
            })
            .await;
        match outcome {
            Ok(report) => {
                finished?;
                self.publish(
                    EventKind::ToolUpdated,
                    &ToolUpdateEventPayload {
                        v: TOOL_UPDATE_EVENT_VERSION,
                        tool: report.tool.clone(),
                        from: report.from.clone(),
                        to: report.to.clone(),
                        version_id: report.to_id,
                        series: Some(report.series.clone()),
                        job_id: Some(ctx.id()),
                        update_id: Some(update_id),
                        rollback_of: None,
                        removed: report.removed.clone(),
                    },
                );
                tracing::info!(
                    tool = %report.tool,
                    from = %report.from,
                    to = %report.to,
                    removed = ?report.removed,
                    "tool updated"
                );
                Ok(report)
            }
            Err((_, err)) => {
                if let Err(e) = finished {
                    tracing::error!(%update_id, "could not record the failed update: {e}");
                }
                tracing::warn!(tool = %update.tool, from = %update.from, to = %update.to, "update failed: {err}");
                Err(err)
            }
        }
    }

    async fn apply_steps(
        &self,
        ctx: &JobContext,
        req: &ApplyRequest,
        cat: &CatalogTool,
        from: &ToolVersionRow,
        update_id: UpdateId,
    ) -> std::result::Result<UpdateReport, (Option<VersionId>, ForgeError)> {
        let update = &req.update;
        let manifest = &cat.manifest;
        let fail = |e: ForgeError| (None, e);
        let step = |message: String| ctx.progress(JobProgress::new().message(message));

        // 1. The new version, side by side.
        let tool = update.tool.clone();
        let canonical = update.to_canonical.clone();
        let existing = self
            .deps
            .reader
            .query(move |conn| ToolVersionRepo::new(conn).find(&tool, &canonical))
            .await
            .map_err(fail)?
            .filter(|r| r.state == VersionState::Installed);
        let (to, installed) = match existing {
            Some(row) => (row, false),
            None => {
                step(messages::update_downloading(
                    update.tool.as_str(),
                    &update.to,
                ));
                let mut release = update.release.clone();
                self.deps
                    .releases
                    .complete(&mut release)
                    .await
                    .map_err(fail)?;
                let priority = match req.trigger {
                    UpdateTrigger::Auto => Priority::AutoUpdate,
                    _ => Priority::UserAction,
                };
                let archive = self
                    .deps
                    .fetcher
                    .fetch(FetchArchive {
                        job: ctx,
                        manifest,
                        release: &release,
                        priority,
                    })
                    .await
                    .map_err(fail)?;
                ctx.check_cancelled().map_err(fail)?;
                step(messages::update_installing(
                    update.tool.as_str(),
                    &update.to,
                ));
                let install = self
                    .deps
                    .layout
                    .install_from_archive(InstallRequest {
                        manifest: Arc::clone(manifest),
                        release,
                        archive,
                        options: InstallOptions {
                            current: CurrentPolicy::Never,
                            allow_run: cat.allow_run,
                            ..InstallOptions::default()
                        },
                    })
                    .map_err(fail)?;
                let report = self.wait_install(ctx, install).await.map_err(fail)?;
                let row = self
                    .row(report.version_id)
                    .await
                    .map_err(fail)?
                    .ok_or_else(|| {
                        fail(ForgeError::Internal(format!(
                            "installed version {} is not registered",
                            report.version_id
                        )))
                    })?;
                (row, true)
            }
        };
        let fail = |e: ForgeError| (Some(to.id), e);
        ctx.check_cancelled().map_err(fail)?;

        // 2. update.migrate (ext/ of the old version).
        step(messages::update_migrating());
        let (old_dir, new_dir) = (PathBuf::from(&from.path), PathBuf::from(&to.path));
        let actions = manifest.spec.update.migrate.clone();
        let job = ctx.clone();
        let migrated = blocking("update.migrate", move || {
            let cancelled = || job.is_cancelled();
            migrate::run(&old_dir, &new_dir, &actions, &cancelled)
        })
        .await
        .map_err(fail)?;

        // 3. Series configuration for the new version.
        step(messages::update_rendering());
        let rendered = self
            .deps
            .layout
            .rerender(Arc::clone(manifest), &to)
            .await
            .map_err(fail)?;

        // 4. Blue/green.
        step(messages::update_switching(&update.series));
        let switch = PoolSwitch {
            tool: update.tool.clone(),
            series: update.series.clone(),
            from: from.id,
            to: to.id,
            from_version: from.version.clone(),
            to_version: to.version.clone(),
        };
        let pools_switched = match self.switch(manifest, switch).await {
            Ok(switched) => switched,
            Err(err) => {
                // The old version keeps serving: its configuration comes back, and a
                // version this job installed goes away (it never passed its health check).
                if let Err(e) = self.deps.layout.rerender(Arc::clone(manifest), from).await {
                    tracing::error!(tool = %from.tool_id, "could not restore the configuration: {e}");
                }
                if installed {
                    match self
                        .deps
                        .layout
                        .remove_version(Arc::clone(manifest), to.id)
                        .await
                    {
                        Ok(_) => return Err((None, err)),
                        Err(e) => tracing::warn!(
                            folder = %to.canonical_name,
                            "could not remove the version that failed to switch: {e}"
                        ),
                    }
                }
                return Err(fail(err));
            }
        };

        // 5. current, when the CLI version was in this line.
        let mut warnings = Vec::new();
        let current_moved = match self.follow_current(manifest, from, &to).await {
            Ok(moved) => moved,
            Err(e) => {
                warnings.push(format!("current: {e}"));
                false
            }
        };

        // 6. Retention (only now: the new version passed its health check in the switch).
        step(messages::update_retention());
        let (removed, retention_warnings) = self.retention(manifest, from, &to).await;
        warnings.extend(retention_warnings);

        Ok(UpdateReport {
            tool: update.tool.clone(),
            series: update.series.clone(),
            from: from.version.clone(),
            to: to.version.clone(),
            from_id: from.id,
            to_id: to.id,
            to_canonical: to.canonical_name.clone(),
            update_id,
            installed,
            pools_switched,
            current_moved,
            migrated,
            rendered,
            removed,
            warnings,
        })
    }

    // ------------------------------------------------------------------ rollback

    async fn run_rollback(&self, ctx: JobContext, req: RollbackRequest) -> Result<RollbackReport> {
        let cat = self.catalog_tool(&req.tool)?;
        let manifest = &cat.manifest;
        let history = self.history(&req.tool).await?;
        let target = history
            .iter()
            .find(|h| {
                h.triggered_by != UpdateTrigger::Rollback
                    && h.status == UpdateStatus::Completed
                    && h.series.as_deref() == Some(req.series.as_str())
            })
            .cloned()
            .ok_or_else(|| {
                ForgeError::NotFound(messages::rollback_nothing(req.tool.as_str(), &req.series))
            })?;
        let not_retained = || {
            ForgeError::NotFound(messages::rollback_not_retained(
                target.from_version.as_deref().unwrap_or("?"),
            ))
        };
        let retained = match target.from_version_id {
            Some(id) => self.row(id).await?,
            None => None,
        }
        .filter(|r| r.state == VersionState::Installed)
        .ok_or_else(not_retained)?;
        let active = match target.to_version_id {
            Some(id) => self.row(id).await?,
            None => None,
        }
        .filter(|r| r.state == VersionState::Installed)
        .ok_or_else(|| ForgeError::NotFound(messages::update_from_changed(&target.to_version)))?;

        let outcome = self
            .rollback_steps(&ctx, manifest, &req, &active, &retained)
            .await;
        let new = NewUpdateHistory {
            tool_id: req.tool.clone(),
            series: Some(req.series.clone()),
            from_version_id: Some(active.id),
            to_version_id: Some(retained.id),
            from_version: Some(active.version.clone()),
            to_version: retained.version.clone(),
            policy: target.policy.clone(),
            triggered_by: UpdateTrigger::Rollback,
            status: UpdateStatus::Completed,
            job_id: Some(ctx.id()),
            rollback_of: Some(target.id),
        };
        match outcome {
            Ok((pools_switched, current_moved, rendered, warnings)) => {
                let of = target.id;
                let update_id = self
                    .deps
                    .writer
                    .execute(move |conn| UpdateHistoryRepo::new(conn).record_rollback(of, &new))
                    .await?;
                self.publish(
                    EventKind::ToolRollback,
                    &ToolUpdateEventPayload {
                        v: TOOL_UPDATE_EVENT_VERSION,
                        tool: req.tool.clone(),
                        from: active.version.clone(),
                        to: retained.version.clone(),
                        version_id: retained.id,
                        series: Some(req.series.clone()),
                        job_id: Some(ctx.id()),
                        update_id: Some(update_id),
                        rollback_of: Some(target.id),
                        removed: Vec::new(),
                    },
                );
                tracing::info!(tool = %req.tool, from = %active.version, to = %retained.version, "tool rolled back");
                Ok(RollbackReport {
                    tool: req.tool,
                    series: req.series,
                    from: active.version,
                    to: retained.version,
                    from_id: active.id,
                    to_id: retained.id,
                    update_id,
                    rollback_of: target.id,
                    pools_switched,
                    current_moved,
                    rendered,
                    warnings,
                })
            }
            Err(err) => {
                let mut failed = new;
                failed.status = UpdateStatus::Failed;
                let message = err.to_string();
                let recorded = self
                    .deps
                    .writer
                    .execute(move |conn| {
                        let repo = UpdateHistoryRepo::new(conn);
                        let id = repo.insert(&NewUpdateHistory {
                            status: UpdateStatus::Started,
                            ..failed
                        })?;
                        repo.finish(id, UpdateStatus::Failed, None, Some(&message))
                    })
                    .await;
                if let Err(e) = recorded {
                    tracing::error!(tool = %req.tool, "could not record the failed rollback: {e}");
                }
                Err(err)
            }
        }
    }

    async fn rollback_steps(
        &self,
        ctx: &JobContext,
        manifest: &Arc<ToolManifest>,
        req: &RollbackRequest,
        active: &ToolVersionRow,
        retained: &ToolVersionRow,
    ) -> Result<(bool, bool, Vec<PathBuf>, Vec<String>)> {
        let step = |message: String| ctx.progress(JobProgress::new().message(message));
        step(messages::update_rendering());
        let rendered = self
            .deps
            .layout
            .rerender(Arc::clone(manifest), retained)
            .await?;
        step(messages::update_switching(&req.series));
        let switch = PoolSwitch {
            tool: req.tool.clone(),
            series: req.series.clone(),
            from: active.id,
            to: retained.id,
            from_version: active.version.clone(),
            to_version: retained.version.clone(),
        };
        let pools_switched = match self.switch(manifest, switch).await {
            Ok(switched) => switched,
            Err(err) => {
                if let Err(e) = self
                    .deps
                    .layout
                    .rerender(Arc::clone(manifest), active)
                    .await
                {
                    tracing::error!(tool = %active.tool_id, "could not restore the configuration: {e}");
                }
                return Err(err);
            }
        };
        let mut warnings = Vec::new();
        let current_moved = match self.follow_current(manifest, active, retained).await {
            Ok(moved) => moved,
            Err(e) => {
                warnings.push(format!("current: {e}"));
                false
            }
        };
        Ok((pools_switched, current_moved, rendered, warnings))
    }

    // ------------------------------------------------------------------ schedule

    async fn metered(&self) -> bool {
        let probe = Arc::clone(&self.deps.metered);
        tokio::task::spawn_blocking(move || probe())
            .await
            .unwrap_or(false)
    }

    async fn next_check_at(
        &self,
        tool: &ToolId,
        now: Timestamp,
        last_check: Option<Timestamp>,
    ) -> Result<NextCheck> {
        let snapshot = self.snapshot(tool).await?;
        let metered = self.metered().await;
        let updates = &self.config.updates;
        schedule::next_check_at(&schedule::ScheduleInput {
            manual: snapshot.policy == forge_manifest::schema::tool::UpdatePolicy::Manual,
            interval: snapshot.manifest.spec.update.check_interval.0,
            last_check,
            now,
            jitter: self.config.jitter,
            seed: schedule::seed_for(tool.as_str(), last_check),
            first_check_delay: self.config.first_check_delay,
            quiet_hours: updates.quiet_hours.as_ref(),
            time_zone: &self.config.time_zone,
            metered,
            metered_policy: updates.metered,
            metered_retry: self.config.metered_retry,
        })
    }
}

// ------------------------------------------------------------------------- actor

type Key = (ToolId, String);

enum Msg {
    Check {
        tool: Option<ToolId>,
        reply: Reply<Result<Vec<AvailableUpdate>>>,
    },
    Apply {
        req: Box<ApplyRequest>,
        reply: Reply<Result<JobId>>,
    },
    Rollback {
        req: RollbackRequest,
        reply: Reply<Result<JobId>>,
    },
    NextCheckAt {
        tool: ToolId,
        now: Timestamp,
        reply: Reply<Result<NextCheck>>,
    },
    RunScheduled {
        tool: ToolId,
        now: Timestamp,
        reply: Reply<Result<ScheduledRun>>,
    },
    Checked {
        tools: Vec<ToolId>,
        at: Timestamp,
    },
    Finished {
        key: Key,
        job: JobId,
    },
    Status {
        reply: Reply<UpdateEngineStatus>,
    },
}

struct InFlight {
    job: JobId,
    /// Target version (`to` of an update, `rollback` for a rollback).
    target: String,
}

struct EngineActor {
    core: Arc<Core>,
    in_flight: HashMap<Key, InFlight>,
    last_check: HashMap<ToolId, Timestamp>,
}

impl EngineActor {
    fn start_job<F, Fut, T>(
        &mut self,
        ctx: &Context<Self>,
        key: Key,
        target: String,
        spec: JobSpec,
        body: F,
    ) -> Result<JobId>
    where
        F: FnOnce(Arc<Core>, JobContext) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
        T: Serialize + Send + 'static,
    {
        if let Some(running) = self.in_flight.get(&key) {
            if running.target == target {
                return Ok(running.job);
            }
            return Err(ForgeError::Conflict(messages::update_in_flight(
                key.0.as_str(),
                &key.1,
            )));
        }
        let core = Arc::clone(&self.core);
        let myself: WeakActorRef<Msg> = ctx.myself_weak();
        let done_key = key.clone();
        let handle = self
            .core
            .deps
            .layout
            .jobs()
            .spawn(spec, move |job_ctx| async move {
                let job = job_ctx.id();
                let result = body(core, job_ctx).await;
                if let Some(actor) = myself.upgrade() {
                    let _ = actor.tell(Msg::Finished { key: done_key, job });
                }
                result
            });
        let job = handle.id();
        // A job already known for this request id may have finished: only track running.
        if handle.info().state == JobState::Running {
            self.in_flight.insert(key, InFlight { job, target });
        }
        Ok(job)
    }

    fn apply(&mut self, ctx: &Context<Self>, req: ApplyRequest) -> Result<JobId> {
        let update = &req.update;
        if req.trigger == UpdateTrigger::Rollback {
            return Err(ForgeError::InvalidInput(
                "an update cannot be triggered as a rollback".into(),
            ));
        }
        if let Some(reason) = &update.blocked {
            return Err(ForgeError::Conflict(reason.clone()));
        }
        self.core.catalog_tool(&update.tool)?;
        let key = (update.tool.clone(), update.series.clone());
        let mut spec = JobSpec::new(format!(
            "tool.update {} {} -> {}",
            update.tool, update.from, update.to
        ));
        if let Some(request_id) = req.request_id {
            spec = spec.request_id(request_id);
        }
        let target = update.to_canonical.clone();
        self.start_job(ctx, key, target, spec, move |core, job| async move {
            core.run_apply(job, req).await
        })
    }

    fn rollback(&mut self, ctx: &Context<Self>, req: RollbackRequest) -> Result<JobId> {
        self.core.catalog_tool(&req.tool)?;
        let key = (req.tool.clone(), req.series.clone());
        let mut spec = JobSpec::new(format!("tool.rollback {} {}", req.tool, req.series));
        if let Some(request_id) = req.request_id {
            spec = spec.request_id(request_id);
        }
        self.start_job(
            ctx,
            key,
            "rollback".into(),
            spec,
            move |core, job| async move { core.run_rollback(job, req).await },
        )
    }
}

impl Actor for EngineActor {
    type Msg = Msg;

    async fn handle(&mut self, msg: Msg, ctx: &mut Context<Self>) {
        match msg {
            Msg::Check { tool, reply } => {
                let core = Arc::clone(&self.core);
                let myself = ctx.myself_weak();
                tokio::spawn(async move {
                    let result = core.check(tool).await;
                    let result = result.map(|(tools, found)| {
                        if let Some(actor) = myself.upgrade() {
                            let _ = actor.tell(Msg::Checked {
                                tools,
                                at: Timestamp::now(),
                            });
                        }
                        found
                    });
                    let _ = reply.send(result);
                });
            }
            Msg::Apply { req, reply } => {
                let _ = reply.send(self.apply(ctx, *req));
            }
            Msg::Rollback { req, reply } => {
                let _ = reply.send(self.rollback(ctx, req));
            }
            Msg::NextCheckAt { tool, now, reply } => {
                let core = Arc::clone(&self.core);
                let last = self.last_check.get(&tool).copied();
                tokio::spawn(async move {
                    let _ = reply.send(core.next_check_at(&tool, now, last).await);
                });
            }
            Msg::RunScheduled { tool, now, reply } => {
                let core = Arc::clone(&self.core);
                let myself = ctx.myself_weak();
                tokio::spawn(async move {
                    let _ = reply.send(run_scheduled(core, myself, tool, now).await);
                });
            }
            Msg::Checked { tools, at } => {
                for tool in tools {
                    self.last_check.insert(tool, at);
                }
            }
            Msg::Finished { key, job } => {
                if self.in_flight.get(&key).is_some_and(|f| f.job == job) {
                    self.in_flight.remove(&key);
                }
            }
            Msg::Status { reply } => {
                let mut in_flight: Vec<(ToolId, String, JobId)> = self
                    .in_flight
                    .iter()
                    .map(|((tool, series), f)| (tool.clone(), series.clone(), f.job))
                    .collect();
                in_flight.sort();
                let mut last_checks: Vec<(ToolId, Timestamp)> = self
                    .last_check
                    .iter()
                    .map(|(t, at)| (t.clone(), *at))
                    .collect();
                last_checks.sort();
                let _ = reply.send(UpdateEngineStatus {
                    in_flight,
                    last_checks,
                });
            }
        }
    }
}

/// Body of [`UpdateEngine::run_scheduled`] (outside the mailbox).
async fn run_scheduled(
    core: Arc<Core>,
    myself: WeakActorRef<Msg>,
    tool: ToolId,
    now: Timestamp,
) -> Result<ScheduledRun> {
    let updates = &core.config.updates;
    let metered = core.metered().await;
    let deferred = schedule::defer_now(
        updates.quiet_hours.as_ref(),
        &core.config.time_zone,
        now,
        metered,
        updates.metered,
    )?;
    if deferred.is_some() {
        return Ok(ScheduledRun {
            tool,
            deferred,
            updates: Vec::new(),
            started: Vec::new(),
        });
    }
    let snapshot = core.snapshot(&tool).await?;
    let found = if snapshot.policy == forge_manifest::schema::tool::UpdatePolicy::Manual {
        Vec::new()
    } else {
        core.check_tool(&tool).await?
    };
    let actor = myself
        .upgrade()
        .ok_or_else(|| ForgeError::Internal("update engine stopped".into()))?;
    let _ = actor.tell(Msg::Checked {
        tools: vec![tool.clone()],
        at: now,
    });
    let mut started = Vec::new();
    for update in found.iter().filter(|u| u.automatic) {
        let req = Box::new(ApplyRequest::auto(update.clone()));
        match actor
            .ask(|reply| Msg::Apply { req, reply }, core.config.ask_timeout)
            .await
            .map_err(ForgeError::from)?
        {
            Ok(job) => started.push(job),
            Err(e) => tracing::warn!(%tool, to = %update.to, "automatic update not started: {e}"),
        }
    }
    Ok(ScheduledRun {
        tool,
        deferred: None,
        updates: found,
        started,
    })
}

// ------------------------------------------------------------------------- handle

/// Handle of the update engine (module docs). Cheap to clone.
#[derive(Clone)]
pub struct UpdateEngine {
    actor: ActorRef<Msg>,
    // Keeps the actor supervised while a handle lives.
    _handle: Arc<ActorHandle<Msg>>,
    core: Arc<Core>,
}

impl std::fmt::Debug for UpdateEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateEngine").finish_non_exhaustive()
    }
}

impl UpdateEngine {
    /// Starts the `update-engine` actor under `supervisor`.
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn spawn(supervisor: &ActorSupervisor, deps: UpdateDeps, config: UpdateConfig) -> Self {
        let core = Arc::new(Core { deps, config });
        let factory_core = Arc::clone(&core);
        let handle = supervisor.spawn(
            ActorSpec::new(UPDATE_ENGINE_ACTOR).mailbox(MAILBOX_CAPACITY),
            move || EngineActor {
                core: Arc::clone(&factory_core),
                in_flight: HashMap::new(),
                last_check: HashMap::new(),
            },
        );
        Self {
            actor: handle.actor_ref().clone(),
            _handle: Arc::new(handle),
            core,
        }
    }

    fn long_timeout(&self) -> Duration {
        // A check of every tool resolves them one after the other.
        self.core.config.check_timeout * 4
    }

    /// Updates available for `tool` (every installed tool with a manifest when `None`).
    pub async fn check(&self, tool: Option<&ToolId>) -> Result<Vec<AvailableUpdate>> {
        let tool = tool.cloned();
        self.actor
            .ask(|reply| Msg::Check { tool, reply }, self.long_timeout())
            .await?
    }

    /// Starts an update job and returns its id at once (module docs). `conflict` while
    /// another update or rollback of the same series runs (the same target returns the
    /// running job) or when the update is blocked.
    pub async fn apply(&self, req: ApplyRequest) -> Result<JobId> {
        let req = Box::new(req);
        self.actor
            .ask(
                |reply| Msg::Apply { req, reply },
                self.core.config.ask_timeout,
            )
            .await?
    }

    /// Starts the rollback of the newest completed update of `req.series` and returns
    /// the job id at once.
    pub async fn rollback(&self, req: RollbackRequest) -> Result<JobId> {
        self.actor
            .ask(
                |reply| Msg::Rollback { req, reply },
                self.core.config.ask_timeout,
            )
            .await?
    }

    /// Scheduler hook: when the next background check of `tool` should run.
    pub async fn next_check_at(&self, tool: &ToolId, now: Timestamp) -> Result<NextCheck> {
        let tool = tool.clone();
        self.actor
            .ask(
                |reply| Msg::NextCheckAt { tool, now, reply },
                self.core.config.check_timeout,
            )
            .await?
    }

    /// Scheduler hook: a due background run of `tool` (module docs).
    pub async fn run_scheduled(&self, tool: &ToolId, now: Timestamp) -> Result<ScheduledRun> {
        let tool = tool.clone();
        self.actor
            .ask(
                |reply| Msg::RunScheduled { tool, now, reply },
                self.long_timeout(),
            )
            .await?
    }

    /// History of `tool` (every tool when `None`), newest first.
    pub async fn history(
        &self,
        tool: Option<&ToolId>,
        limit: usize,
    ) -> Result<Vec<UpdateHistoryRow>> {
        let tool = tool.cloned();
        self.core
            .deps
            .reader
            .query(move |conn| UpdateHistoryRepo::new(conn).list(tool.as_ref(), limit))
            .await
    }

    /// In-flight jobs and last checks.
    pub async fn status(&self) -> Result<UpdateEngineStatus> {
        Ok(self
            .actor
            .ask(|reply| Msg::Status { reply }, self.core.config.ask_timeout)
            .await?)
    }
}
