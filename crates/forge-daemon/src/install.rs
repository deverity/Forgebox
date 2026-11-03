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

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::ids::{ToolId, VersionId};
use forge_core::jobs::{JobContext, JobProgress, JobState};
use forge_core::store::ToolVersionRow;
use forge_manifest::schema::ExtensionManifest;
use forge_manifest::schema::tool::{ServiceKind, ToolManifest};
use forge_manifest::version::Constraint;
use forge_provider::download::Priority;
use forge_provider::layout::{
    ExtensionInstallRequest, ExtensionReport, ExtensionTarget, InstallOptions, InstallReport,
    InstallRequest,
};
use forge_provider::provider::{Release, ResolveRequest};
use forge_provider::update::{ArchiveFetcher, DownloadFetcher, FetchArchive, ReleaseSource};
use serde::de::DeserializeOwned;

use crate::catalog::{self, ExtensionEntry};
use crate::daemon::Daemon;

/// Longest wait for one download.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// Longest wait for the layout job (extract, post_install, move).
const INSTALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

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
    let constraint = version
        .as_deref()
        .map(|v| constraint_for(&manifest, v))
        .transpose()?;
    let variants = variant_filter(&manifest, variant)?;
    job.progress(
        JobProgress::new()
            .pct(2.0)
            .message(format!("resolving {spec}")),
    );
    let releases: Vec<Release> = daemon
        .providers
        .resolve(ResolveRequest {
            tool: &manifest,
            constraint: constraint.as_ref(),
            variants: &variants,
            channel: None,
        })
        .await?;
    let mut release = releases
        .into_iter()
        .next()
        .ok_or_else(|| ForgeError::NotFound(format!("no release of {spec} for this platform")))?;
    // Already installed? Same version and variants → nothing to download.
    let installed = catalog::installed(&daemon.reader, &tool_id, Some(&manifest)).await?;
    if let Some(row) = installed.iter().find(|r| {
        r.version == release.version
            && release
                .variants
                .iter()
                .all(|(k, v)| r.variant.get(k) == Some(v))
    }) {
        job.progress(
            JobProgress::new()
                .pct(100.0)
                .message(format!("{} {} already installed", tool_id, row.version)),
        );
        return Ok(row.clone());
    }
    job.check_cancelled()?;
    daemon.providers.complete(&mut release).await?;
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
    let releases = daemon
        .providers
        .resolve(ResolveRequest {
            tool: &view,
            constraint: constraint.as_ref(),
            variants: &BTreeMap::new(),
            channel: None,
        })
        .await?;
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
        daemon.providers.complete(&mut release).await?;
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
}
