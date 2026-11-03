//! `kind: Extension` (docs/04 §2, docs/03 §2.3, ADR-0043): files installed **inside** an
//! installed version of the tool they extend (`bin/php/8.4.27-nts-vs17-x64/ext/
//! php_xdebug.dll`), one installation per target version.
//!
//! * [`plan`] (pure) matches the extension's `compat` rules and release variants against
//!   the installed target builds ([`ExtensionSpec::compat_for`],
//!   [`ExtensionSpec::release_fits`]): an explicit target that does not fit is
//!   `not_found` naming the compatible series; without a target, incompatible versions
//!   are skipped and only "none fits" is an error.
//! * [`Layout::install_extension`] returns a [`JobId`] at once and runs: **verify**
//!   (sha256/size, the same [`Verifier`] as tools) → **extract** into
//!   `tmp/stage/<job>/payload` (a single file gets the name of `install.target`) → the
//!   target tool's turn → **move** into `install.target` (an existing file or folder is set
//!   aside and comes back on failure) → **post_install** with the target's variables
//!   (`render` of `conf.d/20-xdebug.ini`, rendered by the target tool's renderer) →
//!   **register** a `tool_versions` row of the extension. Nothing in `bin/` changes before
//!   the archive is verified, and a failure after the move restores what was there.
//! * The row is linked to its target through the variant map (no migration):
//!   [`TARGET_TOOL_KEY`], [`TARGET_ID_KEY`], [`TARGET_NAME_KEY`] next to the release
//!   variants; its `canonical_name` is `<target tool>-<target folder>` (unique per target)
//!   and its `path` is the installed file or folder.
//! * [`Layout::remove_extension`] deletes the file and the files the `render` actions
//!   wrote for that target, unless another installation of the same extension renders the
//!   same file (a series overlay shared by `8.4.26` and `8.4.27`), then the row.
//! * Target versions going away: [`Layout::remove_version`](super::Layout::remove_version)
//!   drops the rows linked to the removed version (their files went with its folder);
//!   [`Layout::carry_extensions`] re-links the installations that the update engine's
//!   `update.migrate` carried into the new version of a series.
//!
//! `post_install.run` never executes for an extension (there is no executable of its own
//! to run); `copy`/`mkdir`/`render` resolve `{{ paths.version }}` to the target version.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::event::EventKind;
use forge_core::ids::{JobId, ToolId, VersionId};
use forge_core::jobs::{JobContext, JobErrorInfo, JobProgress, JobSpec};
use forge_core::paths::posix_string;
use forge_core::store::{NewToolVersion, ToolVersionRepo, ToolVersionRow, VersionState};
use forge_manifest::schema::ToolManifest;
use forge_manifest::schema::extension::{
    ExtensionManifest, TARGET_ID_KEY, TARGET_NAME_KEY, TARGET_TOOL_KEY, TargetBuild,
};
use forge_manifest::schema::tool::{ArchiveFormat, PostInstallAction};
use forge_manifest::version::Version;
use serde::{Deserialize, Serialize};

use super::actions::{self, ActionContext};
use super::{
    InstallOptions, InstallStep, Layout, StageDir, TOOL_INSTALL_EVENT_VERSION,
    ToolInstallEventPayload, blocking, expectation,
};
use crate::extract::install::{remove_tree_logged, rename_retry};
use crate::extract::{ExtractRequest, Extractor};
use crate::messages;
use crate::provider::Release;
use crate::verify::{Expect, Verifier};

/// The target version an extension row belongs to (its variant keys).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionLink {
    /// Extended tool (`php`).
    pub tool: ToolId,
    /// The target's `tool_versions` id.
    pub id: VersionId,
    /// Folder name of the target (`8.4.27-nts-vs17-x64`).
    pub name: String,
}

impl ExtensionLink {
    /// The link of `row`, when it is an installed extension.
    pub fn of(row: &ToolVersionRow) -> Option<Self> {
        Some(Self {
            tool: row.variant.get(TARGET_TOOL_KEY)?.parse().ok()?,
            id: row.variant.get(TARGET_ID_KEY)?.parse().ok()?,
            name: row.variant.get(TARGET_NAME_KEY)?.clone(),
        })
    }

    /// The link to `target`.
    pub fn to(target: &ToolVersionRow) -> Self {
        Self {
            tool: target.tool_id.clone(),
            id: target.id,
            name: target.canonical_name.clone(),
        }
    }

    /// `canonical_name` of the extension row for this target (`php-8.4.27-nts-vs17-x64`).
    pub fn row_name(&self) -> String {
        format!("{}-{}", self.tool, self.name)
    }

    /// `variants` with the link keys set.
    pub fn apply(&self, mut variants: BTreeMap<String, String>) -> BTreeMap<String, String> {
        variants.insert(TARGET_TOOL_KEY.to_owned(), self.tool.to_string());
        variants.insert(TARGET_ID_KEY.to_owned(), self.id.to_string());
        variants.insert(TARGET_NAME_KEY.to_owned(), self.name.clone());
        variants
    }
}

/// An installed version of the extended tool, with its manifest.
#[derive(Debug, Clone)]
pub struct ExtensionTarget {
    pub manifest: Arc<ToolManifest>,
    pub row: ToolVersionRow,
}

impl ExtensionTarget {
    fn vars(&self, root: &forge_core::paths::ForgeRoot) -> super::TemplateVars {
        super::TemplateVars::of_version(
            root,
            &self.manifest,
            &self.row.version,
            &self.row.canonical_name,
            &self.row.variant,
            Path::new(&self.row.path),
        )
    }
}

/// One target of an installation: the release that fits it and the row of the extension
/// already installed there, if any.
#[derive(Debug, Clone)]
pub struct ExtensionPlanItem {
    pub target: ToolVersionRow,
    pub release: Release,
    /// Installed row of the extension for this target (same version = nothing to do).
    pub installed: Option<ToolVersionRow>,
}

impl ExtensionPlanItem {
    /// The same release is already installed into this target.
    pub fn up_to_date(&self) -> bool {
        self.installed
            .as_ref()
            .is_some_and(|row| row.version == self.release.version)
    }
}

/// Result of [`plan`].
#[derive(Debug, Clone, Default)]
pub struct ExtensionPlan {
    pub items: Vec<ExtensionPlanItem>,
    /// Targets left out (no compatible build): `(folder name, reason)`.
    pub skipped: Vec<(String, String)>,
}

/// Matches `releases` (resolved from [`ExtensionManifest::release_view`], preferred
/// first) against `targets` (installed versions of `target_manifest`, the extended tool).
/// With `explicit` (`--for php@8.4`), a target without a compatible build is an error;
/// otherwise it is skipped and only "no target fits" is. `installed` are the extension's
/// rows (to report what is already there). Errors are `not_found` naming the compatible
/// series.
pub fn plan(
    manifest: &ExtensionManifest,
    target_manifest: &ToolManifest,
    targets: &[ToolVersionRow],
    explicit: bool,
    releases: &[Release],
    installed: &[ToolVersionRow],
) -> Result<ExtensionPlan> {
    let spec = &manifest.spec;
    let id = &manifest.metadata.id;
    let extends = &spec.extends;
    if target_manifest.metadata.id != *extends {
        return Err(ForgeError::InvalidInput(format!(
            "{id} extends {extends}, not {}",
            target_manifest.metadata.id
        )));
    }
    if targets.is_empty() {
        return Err(ForgeError::NotFound(format!(
            "{id} installs into {extends}, and no version of {extends} is installed \
             (forge tool install {extends})"
        )));
    }
    let versioning = &target_manifest.spec.versioning;
    let compatible = || spec.compatible_series().join(", ");
    let mut out = ExtensionPlan::default();
    for row in targets {
        let series = Version::parse(versioning.scheme, &row.version)
            .ok()
            .and_then(|v| v.series(&versioning.series_key).ok())
            .unwrap_or_else(|| row.version.clone());
        let build = TargetBuild {
            scheme: versioning.scheme,
            version: &row.version,
            series: &series,
            variants: &row.variant,
        };
        let describe = || {
            let variants: Vec<&str> = row.variant.values().map(String::as_str).collect();
            if variants.is_empty() {
                format!("{extends} {}", row.version)
            } else {
                format!("{extends} {} ({})", row.version, variants.join(", "))
            }
        };
        let release = spec.compat_for(&build).and_then(|compat| {
            releases
                .iter()
                .find(|r| spec.release_fits(compat, &r.version, &r.variants, &build))
        });
        match release {
            Some(release) => {
                let link = ExtensionLink::to(row);
                let installed = installed
                    .iter()
                    .find(|r| {
                        r.tool_id == *id && ExtensionLink::of(r).is_some_and(|l| l.id == row.id)
                    })
                    .or_else(|| {
                        installed
                            .iter()
                            .find(|r| r.canonical_name == link.row_name())
                    })
                    .cloned();
                out.items.push(ExtensionPlanItem {
                    target: row.clone(),
                    release: release.clone(),
                    installed,
                });
            }
            None if explicit => {
                return Err(ForgeError::NotFound(format!(
                    "{id} has no build for {}; compatible {extends} series: {}",
                    describe(),
                    compatible()
                )));
            }
            None => out.skipped.push((
                row.canonical_name.clone(),
                format!("no {id} build for {}", describe()),
            )),
        }
    }
    if out.items.is_empty() {
        let installed: Vec<String> = targets.iter().map(|r| r.version.clone()).collect();
        return Err(ForgeError::NotFound(format!(
            "{id} has no build for the installed {extends} versions ({}); compatible {extends} \
             series: {}",
            installed.join(", "),
            compatible()
        )));
    }
    Ok(out)
}

/// An archive to install into one target version.
#[derive(Debug, Clone)]
pub struct ExtensionInstallRequest {
    pub manifest: Arc<ExtensionManifest>,
    pub target: ExtensionTarget,
    /// A release that fits the target ([`plan`]).
    pub release: Release,
    /// The downloaded file.
    pub archive: PathBuf,
    /// `expect`, `source`, `request_id` and `limits` apply; an existing file is always
    /// replaced and `run` never executes.
    pub options: InstallOptions,
}

/// Result of an extension installation (`job.done` payload).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionReport {
    pub tool: ToolId,
    pub version: String,
    /// `<target tool>-<target folder>`.
    pub canonical_name: String,
    pub version_id: VersionId,
    pub target: ExtensionLink,
    /// Installed file or folder (informative path).
    pub path: PathBuf,
    /// Files written by `post_install` `render` actions.
    pub rendered: Vec<PathBuf>,
    pub sha256: Option<String>,
    pub size: u64,
    /// The target already had a row of this extension (reinstall, other version).
    pub reinstalled: bool,
}

/// Result of [`Layout::remove_extension`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedExtension {
    pub tool: ToolId,
    pub version: String,
    pub version_id: VersionId,
    pub target: Option<ExtensionLink>,
    /// The deleted file or folder.
    pub path: PathBuf,
    /// Rendered files deleted with it.
    pub removed_files: Vec<PathBuf>,
    /// Rendered files kept because another installation of the extension uses them.
    pub shared_files: Vec<PathBuf>,
}

/// What the request resolves to before the job starts.
#[derive(Debug, Clone)]
struct ExtPlan {
    tool: ToolId,
    tool_name: String,
    version: String,
    link: ExtensionLink,
    /// `install.target` with the target's variables.
    dest: PathBuf,
    format: ArchiveFormat,
    expect: Expect,
    vars: super::TemplateVars,
}

impl ExtPlan {
    fn new(root: &forge_core::paths::ForgeRoot, req: &ExtensionInstallRequest) -> Result<Self> {
        let manifest = &req.manifest;
        let tool = manifest.metadata.id.clone();
        let target = &req.target;
        if target.manifest.metadata.id != manifest.spec.extends
            || target.row.tool_id != manifest.spec.extends
        {
            return Err(ForgeError::InvalidInput(format!(
                "{tool} extends {}, not {}",
                manifest.spec.extends, target.row.tool_id
            )));
        }
        let vars = target.vars(root);
        let dest = PathBuf::from(vars.substitute(&manifest.spec.install.target)?);
        let target_dir = Path::new(&target.row.path);
        if !inside(&dest, target_dir) || dest.components().any(|c| c.as_os_str() == "..") {
            return Err(ForgeError::Forbidden(format!(
                "{tool}: install.target {} is not inside {}",
                dest.display(),
                target_dir.display()
            )));
        }
        let release = &req.release;
        let format = manifest
            .spec
            .install
            .archive
            .unwrap_or_else(|| ArchiveFormat::infer(&release.file_name));
        Ok(Self {
            tool_name: manifest.metadata.name.clone(),
            expect: expectation(&tool, release, req.options.expect.as_ref())?,
            tool,
            version: release.version.clone(),
            link: ExtensionLink::to(&target.row),
            dest,
            format,
            vars,
        })
    }

    fn event(&self, ctx: &JobContext, step: Option<InstallStep>) -> ToolInstallEventPayload {
        ToolInstallEventPayload {
            v: TOOL_INSTALL_EVENT_VERSION,
            job_id: Some(ctx.id()),
            tool: self.tool.clone(),
            version: self.version.clone(),
            version_id: None,
            progress: None,
            error: None,
            step,
            canonical_name: Some(self.link.row_name()),
        }
    }
}

/// `path` is `base` or below it (case-insensitive, either separator).
fn inside(path: &Path, base: &Path) -> bool {
    let norm = |p: &Path| posix_string(p).trim_end_matches('/').to_lowercase();
    let (p, b) = (norm(path), norm(base));
    p != b && p.starts_with(&format!("{b}/"))
}

/// Same file, compared as lowercase POSIX strings.
fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| posix_string(p).trim_end_matches('/').to_lowercase();
    norm(a) == norm(b)
}

fn remove_any(path: &Path) {
    if path.is_dir() {
        remove_tree_logged(path);
    } else if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), "could not remove: {e}");
    }
}

/// The destinations of the `render` actions of `actions` with `vars`.
fn rendered_paths(actions: &[PostInstallAction], vars: &super::TemplateVars) -> Vec<PathBuf> {
    actions
        .iter()
        .filter_map(|a| match a {
            PostInstallAction::Render(render) => vars.substitute(&render.to).ok(),
            _ => None,
        })
        .map(PathBuf::from)
        .collect()
}

/// Undo record of the move: the previous file or folder set aside.
struct Placed {
    dest: PathBuf,
    aside: Option<PathBuf>,
}

impl Placed {
    /// Takes the new file out and puts the previous one back. Blocking.
    fn undo(&self) {
        remove_any(&self.dest);
        if let Some(aside) = &self.aside
            && let Err(e) = rename_retry(aside, &self.dest)
        {
            tracing::error!(dest = %self.dest.display(), "could not restore the previous file: {e}");
        }
    }

    /// Drops the previous file. Blocking.
    fn commit(&self) {
        if let Some(aside) = &self.aside {
            remove_any(aside);
        }
    }
}

impl Layout {
    /// Starts the installation of an extension into one target version (module docs)
    /// and returns the job id at once. Requests that cannot work (no checksum, a target
    /// path outside the target version, a target of another tool) fail here.
    pub fn install_extension(&self, req: ExtensionInstallRequest) -> Result<JobId> {
        let plan = ExtPlan::new(&self.inner.root, &req)?;
        let mut spec = JobSpec::new(format!(
            "tool.install {}@{} for {}@{}",
            plan.tool, plan.version, plan.link.tool, plan.link.name
        ));
        if let Some(request_id) = req.options.request_id {
            spec = spec.request_id(request_id);
        }
        let this = self.clone();
        let handle = self.inner.jobs.spawn(spec, move |ctx| async move {
            this.run_extension_install(ctx, req, plan).await
        });
        Ok(handle.id())
    }

    fn ext_step(&self, ctx: &JobContext, plan: &ExtPlan, step: InstallStep, message: String) {
        let progress = JobProgress::new().message(message);
        ctx.progress(progress.clone());
        let mut payload = plan.event(ctx, Some(step));
        payload.progress = Some(progress);
        self.publish(EventKind::ToolInstallProgress, &payload);
    }

    async fn run_extension_install(
        self,
        ctx: JobContext,
        req: ExtensionInstallRequest,
        plan: ExtPlan,
    ) -> Result<ExtensionReport> {
        self.publish(EventKind::ToolInstallStarted, &plan.event(&ctx, None));
        let stage = self.inner.root.tmp_stage().join(ctx.id().to_string());
        let guard = StageDir(Some(stage.clone()));
        let outcome = self.extension_steps(&ctx, &req, &plan, &stage).await;
        guard.remove().await;
        match outcome {
            Ok(report) => {
                let mut payload = plan.event(&ctx, None);
                payload.version_id = Some(report.version_id);
                self.publish(EventKind::ToolInstallCompleted, &payload);
                tracing::info!(
                    tool = %plan.tool,
                    version = %plan.version,
                    target = %plan.link.row_name(),
                    "extension installed"
                );
                Ok(report)
            }
            Err((step, err)) => {
                let mut payload = plan.event(&ctx, Some(step));
                payload.error = Some(JobErrorInfo::from(&err));
                self.publish(EventKind::ToolInstallFailed, &payload);
                tracing::warn!(tool = %plan.tool, target = %plan.link.row_name(), ?step, "extension install failed: {err}");
                Err(err)
            }
        }
    }

    async fn extension_steps(
        &self,
        ctx: &JobContext,
        req: &ExtensionInstallRequest,
        plan: &ExtPlan,
        stage: &Path,
    ) -> std::result::Result<ExtensionReport, (InstallStep, ForgeError)> {
        use InstallStep as S;
        let inner = &self.inner;
        let root = inner.root.clone();

        // 1. Verify: nothing is written anywhere before the archive matches.
        self.ext_step(ctx, plan, S::Verify, messages::install_verifying());
        let verified = Verifier::new(&inner.platform)
            .verify_in_job(ctx, &req.archive, plan.expect.clone())
            .await
            .map_err(|e| (S::Verify, e))?;

        // 2. Extract into tmp/stage/<job>/payload (a single file under the target's name).
        let target_dir = PathBuf::from(&req.target.row.path);
        let (stage2, dir2) = (stage.to_path_buf(), target_dir.clone());
        blocking("staging", move || {
            if !dir2.is_dir() {
                return Err(ForgeError::NotFound(format!(
                    "target version folder {} is missing",
                    dir2.display()
                )));
            }
            std::fs::create_dir_all(&stage2).at("creating", &stage2)
        })
        .await
        .map_err(|e| (S::Extract, e))?;
        let payload_dir = stage.join("payload");
        let mut extract =
            ExtractRequest::new(&req.archive, plan.format, &payload_dir, root.tmp_stage())
                .strip_root(req.manifest.spec.install.strip_root)
                .limits(req.options.limits);
        let payload = if plan.format == ArchiveFormat::File {
            let name = plan
                .dest
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .ok_or_else(|| {
                    (
                        S::Extract,
                        ForgeError::InvalidInput("install.target has no file name".into()),
                    )
                })?;
            extract = extract.file_name(name.clone());
            payload_dir.join(name)
        } else {
            payload_dir.clone()
        };
        let extracted = Extractor::new(&inner.platform)
            .extract_in_job(ctx, extract)
            .await
            .map_err(|e| (S::Extract, e))?;
        ctx.check_cancelled().map_err(|e| (S::Extract, e))?;

        // 3. The target's folder changes from here on: take the target tool's turn.
        let _turn = self
            .tool_turn(&plan.link.tool)
            .await
            .map_err(|e| (S::Move, e))?;
        self.ext_step(
            ctx,
            plan,
            S::Move,
            messages::install_extension_moving(plan.link.tool.as_str(), &plan.link.name),
        );
        let (from, dest, id) = (payload.clone(), plan.dest.clone(), ctx.id().to_string());
        let placed = blocking("moving into the target version", move || {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).at("creating", parent)?;
            }
            let aside = if std::fs::symlink_metadata(&dest).is_ok() {
                let name = dest
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let aside = dest.with_file_name(format!(".{name}.forgebox-old-{id}"));
                rename_retry(&dest, &aside).map_err(|e| {
                    ForgeError::Conflict(format!(
                        "{} cannot be replaced (in use by a running process?): {e}",
                        dest.display()
                    ))
                })?;
                Some(aside)
            } else {
                None
            };
            let placed = Placed { dest, aside };
            if let Err(e) = rename_retry(&from, &placed.dest) {
                placed.undo();
                return Err(ForgeError::io("moving into the target version", e));
            }
            Ok(placed)
        })
        .await
        .map_err(|e| (S::Move, e))?;
        let placed = Arc::new(placed);
        let undo = |placed: &Arc<Placed>| {
            let placed = Arc::clone(placed);
            async move {
                let _ = tokio::task::spawn_blocking(move || placed.undo()).await;
            }
        };

        // 4. post_install with the target's variables (no `run`: nothing of its own runs).
        self.ext_step(ctx, plan, S::PostInstall, messages::install_post_install());
        let renderer = Arc::clone(&inner.renderer);
        let (vars, root2, manifest, tool, dir) = (
            plan.vars.clone(),
            root.clone(),
            Arc::clone(&req.manifest),
            plan.link.tool.clone(),
            target_dir.clone(),
        );
        let job = ctx.clone();
        let actions = tokio::task::spawn_blocking(move || {
            let cancelled = || job.is_cancelled();
            let action_ctx = ActionContext {
                root: &root2,
                tool: &tool,
                staged_dir: &dir,
                physical: &vars,
                final_vars: &vars,
                renderer: renderer.as_ref(),
                allow_run: false,
                cancelled: &cancelled,
            };
            actions::run_actions(&action_ctx, &manifest.spec.install.post_install)
        })
        .await
        .map_err(|e| ForgeError::Internal(format!("post_install task failed: {e}")))
        .and_then(|r| r);
        if let Err(err) = actions {
            undo(&placed).await;
            return Err((S::PostInstall, err));
        }

        // 5. Register (an existing row for this target is updated in place).
        self.ext_step(ctx, plan, S::Register, messages::install_registering());
        let new = NewToolVersion {
            tool_id: plan.tool.clone(),
            tool_name: plan.tool_name.clone(),
            version: plan.version.clone(),
            canonical_name: plan.link.row_name(),
            variant: plan.link.apply(req.release.variants.clone()),
            path: posix_string(&plan.dest),
            state: VersionState::Installed,
            source: req.options.source,
        };
        let (sha256, size) = (verified.sha256.clone(), extracted.bytes);
        let registered = inner
            .writer
            .execute(move |conn| {
                ToolVersionRepo::new(conn).record_install(&new, sha256.as_deref(), Some(size))
            })
            .await;
        let (version_id, inserted) = match registered {
            Ok(done) => done,
            Err(err) => {
                undo(&placed).await;
                return Err((S::Register, err));
            }
        };
        let committed = Arc::clone(&placed);
        let _ = tokio::task::spawn_blocking(move || committed.commit()).await;
        Ok(ExtensionReport {
            tool: plan.tool.clone(),
            version: plan.version.clone(),
            canonical_name: plan.link.row_name(),
            version_id,
            target: plan.link.clone(),
            path: plan.dest.clone(),
            rendered: rendered_paths(&req.manifest.spec.install.post_install, &plan.vars),
            sha256: verified.sha256,
            size: extracted.bytes,
            reinstalled: !inserted,
        })
    }

    /// Removes the installation `id` of `manifest` (module docs): its file or folder, the
    /// files its `render` actions wrote for that target unless another installation of the
    /// extension renders the same file, and its row. `target` is the extended tool's
    /// manifest. A file that cannot be moved away (loaded by a running process) is
    /// `conflict` and nothing changes. Takes the target tool's turn.
    pub async fn remove_extension(
        &self,
        manifest: Arc<ExtensionManifest>,
        target: Arc<ToolManifest>,
        id: VersionId,
    ) -> Result<RemovedExtension> {
        let tool = manifest.metadata.id.clone();
        // The extended tool's turn first: an install into the same target waits.
        let _turn = self.tool_turn(&manifest.spec.extends).await?;
        let rows = self.versions(&tool).await?;
        let row = rows
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or_else(|| ForgeError::NotFound(format!("{tool} installation {id}")))?;
        let link = ExtensionLink::of(&row);
        let targets = self.versions(&manifest.spec.extends).await?;
        let target_row = |link: &ExtensionLink| targets.iter().find(|t| t.id == link.id).cloned();
        let vars_of = |row: &ToolVersionRow| {
            ExtensionLink::of(row)
                .and_then(|l| target_row(&l))
                .map(|t| {
                    ExtensionTarget {
                        manifest: Arc::clone(&target),
                        row: t,
                    }
                    .vars(&self.inner.root)
                })
        };
        let actions = &manifest.spec.install.post_install;
        let mine = vars_of(&row)
            .map(|v| rendered_paths(actions, &v))
            .unwrap_or_default();
        let others: Vec<PathBuf> = rows
            .iter()
            .filter(|r| r.id != id && r.state == VersionState::Installed)
            .filter_map(vars_of)
            .flat_map(|v| rendered_paths(actions, &v))
            .collect();
        let (shared, owned): (Vec<PathBuf>, Vec<PathBuf>) = mine
            .into_iter()
            .partition(|p| others.iter().any(|o| same_path(o, p)));

        self.inner
            .writer
            .execute(move |conn| ToolVersionRepo::new(conn).set_state(id, VersionState::Removing))
            .await?;
        let path = PathBuf::from(&row.path);
        let (from, owned2) = (path.clone(), owned.clone());
        let removed = blocking("removing the extension", move || {
            if std::fs::symlink_metadata(&from).is_ok() {
                let name = from
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let aside = from.with_file_name(format!(
                    ".{name}.forgebox-removed-{}",
                    ulid::Ulid::generate()
                ));
                rename_retry(&from, &aside).map_err(|e| {
                    ForgeError::Conflict(format!(
                        "{} cannot be removed (loaded by a running process? stop the pools \
                         of that version or disable the extension first): {e}",
                        from.display()
                    ))
                })?;
                remove_any(&aside);
            }
            let mut removed = Vec::new();
            for file in owned2 {
                match std::fs::remove_file(&file) {
                    Ok(()) => removed.push(file),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => tracing::warn!(file = %file.display(), "could not remove: {e}"),
                }
            }
            Ok(removed)
        })
        .await;
        let removed_files = match removed {
            Ok(files) => files,
            Err(err) => {
                let restore = self
                    .inner
                    .writer
                    .execute(move |conn| {
                        ToolVersionRepo::new(conn).set_state(id, VersionState::Installed)
                    })
                    .await;
                if let Err(e) = restore {
                    tracing::error!(%id, "could not restore the extension state: {e}");
                }
                return Err(err);
            }
        };
        self.inner
            .writer
            .execute(move |conn| ToolVersionRepo::new(conn).delete(id))
            .await?;
        tracing::info!(%tool, target = row.canonical_name, "extension removed");
        Ok(RemovedExtension {
            tool,
            version: row.version,
            version_id: id,
            target: link,
            path,
            removed_files,
            shared_files: shared,
        })
    }

    /// Re-links the extensions installed into `from` whose files `update.migrate` carried
    /// into `to` (same path relative to the version folder): each gets a row for `to`
    /// (the row of `from` goes away with `from`). Returns the new rows' ids.
    pub async fn carry_extensions(
        &self,
        from: &ToolVersionRow,
        to: &ToolVersionRow,
    ) -> Result<Vec<VersionId>> {
        let rows = self
            .inner
            .reader
            .query(|conn| ToolVersionRepo::new(conn).list(None))
            .await?;
        let linked: Vec<ToolVersionRow> = rows
            .into_iter()
            .filter(|r| ExtensionLink::of(r).is_some_and(|l| l.id == from.id))
            .collect();
        if linked.is_empty() {
            return Ok(Vec::new());
        }
        let (old_dir, new_dir) = (PathBuf::from(&from.path), PathBuf::from(&to.path));
        let link = ExtensionLink::to(to);
        let mut carried = Vec::new();
        for row in linked {
            let old_path = PathBuf::from(&row.path);
            let Some(relative) = relative_to(&old_path, &old_dir) else {
                continue;
            };
            let new_path = new_dir.join(relative);
            let probe = new_path.clone();
            let exists = blocking("checking a migrated file", move || {
                Ok(std::fs::symlink_metadata(&probe).is_ok())
            })
            .await?;
            if !exists {
                continue;
            }
            let new = NewToolVersion {
                tool_id: row.tool_id.clone(),
                tool_name: row.tool_id.to_string(),
                version: row.version.clone(),
                canonical_name: link.row_name(),
                variant: link.apply(row.variant.clone()),
                path: posix_string(&new_path),
                state: VersionState::Installed,
                source: row.source,
            };
            let (id, _) = self
                .inner
                .writer
                .execute(move |conn| ToolVersionRepo::new(conn).record_install(&new, None, None))
                .await?;
            tracing::info!(tool = %row.tool_id, target = %link.row_name(), "extension carried over");
            carried.push(id);
        }
        Ok(carried)
    }
}

/// `path` relative to `base` (case-insensitive prefix match), if it is inside it.
fn relative_to(path: &Path, base: &Path) -> Option<String> {
    let p = posix_string(path);
    let b = posix_string(base);
    let b = b.trim_end_matches('/');
    let prefix = format!("{}/", b.to_lowercase());
    p.to_lowercase()
        .starts_with(&prefix)
        .then(|| p[prefix.len()..].to_owned())
}

/// Extension rows linked to the target version `id` (`tool_versions` of every tool).
pub(crate) fn linked_rows(rows: &[ToolVersionRow], id: VersionId) -> Vec<VersionId> {
    rows.iter()
        .filter(|r| ExtensionLink::of(r).is_some_and(|l| l.id == id))
        .map(|r| r.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(tool: &str, name: &str, variants: &[(&str, &str)]) -> ToolVersionRow {
        ToolVersionRow {
            id: VersionId::new(),
            tool_id: ToolId::new(tool).unwrap(),
            version: name.split('-').next().unwrap().to_owned(),
            canonical_name: name.to_owned(),
            variant: variants
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            path: format!("C:/fb/bin/{tool}/{name}"),
            state: VersionState::Installed,
            source: forge_core::store::VersionSource::Registry,
        }
    }

    #[test]
    fn links_round_trip_through_the_variant_map() {
        let target = row("php", "8.4.27-nts-vs17-x64", &[("ts", "nts")]);
        let link = ExtensionLink::to(&target);
        assert_eq!(link.row_name(), "php-8.4.27-nts-vs17-x64");
        let mut ext = row("xdebug", "php-8.4.27-nts-vs17-x64", &[]);
        ext.variant = link.apply(BTreeMap::from([(
            "php_series".to_owned(),
            "8.4".to_owned(),
        )]));
        assert_eq!(ExtensionLink::of(&ext), Some(link));
        assert_eq!(ext.variant["php_series"], "8.4");
        assert_eq!(
            linked_rows(&[ext.clone(), target.clone()], target.id),
            [ext.id]
        );
        assert!(ExtensionLink::of(&target).is_none());
    }

    #[test]
    fn relative_paths_follow_the_version_folder() {
        assert_eq!(
            relative_to(
                Path::new("C:/fb/bin/php/8.4.26/ext/php_xdebug.dll"),
                Path::new(r"C:\fb\bin\PHP\8.4.26")
            )
            .as_deref(),
            Some("ext/php_xdebug.dll")
        );
        assert!(relative_to(Path::new("C:/fb/etc/x"), Path::new("C:/fb/bin")).is_none());
        assert!(inside(
            Path::new("C:/fb/bin/php/8.4/ext/x.dll"),
            Path::new("C:/fb/bin/php/8.4")
        ));
        assert!(!inside(
            Path::new("C:/fb/bin/php/8.4"),
            Path::new("C:/fb/bin/php/8.4")
        ));
        assert!(!inside(
            Path::new("C:/fb/bin/php/8.41/x"),
            Path::new("C:/fb/bin/php/8.4")
        ));
    }
}
