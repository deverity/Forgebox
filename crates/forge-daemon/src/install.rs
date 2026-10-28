//! `tools.install` (docs/03 §5.1): provider → download → verify/extract/layout, as one
//! daemon job per request. The pieces are `forge-provider`'s: releases resolved by the
//! manifest's providers (`ProviderRegistry`), checksum completed when it lives in a
//! second document, the archive fetched through the download queue with its sha256 and
//! size enforced, then `Layout::install_from_archive` (its own job, awaited here with the
//! progress forwarded). A release without a checksum is refused by the layout engine.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::ids::ToolId;
use forge_core::jobs::{JobContext, JobProgress, JobState};
use forge_core::store::ToolVersionRow;
use forge_manifest::schema::tool::ToolManifest;
use forge_manifest::version::Constraint;
use forge_provider::download::Priority;
use forge_provider::layout::{InstallOptions, InstallReport, InstallRequest};
use forge_provider::provider::{Release, ResolveRequest};
use forge_provider::update::{ArchiveFetcher, DownloadFetcher, FetchArchive, ReleaseSource};

use crate::catalog;
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

async fn wait_layout_job(
    daemon: &Daemon,
    job: &JobContext,
    id: forge_core::ids::JobId,
) -> Result<InstallReport> {
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
    let report = wait_layout_job(daemon, job, install_job).await?;
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
}
