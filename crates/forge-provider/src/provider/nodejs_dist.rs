//! `type: nodejs-dist` (docs/05 §1): `dist/index.json` lists every version with its
//! `files` and `lts` codename; the zip is `vX.Y.Z/node-vX.Y.Z-win-x64.zip` and its sha256
//! is in `vX.Y.Z/SHASUMS256.txt`, which is signed (`SHASUMS256.txt.sig`, PGP; the
//! signature subject is the checksum file, not the zip).
//!
//! * Versions without a `win-x64-zip` file are skipped.
//! * Tags: `lts` and `lts:<Codename>` for LTS lines. A request for the channel named
//!   `lts` keeps only those; any other channel is decided by its manifest constraint.
//! * `SHASUMS256.txt` is read eagerly for the newest
//!   [`super::ProviderConfig::max_checksum_fetches`] releases; the others keep
//!   `checksum_url` (and `signature`) for a lazy lookup.

use forge_core::error::Result;
use futures::future::BoxFuture;
use serde::Deserialize;

use super::checksum::fill_checksums;
use super::{
    Ctx, Provider, Release, ResolveRequest, SigKind, SigRef, SigSubject, filter, join_url,
};

const NAME: &str = "nodejs-dist";

pub(crate) struct NodejsDist {
    ctx: Ctx,
}

impl NodejsDist {
    pub(crate) fn new(ctx: Ctx) -> Self {
        Self { ctx }
    }
}

#[derive(Debug, Deserialize)]
struct IndexEntry {
    /// `v22.11.0`.
    version: String,
    /// `2024-10-29`.
    date: Option<String>,
    #[serde(default)]
    files: Vec<String>,
    /// `false` or the codename (`"Jod"`).
    #[serde(default)]
    lts: serde_json::Value,
}

fn to_releases(dist: &str, entries: Vec<IndexEntry>) -> Vec<Release> {
    let mut out = Vec::new();
    for entry in entries {
        if !entry.files.iter().any(|f| f == "win-x64-zip") {
            continue;
        }
        let tag = &entry.version;
        let bare = tag.strip_prefix('v').unwrap_or(tag);
        let dir = join_url(dist, tag);
        let mut release = Release::new(bare, join_url(&dir, &format!("node-{tag}-win-x64.zip")));
        let sums = join_url(&dir, "SHASUMS256.txt");
        release.signature = Some(SigRef {
            kind: SigKind::Pgp,
            url: format!("{sums}.sig"),
            subject: SigSubject::ChecksumFile { url: sums.clone() },
        });
        release.checksum_url = Some(sums);
        release.published_at = entry
            .date
            .as_deref()
            .and_then(|d| format!("{d}T00:00:00Z").parse().ok());
        if let Some(codename) = entry.lts.as_str() {
            release.tags.push("lts".to_owned());
            release.tags.push(format!("lts:{codename}"));
        }
        out.push(release);
    }
    out
}

impl Provider for NodejsDist {
    fn name(&self) -> &'static str {
        NAME
    }

    fn resolve<'a>(&'a self, req: ResolveRequest<'a>) -> BoxFuture<'a, Result<Vec<Release>>> {
        Box::pin(async move {
            let dist = &self.ctx.config.endpoints.nodejs_dist;
            let url = join_url(dist, "index.json");
            let entries: Vec<IndexEntry> = self.ctx.get(NAME, &url).await?.json(&url)?;
            let mut releases = to_releases(dist, entries);
            if req.channel == Some("lts") {
                releases.retain(|r| r.tags.iter().any(|t| t == "lts"));
            }
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
