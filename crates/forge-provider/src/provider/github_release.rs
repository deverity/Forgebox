//! `type: github-release` (docs/05 §1): `GET /repos/<repo>/releases` (first 100, newest
//! first), one asset per release picked by a case-sensitive regex.
//!
//! * Hash: the `digest` GitHub computes for every asset (`sha256:<hex>`) when present;
//!   otherwise the checksum asset declared in the manifest (`checksums.txt`,
//!   `<asset>.sha256`), read for the newest releases only.
//! * Rate limit: metadata is cached with its `ETag` (a 304 does not count against the
//!   limit); on 403/429 with an exhausted quota a stale cache is served, else the error
//!   says when the quota resets. A token from `prefs` raises the limit; it is sent only
//!   to the configured API host.
//! * Pre-releases only when the manifest asks for them (`prerelease: true`); drafts never.

use forge_core::error::{ForgeError, Result};
use forge_manifest::schema::tool::GithubReleaseProvider;
use futures::future::BoxFuture;
use regex::Regex;
use serde::Deserialize;

use super::checksum::fill_checksums;
use super::fetch::FetchRequest;
use super::{Ctx, Provider, Release, ResolveRequest, filter, join_url};

const NAME: &str = "github-release";

pub(crate) struct GithubRelease {
    ctx: Ctx,
    spec: GithubReleaseProvider,
}

impl GithubRelease {
    pub(crate) fn new(ctx: Ctx, spec: GithubReleaseProvider) -> Self {
        Self { ctx, spec }
    }
}

#[derive(Debug, Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Debug, Deserialize)]
struct ApiAsset {
    name: String,
    size: Option<u64>,
    /// `sha256:<hex>`.
    digest: Option<String>,
    browser_download_url: String,
}

fn compile(pattern: &str, what: &str) -> Result<Regex> {
    Regex::new(pattern)
        .map_err(|e| ForgeError::ConfigInvalid(format!("invalid {what} regex `{pattern}`: {e}")))
}

fn version_of(tag: &str, version_re: Option<&Regex>) -> Option<String> {
    match version_re {
        Some(re) => re
            .captures(tag)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_owned()),
        None => Some(tag.strip_prefix('v').unwrap_or(tag).to_owned()),
    }
}

fn to_releases(
    spec: &GithubReleaseProvider,
    api: Vec<ApiRelease>,
    asset_re: &Regex,
    version_re: Option<&Regex>,
) -> Vec<Release> {
    let mut out = Vec::new();
    for rel in api {
        if rel.draft || (rel.prerelease && !spec.prerelease) {
            continue;
        }
        let Some(version) = version_of(&rel.tag_name, version_re) else {
            tracing::debug!(tag = rel.tag_name, "tag does not match `version_regex`");
            continue;
        };
        let Some(asset) = rel.assets.iter().find(|a| asset_re.is_match(&a.name)) else {
            continue;
        };
        let mut release = Release::new(version, asset.browser_download_url.clone());
        release.file_name = asset.name.clone();
        release.size = asset.size;
        release.published_at = rel.published_at.as_deref().and_then(|t| t.parse().ok());
        if rel.prerelease {
            release.tags.push("prerelease".to_owned());
        }
        release.sha256 = asset
            .digest
            .as_deref()
            .and_then(|d| d.strip_prefix("sha256:"))
            .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(str::to_ascii_lowercase);
        if release.sha256.is_none() {
            if let Some(checksum) = &spec.checksum {
                let wanted = checksum.asset.replace("{asset}", &asset.name);
                if let Some(doc) = rel.assets.iter().find(|a| a.name == wanted) {
                    release.checksum_url = Some(doc.browser_download_url.clone());
                }
            }
        }
        out.push(release);
    }
    out
}

impl Provider for GithubRelease {
    fn name(&self) -> &'static str {
        NAME
    }

    fn resolve<'a>(&'a self, req: ResolveRequest<'a>) -> BoxFuture<'a, Result<Vec<Release>>> {
        Box::pin(async move {
            let asset_re = compile(&self.spec.asset, "asset")?;
            let version_re = self
                .spec
                .version_regex
                .as_deref()
                .map(|p| compile(p, "version_regex"))
                .transpose()?;
            let url = join_url(
                &self.ctx.config.endpoints.github_api,
                &format!("repos/{}/releases?per_page=100", self.spec.repo),
            );
            let fetched = self
                .ctx
                .fetcher
                .get(FetchRequest {
                    provider: NAME,
                    url: &url,
                    accept: Some("application/vnd.github+json"),
                    bearer: self.ctx.config.github_token.as_deref(),
                })
                .await?;
            let api: Vec<ApiRelease> = fetched.json(&url)?;
            let releases = to_releases(&self.spec, api, &asset_re, version_re.as_ref());
            let mut releases = filter::finish(&req, releases)?;
            fill_checksums(
                &self.ctx.fetcher,
                NAME,
                &mut releases,
                self.ctx.config.max_checksum_fetches,
            )
            .await;
            Ok(releases)
        })
    }
}
