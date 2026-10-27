//! Native providers (docs/05 §1): `(tool, constraint, variants, channel) → Vec<Release>`.
//!
//! A [`Provider`] is built from the `source.providers[]` entry of a manifest by
//! [`ProviderRegistry::for_spec`], the only place that matches on the provider type
//! (this is per provider, not per tool: the tool-specific data lives in the manifests).
//!
//! What every provider guarantees:
//! * metadata goes through the cached [`fetch::Fetcher`] (TTL + `ETag`, proxy,
//!   `prefs.offline` = cache only);
//! * it returns **all** variants (NTS/TS, VS16/VS17...) so the UI can explain them; the
//!   result is sorted preferred-first (newest version, then the variant order and
//!   `default` of the manifest, [`forge_manifest::version::cmp_builds`]), so the first
//!   item is what an unqualified install picks;
//! * `sha256` is filled whenever upstream publishes it or the manifest curates it in
//!   `source.releases` (merged by [`filter::finish`]); otherwise it stays `None` and the
//!   caller must refuse the download (or require `--allow-unverified`);
//! * when the hash lives in a second document (Node `SHASUMS256.txt`, Apache Lounge
//!   `.txt`) only the newest [`ProviderConfig::max_checksum_fetches`] releases are
//!   resolved eagerly; the others carry [`Release::checksum_url`] for a lazy lookup
//!   with [`resolve_checksum`].

mod apache_lounge;
mod checksum;
mod curated;
mod fetch;
mod filter;
mod github_release;
mod http;
mod mariadb;
mod messages;
mod mysql;
mod nginx_org;
mod nodejs_dist;
mod php_windows;
mod postgresql_edb;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_manifest::schema::ToolManifest;
pub use forge_manifest::schema::tool::Provider as ProviderSpec;
use forge_manifest::version::Constraint;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

pub use self::checksum::{Digests, parse_checksums, resolve_checksum};
pub use self::fetch::{FetchRequest, FetchSource, Fetched, Fetcher};
use crate::download::ProxySetting;

/// What to resolve.
#[derive(Debug, Clone, Copy)]
pub struct ResolveRequest<'a> {
    pub tool: &'a ToolManifest,
    /// Only versions that satisfy it; `None` = every version.
    pub constraint: Option<&'a Constraint>,
    /// Only builds with these variant values (`{ ts: nts }`); empty = all variants.
    pub variants: &'a BTreeMap<String, String>,
    /// A key of `versioning.channels` (`stable`, `preview`, `lts`); `None` = stable
    /// versions only (no pre-releases).
    pub channel: Option<&'a str>,
}

/// Detached signature of a [`Release`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SigRef {
    pub kind: SigKind,
    pub url: String,
    pub subject: SigSubject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigKind {
    /// Detached OpenPGP signature (`.asc` armored or `.sig` binary).
    Pgp,
}

/// What the signature covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigSubject {
    /// The archive itself (`nginx-1.30.6.zip.asc`).
    Archive,
    /// The checksum document, which in turn lists the archive (`SHASUMS256.txt.sig`).
    ChecksumFile { url: String },
}

/// One downloadable build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// Normalized version (`8.3.36+1` for a PHP rebuild, `26.12.1` for Node).
    pub version: String,
    pub url: String,
    pub sha256: Option<String>,
    pub sha512: Option<String>,
    /// Exact bytes; `None` when upstream only gives a rounded figure (PHP `"32.75MB"`).
    pub size: Option<u64>,
    /// Variant values of this build (`{ ts: nts, compiler: vs17, arch: x64 }`).
    pub variants: BTreeMap<String, String>,
    pub published_at: Option<jiff::Timestamp>,
    pub signature: Option<SigRef>,
    /// Name of the file as published (last URL segment).
    pub file_name: String,
    /// Document holding the hash when it was not resolved eagerly.
    pub checksum_url: Option<String>,
    /// Provider labels: `stable`, `mainline`, `lts`, `lts:Krypton`, `qa`...
    pub tags: Vec<String>,
}

impl Release {
    pub(crate) fn new(version: impl Into<String>, url: impl Into<String>) -> Self {
        let url = url.into();
        let file_name = file_name_of(&url);
        Self {
            version: version.into(),
            url,
            sha256: None,
            sha512: None,
            size: None,
            variants: BTreeMap::new(),
            published_at: None,
            signature: None,
            file_name,
            checksum_url: None,
            tags: Vec::new(),
        }
    }
}

/// Last path segment of `url`, without query or fragment.
pub(crate) fn file_name_of(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

/// A source of releases.
pub trait Provider: Send + Sync {
    /// The manifest `type` (`php-windows`...).
    fn name(&self) -> &'static str;

    /// Lists the releases that satisfy `req` (see the module docs for the guarantees).
    fn resolve<'a>(&'a self, req: ResolveRequest<'a>) -> BoxFuture<'a, Result<Vec<Release>>>;
}

/// Base URLs of the upstreams, without trailing slash. The defaults are the real ones;
/// tests point them at a local server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// `releases/releases.json`, `releases/archives/` and `qa/` live below it.
    pub php_windows: String,
    pub apache_lounge: String,
    pub nginx_org: String,
    pub github_api: String,
    /// `index.json` and `vX.Y.Z/SHASUMS256.txt` live below it.
    pub nodejs_dist: String,
    /// `<series>/` and the index of series live below it.
    pub mariadb: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            php_windows: "https://downloads.php.net/~windows".to_owned(),
            apache_lounge: "https://www.apachelounge.com".to_owned(),
            nginx_org: "https://nginx.org".to_owned(),
            github_api: "https://api.github.com".to_owned(),
            nodejs_dist: "https://nodejs.org/dist".to_owned(),
            mariadb: "https://downloads.mariadb.org/rest-api/mariadb".to_owned(),
        }
    }
}

/// Settings of a [`ProviderRegistry`].
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// `state/registry/cache/` (one folder per provider below it).
    pub cache_dir: PathBuf,
    /// How long a cached document is served without revalidation.
    pub ttl: Duration,
    /// `prefs.offline`: cache only.
    pub offline: bool,
    pub proxy: ProxySetting,
    pub user_agent: String,
    /// Optional GitHub token (`prefs`), sent only to [`Endpoints::github_api`].
    pub github_token: Option<String>,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    /// How many releases get their hash from a second document at resolve time.
    pub max_checksum_fetches: usize,
    pub endpoints: Endpoints,
}

impl ProviderConfig {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            ttl: Duration::from_secs(3600),
            offline: false,
            proxy: ProxySetting::System,
            user_agent: format!("Forgebox/{}", env!("CARGO_PKG_VERSION")),
            github_token: None,
            connect_timeout: Duration::from_secs(15),
            request_timeout: Duration::from_secs(30),
            max_checksum_fetches: 20,
            endpoints: Endpoints::default(),
        }
    }
}

/// Builds providers that share one cached HTTP client.
pub struct ProviderRegistry {
    fetcher: Arc<Fetcher>,
    config: Arc<ProviderConfig>,
}

impl ProviderRegistry {
    pub fn new(config: ProviderConfig) -> Result<Self> {
        Ok(Self {
            fetcher: Arc::new(Fetcher::new(&config)?),
            config: Arc::new(config),
        })
    }

    /// Switches `prefs.offline` at run time.
    pub fn set_offline(&self, offline: bool) {
        self.fetcher.set_offline(offline);
    }

    pub fn fetcher(&self) -> &Arc<Fetcher> {
        &self.fetcher
    }

    /// The provider for one `source.providers[]` entry. This is the only `match` on the
    /// provider type. `local` has nothing to resolve and `python-embed` is not
    /// implemented yet (`not_implemented`).
    pub fn for_spec(&self, spec: &ProviderSpec) -> Result<Box<dyn Provider>> {
        let ctx = Ctx {
            fetcher: self.fetcher.clone(),
            config: self.config.clone(),
        };
        Ok(match spec {
            ProviderSpec::PhpWindows(s) => Box::new(php_windows::PhpWindows::new(ctx, s.clone())),
            ProviderSpec::ApacheLounge(s) => {
                Box::new(apache_lounge::ApacheLounge::new(ctx, s.clone()))
            }
            ProviderSpec::NginxOrg(_) => Box::new(nginx_org::NginxOrg::new(ctx)),
            ProviderSpec::GithubRelease(s) => {
                Box::new(github_release::GithubRelease::new(ctx, s.clone()))
            }
            ProviderSpec::Mysql(_) => Box::new(mysql::Mysql),
            ProviderSpec::Mariadb(_) => Box::new(mariadb::Mariadb::new(ctx)),
            ProviderSpec::PostgresqlEdb(_) => Box::new(postgresql_edb::PostgresqlEdb),
            ProviderSpec::NodejsDist(_) => Box::new(nodejs_dist::NodejsDist::new(ctx)),
            ProviderSpec::Http(s) => Box::new(http::Http::new(ctx, s.clone())),
            ProviderSpec::PythonEmbed(_) => {
                return Err(ForgeError::NotImplemented(
                    "provider `python-embed`".to_owned(),
                ));
            }
            ProviderSpec::Local(_) => {
                return Err(ForgeError::InvalidInput(
                    "provider `local` has no remote releases to resolve".to_owned(),
                ));
            }
        })
    }

    /// Resolves `req` with the providers of the manifest in order; the first one that
    /// returns releases wins (docs/04 `source.providers`). If none does, the error of the
    /// last failing provider is returned (or an empty list when all were just empty).
    pub async fn resolve(&self, req: ResolveRequest<'_>) -> Result<Vec<Release>> {
        let mut last_err = None;
        for spec in &req.tool.spec.source.providers {
            let provider = match self.for_spec(spec) {
                Ok(provider) => provider,
                Err(ForgeError::InvalidInput(_)) => continue,
                Err(err) => {
                    last_err = Some(err);
                    continue;
                }
            };
            match provider.resolve(req).await {
                Ok(releases) if !releases.is_empty() => return Ok(releases),
                Ok(_) => {}
                Err(err) => {
                    tracing::warn!(provider = provider.name(), error = %err, "provider failed");
                    last_err = Some(err);
                }
            }
        }
        match last_err {
            Some(err) => Err(err),
            None => Ok(Vec::new()),
        }
    }
}

/// Shared by the HTTP providers.
#[derive(Clone)]
pub(crate) struct Ctx {
    pub(crate) fetcher: Arc<Fetcher>,
    pub(crate) config: Arc<ProviderConfig>,
}

impl Ctx {
    pub(crate) async fn get(&self, provider: &str, url: &str) -> Result<Fetched> {
        self.fetcher
            .get(FetchRequest {
                provider,
                url,
                ..FetchRequest::default()
            })
            .await
    }
}

/// Joins a base URL and a path with exactly one `/`.
pub(crate) fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}
