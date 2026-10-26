//! The transfer itself: one mirror at a time, sequential or segmented (several `Range`
//! connections), resumable through the `.part` file, hashed while streaming.
//!
//! Nothing here blocks the runtime: file writes and hashing happen in `spawn_blocking`
//! in buffers of [`DownloadConfig::buffer_size`] (1 MiB), network I/O is async, and every
//! await point that can last is raced against the job's cancellation token.

use std::fmt::Write as _;
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use forge_core::error::ForgeError;
use forge_core::jobs::{JobContext, JobProgress};
use futures::StreamExt;
use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::limiter::RateLimiter;
use super::progress::Progress;
use super::{
    DownloadConfig, DownloadOutcome, DownloadRequest, FailureKind, MirrorFailure, messages,
};

/// Segments are never smaller than this.
const MIN_SEGMENT: u64 = 1024 * 1024;

/// Why a step failed, which decides what happens next.
#[derive(Debug)]
pub(crate) enum Fail {
    /// Cancellation was requested; the `.part` stays for a later resume.
    Cancelled,
    /// Network hiccup: retry the same mirror, resuming.
    Transient(String),
    /// This mirror cannot serve the file: try the next one.
    Mirror(String),
    /// The bytes are not the expected ones (size or sha256).
    Verification(String),
    /// Local failure (disk, permissions): no mirror can fix it.
    Local(ForgeError),
}

type Step<T> = std::result::Result<T, Fail>;

/// Shared by every transfer of one manager.
pub(crate) struct Env {
    pub(crate) client: reqwest::Client,
    pub(crate) limiter: Arc<RateLimiter>,
    pub(crate) config: Arc<DownloadConfig>,
}

/// Name (without extension) of the partial file of `req`: first 16 hex digits of the
/// expected sha256, or of the sha256 of the first URL when there is none.
pub(crate) fn part_key(req: &DownloadRequest) -> String {
    let source = match &req.expected_sha256 {
        Some(sha) => sha.to_ascii_lowercase(),
        None => hex(&Sha256::digest(
            req.urls.first().map(Url::as_str).unwrap_or("").as_bytes(),
        )),
    };
    source.chars().take(16).collect()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn local_join(err: tokio::task::JoinError) -> Fail {
    Fail::Local(ForgeError::Internal(format!("blocking task failed: {err}")))
}

async fn blocking<T, F>(f: F) -> Step<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(local_join)
}

async fn cancellable<T>(token: &CancellationToken, fut: impl Future<Output = T>) -> Step<T> {
    tokio::select! {
        biased;
        () = token.cancelled() => Err(Fail::Cancelled),
        value = fut => Ok(value),
    }
}

fn describe(err: &(dyn std::error::Error + 'static)) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(inner) = source {
        let _ = write!(text, ": {inner}");
        source = inner.source();
    }
    text
}

fn status_fail(status: StatusCode) -> Fail {
    let text = format!("HTTP {status}");
    match status.as_u16() {
        408 | 429 | 502 | 503 | 504 => Fail::Transient(text),
        _ => Fail::Mirror(text),
    }
}

/// Parses `bytes START-END/TOTAL` (total may be `*`).
fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    Some((start.trim().parse().ok()?, end.trim().parse().ok()?, total))
}

/// Parses `bytes */TOTAL` (answer of a 416).
fn parse_unsatisfied_total(value: &str) -> Option<u64> {
    value
        .trim()
        .strip_prefix("bytes")?
        .trim_start()
        .strip_prefix("*/")?
        .trim()
        .parse()
        .ok()
}

fn content_range(resp: &Response) -> Option<(u64, u64, Option<u64>)> {
    resp.headers()
        .get(CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_range)
}

fn content_length(resp: &Response) -> Option<u64> {
    resp.headers()
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Positions of a segmented download, persisted next to the `.part` so that a later
/// attempt only fetches what is missing. A stale (lower) position is harmless: bytes
/// are simply fetched again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SegmentMap {
    pub(crate) total: u64,
    pub(crate) segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Segment {
    pub(crate) start: u64,
    /// Exclusive.
    pub(crate) end: u64,
    /// Next byte to fetch (`start..=end`).
    pub(crate) pos: u64,
}

impl SegmentMap {
    pub(crate) fn split(total: u64, connections: usize) -> Self {
        let by_size = usize::try_from(total.div_ceil(MIN_SEGMENT)).unwrap_or(usize::MAX);
        let count = connections.clamp(1, by_size.max(1)) as u64;
        let base = total / count;
        let mut segments = Vec::new();
        let mut start = 0;
        for i in 0..count {
            let end = if i + 1 == count { total } else { start + base };
            segments.push(Segment {
                start,
                end,
                pos: start,
            });
            start = end;
        }
        Self { total, segments }
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.segments.iter().all(|s| s.pos >= s.end)
    }

    pub(crate) fn done(&self) -> u64 {
        self.segments.iter().map(|s| s.pos - s.start).sum()
    }

    pub(crate) fn valid(&self) -> bool {
        let mut expected_start = 0;
        for s in &self.segments {
            if s.start != expected_start || s.end < s.start || s.pos < s.start || s.pos > s.end {
                return false;
            }
            expected_start = s.end;
        }
        !self.segments.is_empty() && expected_start == self.total
    }
}

/// What the `.part` on disk looks like.
#[derive(Debug)]
enum PartState {
    Fresh,
    Sequential(u64),
    Segmented(SegmentMap),
}

/// Result of a successful transfer, before final verification.
struct Transferred {
    sha256: String,
    size: u64,
}

/// Outcome of the single-connection path.
enum Seq {
    Done(Transferred),
    /// The first response showed a big file on a server that honours `Range`: the
    /// caller continues with this many bytes over several connections.
    Segments(u64),
}

/// A file being written: blocking handle plus the hasher, moved into `spawn_blocking`
/// for every write.
struct Sink {
    inner: Option<SinkInner>,
}

struct SinkInner {
    file: std::fs::File,
    hasher: Option<Sha256>,
    path: PathBuf,
}

impl Sink {
    /// Opens `path` for writing at `offset`, truncating anything beyond. With `hasher`
    /// the bytes written are hashed (the caller must have fed it the first `offset`
    /// bytes).
    async fn open(path: &Path, offset: u64, hasher: Sha256) -> Step<Self> {
        let path = path.to_path_buf();
        blocking(move || {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
                .map_err(|e| ForgeError::io_at("opening", &path, e))?;
            file.set_len(offset)
                .map_err(|e| ForgeError::io_at("truncating", &path, e))?;
            file.seek(SeekFrom::Start(offset))
                .map_err(|e| ForgeError::io_at("seeking", &path, e))?;
            Ok(SinkInner {
                file,
                hasher: Some(hasher),
                path,
            })
        })
        .await?
        .map(|inner| Self { inner: Some(inner) })
        .map_err(Fail::Local)
    }

    /// Opens the pre-allocated file for a segment starting at `offset` (no truncation,
    /// no hashing).
    async fn open_segment(path: &Path, offset: u64) -> Step<Self> {
        let path = path.to_path_buf();
        blocking(move || {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .map_err(|e| ForgeError::io_at("opening", &path, e))?;
            file.seek(SeekFrom::Start(offset))
                .map_err(|e| ForgeError::io_at("seeking", &path, e))?;
            Ok(SinkInner {
                file,
                hasher: None,
                path,
            })
        })
        .await?
        .map(|inner| Self { inner: Some(inner) })
        .map_err(Fail::Local)
    }

    /// Writes (and hashes) `data`, giving the buffer back, cleared, for reuse.
    async fn write(&mut self, data: Vec<u8>) -> Step<Vec<u8>> {
        let mut inner = self.inner.take().ok_or_else(|| {
            Fail::Local(ForgeError::Internal(
                "download sink used after failure".into(),
            ))
        })?;
        let mut data = data;
        let (inner, data, result) = blocking(move || {
            if let Some(hasher) = inner.hasher.as_mut() {
                hasher.update(&data);
            }
            let result = inner
                .file
                .write_all(&data)
                .map_err(|e| ForgeError::io_at("writing", &inner.path, e));
            data.clear();
            (inner, data, result)
        })
        .await?;
        result.map_err(Fail::Local)?;
        self.inner = Some(inner);
        Ok(data)
    }

    /// Flushes to disk and returns the final sha256, when hashing.
    async fn finish(mut self) -> Step<Option<String>> {
        let Some(inner) = self.inner.take() else {
            return Ok(None);
        };
        blocking(move || {
            inner
                .file
                .sync_data()
                .map_err(|e| ForgeError::io_at("syncing", &inner.path, e))?;
            Ok(inner.hasher.map(|h| hex(&h.finalize())))
        })
        .await?
        .map_err(Fail::Local)
    }
}

/// Hashes `path` in `buffer`-sized reads. `token` is polled between reads.
async fn hash_file(path: &Path, buffer: usize, token: &CancellationToken) -> Step<(Sha256, u64)> {
    let path = path.to_path_buf();
    let token = token.clone();
    let result = blocking(move || {
        let mut file =
            std::fs::File::open(&path).map_err(|e| ForgeError::io_at("opening", &path, e))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0_u8; buffer];
        let mut total = 0_u64;
        loop {
            if token.is_cancelled() {
                return Ok(None);
            }
            let n = file
                .read(&mut buf)
                .map_err(|e| ForgeError::io_at("reading", &path, e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            total += n as u64;
        }
        Ok::<_, ForgeError>(Some((hasher, total)))
    })
    .await?;
    match result {
        Ok(Some(done)) => Ok(done),
        Ok(None) => Err(Fail::Cancelled),
        Err(e) => Err(Fail::Local(e)),
    }
}

pub(crate) fn sidecar_path(part: &Path) -> PathBuf {
    let mut name = part.as_os_str().to_owned();
    name.push(".json");
    PathBuf::from(name)
}

async fn remove_part_files(part: &Path) {
    let part = part.to_path_buf();
    let _ = blocking(move || {
        let _ = std::fs::remove_file(&part);
        let _ = std::fs::remove_file(sidecar_path(&part));
    })
    .await;
}

async fn save_map(part: &Path, map: SegmentMap) -> Step<()> {
    let sidecar = sidecar_path(part);
    blocking(move || {
        let json = serde_json::to_vec(&map)
            .map_err(|e| ForgeError::Internal(format!("serializing segment map: {e}")))?;
        std::fs::write(&sidecar, json).map_err(|e| ForgeError::io_at("writing", &sidecar, e))
    })
    .await?
    .map_err(Fail::Local)
}

async fn inspect(part: &Path, expected_size: Option<u64>) -> Step<PartState> {
    let part = part.to_path_buf();
    blocking(move || {
        let sidecar = sidecar_path(&part);
        let len = match std::fs::metadata(&part) {
            Ok(meta) => meta.len(),
            Err(_) => {
                let _ = std::fs::remove_file(&sidecar);
                return PartState::Fresh;
            }
        };
        if let Ok(raw) = std::fs::read(&sidecar) {
            match serde_json::from_slice::<SegmentMap>(&raw) {
                Ok(map) if map.valid() && map.total == len => return PartState::Segmented(map),
                _ => {
                    let _ = std::fs::remove_file(&sidecar);
                }
            }
        }
        if expected_size.is_some_and(|e| len > e) {
            let _ = std::fs::remove_file(&part);
            return PartState::Fresh;
        }
        if len == 0 {
            PartState::Fresh
        } else {
            PartState::Sequential(len)
        }
    })
    .await
}

fn host_of(url: &Url) -> &str {
    url.host_str().unwrap_or("?")
}

fn move_into_place(part: &Path, dest: &Path) -> forge_core::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ForgeError::io_at("creating", parent, e))?;
    }
    // Rename replaces an existing file on Windows and Unix. Different volumes: copy.
    if std::fs::rename(part, dest).is_err() {
        std::fs::copy(part, dest).map_err(|e| ForgeError::io_at("copying to", dest, e))?;
        let _ = std::fs::remove_file(part);
    }
    let _ = std::fs::remove_file(sidecar_path(part));
    Ok(())
}

fn all_failed(failures: &[MirrorFailure]) -> ForgeError {
    let detail = failures
        .iter()
        .map(|f| format!("{}: {}", f.url, f.reason))
        .collect::<Vec<_>>()
        .join("; ");
    if !failures.is_empty() && failures.iter().all(|f| f.kind == FailureKind::Verification) {
        return ForgeError::VerificationFailed(detail);
    }
    ForgeError::io(
        format!("download failed from every source ({detail})"),
        std::io::Error::other("all mirrors failed"),
    )
}

/// One download job's worth of state.
pub(crate) struct Transfer<'a> {
    pub(crate) env: &'a Env,
    pub(crate) req: &'a DownloadRequest,
    pub(crate) part: PathBuf,
    pub(crate) ctx: &'a JobContext,
    pub(crate) token: CancellationToken,
    pub(crate) progress: Arc<Progress>,
}

impl Transfer<'_> {
    fn cfg(&self) -> &DownloadConfig {
        &self.env.config
    }

    fn say(&self, message: String) {
        self.ctx.progress(JobProgress::new().message(message));
    }

    /// Tries the mirrors in order; each mirror gets resumable retries.
    pub(crate) async fn run(&self) -> forge_core::Result<DownloadOutcome> {
        let mut failures: Vec<MirrorFailure> = Vec::new();
        for (index, url) in self.req.urls.iter().enumerate() {
            if index > 0 {
                self.say(messages::next_mirror(host_of(url)));
            }
            let mut attempt = 0_u32;
            let outcome = loop {
                match self.fetch_once(url).await {
                    Err(Fail::Transient(reason)) if attempt < self.cfg().max_retries => {
                        attempt += 1;
                        tracing::warn!(%url, %reason, attempt, "transient download failure, resuming");
                        self.say(messages::retrying(attempt, self.cfg().max_retries));
                        let wait = self.cfg().retry_backoff * attempt;
                        if cancellable(&self.token, tokio::time::sleep(wait))
                            .await
                            .is_err()
                        {
                            break Err(Fail::Cancelled);
                        }
                    }
                    other => break other,
                }
            };
            match outcome {
                Ok(done) => {
                    return Ok(DownloadOutcome {
                        path: self.req.dest.clone(),
                        size: done.size,
                        sha256: done.sha256,
                        source: url.to_string(),
                        mirror_failures: failures,
                    });
                }
                Err(Fail::Cancelled) => {
                    return Err(ForgeError::Cancelled(format!("download of {url}")));
                }
                Err(Fail::Local(err)) => return Err(err),
                Err(Fail::Transient(reason) | Fail::Mirror(reason)) => {
                    tracing::warn!(%url, %reason, "mirror failed");
                    failures.push(MirrorFailure {
                        url: url.to_string(),
                        reason,
                        kind: FailureKind::Network,
                    });
                }
                Err(Fail::Verification(reason)) => {
                    tracing::warn!(%url, %reason, "mirror served unverifiable data");
                    failures.push(MirrorFailure {
                        url: url.to_string(),
                        reason,
                        kind: FailureKind::Verification,
                    });
                }
            }
        }
        Err(all_failed(&failures))
    }

    /// Transfers the file from `url` into the `.part`, verifies it and moves it to
    /// `dest`.
    async fn fetch_once(&self, url: &Url) -> Step<Transferred> {
        let state = inspect(&self.part, self.req.expected_size).await?;
        let resumed = !matches!(state, PartState::Fresh);
        let transferred = match state {
            PartState::Segmented(map) => self.segmented(url, map).await?,
            PartState::Sequential(len) => match self.sequential(url, len).await? {
                Seq::Done(done) => done,
                // Never requested for a resume (`existing > 0`).
                Seq::Segments(_) => unreachable!("segments are only proposed from zero"),
            },
            PartState::Fresh => match self.sequential(url, 0).await? {
                Seq::Done(done) => done,
                Seq::Segments(total) => self.segmented_fresh(url, total).await?,
            },
        };
        self.verify_and_publish(transferred, resumed).await
    }

    async fn verify_and_publish(&self, done: Transferred, resumed: bool) -> Step<Transferred> {
        let bad = if let Some(expected) = self.req.expected_size.filter(|e| *e != done.size) {
            Some(format!(
                "size mismatch: got {} bytes, expected {expected}",
                done.size
            ))
        } else {
            self.req
                .expected_sha256
                .as_ref()
                .filter(|e| !e.eq_ignore_ascii_case(&done.sha256))
                .map(|e| format!("sha256 mismatch: got {}, expected {e}", done.sha256))
        };
        if let Some(reason) = bad {
            remove_part_files(&self.part).await;
            // Data we resumed from may be stale (the file changed on the server):
            // start over once before blaming the mirror.
            return Err(if resumed {
                Fail::Transient(format!("resumed data rejected ({reason}); restarting"))
            } else {
                Fail::Verification(reason)
            });
        }
        let (part, dest) = (self.part.clone(), self.req.dest.clone());
        blocking(move || move_into_place(&part, &dest))
            .await?
            .map_err(Fail::Local)?;
        Ok(done)
    }

    async fn send(&self, rq: RequestBuilder) -> Step<Response> {
        let sent = cancellable(
            &self.token,
            tokio::time::timeout(self.cfg().request_timeout, rq.send()),
        )
        .await?;
        match sent {
            Err(_) => Err(Fail::Transient("request timed out".into())),
            Ok(Err(err)) => Err(Fail::Transient(describe(&err))),
            Ok(Ok(resp)) => Ok(resp),
        }
    }

    /// Waits for the bandwidth limiter, cancellably.
    async fn throttle(&self, bytes: usize, token: &CancellationToken) -> Step<()> {
        let wait = self.env.limiter.reserve(bytes);
        if wait.is_zero() {
            return Ok(());
        }
        cancellable(token, tokio::time::sleep(wait)).await
    }

    /// Reads the next chunk of `body`, honouring cancellation and the idle timeout.
    async fn next_chunk(
        &self,
        body: &mut (impl futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin),
        token: &CancellationToken,
    ) -> Step<Option<bytes::Bytes>> {
        let next = cancellable(
            token,
            tokio::time::timeout(self.cfg().read_timeout, body.next()),
        )
        .await?;
        match next {
            Err(_) => Err(Fail::Transient("read timed out".into())),
            Ok(None) => Ok(None),
            Ok(Some(Err(err))) => Err(Fail::Transient(describe(&err))),
            Ok(Some(Ok(chunk))) => Ok(Some(chunk)),
        }
    }

    /// Single connection, resuming at `existing` bytes.
    async fn sequential(&self, url: &Url, mut existing: u64) -> Step<Seq> {
        // Segments are only proposed for a transfer that began from zero; a server that
        // ignores Range on a resume just restarts the sequential download.
        let started_fresh = existing == 0;
        let host = host_of(url);
        let buffer_size = self.cfg().buffer_size;
        let mut rq = self.env.client.get(url.clone());
        if existing > 0 {
            rq = rq.header(RANGE, format!("bytes={existing}-"));
            self.say(messages::resuming_from(host, existing));
        } else {
            self.say(messages::downloading_from(host));
        }
        let resp = self.send(rq).await?;
        let status = resp.status();
        let mut total = self.req.expected_size;
        if status == StatusCode::PARTIAL_CONTENT && existing > 0 {
            match content_range(&resp) {
                Some((start, _, size)) if start == existing => {
                    total = size.or(content_length(&resp).map(|l| l + existing));
                }
                _ => return Err(Fail::Mirror("invalid Content-Range in 206 response".into())),
            }
        } else if status == StatusCode::OK {
            if existing > 0 {
                tracing::info!(%url, existing, "server ignored Range; restarting from zero");
                existing = 0;
            }
            total = content_length(&resp).or(total);
        } else if status == StatusCode::RANGE_NOT_SATISFIABLE && existing > 0 {
            let size = resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_unsatisfied_total);
            if size == Some(existing) {
                // The .part already holds the whole file.
                return self.hash_whole().await.map(Seq::Done);
            }
            remove_part_files(&self.part).await;
            return Err(Fail::Transient(
                "stored partial file no longer matches; restarting".into(),
            ));
        } else {
            return Err(if status.is_success() {
                Fail::Mirror(format!("unexpected HTTP {status}"))
            } else {
                status_fail(status)
            });
        }
        if started_fresh && status == StatusCode::OK && self.wants_segments(&resp, total) {
            // Drop this response (closing the connection) and fetch with Range.
            return Ok(Seq::Segments(total.unwrap_or_default()));
        }
        if let (Some(expected), Some(actual)) = (self.req.expected_size, total) {
            if expected != actual {
                return Err(Fail::Verification(format!(
                    "size mismatch: server announces {actual} bytes, expected {expected}"
                )));
            }
        }
        self.progress.set_total(total);

        // Hash of what is already on disk, then continue streaming.
        let mut hasher = Sha256::new();
        if existing > 0 {
            self.say(messages::verifying_existing());
            let (h, hashed) = hash_file(&self.part, buffer_size, &self.token).await?;
            if hashed < existing {
                remove_part_files(&self.part).await;
                return Err(Fail::Transient("partial file shrank; restarting".into()));
            }
            hasher = h;
            existing = hashed;
            self.say(messages::resuming_from(host, existing));
        }
        self.progress.set_done(existing);
        let mut sink = Sink::open(&self.part, existing, hasher).await?;
        let mut written = existing;
        let mut buf: Vec<u8> = Vec::with_capacity(buffer_size);
        let mut body = resp.bytes_stream();
        let failure = loop {
            match self.next_chunk(&mut body, &self.token).await {
                Ok(Some(chunk)) => {
                    if let Err(f) = self.throttle(chunk.len(), &self.token).await {
                        break Some(f);
                    }
                    buf.extend_from_slice(&chunk);
                    self.progress.add(chunk.len() as u64);
                    if buf.len() >= buffer_size {
                        written += buf.len() as u64;
                        buf = sink.write(buf).await?;
                    }
                }
                Ok(None) => break None,
                Err(f) => break Some(f),
            }
        };
        // Keep what arrived, whatever happened, so the next attempt resumes from it.
        if !buf.is_empty() {
            written += buf.len() as u64;
            sink.write(buf).await?;
        }
        let sha = sink.finish().await?;
        if let Some(f) = failure {
            return Err(f);
        }
        if let Some(total) = total {
            if written != total {
                return Err(Fail::Transient(format!(
                    "connection closed after {written} of {total} bytes"
                )));
            }
        }
        Ok(Seq::Done(Transferred {
            sha256: sha.unwrap_or_default(),
            size: written,
        }))
    }

    /// Big enough, several connections allowed and the server advertises byte ranges.
    fn wants_segments(&self, resp: &Response, total: Option<u64>) -> bool {
        let cfg = self.cfg();
        let accepts_ranges = resp
            .headers()
            .get(ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("bytes"));
        cfg.max_connections > 1
            && accepts_ranges
            && total.is_some_and(|t| t >= cfg.multi_connection_threshold)
    }

    async fn hash_whole(&self) -> Step<Transferred> {
        self.say(messages::verifying());
        let (hasher, size) = hash_file(&self.part, self.cfg().buffer_size, &self.token).await?;
        Ok(Transferred {
            sha256: hex(&hasher.finalize()),
            size,
        })
    }

    async fn segmented_fresh(&self, url: &Url, total: u64) -> Step<Transferred> {
        let map = SegmentMap::split(total, self.cfg().max_connections);
        // Persist the (all zero) positions before pre-allocating: a crash can then only
        // leave positions that are lower than the truth.
        save_map(&self.part, map.clone()).await?;
        let part = self.part.clone();
        blocking(move || {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&part)
                .and_then(|f| f.set_len(total))
                .map_err(|e| ForgeError::io_at("preallocating", &part, e))
        })
        .await?
        .map_err(Fail::Local)?;
        self.segmented(url, map).await
    }

    /// Several `Range` connections, each writing its own slice of the `.part`.
    async fn segmented(&self, url: &Url, map: SegmentMap) -> Step<Transferred> {
        let host = host_of(url);
        self.progress.set_total(Some(map.total));
        self.progress.set_done(map.done());
        self.say(if map.done() > 0 {
            messages::resuming_from(host, map.done())
        } else {
            messages::downloading_from(host)
        });
        let positions: Arc<Vec<AtomicU64>> =
            Arc::new(map.segments.iter().map(|s| AtomicU64::new(s.pos)).collect());
        let group = self.token.child_token();
        let results =
            futures::future::join_all(map.segments.iter().enumerate().map(|(index, segment)| {
                let positions = Arc::clone(&positions);
                let group = group.clone();
                async move {
                    let result = self
                        .segment(url, segment.end, &positions[index], &group)
                        .await;
                    if result.is_err() {
                        // First failure stops the siblings; they keep their progress.
                        group.cancel();
                    }
                    result
                }
            }))
            .await;
        let saved = SegmentMap {
            total: map.total,
            segments: map
                .segments
                .iter()
                .zip(positions.iter())
                .map(|(s, pos)| Segment {
                    pos: pos.load(Ordering::Relaxed),
                    ..s.clone()
                })
                .collect(),
        };
        let complete = saved.is_complete();
        save_map(&self.part, saved).await?;
        if self.token.is_cancelled() {
            return Err(Fail::Cancelled);
        }
        let mut first_error = None;
        for result in results {
            match result {
                Ok(()) | Err(Fail::Cancelled) => {}
                Err(err @ Fail::Local(_)) => return Err(err),
                Err(err) => {
                    first_error.get_or_insert(err);
                }
            }
        }
        if let Some(err) = first_error {
            return Err(err);
        }
        if !complete {
            return Err(Fail::Transient("segments ended before completion".into()));
        }
        let sidecar = sidecar_path(&self.part);
        blocking(move || {
            let _ = std::fs::remove_file(sidecar);
        })
        .await?;
        self.hash_whole().await
    }

    /// Fetches `[position, end)` of the file over its own connection.
    async fn segment(
        &self,
        url: &Url,
        end: u64,
        position: &AtomicU64,
        group: &CancellationToken,
    ) -> Step<()> {
        let start = position.load(Ordering::Relaxed);
        if start >= end {
            return Ok(());
        }
        let buffer_size = self.cfg().buffer_size;
        let rq = self
            .env
            .client
            .get(url.clone())
            .header(RANGE, format!("bytes={start}-{}", end - 1));
        let sent = cancellable(
            group,
            tokio::time::timeout(self.cfg().request_timeout, rq.send()),
        )
        .await?;
        let resp = match sent {
            Err(_) => return Err(Fail::Transient("request timed out".into())),
            Ok(Err(err)) => return Err(Fail::Transient(describe(&err))),
            Ok(Ok(resp)) => resp,
        };
        let status = resp.status();
        if status != StatusCode::PARTIAL_CONTENT {
            return Err(if status.is_success() {
                Fail::Mirror("server stopped honouring Range".into())
            } else {
                status_fail(status)
            });
        }
        match content_range(&resp) {
            Some((s, _, _)) if s == start => {}
            _ => return Err(Fail::Mirror("invalid Content-Range in 206 response".into())),
        }
        let mut sink = Sink::open_segment(&self.part, start).await?;
        let mut buf: Vec<u8> = Vec::with_capacity(buffer_size);
        let mut body = resp.bytes_stream();
        let mut pos = start;
        let failure = loop {
            match self.next_chunk(&mut body, group).await {
                Ok(Some(chunk)) => {
                    let pending = pos + buf.len() as u64;
                    if pending + chunk.len() as u64 > end {
                        break Some(Fail::Mirror("server sent more bytes than requested".into()));
                    }
                    if let Err(f) = self.throttle(chunk.len(), group).await {
                        break Some(f);
                    }
                    buf.extend_from_slice(&chunk);
                    self.progress.add(chunk.len() as u64);
                    if buf.len() >= buffer_size {
                        pos += buf.len() as u64;
                        buf = sink.write(buf).await?;
                        position.store(pos, Ordering::Relaxed);
                    }
                }
                Ok(None) => break None,
                Err(f) => break Some(f),
            }
        };
        if !buf.is_empty() {
            pos += buf.len() as u64;
            sink.write(buf).await?;
            position.store(pos, Ordering::Relaxed);
        }
        sink.finish().await?;
        if let Some(f) = failure {
            return Err(f);
        }
        if pos != end {
            return Err(Fail::Transient(format!(
                "segment closed at {pos} of {end} bytes"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_parsing() {
        assert_eq!(parse_content_range("bytes 5-9/20"), Some((5, 9, Some(20))));
        assert_eq!(parse_content_range("bytes 0-0/*"), Some((0, 0, None)));
        assert_eq!(parse_content_range("items 0-1/2"), None);
        assert_eq!(parse_unsatisfied_total("bytes */1234"), Some(1234));
        assert_eq!(parse_unsatisfied_total("bytes 0-1/2"), None);
    }

    #[test]
    fn split_covers_the_file_without_gaps() {
        let total = 10 * MIN_SEGMENT + 123;
        let map = SegmentMap::split(total, 4);
        assert!(map.valid());
        assert_eq!(map.segments.len(), 4);
        assert_eq!(map.segments.last().map(|s| s.end), Some(total));
        // Small files never get more segments than MIN_SEGMENT allows.
        assert_eq!(SegmentMap::split(100, 4).segments.len(), 1);
        assert_eq!(SegmentMap::split(2 * MIN_SEGMENT, 4).segments.len(), 2);
        assert!(!map.is_complete());
        assert_eq!(map.done(), 0);
    }

    #[test]
    fn invalid_maps_are_rejected() {
        let mut map = SegmentMap::split(3 * MIN_SEGMENT, 3);
        map.segments[1].pos = map.segments[1].end + 1;
        assert!(!map.valid());
        let mut map = SegmentMap::split(3 * MIN_SEGMENT, 3);
        map.total += 1;
        assert!(!map.valid());
    }

    #[test]
    fn part_key_prefers_the_expected_sha() {
        let mut req = DownloadRequest::new(
            vec![Url::parse("https://example.org/a.zip").unwrap()],
            std::path::PathBuf::from("a.zip"),
        );
        let by_url = part_key(&req);
        assert_eq!(by_url.len(), 16);
        req.expected_sha256 = Some("ABCDEF0123456789ffffffff".into());
        assert_eq!(part_key(&req), "abcdef0123456789");
    }

    #[test]
    fn status_classes() {
        assert!(matches!(
            status_fail(StatusCode::SERVICE_UNAVAILABLE),
            Fail::Transient(_)
        ));
        assert!(matches!(
            status_fail(StatusCode::INTERNAL_SERVER_ERROR),
            Fail::Mirror(_)
        ));
        assert!(matches!(
            status_fail(StatusCode::NOT_FOUND),
            Fail::Mirror(_)
        ));
    }
}
