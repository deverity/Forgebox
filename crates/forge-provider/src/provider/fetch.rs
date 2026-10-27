//! Metadata fetching with an on-disk cache (docs/05 §1): `state/registry/cache/<provider>/`
//! holds the body and a small JSON sidecar (`ETag`, `Last-Modified`, fetch time) per URL.
//!
//! * Fresh entries (younger than the TTL) are served without touching the network.
//! * Older entries are revalidated with `If-None-Match`/`If-Modified-Since`; a 304 renews
//!   the entry.
//! * When the network fails (or GitHub's rate limit is hit) a stale entry is served
//!   instead of an error; without an entry the error is reported.
//! * `prefs.offline` serves the cache only, whatever its age.
//!
//! All disk I/O goes through `tokio::fs` (blocking pool) and every request is wrapped
//! in `tokio::time::timeout` (ADR-0011).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use forge_core::error::{ForgeError, Result};
use reqwest::StatusCode;
use reqwest::header::{
    ACCEPT, AUTHORIZATION, ETAG, HeaderName, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::ProviderConfig;
use super::messages;
use crate::download::ProxySetting;

/// Largest metadata body accepted (the biggest real one is a few MB).
const MAX_BODY: u64 = 64 * 1024 * 1024;

/// Where the bytes of a [`Fetched`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchSource {
    /// Downloaded (or revalidated with a 304) just now.
    Network,
    /// Fresh cache entry, no request made.
    Cache,
    /// Expired entry served because the network failed or `offline` is on.
    Stale,
}

#[derive(Debug, Clone)]
pub struct Fetched {
    pub body: Bytes,
    pub source: FetchSource,
}

impl Fetched {
    /// The body as UTF-8 text (lossy; a BOM is dropped).
    pub fn text(&self) -> String {
        let text = String::from_utf8_lossy(&self.body);
        text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned()
    }

    pub fn json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        serde_json::from_slice(&self.body)
            .map_err(|e| ForgeError::InvalidInput(format!("unexpected JSON from {url}: {e}")))
    }
}

/// One metadata request.
#[derive(Debug, Clone, Default)]
pub struct FetchRequest<'a> {
    /// Cache folder name (the provider `type`).
    pub provider: &'a str,
    pub url: &'a str,
    pub accept: Option<&'a str>,
    /// `Authorization: Bearer` value (GitHub token).
    pub bearer: Option<&'a str>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Meta {
    url: String,
    etag: Option<String>,
    last_modified: Option<String>,
    /// Unix seconds of the last successful (re)validation.
    fetched_at: u64,
}

/// Cached HTTP GET shared by the providers of a [`super::ProviderRegistry`].
pub struct Fetcher {
    client: reqwest::Client,
    cache_dir: PathBuf,
    ttl: Duration,
    request_timeout: Duration,
    offline: Arc<AtomicBool>,
}

impl Fetcher {
    pub fn new(config: &ProviderConfig) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .user_agent(config.user_agent.clone())
            .connect_timeout(config.connect_timeout);
        builder = match &config.proxy {
            ProxySetting::System => builder,
            ProxySetting::Direct => builder.no_proxy(),
            ProxySetting::Manual(url) => {
                let proxy = reqwest::Proxy::all(url.as_str())
                    .map_err(|e| ForgeError::ConfigInvalid(format!("invalid proxy {url}: {e}")))?;
                builder.proxy(proxy)
            }
        };
        let client = builder
            .build()
            .map_err(|e| ForgeError::Internal(format!("could not build the HTTP client: {e}")))?;
        Ok(Self {
            client,
            cache_dir: config.cache_dir.clone(),
            ttl: config.ttl,
            request_timeout: config.request_timeout,
            offline: Arc::new(AtomicBool::new(config.offline)),
        })
    }

    pub fn set_offline(&self, offline: bool) {
        self.offline.store(offline, Ordering::SeqCst);
    }

    pub fn is_offline(&self) -> bool {
        self.offline.load(Ordering::SeqCst)
    }

    pub async fn get(&self, req: FetchRequest<'_>) -> Result<Fetched> {
        let dir = self.cache_dir.join(req.provider);
        let key = cache_key(req.url);
        let cached = read_entry(&dir, &key).await;

        if self.is_offline() {
            return match cached {
                Some((_, body)) => Ok(Fetched {
                    body,
                    source: FetchSource::Stale,
                }),
                None => Err(ForgeError::Conflict(messages::offline_no_cache(req.url))),
            };
        }
        if let Some((meta, body)) = &cached {
            if now_secs().saturating_sub(meta.fetched_at) < self.ttl.as_secs() {
                return Ok(Fetched {
                    body: body.clone(),
                    source: FetchSource::Cache,
                });
            }
        }

        match self.fetch(&req, &dir, &key, cached.as_ref()).await {
            Ok(fetched) => Ok(fetched),
            Err(err) => match cached {
                Some((_, body)) if !matches!(err, ForgeError::NotFound(_)) => {
                    tracing::warn!(url = req.url, error = %err, "{}", messages::stale_cache(req.url));
                    Ok(Fetched {
                        body,
                        source: FetchSource::Stale,
                    })
                }
                _ => Err(err),
            },
        }
    }

    async fn fetch(
        &self,
        req: &FetchRequest<'_>,
        dir: &Path,
        key: &str,
        cached: Option<&(Meta, Bytes)>,
    ) -> Result<Fetched> {
        let mut builder = self.client.get(req.url);
        if let Some(accept) = req.accept {
            builder = builder.header(ACCEPT, accept);
        }
        if let Some(token) = req.bearer {
            builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some((meta, _)) = cached {
            if let Some(etag) = &meta.etag {
                builder = builder.header(IF_NONE_MATCH, etag);
            } else if let Some(modified) = &meta.last_modified {
                builder = builder.header(IF_MODIFIED_SINCE, modified);
            }
        }
        let timeout_err = || ForgeError::Timeout {
            operation: format!("GET {}", req.url),
            after: self.request_timeout,
        };
        let net_err = |e: reqwest::Error| {
            ForgeError::io(
                messages::upstream_unavailable(req.url, "falha de rede"),
                io::Error::other(describe(&e)),
            )
        };

        let resp = tokio::time::timeout(self.request_timeout, builder.send())
            .await
            .map_err(|_| timeout_err())?
            .map_err(net_err)?;
        let status = resp.status();

        if status == StatusCode::NOT_MODIFIED {
            if let Some((meta, body)) = cached {
                let renewed = Meta {
                    url: meta.url.clone(),
                    etag: meta.etag.clone(),
                    last_modified: meta.last_modified.clone(),
                    fetched_at: now_secs(),
                };
                write_meta(dir, key, &renewed).await;
                return Ok(Fetched {
                    body: body.clone(),
                    source: FetchSource::Network,
                });
            }
            return Err(ForgeError::Internal(format!(
                "{} answered 304 to an unconditional request",
                req.url
            )));
        }
        if status == StatusCode::NOT_FOUND {
            return Err(ForgeError::NotFound(req.url.to_owned()));
        }
        if status == StatusCode::TOO_MANY_REQUESTS
            || (status == StatusCode::FORBIDDEN
                && resp
                    .headers()
                    .get("x-ratelimit-remaining")
                    .and_then(|v| v.to_str().ok())
                    == Some("0"))
        {
            let reset = resp
                .headers()
                .get("x-ratelimit-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok())
                .and_then(|secs| jiff::Timestamp::from_second(secs).ok())
                .map(|ts| ts.to_string());
            return Err(ForgeError::io(
                messages::rate_limited(req.url, reset.as_deref()),
                io::Error::other(format!("HTTP {status}")),
            ));
        }
        if !status.is_success() {
            return Err(ForgeError::io(
                messages::upstream_unavailable(req.url, &format!("HTTP {status}")),
                io::Error::other(format!("HTTP {status}")),
            ));
        }
        if resp.content_length().is_some_and(|len| len > MAX_BODY) {
            return Err(ForgeError::InvalidInput(format!(
                "{} is larger than {MAX_BODY} bytes",
                req.url
            )));
        }
        let etag = header(&resp, ETAG);
        let last_modified = header(&resp, LAST_MODIFIED);
        let body = tokio::time::timeout(self.request_timeout, resp.bytes())
            .await
            .map_err(|_| timeout_err())?
            .map_err(net_err)?;
        if body.len() as u64 > MAX_BODY {
            return Err(ForgeError::InvalidInput(format!(
                "{} is larger than {MAX_BODY} bytes",
                req.url
            )));
        }
        write_entry(
            dir,
            key,
            &Meta {
                url: req.url.to_owned(),
                etag,
                last_modified,
                fetched_at: now_secs(),
            },
            &body,
        )
        .await;
        Ok(Fetched {
            body,
            source: FetchSource::Network,
        })
    }
}

fn header(resp: &reqwest::Response, name: HeaderName) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn describe(err: &(dyn std::error::Error + 'static)) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(inner) = source {
        text.push_str(": ");
        text.push_str(&inner.to_string());
        source = inner.source();
    }
    text
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// First 24 hex digits of the sha256 of the URL.
fn cache_key(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    digest.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

async fn read_entry(dir: &Path, key: &str) -> Option<(Meta, Bytes)> {
    let meta = tokio::fs::read(dir.join(format!("{key}.json")))
        .await
        .ok()?;
    let meta: Meta = serde_json::from_slice(&meta).ok()?;
    let body = tokio::fs::read(dir.join(format!("{key}.body")))
        .await
        .ok()?;
    Some((meta, Bytes::from(body)))
}

async fn write_meta(dir: &Path, key: &str, meta: &Meta) {
    if let Err(err) = write_meta_inner(dir, key, meta).await {
        tracing::warn!(error = %err, "could not write the metadata cache");
    }
}

async fn write_meta_inner(dir: &Path, key: &str, meta: &Meta) -> io::Result<()> {
    let bytes = serde_json::to_vec(meta).map_err(io::Error::other)?;
    atomic_write(&dir.join(format!("{key}.json")), &bytes).await
}

/// A cache that cannot be written is a warning, never a failure of the request.
async fn write_entry(dir: &Path, key: &str, meta: &Meta, body: &[u8]) {
    let result = async {
        tokio::fs::create_dir_all(dir).await?;
        // Body first: a sidecar without a body is ignored by `read_entry`.
        atomic_write(&dir.join(format!("{key}.body")), body).await?;
        write_meta_inner(dir, key, meta).await
    }
    .await;
    if let Err(err) = result {
        tracing::warn!(error = %err, "could not write the metadata cache");
    }
}

/// Every write gets its own temp file (unique per call, same directory), so
/// concurrent fetches of the same URL can never interleave body and sidecar.
async fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!("{name}.{}.tmp", ulid::Ulid::generate()));
    if let Err(err) = tokio::fs::write(&tmp, bytes).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(err);
    }
    if let Err(err) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(err);
    }
    Ok(())
}
