//! Shared helpers of the forge-fcgi integration tests (Windows only).
//!
//! * the fake php-cgi (`tests/fake_php_cgi`, an independent cargo project without
//!   dependencies) built once per test binary into its own target dir under
//!   `CARGO_TARGET_TMPDIR`, keyed by the worktree;
//! * a supervisor with its own root job and event bus;
//! * [`TestPorts`]: port reservations handing out free ports of 20000–29999 (below the
//!   ephemeral range), checked by a bind;
//! * [`Router`]: plays the web server — keeps each upstream's backends as the pool's
//!   listener hands them over, and which upstream "the site" uses (blue/green);
//! * [`Load`]: concurrent FastCGI clients routed through the [`Router`] at each request,
//!   recording every answer (version, pid) and every error;
//! * [`php`]: real PHP builds (download verified by SHA-256, cached, extracted once) for
//!   the `*_online` tests.
#![allow(dead_code)]

pub mod php;

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use forge_core::actor::ActorSupervisor;
use forge_core::event::{Event, EventBus, EventKind, EventStream};
use forge_core::model::port::{
    OwnerKind, PortFuture, PortReservation, PortReservations, ReservationOwner,
};
use forge_core::platform::{ProcessControl, ProcessControlOptions, process_control};
use forge_core::{ForgeError, Result};
use forge_fcgi::probe::{self, HealthScript};
use forge_fcgi::{FcgiRequest, UpstreamChange, UpstreamFuture, UpstreamListener, UpstreamModel};
use forge_supervisor::{JobTree, Supervisor};

/// Path of the fake php-cgi executable (built on first use).
pub fn fake_php_cgi() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(build_fake).clone()
}

fn build_fake() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = manifest_dir
        .join("tests")
        .join("fake_php_cgi")
        .join("Cargo.toml");
    let mut hasher = DefaultHasher::new();
    manifest_dir.hash(&mut hasher);
    let target = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("fake-php-cgi-{:016x}", hasher.finish()));
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .arg("build")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&target)
        .env_remove("CARGO_MAKEFLAGS")
        .env_remove("MAKEFLAGS")
        .env_remove("MFLAGS")
        .output()
        .expect("running cargo to build the fake php-cgi");
    assert!(
        output.status.success(),
        "building the fake php-cgi failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let exe = target.join("debug").join("fake_php_cgi.exe");
    assert!(exe.is_file(), "{} missing", exe.display());
    exe
}

/// The `forgebox-ctrl` helper of the workspace build, when it was built
/// (`cargo build --workspace`); without it workers are stopped by `TerminateJobObject`.
fn ctrl_helper() -> Option<PathBuf> {
    let deps = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let helper = deps.parent()?.join("forgebox-ctrl.exe");
    helper.is_file().then_some(helper)
}

/// Platform process operations (with the CTRL_BREAK helper when available).
pub fn processes() -> Arc<dyn ProcessControl> {
    let options = match ctrl_helper() {
        Some(helper) => ProcessControlOptions::with_ctrl_break_helper(helper),
        None => ProcessControlOptions::default(),
    };
    process_control(options)
}

/// A supervisor with its own root job and event bus.
pub fn supervisor() -> (Supervisor, EventBus) {
    let bus = EventBus::with_capacity(65_536);
    let jobs = JobTree::new(processes()).expect("root job");
    (
        Supervisor::new(jobs, ActorSupervisor::new(bus.clone())),
        bus,
    )
}

/// Whether `pid` is still running.
pub fn alive(pid: u32) -> bool {
    processes().is_alive(pid).unwrap_or(false)
}

/// Kills `pid` from outside (as a crash would).
pub fn kill(pid: u32) {
    let status = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .output()
        .expect("taskkill");
    assert!(status.status.success(), "taskkill {pid}: {status:?}");
}

/// Port reservations over free ports of 20000–29999 (checked by a bind), recording what
/// was allocated and released.
#[derive(Debug, Default)]
pub struct TestPorts {
    held: Mutex<BTreeMap<String, u16>>,
    pub allocations: AtomicUsize,
    pub releases: AtomicUsize,
}

impl TestPorts {
    fn key(owner: &ReservationOwner) -> String {
        owner.to_string()
    }

    /// Ports currently reserved.
    pub fn held(&self) -> Vec<u16> {
        self.held.lock().unwrap().values().copied().collect()
    }

    fn pick(&self) -> Result<u16> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let seed = u64::from(std::process::id()) * 7919 + NEXT.fetch_add(1, Ordering::Relaxed) * 37;
        let held: Vec<u16> = self.held();
        for i in 0..10_000_u64 {
            let port = 20_000 + ((seed + i * 101) % 10_000) as u16;
            if held.contains(&port) {
                continue;
            }
            if TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok() {
                return Ok(port);
            }
        }
        Err(ForgeError::Conflict("no free test port".into()))
    }
}

impl PortReservations for TestPorts {
    fn allocate<'a>(
        &'a self,
        range: &'a str,
        owner: ReservationOwner,
    ) -> PortFuture<'a, PortReservation> {
        Box::pin(async move {
            let key = Self::key(&owner);
            if let Some(port) = self.held.lock().unwrap().get(&key) {
                return Ok(PortReservation::new_tcp(
                    *port,
                    Some(range.to_owned()),
                    owner,
                ));
            }
            let port = self.pick()?;
            self.held.lock().unwrap().insert(key, port);
            self.allocations.fetch_add(1, Ordering::Relaxed);
            Ok(PortReservation::new_tcp(
                port,
                Some(range.to_owned()),
                owner,
            ))
        })
    }

    fn release<'a>(
        &'a self,
        owner: &'a ReservationOwner,
    ) -> PortFuture<'a, Option<PortReservation>> {
        Box::pin(async move {
            let removed = self.held.lock().unwrap().remove(&Self::key(owner));
            if removed.is_some() {
                self.releases.fetch_add(1, Ordering::Relaxed);
            }
            Ok(removed.map(|port| PortReservation::new_tcp(port, None, owner.clone())))
        })
    }

    fn release_owner<'a>(&'a self, kind: OwnerKind, id: &'a str) -> PortFuture<'a, usize> {
        Box::pin(async move {
            let prefix = format!("{kind}/{id}#");
            let mut held = self.held.lock().unwrap();
            let before = held.len();
            held.retain(|key, _| !key.starts_with(&prefix));
            let removed = before - held.len();
            self.releases.fetch_add(removed, Ordering::Relaxed);
            Ok(removed)
        })
    }
}

/// The web server of the tests: upstream backends as applied by the pools' listener and
/// the upstream "the site" uses. Requests read it at each request, like nginx after a
/// reload.
#[derive(Debug, Default)]
pub struct Router {
    state: Mutex<RouterState>,
    next: AtomicUsize,
    /// Delay of every apply (a slow reload).
    pub apply_delay: Mutex<Duration>,
    /// Applies refused while > 0 (a failing reload).
    pub fail_next: AtomicUsize,
    /// Calls blocked while set.
    pub gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
}

#[derive(Debug, Default)]
struct RouterState {
    upstreams: HashMap<String, Vec<SocketAddr>>,
    site: Option<String>,
    log: Vec<(Instant, UpstreamChange)>,
}

impl Router {
    pub fn new(site: &str) -> Arc<Self> {
        let router = Arc::new(Self::default());
        router.state.lock().unwrap().site = Some(site.to_owned());
        router
    }

    /// Backends of the site's upstream.
    pub fn site_backends(&self) -> Vec<SocketAddr> {
        let state = self.state.lock().unwrap();
        state
            .site
            .as_ref()
            .and_then(|name| state.upstreams.get(name))
            .cloned()
            .unwrap_or_default()
    }

    /// Upstream the site uses.
    pub fn site(&self) -> Option<String> {
        self.state.lock().unwrap().site.clone()
    }

    /// Changes applied so far.
    pub fn log(&self) -> Vec<UpstreamChange> {
        self.state
            .lock()
            .unwrap()
            .log
            .iter()
            .map(|(_, c)| c.clone())
            .collect()
    }

    /// Next backend of the site (round robin).
    pub fn pick(&self) -> Option<SocketAddr> {
        let backends = self.site_backends();
        if backends.is_empty() {
            return None;
        }
        let i = self.next.fetch_add(1, Ordering::Relaxed);
        Some(backends[i % backends.len()])
    }

    fn apply(&self, change: &UpstreamChange) {
        let mut state = self.state.lock().unwrap();
        match change {
            UpstreamChange::Scaled { model, .. } => {
                state
                    .upstreams
                    .insert(model.name.clone(), model.backends.clone());
            }
            UpstreamChange::Swapped { to, .. } => {
                state.upstreams.insert(to.name.clone(), to.backends.clone());
                state.site = Some(to.name.clone());
            }
        }
        state.log.push((Instant::now(), change.clone()));
    }

    /// Sets an upstream directly (a pool started without listener).
    pub fn set(&self, model: &UpstreamModel) {
        self.apply(&UpstreamChange::Scaled {
            model: model.clone(),
            reason: forge_fcgi::ScaleReason::Start,
        });
    }

    /// The listener to give the pools.
    pub fn listener(self: &Arc<Self>) -> Arc<dyn UpstreamListener> {
        let router = Arc::clone(self);
        Arc::new(move |change: UpstreamChange| -> UpstreamFuture {
            let router = Arc::clone(&router);
            Box::pin(async move {
                let gate = router.gate.lock().unwrap().clone();
                if let Some(gate) = gate {
                    let _permit = gate.acquire().await;
                }
                let delay = *router.apply_delay.lock().unwrap();
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                if router
                    .fail_next
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
                    .is_ok()
                {
                    return Err(ForgeError::Internal("reload failed (test)".into()));
                }
                router.apply(&change);
                Ok(())
            })
        })
    }
}

/// One answered request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub backend: SocketAddr,
    pub version: String,
    pub pid: Option<u32>,
}

/// `GET <uri>` of the health script under `root` (the pool's health root).
pub fn health_request(root: &Path, uri: &str) -> FcgiRequest {
    let script = HealthScript::new(root);
    FcgiRequest::get(uri, &script.path(), &script.document_root)
}

/// Shared results of the clients.
#[derive(Debug, Default)]
struct Results {
    answers: Mutex<Vec<Answer>>,
    errors: Mutex<Vec<String>>,
}

impl Results {
    /// Sends `request` to the router's current pick and records the outcome.
    async fn one(&self, router: &Router, request: &FcgiRequest) {
        let Some(backend) = router.pick() else {
            self.errors.lock().unwrap().push("no backend".into());
            tokio::time::sleep(Duration::from_millis(20)).await;
            return;
        };
        match probe::request(backend, request, Duration::from_secs(60)).await {
            Ok(response) if response.status == 200 => {
                match probe::parse_health_body(&response.body_text()) {
                    Some((version, _, pid)) => self.answers.lock().unwrap().push(Answer {
                        backend,
                        version,
                        pid,
                    }),
                    None => self.errors.lock().unwrap().push(format!(
                        "{backend}: unexpected body {:?}",
                        response.body_text()
                    )),
                }
            }
            Ok(response) => self.errors.lock().unwrap().push(format!(
                "{backend}: status {} {}",
                response.status,
                response.body_text().trim()
            )),
            Err(err) => self.errors.lock().unwrap().push(err.to_string()),
        }
    }
}

/// Concurrent FastCGI clients routed through a [`Router`] (the web server's choice is
/// made at each request).
#[derive(Debug)]
pub struct Load {
    stop: Arc<AtomicBool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    results: Arc<Results>,
}

impl Load {
    /// `clients` loops sending `request`, pausing `pause` between requests, until
    /// stopped or `limit` requests in total were sent.
    pub fn start(
        router: Arc<Router>,
        request: FcgiRequest,
        clients: usize,
        pause: Duration,
        limit: Option<usize>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let results = Arc::new(Results::default());
        let sent = Arc::new(AtomicUsize::new(0));
        let tasks = (0..clients)
            .map(|_| {
                let (router, stop, results, sent, request) = (
                    Arc::clone(&router),
                    Arc::clone(&stop),
                    Arc::clone(&results),
                    Arc::clone(&sent),
                    request.clone(),
                );
                tokio::spawn(async move {
                    while !stop.load(Ordering::Relaxed) {
                        if limit.is_some_and(|l| sent.fetch_add(1, Ordering::Relaxed) >= l) {
                            break;
                        }
                        results.one(&router, &request).await;
                        if !pause.is_zero() {
                            tokio::time::sleep(pause).await;
                        }
                    }
                })
            })
            .collect();
        Self {
            stop,
            tasks,
            results,
        }
    }

    /// `count` single requests, all in flight together; request `i` picks its backend
    /// (and is sent) `spread · i / count` after the start.
    pub fn burst(
        router: Arc<Router>,
        request: FcgiRequest,
        count: usize,
        spread: Duration,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let results = Arc::new(Results::default());
        let tasks = (0..count)
            .map(|i| {
                let (router, results, request) =
                    (Arc::clone(&router), Arc::clone(&results), request.clone());
                let delay = spread.mul_f64(i as f64 / count.max(1) as f64);
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    results.one(&router, &request).await;
                })
            })
            .collect();
        Self {
            stop,
            tasks,
            results,
        }
    }

    pub fn answers(&self) -> Vec<Answer> {
        self.results.answers.lock().unwrap().clone()
    }

    pub fn errors(&self) -> Vec<String> {
        self.results.errors.lock().unwrap().clone()
    }

    /// Stops the clients and waits for them; returns (answers, errors).
    pub async fn finish(self) -> (Vec<Answer>, Vec<String>) {
        self.stop.store(true, Ordering::Relaxed);
        for task in self.tasks {
            task.await.unwrap();
        }
        let answers = self.results.answers.lock().unwrap().clone();
        let errors = self.results.errors.lock().unwrap().clone();
        (answers, errors)
    }
}

/// Every event already published (non-blocking drain).
pub fn drain(stream: &mut EventStream) -> Vec<Event> {
    let mut events = Vec::new();
    while let Some(event) = stream.try_recv() {
        events.push(event);
    }
    events
}

/// Events of `kind` among `events`.
pub fn of_kind(events: &[Event], kind: EventKind) -> Vec<&Event> {
    events.iter().filter(|e| e.kind == kind).collect()
}

/// Waits until `cond` holds (polling every 25 ms), at most `timeout`.
pub async fn eventually(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
