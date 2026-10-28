//! Test support: a configurable local HTTP server (axum) and a harness that wires the
//! download manager to an event bus, a job registry and a temp directory.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::Response;
use bytes::Bytes;
use forge_core::actor::ActorSupervisor;
use forge_core::event::{Event, EventBus, EventKind, EventStream};
use forge_core::jobs::{JobId, JobRegistry};
use forge_provider::download::{DownloadConfig, DownloadManager, ProxySetting};
use sha2::{Digest, Sha256};
use url::Url;

/// Deterministic, incompressible-looking bytes.
pub fn payload(len: usize) -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// How the server answers for one path.
#[derive(Clone)]
pub struct Behavior {
    data: Arc<Vec<u8>>,
    status: Option<u16>,
    ignore_range: bool,
    delay: Duration,
    throttle: Option<(usize, Duration)>,
    cut_after: Option<usize>,
    cut_done: Arc<AtomicBool>,
    cut_ranged_only: bool,
    fail_first: Option<(usize, u16)>,
    served: Arc<AtomicUsize>,
}

impl Behavior {
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data: Arc::new(data),
            status: None,
            ignore_range: false,
            delay: Duration::ZERO,
            throttle: None,
            cut_after: None,
            cut_done: Arc::default(),
            cut_ranged_only: false,
            fail_first: None,
            served: Arc::default(),
        }
    }

    /// Always answers with this status and no body.
    pub fn status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    /// The first `n` requests get `status`; later ones succeed.
    pub fn fail_first_requests(mut self, n: usize, status: u16) -> Self {
        self.fail_first = Some((n, status));
        self
    }

    /// Answers 200 with the whole body even when a Range is requested.
    pub fn ignore_range(mut self) -> Self {
        self.ignore_range = true;
        self
    }

    /// Waits before answering (keeps the request "in flight").
    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Sends the body in `chunk`-byte pieces, sleeping `pause` between them.
    pub fn throttle(mut self, chunk: usize, pause: Duration) -> Self {
        self.throttle = Some((chunk, pause));
        self
    }

    /// Only responses to Range requests can be cut (see [`Behavior::cut_first_after`]).
    pub fn cut_ranged_only(mut self) -> Self {
        self.cut_ranged_only = true;
        self
    }

    /// The first response whose body is longer than `n` bytes breaks the connection
    /// after `n` bytes.
    pub fn cut_first_after(mut self, n: usize) -> Self {
        self.cut_after = Some(n);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    pub range: Option<String>,
}

impl Hit {
    /// First byte of a `bytes=A-B` range.
    pub fn range_start(&self) -> Option<u64> {
        self.range
            .as_deref()?
            .strip_prefix("bytes=")?
            .split('-')
            .next()?
            .parse()
            .ok()
    }
}

#[derive(Default)]
struct Shared {
    files: Mutex<HashMap<String, Behavior>>,
    hits: Mutex<Vec<Hit>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

pub struct TestServer {
    addr: SocketAddr,
    shared: Arc<Shared>,
}

impl TestServer {
    pub async fn start() -> Self {
        let shared = Arc::new(Shared::default());
        let app = Router::new()
            .fallback(serve)
            .with_state(Arc::clone(&shared));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { addr, shared }
    }

    pub fn add(&self, path: &str, behavior: Behavior) {
        self.shared
            .files
            .lock()
            .unwrap()
            .insert(path.to_owned(), behavior);
    }

    pub fn url(&self, path: &str) -> Url {
        Url::parse(&format!("http://{}{path}", self.addr)).unwrap()
    }

    pub fn hits(&self, path: &str) -> Vec<Hit> {
        self.shared
            .hits
            .lock()
            .unwrap()
            .iter()
            .filter(|h| h.path == path)
            .cloned()
            .collect()
    }

    pub fn arrival_order(&self) -> Vec<String> {
        self.shared
            .hits
            .lock()
            .unwrap()
            .iter()
            .map(|h| h.path.clone())
            .collect()
    }

    pub fn max_in_flight(&self) -> usize {
        self.shared.max_in_flight.load(Ordering::SeqCst)
    }

    pub async fn wait_for_hit(&self, path: &str) {
        for _ in 0..500 {
            if !self.hits(path).is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("server never saw {path}");
    }
}

struct InFlight(Arc<Shared>);

impl InFlight {
    fn enter(shared: &Arc<Shared>) -> Self {
        let now = shared.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        shared.max_in_flight.fetch_max(now, Ordering::SeqCst);
        Self(Arc::clone(shared))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

fn parse_range(value: &str, len: usize) -> Option<(usize, usize)> {
    let spec = value.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    let start: usize = a.parse().ok()?;
    let end = if b.is_empty() {
        len.checked_sub(1)?
    } else {
        b.parse::<usize>().ok()?.min(len.checked_sub(1)?)
    };
    (start <= end).then_some((start, end))
}

fn empty(status: u16) -> Response {
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap())
        .body(Body::empty())
        .unwrap()
}

async fn serve(State(shared): State<Arc<Shared>>, uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().to_owned();
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    shared.hits.lock().unwrap().push(Hit {
        path: path.clone(),
        range: range.clone(),
    });
    let behavior = shared.files.lock().unwrap().get(&path).cloned();
    let Some(behavior) = behavior else {
        return empty(404);
    };
    let guard = InFlight::enter(&shared);
    if !behavior.delay.is_zero() {
        tokio::time::sleep(behavior.delay).await;
    }
    let seen = behavior.served.fetch_add(1, Ordering::SeqCst);
    if let Some(status) = behavior.status {
        return empty(status);
    }
    if let Some((n, status)) = behavior.fail_first {
        if seen < n {
            return empty(status);
        }
    }
    let total = behavior.data.len();
    let (start, end, partial) = match range.as_deref() {
        Some(value) if !behavior.ignore_range => match parse_range(value, total) {
            Some((s, e)) => (s, e, true),
            None => {
                return Response::builder()
                    .status(416)
                    .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                    .body(Body::empty())
                    .unwrap();
            }
        },
        _ => (0, total.saturating_sub(1), false),
    };
    let slice = if total == 0 {
        Bytes::new()
    } else {
        Bytes::copy_from_slice(&behavior.data[start..=end])
    };
    let cut = behavior.cut_after.filter(|n| {
        slice.len() > *n
            && (!behavior.cut_ranged_only || partial)
            && !behavior.cut_done.swap(true, Ordering::SeqCst)
    });

    let mut builder = Response::builder()
        .status(if partial { 206 } else { 200 })
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, slice.len());
    if partial {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total}"),
        );
    }
    let chunk = behavior.throttle.map_or(64 * 1024, |(c, _)| c);
    let pause = behavior.throttle.map_or(Duration::ZERO, |(_, p)| p);
    let limit = cut.unwrap_or(slice.len());
    let state = (slice, 0_usize, guard);
    let stream = futures::stream::unfold(state, move |(slice, sent, guard)| async move {
        if sent == usize::MAX {
            return None;
        }
        if sent >= limit {
            if cut.is_some() {
                // Break the connection in the middle of the body. The pause lets the
                // client read what was sent: an immediate RST may discard it.
                tokio::time::sleep(Duration::from_millis(100)).await;
                let err: io::Error = io::ErrorKind::ConnectionAborted.into();
                return Some((Err(err), (slice.slice(0..0), usize::MAX, guard)));
            }
            return None;
        }
        if !pause.is_zero() {
            tokio::time::sleep(pause).await;
        }
        let next = (sent + chunk).min(limit);
        let part = slice.slice(sent..next);
        Some((Ok::<_, io::Error>(part), (slice, next, guard)))
    });
    let mut response = builder.body(Body::from_stream(stream)).unwrap();
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    response
}

/// Everything a download test needs.
pub struct Harness {
    pub dir: tempfile::TempDir,
    pub bus: EventBus,
    pub jobs: JobRegistry,
    pub supervisor: ActorSupervisor,
    pub manager: DownloadManager,
}

impl Harness {
    pub fn new(tweak: impl FnOnce(&mut DownloadConfig)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::new();
        let jobs = JobRegistry::new(bus.clone());
        let supervisor = ActorSupervisor::new(bus.clone());
        let mut config = DownloadConfig::new(dir.path().join("tmp").join("downloads"));
        // Deterministic tests: no ambient proxy unless a test asks for one.
        config.proxy = ProxySetting::Direct;
        config.progress_interval = Duration::from_millis(50);
        config.retry_backoff = Duration::from_millis(10);
        config.read_timeout = Duration::from_secs(10);
        tweak(&mut config);
        let manager = DownloadManager::spawn(&supervisor, jobs.clone(), config).unwrap();
        Self {
            dir,
            bus,
            jobs,
            supervisor,
            manager,
        }
    }

    pub fn events(&self) -> Events {
        Events {
            stream: self.bus.subscribe(),
            log: Vec::new(),
        }
    }

    pub fn dest(&self, name: &str) -> PathBuf {
        self.dir.path().join("out").join(name)
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.dir.path().join("tmp").join("downloads")
    }

    /// Files left in `tmp/downloads` (parts and segment maps).
    pub fn part_files(&self) -> Vec<PathBuf> {
        match std::fs::read_dir(self.tmp_dir()) {
            Ok(rd) => rd.map(|e| e.unwrap().path()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Plants a partial file as a previous run would have left it.
    pub fn write_part(&self, expected_sha256: &str, bytes: &[u8]) {
        std::fs::create_dir_all(self.tmp_dir()).unwrap();
        let key: String = expected_sha256.chars().take(16).collect();
        std::fs::write(self.tmp_dir().join(format!("{key}.part")), bytes).unwrap();
    }

    /// Returns the `job.*` events of `job` up to and including the terminal one.
    pub async fn wait_terminal(&self, events: &mut Events, job: JobId) -> Vec<Event> {
        let id = job.to_string();
        let terminal = |e: &Event| {
            matches!(
                e.kind,
                EventKind::JobDone | EventKind::JobFailed | EventKind::JobCancelled
            ) && e.payload.get("job_id").and_then(|v| v.as_str()) == Some(id.as_str())
        };
        events.until(terminal).await;
        events.of_job(&id)
    }

    /// Waits for the first event of `job` satisfying `pred`.
    pub async fn wait_for(
        &self,
        events: &mut Events,
        job: JobId,
        pred: impl Fn(&Event) -> bool,
    ) -> Event {
        let id = job.to_string();
        events
            .until(|e| {
                e.payload.get("job_id").and_then(|v| v.as_str()) == Some(id.as_str()) && pred(e)
            })
            .await
    }
}

/// Event stream that remembers everything it received, so that several jobs can be
/// awaited one after the other.
pub struct Events {
    stream: EventStream,
    log: Vec<Event>,
}

impl Events {
    /// First event (already received or yet to come) satisfying `pred`.
    async fn until(&mut self, pred: impl Fn(&Event) -> bool) -> Event {
        if let Some(found) = self.log.iter().find(|e| pred(e)) {
            return found.clone();
        }
        let waited = tokio::time::timeout(Duration::from_secs(60), async {
            while let Some(event) = self.stream.recv().await {
                self.log.push(event.clone());
                if pred(&event) {
                    return Some(event);
                }
            }
            None
        })
        .await;
        match waited {
            Ok(Some(event)) => event,
            Ok(None) => panic!("event stream closed"),
            Err(_) => panic!(
                "expected event never arrived; saw {} events",
                self.log.len()
            ),
        }
    }

    fn of_job(&self, id: &str) -> Vec<Event> {
        self.log
            .iter()
            .filter(|e| e.payload.get("job_id").and_then(|v| v.as_str()) == Some(id))
            .cloned()
            .collect()
    }
}
