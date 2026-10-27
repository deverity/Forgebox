//! Versioned layout (docs/03 §5.1-§5.2, docs/08 §2 and §5, ADR-0005, ADR-0025):
//! `bin/<tool>/<canonical_name>/`, the `bin/<tool>/current` junction, `bin/shims/*.cmd`
//! and the user `PATH`.
//!
//! [`Layout`] is the engine's handle (cheap to clone):
//!
//! * [`Layout::install_from_archive`] returns a [`JobId`] at once and runs the pipeline of
//!   docs/03 §5.1 as a job: **verify** (sha256/sha512, size, optional signature;
//!   [`crate::verify`]) → **stage** (`tmp/stage/<job id>/`, with the case sensitivity of
//!   `bin/`) → **extract** ([`crate::extract`], `strip_root`) → **post_install**
//!   (declarative `copy`/`render`/`mkdir`/`run`, then `install.verify`) on the staged
//!   copy → **move** into `bin/<tool>/<canonical_name>` in one rename → **register** the
//!   version in `tool_versions` → optionally point **current** at it → refresh the
//!   tool's **shims**. `tool.install.started|progress|completed|failed` are published;
//!   a failure in any step before the move leaves `bin/` untouched (only the staging
//!   folder is used, and it is always removed), and a failed registration moves the
//!   folder back out. Side effects of `post_install` outside `bin/` (a `php.ini` copied to
//!   `etc/` with `if_missing`) are idempotent by contract and stay.
//! * [`Layout::use_version`] points `current` at an installed version (`current.tmp` +
//!   rename through [`forge_core::platform::LinkFs::replace_junction`]), measures and logs
//!   the switch and publishes `tool.current_changed`. [`Layout::current_version`] reads it.
//! * [`Layout::ensure_shims`] and [`Layout::ensure_path`] are the idempotent shim and
//!   `PATH` steps ([`shims`], [`path`]).
//!
//! Concurrency (docs/03 §7, ADR-0011): long work runs in the job's task; file system and
//! Win32 calls run in `spawn_blocking`; SQLite goes through the store's writer thread and
//! read pool. Operations that change `bin/<tool>` (the final move, registration,
//! `current`, shims) take the tool's *turn*, a one-permit semaphore acquired with a
//! timeout, so `forge use php` never interleaves with the end of a php install, while
//! downloads, extraction and `post_install` of different versions run in parallel. The
//! semaphore map is behind an [`OrderedMutex`] (`layout.tools`, leaf, never held across
//! `.await`). `PATH` changes have a turn of their own.

mod actions;
mod current;
pub mod path;
pub mod shims;
mod vars;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::config::PathConfig;
use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::event::EventKind;
use forge_core::ids::{JobId, RequestId, ToolId, VersionId};
use forge_core::jobs::{JobContext, JobErrorInfo, JobProgress, JobRegistry, JobSpec};
use forge_core::paths::{ForgeRoot, posix_string};
use forge_core::platform::{CaseSensitivitySupport, Platform};
use forge_core::store::{
    NewToolVersion, StoreReader, StoreWriter, ToolVersionRepo, ToolVersionRow, VersionSource,
    VersionState,
};
use forge_core::sync::{LockLevel, OrderedMutex};
use forge_manifest::schema::ToolManifest;
use forge_manifest::schema::tool::{ArchiveFormat, Layout as InstallLayout};
use forge_manifest::version::{Version, canonical_name};
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub use self::actions::{
    DEFAULT_RUN_TIMEOUT, NoTemplates, RenderRequest, TemplateRenderer, VERIFY_TIMEOUT,
};
pub use self::current::{
    CurrentVersion, TOOL_CURRENT_EVENT_VERSION, ToolCurrentEventPayload, UseReport, select,
};
pub use self::path::PathReport;
pub use self::shims::{ShimReport, ShimVersion};
pub use self::vars::{PathVars, TemplateVars};
use crate::extract::install::{match_case_sensitivity, remove_tree_logged, rename_retry};
use crate::extract::{ExtractLimits, ExtractRequest, Extractor};
use crate::messages;
use crate::provider::Release;
use crate::verify::{Expect, Verifier};

/// Version of [`ToolInstallEventPayload`].
pub const TOOL_INSTALL_EVENT_VERSION: u32 = 1;
/// How long an operation waits for the tool's turn (or the `PATH` turn).
pub const DEFAULT_TURN_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether an installation points `current` at the new version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurrentPolicy {
    /// Only when the tool has no `current` yet (first installed version). Default.
    #[default]
    IfMissing,
    Always,
    Never,
}

/// Options of [`Layout::install_from_archive`].
#[derive(Debug, Clone)]
pub struct InstallOptions {
    /// What the archive must match. Default: the release's sha256 (or sha512) and size;
    /// a release without a checksum is refused unless this is
    /// [`Expect::unverified`] (`--allow-unverified`) or carries one.
    pub expect: Option<Expect>,
    pub current: CurrentPolicy,
    /// Reinstall over an existing `bin/<tool>/<canonical_name>` (the old folder is moved
    /// aside and deleted only after the new one is in place).
    pub replace_existing: bool,
    /// `post_install[].run` may execute: official registry or the user's own manifest
    /// (docs/11 §2). Default false.
    pub allow_run: bool,
    /// `tool_versions.source`. Default `registry`.
    pub source: VersionSource,
    /// Idempotency key of the job (ADR-0009).
    pub request_id: Option<RequestId>,
    pub limits: ExtractLimits,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            expect: None,
            current: CurrentPolicy::default(),
            replace_existing: false,
            allow_run: false,
            source: VersionSource::Registry,
            request_id: None,
            limits: ExtractLimits::default(),
        }
    }
}

/// An archive to install as a version of a tool.
#[derive(Debug, Clone)]
pub struct InstallRequest {
    pub manifest: Arc<ToolManifest>,
    /// The resolved release ([`crate::provider`]): version, variants, checksum, file name.
    pub release: Release,
    /// The downloaded file (`tmp/downloads/...`).
    pub archive: PathBuf,
    pub options: InstallOptions,
}

/// Steps of an installation (`step` of `tool.install.*` payloads).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallStep {
    Verify,
    Extract,
    PostInstall,
    VerifyCommand,
    Move,
    Register,
    Current,
    Shims,
}

/// Payload of `tool.install.started|progress|completed|failed`: the fields of
/// `forge_ipc`'s `ToolInstallPayload` (docs/03 §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInstallEventPayload {
    pub v: u32,
    pub job_id: Option<JobId>,
    pub tool: ToolId,
    pub version: String,
    pub version_id: Option<VersionId>,
    pub progress: Option<JobProgress>,
    pub error: Option<JobErrorInfo>,
    /// Step that is running (progress) or that failed.
    pub step: Option<InstallStep>,
    /// Folder name of the version (`8.3.37-nts-vs17-x64`).
    pub canonical_name: Option<String>,
}

/// Result of an installation (`job.done` payload).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallReport {
    pub tool: ToolId,
    pub version: String,
    pub canonical_name: String,
    pub version_id: VersionId,
    /// `bin/<tool>/<canonical_name>` (informative path).
    pub path: PathBuf,
    /// Computed sha256 of the archive.
    pub sha256: Option<String>,
    /// Bytes installed.
    pub size: u64,
    /// `current` points at this version after the installation.
    pub current: bool,
    /// The version was already registered (reinstall / repair).
    pub reinstalled: bool,
    pub shims: ShimReport,
    /// Non-fatal problems (`current` or shims could not be updated).
    pub warnings: Vec<String>,
}

/// What the request resolves to before the job starts.
#[derive(Debug, Clone)]
struct Plan {
    tool: ToolId,
    tool_name: String,
    version: String,
    series: String,
    canonical_name: String,
    variant: BTreeMap<String, String>,
    /// `bin/<tool>/<canonical_name>` (`bin/<tool>` for `layout: single`).
    dest: PathBuf,
    format: ArchiveFormat,
    expect: Expect,
}

impl Plan {
    fn new(root: &ForgeRoot, req: &InstallRequest) -> Result<Self> {
        let manifest = &req.manifest;
        let release = &req.release;
        let tool = manifest.metadata.id.clone();
        let versioning = &manifest.spec.versioning;
        let canonical = canonical_name(
            &release.version,
            &release.variants,
            &versioning.variant_order(),
        );
        let dest = match manifest.spec.install.layout {
            InstallLayout::Versioned => root.tool_version_dir(&tool, &canonical)?,
            InstallLayout::Single => root.tool_dir(&tool),
        };
        let series = Version::parse(versioning.scheme, &release.version)
            .ok()
            .and_then(|v| v.series(&versioning.series_key).ok())
            .unwrap_or_else(|| release.version.clone());
        let format = manifest
            .spec
            .install
            .archive
            .unwrap_or_else(|| ArchiveFormat::infer(&release.file_name));
        let expect = match &req.options.expect {
            Some(expect) => expect.clone(),
            None => {
                let mut expect = match (&release.sha256, &release.sha512) {
                    (Some(sha256), _) => Expect::sha256(sha256.clone()),
                    (None, Some(sha512)) => Expect::sha512(sha512.clone()),
                    (None, None) => {
                        return Err(ForgeError::VerificationFailed(format!(
                            "{tool} {}: the release has no checksum; refusing to install an \
                             unverified file (docs/05 §3)",
                            release.version
                        )));
                    }
                };
                if let Some(size) = release.size {
                    expect = expect.size(size);
                }
                expect
            }
        };
        Ok(Self {
            tool_name: manifest.metadata.name.clone(),
            tool,
            version: release.version.clone(),
            series,
            canonical_name: canonical,
            variant: release.variants.clone(),
            dest,
            format,
            expect,
        })
    }

    fn vars(&self, root: &ForgeRoot) -> TemplateVars {
        TemplateVars {
            tool: self.tool.to_string(),
            version: self.version.clone(),
            series: self.series.clone(),
            canonical_name: self.canonical_name.clone(),
            variant: self.variant.clone(),
            paths: PathVars::new(root, &self.dest),
        }
    }
}

/// The install staging folder: removed explicitly (awaited) on the normal paths, from a
/// blocking task if dropped early (panic, forced cancellation).
struct StageDir(Option<PathBuf>);

impl StageDir {
    async fn remove(mut self) {
        if let Some(path) = self.0.take() {
            let _ = tokio::task::spawn_blocking(move || remove_tree_logged(&path)).await;
        }
    }
}

impl Drop for StageDir {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn_blocking(move || remove_tree_logged(&path));
                }
                Err(_) => remove_tree_logged(&path),
            }
        }
    }
}

/// Runs blocking `f` on the blocking pool.
async fn blocking<T, F>(what: &'static str, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ForgeError::Internal(format!("{what} task failed: {e}")))?
}

/// Same folder, compared as lowercase POSIX strings without `\\?\`.
fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| posix_string(p).trim_end_matches('/').to_lowercase();
    norm(a) == norm(b)
}

/// The versioned layout engine (module docs). Cheap to clone.
#[derive(Clone)]
pub struct Layout {
    inner: Arc<Inner>,
}

struct Inner {
    root: ForgeRoot,
    platform: Platform,
    writer: StoreWriter,
    reader: StoreReader,
    jobs: JobRegistry,
    renderer: Arc<dyn TemplateRenderer>,
    /// Lock `layout.tools` (leaf): only to look a semaphore up.
    turns: OrderedMutex<HashMap<ToolId, Arc<Semaphore>>>,
    path_turn: Arc<Semaphore>,
    turn_timeout: Duration,
}

impl std::fmt::Debug for Layout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Layout")
            .field("root", &self.inner.root)
            .finish_non_exhaustive()
    }
}

impl Layout {
    /// Engine for `root`. Jobs and events go to `jobs` (and its event bus); `render`
    /// actions to `renderer` ([`NoTemplates`] when there is none).
    pub fn new(
        root: ForgeRoot,
        platform: Platform,
        writer: StoreWriter,
        reader: StoreReader,
        jobs: JobRegistry,
        renderer: Arc<dyn TemplateRenderer>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                root,
                platform,
                writer,
                reader,
                jobs,
                renderer,
                turns: OrderedMutex::new(LockLevel::LAYOUT_TOOLS, HashMap::new()),
                path_turn: Arc::new(Semaphore::new(1)),
                turn_timeout: DEFAULT_TURN_TIMEOUT,
            }),
        }
    }

    /// Changes how long operations wait for a tool's turn (default
    /// [`DEFAULT_TURN_TIMEOUT`]). Call before cloning.
    pub fn with_turn_timeout(mut self, timeout: Duration) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.turn_timeout = timeout;
        }
        self
    }

    pub fn root(&self) -> &ForgeRoot {
        &self.inner.root
    }

    fn turn(&self, tool: &ToolId) -> Arc<Semaphore> {
        let mut turns = self.inner.turns.lock();
        Arc::clone(
            turns
                .entry(tool.clone())
                .or_insert_with(|| Arc::new(Semaphore::new(1))),
        )
    }

    async fn acquire(
        &self,
        semaphore: Arc<Semaphore>,
        what: String,
    ) -> Result<OwnedSemaphorePermit> {
        tokio::time::timeout(self.inner.turn_timeout, semaphore.acquire_owned())
            .await
            .map_err(|_| ForgeError::Timeout {
                operation: what,
                after: self.inner.turn_timeout,
            })?
            .map_err(|_| ForgeError::Internal("layout turn closed".into()))
    }

    async fn tool_turn(&self, tool: &ToolId) -> Result<OwnedSemaphorePermit> {
        self.acquire(
            self.turn(tool),
            format!("waiting for other operations on {tool}"),
        )
        .await
    }

    fn publish<T: Serialize>(&self, kind: EventKind, payload: &T) {
        match serde_json::to_value(payload) {
            Ok(value) => {
                self.inner.jobs.events().publish(kind, value);
            }
            Err(e) => tracing::error!(%kind, "event payload not serializable: {e}"),
        }
    }

    /// Registered versions of `tool` (all states).
    pub async fn versions(&self, tool: &ToolId) -> Result<Vec<ToolVersionRow>> {
        let tool = tool.clone();
        self.inner
            .reader
            .query(move |conn| ToolVersionRepo::new(conn).list(Some(&tool)))
            .await
    }

    // ------------------------------------------------------------------ install

    /// Starts the installation of `req.archive` (module docs) and returns the job id at
    /// once. Requests that cannot work (no checksum, invalid folder name) fail here,
    /// without a job.
    pub fn install_from_archive(&self, req: InstallRequest) -> Result<JobId> {
        let plan = Plan::new(&self.inner.root, &req)?;
        let mut spec = JobSpec::new(format!("tool.install {}@{}", plan.tool, plan.version));
        if let Some(request_id) = req.options.request_id {
            spec = spec.request_id(request_id);
        }
        let this = self.clone();
        let handle = self.inner.jobs.spawn(spec, move |ctx| async move {
            this.run_install(ctx, req, plan).await
        });
        Ok(handle.id())
    }

    fn install_event(
        &self,
        ctx: &JobContext,
        plan: &Plan,
        step: Option<InstallStep>,
    ) -> ToolInstallEventPayload {
        ToolInstallEventPayload {
            v: TOOL_INSTALL_EVENT_VERSION,
            job_id: Some(ctx.id()),
            tool: plan.tool.clone(),
            version: plan.version.clone(),
            version_id: None,
            progress: None,
            error: None,
            step,
            canonical_name: Some(plan.canonical_name.clone()),
        }
    }

    fn step(&self, ctx: &JobContext, plan: &Plan, step: InstallStep, message: String) {
        let progress = JobProgress::new().message(message);
        ctx.progress(progress.clone());
        let mut payload = self.install_event(ctx, plan, Some(step));
        payload.progress = Some(progress);
        self.publish(EventKind::ToolInstallProgress, &payload);
    }

    async fn run_install(
        self,
        ctx: JobContext,
        req: InstallRequest,
        plan: Plan,
    ) -> Result<InstallReport> {
        self.publish(
            EventKind::ToolInstallStarted,
            &self.install_event(&ctx, &plan, None),
        );
        let stage = self.inner.root.tmp_stage().join(ctx.id().to_string());
        let guard = StageDir(Some(stage.clone()));
        let outcome = self.install_steps(&ctx, &req, &plan, &stage).await;
        guard.remove().await;
        match outcome {
            Ok(report) => {
                let mut payload = self.install_event(&ctx, &plan, None);
                payload.version_id = Some(report.version_id);
                self.publish(EventKind::ToolInstallCompleted, &payload);
                tracing::info!(
                    tool = %plan.tool,
                    folder = %plan.canonical_name,
                    current = report.current,
                    "tool installed"
                );
                Ok(report)
            }
            Err((step, err)) => {
                let mut payload = self.install_event(&ctx, &plan, Some(step));
                payload.error = Some(JobErrorInfo::from(&err));
                self.publish(EventKind::ToolInstallFailed, &payload);
                tracing::warn!(tool = %plan.tool, version = %plan.version, ?step, "install failed: {err}");
                Err(err)
            }
        }
    }

    async fn install_steps(
        &self,
        ctx: &JobContext,
        req: &InstallRequest,
        plan: &Plan,
        stage: &Path,
    ) -> std::result::Result<InstallReport, (InstallStep, ForgeError)> {
        use InstallStep as S;
        let inner = &self.inner;
        let root = inner.root.clone();
        let manifest = Arc::clone(&req.manifest);

        // 1. Verify.
        self.step(ctx, plan, S::Verify, messages::install_verifying());
        let verified = Verifier::new(&inner.platform)
            .verify_in_job(ctx, &req.archive, plan.expect.clone())
            .await
            .map_err(|e| (S::Verify, e))?;

        // 2. Stage, with the case sensitivity of the destination's nearest existing parent.
        let platform = inner.platform.clone();
        let (stage_dir, dest) = (stage.to_path_buf(), plan.dest.clone());
        let bin = root.bin();
        blocking("staging", move || {
            std::fs::create_dir_all(&stage_dir).at("creating", &stage_dir)?;
            let reference = dest
                .ancestors()
                .skip(1)
                .find(|p| p.is_dir())
                .map_or(bin, Path::to_path_buf);
            let cs = platform.case_sensitive_fs();
            if matches!(cs.support(&stage_dir), CaseSensitivitySupport::Supported) {
                match_case_sensitivity(cs, &stage_dir, &reference);
            }
            Ok(())
        })
        .await
        .map_err(|e| (S::Extract, e))?;

        // 3. Extract into tmp/stage/<job>/<canonical_name>.
        let staged = stage.join(&plan.canonical_name);
        let mut extract = ExtractRequest::new(&req.archive, plan.format, &staged, root.tmp_stage())
            .strip_root(manifest.spec.install.strip_root)
            .limits(req.options.limits);
        if plan.format == ArchiveFormat::File {
            extract = extract.file_name(req.release.file_name.clone());
        }
        let extracted = Extractor::new(&inner.platform)
            .extract_in_job(ctx, extract)
            .await
            .map_err(|e| (S::Extract, e))?;

        // 4. post_install and install.verify on the staged copy.
        self.step(ctx, plan, S::PostInstall, messages::install_post_install());
        let final_vars = plan.vars(&root);
        let physical = final_vars.with_version_dir(&staged);
        let renderer = Arc::clone(&inner.renderer);
        let (staged2, root2, manifest2, tool) = (
            staged.clone(),
            root.clone(),
            Arc::clone(&manifest),
            plan.tool.clone(),
        );
        let allow_run = req.options.allow_run;
        let job = ctx.clone();
        tokio::task::spawn_blocking(move || {
            let cancelled = || job.is_cancelled();
            let action_ctx = actions::ActionContext {
                root: &root2,
                tool: &tool,
                staged_dir: &staged2,
                physical: &physical,
                final_vars: &final_vars,
                renderer: renderer.as_ref(),
                allow_run,
                cancelled: &cancelled,
            };
            let install = &manifest2.spec.install;
            actions::run_actions(&action_ctx, &install.post_install)
                .map_err(|e| (S::PostInstall, e))?;
            if let Some(verify) = &install.verify {
                actions::run_verify(&staged2, &physical, verify)
                    .map_err(|e| (S::VerifyCommand, e))?;
            }
            Ok(())
        })
        .await
        .map_err(|e| {
            (
                S::PostInstall,
                ForgeError::Internal(format!("post_install task failed: {e}")),
            )
        })??;
        ctx.check_cancelled().map_err(|e| (S::PostInstall, e))?;

        // 5. From here on bin/<tool> changes: take the tool's turn.
        let _turn = self.tool_turn(&plan.tool).await.map_err(|e| (S::Move, e))?;
        self.step(
            ctx,
            plan,
            S::Move,
            messages::install_moving(plan.tool.as_str(), &plan.canonical_name),
        );
        let (from, dest) = (staged.clone(), plan.dest.clone());
        let replace =
            req.options.replace_existing || manifest.spec.install.layout == InstallLayout::Single;
        let move_id = ctx.id().to_string();
        blocking("moving into bin", move || {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).at("creating", parent)?;
            }
            crate::extract::install::install(&from, &dest, replace, &move_id, false)
        })
        .await
        .map_err(|e| (S::Move, e))?;

        // 6. Register.
        self.step(ctx, plan, S::Register, messages::install_registering());
        let new = NewToolVersion {
            tool_id: plan.tool.clone(),
            tool_name: plan.tool_name.clone(),
            version: plan.version.clone(),
            canonical_name: plan.canonical_name.clone(),
            variant: plan.variant.clone(),
            path: posix_string(&plan.dest),
            state: VersionState::Installed,
            source: req.options.source,
        };
        let sha256 = verified.sha256.clone();
        let size = extracted.bytes;
        let registered = inner
            .writer
            .execute(move |conn| {
                ToolVersionRepo::new(conn).record_install(&new, sha256.as_deref(), Some(size))
            })
            .await;
        let (version_id, inserted) = match registered {
            Ok(done) => done,
            Err(err) => {
                // Keep bin/ and the database consistent: take the folder back out (with
                // `replace_existing` the previous folder is already gone).
                let (dest, back) = (plan.dest.clone(), staged.clone());
                let restored = blocking("undoing the move", move || {
                    rename_retry(&dest, &back).at("moving back", &dest)
                })
                .await;
                if let Err(undo) = restored {
                    tracing::error!(dest = %plan.dest.display(), "could not undo the move: {undo}");
                }
                return Err((S::Register, err));
            }
        };

        // 7. current (optional; failures are warnings: the version is installed).
        let mut warnings = Vec::new();
        let install = &manifest.spec.install;
        let mut is_current = false;
        if install.current_link && install.layout == InstallLayout::Versioned {
            let link = root.tool_current_link(&plan.tool);
            let platform = inner.platform.clone();
            let (dest, policy) = (plan.dest.clone(), req.options.current);
            let switched = blocking("current junction", move || {
                let before = match platform.link_fs().read_junction(&link) {
                    Ok(target) => Some(target),
                    Err(ForgeError::NotFound(_)) => None,
                    Err(e) => return Err(e),
                };
                if before.as_deref().is_some_and(|b| same_dir(b, &dest)) {
                    return Ok((true, None));
                }
                let wanted = match policy {
                    CurrentPolicy::Always => true,
                    CurrentPolicy::Never => false,
                    CurrentPolicy::IfMissing => before.is_none(),
                };
                if !wanted {
                    return Ok((false, None));
                }
                let started = Instant::now();
                platform.link_fs().replace_junction(&link, &dest)?;
                Ok((true, Some((before, started.elapsed()))))
            })
            .await;
            match switched {
                Ok((now_current, switch)) => {
                    is_current = now_current;
                    if let Some((before, elapsed)) = switch {
                        self.step(ctx, plan, S::Current, messages::install_activating());
                        self.publish(
                            EventKind::ToolCurrentChanged,
                            &ToolCurrentEventPayload {
                                v: TOOL_CURRENT_EVENT_VERSION,
                                tool: plan.tool.clone(),
                                from: before.and_then(|b| folder_name(&b)),
                                to: plan.canonical_name.clone(),
                                version_id: Some(version_id),
                                elapsed_us: elapsed.as_micros() as u64,
                            },
                        );
                    }
                }
                Err(e) => warnings.push(format!("current: {e}")),
            }
        }

        // 8. Shims.
        let shims = match self.refresh_shims(&manifest).await {
            Ok(report) => {
                warnings.extend(report.warnings.iter().cloned());
                report
            }
            Err(e) => {
                warnings.push(format!("shims: {e}"));
                ShimReport::default()
            }
        };

        Ok(InstallReport {
            tool: plan.tool.clone(),
            version: plan.version.clone(),
            canonical_name: plan.canonical_name.clone(),
            version_id,
            path: plan.dest.clone(),
            sha256: verified.sha256,
            size: extracted.bytes,
            current: is_current,
            reinstalled: !inserted,
            shims,
            warnings,
        })
    }

    // ------------------------------------------------------------------ current

    /// Points `bin/<tool>/current` at the installed version `spec` designates
    /// ([`select`]: folder name, exact version or series). Atomic (`current.tmp` +
    /// rename); publishes `tool.current_changed` when the target changed.
    pub async fn use_version(&self, manifest: &ToolManifest, spec: &str) -> Result<UseReport> {
        let install = &manifest.spec.install;
        let tool = manifest.metadata.id.clone();
        if !install.current_link || install.layout != InstallLayout::Versioned {
            return Err(ForgeError::InvalidInput(format!(
                "{tool} has no `current` junction (install.current_link: false or layout: single)"
            )));
        }
        let started = Instant::now();
        let rows = self.versions(&tool).await?;
        let row = current::select(manifest, &rows, spec)?.clone();
        let _turn = self.tool_turn(&tool).await?;
        let link = self.inner.root.tool_current_link(&tool);
        let target = PathBuf::from(&row.path);
        let platform = self.inner.platform.clone();
        let (target2, link2) = (target.clone(), link.clone());
        let (before, switch) = blocking("switching current", move || {
            let before = match platform.link_fs().read_junction(&link2) {
                Ok(t) => Some(t),
                Err(ForgeError::NotFound(_)) => None,
                Err(e) => return Err(e),
            };
            if before.as_deref().is_some_and(|b| same_dir(b, &target2)) {
                return Ok((before, None));
            }
            let switch_started = Instant::now();
            platform.link_fs().replace_junction(&link2, &target2)?;
            Ok((before, Some(switch_started.elapsed())))
        })
        .await?;
        let from = before.as_deref().and_then(folder_name);
        let report = UseReport {
            tool: tool.clone(),
            from: from.clone(),
            to: row.canonical_name.clone(),
            version: row.version.clone(),
            version_id: row.id,
            path: target,
            switch_us: switch.map_or(0, |d| d.as_micros() as u64),
            unchanged: switch.is_none(),
        };
        tracing::info!(
            %tool,
            from = from.as_deref().unwrap_or("-"),
            to = %report.to,
            switch_us = report.switch_us,
            total_ms = started.elapsed().as_secs_f64() * 1000.0,
            "current switched"
        );
        if !report.unchanged {
            self.publish(
                EventKind::ToolCurrentChanged,
                &ToolCurrentEventPayload {
                    v: TOOL_CURRENT_EVENT_VERSION,
                    tool,
                    from,
                    to: report.to.clone(),
                    version_id: Some(row.id),
                    elapsed_us: report.switch_us,
                },
            );
        }
        Ok(report)
    }

    /// The version behind `bin/<tool>/current`, if the junction exists.
    pub async fn current_version(&self, tool: &ToolId) -> Result<Option<CurrentVersion>> {
        let link = self.inner.root.tool_current_link(tool);
        let platform = self.inner.platform.clone();
        let tool = tool.clone();
        blocking("reading current", move || {
            match platform.link_fs().read_junction(&link) {
                Ok(path) => Ok(Some(CurrentVersion {
                    tool,
                    canonical_name: folder_name(&path).unwrap_or_default(),
                    path,
                })),
                Err(ForgeError::NotFound(_)) => Ok(None),
                Err(e) => Err(e),
            }
        })
        .await
    }

    // ------------------------------------------------------------------ shims, PATH

    async fn refresh_shims(&self, manifest: &Arc<ToolManifest>) -> Result<ShimReport> {
        let rows = self.versions(&manifest.metadata.id).await?;
        let versions = ShimVersion::from_rows(&rows);
        let (root, manifest) = (self.inner.root.clone(), Arc::clone(manifest));
        blocking("writing shims", move || {
            shims::ensure(&root, &manifest, &versions)
        })
        .await
    }

    /// Regenerates the shims of `manifest`'s tool from its installed versions
    /// (idempotent).
    pub async fn ensure_shims(&self, manifest: Arc<ToolManifest>) -> Result<ShimReport> {
        let _turn = self.tool_turn(&manifest.metadata.id).await?;
        self.refresh_shims(&manifest).await
    }

    /// Adds what `prefs` asks for to the user `PATH` for `manifests` ([`path::ensure`]):
    /// idempotent, never in portable mode.
    pub async fn ensure_path(
        &self,
        manifests: Vec<Arc<ToolManifest>>,
        prefs: PathConfig,
    ) -> Result<PathReport> {
        let _turn = self
            .acquire(
                Arc::clone(&self.inner.path_turn),
                "waiting for another PATH change".into(),
            )
            .await?;
        let root = self.inner.root.clone();
        let env = Arc::clone(self.inner.platform.user_env());
        blocking("updating the user PATH", move || {
            let refs: Vec<&ToolManifest> = manifests.iter().map(AsRef::as_ref).collect();
            path::ensure(&root, &refs, &prefs, env.as_ref())
        })
        .await
    }

    /// Removes every user `PATH` entry under `<root>\bin` ([`path::remove_root_entries`]).
    pub async fn remove_path_entries(&self) -> Result<Vec<String>> {
        let _turn = self
            .acquire(
                Arc::clone(&self.inner.path_turn),
                "waiting for another PATH change".into(),
            )
            .await?;
        let root = self.inner.root.clone();
        let env = Arc::clone(self.inner.platform.user_env());
        blocking("updating the user PATH", move || {
            path::remove_root_entries(&root, env.as_ref())
        })
        .await
    }
}

/// Last component of a junction target (`8.3.37-nts-vs17-x64`).
fn folder_name(path: &Path) -> Option<String> {
    path.file_name().map(|n| n.to_string_lossy().into_owned())
}
