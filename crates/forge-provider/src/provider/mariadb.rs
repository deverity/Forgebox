//! `type: mariadb` (docs/05 §1): official REST API. `GET <base>/` lists the series
//! (`major_releases`), `GET <base>/<series>/` lists its releases with their files and
//! checksums; the Windows build is `mariadb-X.Y.Z-winx64.zip` (the `.msi` and
//! `debugsymbols` files are ignored).
//!
//! * Only series that can contain a version of the request's constraint are fetched
//!   (the constraint is probed with `X.Y.0` and `X.Y.9999`), and a series that fails is
//!   skipped with a warning unless every series fails.
//! * Series whose status is not `Stable` (`RC`, `Preview`, `Alpha`...) are pre-releases:
//!   versions get a `-rc`/`-preview` suffix (`13.1.1-rc`) so channels with `prerelease`
//!   select them and the others do not.
//! * The URL is built from the configured base (`<base>/<version>/<file>`, which
//!   redirects to a mirror), not from the `http://` URLs inside the JSON.

use std::collections::BTreeMap;

use forge_core::error::Result;
use forge_manifest::version::Version;
use futures::StreamExt;
use futures::future::BoxFuture;
use serde::Deserialize;

use super::{
    Ctx, Provider, Release, ResolveRequest, SigKind, SigRef, SigSubject, filter, join_url,
};

const NAME: &str = "mariadb";

pub(crate) struct Mariadb {
    ctx: Ctx,
}

impl Mariadb {
    pub(crate) fn new(ctx: Ctx) -> Self {
        Self { ctx }
    }
}

#[derive(Debug, Deserialize)]
struct SeriesIndex {
    major_releases: Vec<SeriesInfo>,
}

#[derive(Debug, Deserialize)]
struct SeriesInfo {
    release_id: String,
    release_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SeriesReleases {
    releases: BTreeMap<String, ApiRelease>,
}

#[derive(Debug, Deserialize)]
struct ApiRelease {
    date_of_release: Option<String>,
    #[serde(default)]
    files: Vec<ApiFile>,
}

#[derive(Debug, Deserialize)]
struct ApiFile {
    file_name: String,
    os: Option<String>,
    checksum: Option<ApiChecksum>,
}

#[derive(Debug, Deserialize)]
struct ApiChecksum {
    sha256sum: Option<String>,
    sha512sum: Option<String>,
}

/// Suffix that makes a non-stable series a semver pre-release.
fn prerelease_suffix(status: Option<&str>) -> Option<String> {
    match status {
        None | Some("Stable") => None,
        Some(other) => Some(other.to_ascii_lowercase().replace(' ', "-")),
    }
}

fn to_releases(base: &str, series_status: Option<&str>, api: SeriesReleases) -> Vec<Release> {
    let suffix = prerelease_suffix(series_status);
    let mut out = Vec::new();
    for (id, rel) in api.releases {
        let wanted = format!("mariadb-{id}-winx64.zip");
        let Some(file) = rel
            .files
            .iter()
            .find(|f| f.os.as_deref() == Some("Windows") && f.file_name == wanted)
        else {
            continue;
        };
        let version = match &suffix {
            Some(s) => format!("{id}-{s}"),
            None => id.clone(),
        };
        let url = join_url(&join_url(base, &id), &file.file_name);
        let mut release = Release::new(version, url.clone());
        if let Some(sum) = &file.checksum {
            release.sha256 = sum.sha256sum.as_deref().map(str::to_ascii_lowercase);
            release.sha512 = sum.sha512sum.as_deref().map(str::to_ascii_lowercase);
        }
        release.published_at = rel
            .date_of_release
            .as_deref()
            .and_then(|d| format!("{d}T00:00:00Z").parse().ok());
        release.signature = Some(SigRef {
            kind: SigKind::Pgp,
            url: format!("{url}/signature/"),
            subject: SigSubject::Archive,
        });
        release
            .tags
            .push(series_status.map_or_else(|| "stable".to_owned(), str::to_ascii_lowercase));
        out.push(release);
    }
    out
}

impl Provider for Mariadb {
    fn name(&self) -> &'static str {
        NAME
    }

    fn resolve<'a>(&'a self, req: ResolveRequest<'a>) -> BoxFuture<'a, Result<Vec<Release>>> {
        Box::pin(async move {
            let base = &self.ctx.config.endpoints.mariadb;
            let index_url = join_url(base, "");
            let index: SeriesIndex = self.ctx.get(NAME, &index_url).await?.json(&index_url)?;
            let scheme = req.tool.spec.versioning.scheme;

            let series: Vec<(String, Option<String>)> = index
                .major_releases
                .into_iter()
                .filter(|s| {
                    let Some(constraint) = req.constraint else {
                        return true;
                    };
                    // A series is worth fetching if the constraint can match inside it.
                    ["0", "9999"].iter().any(|patch| {
                        Version::parse(scheme, &format!("{}.{patch}", s.release_id))
                            .is_ok_and(|v| constraint.matches_with(&v, true))
                    })
                })
                .map(|s| (s.release_id, s.release_status))
                .collect();

            let results: Vec<(String, Result<Vec<Release>>)> = futures::stream::iter(series)
                .map(|(id, status)| async move {
                    let url = join_url(base, &format!("{id}/"));
                    let result = async {
                        let api: SeriesReleases = self.ctx.get(NAME, &url).await?.json(&url)?;
                        Ok(to_releases(base, status.as_deref(), api))
                    }
                    .await;
                    (url, result)
                })
                .buffer_unordered(4)
                .collect()
                .await;

            let total = results.len();
            let mut releases = Vec::new();
            let mut failures = Vec::new();
            for (url, result) in results {
                match result {
                    Ok(found) => releases.extend(found),
                    Err(err) => {
                        tracing::warn!(url, error = %err, "mariadb series unavailable");
                        failures.push(err);
                    }
                }
            }
            if total > 0 && failures.len() == total {
                return Err(failures.remove(0));
            }
            if total == 0 && releases.is_empty() {
                return Ok(Vec::new());
            }
            filter::finish(&req, releases)
        })
    }
}
