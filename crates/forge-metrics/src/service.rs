//! The metrics service (M2-E5-T8, docs/14 section 3, ADR-0044): one actor that owns the
//! access log tailers ([`crate::tail`]), the [`Aggregator`], the CPU/RAM attribution
//! ([`crate::attribute`]) and the persistence ([`crate::persist`]).
//!
//! # Mailbox and priorities
//!
//! The actor is a task with a bounded mailbox; every request is an *ask* with a timeout
//! ([`MetricsHandle`]), never a lock. It waits on three sources, in this order of priority:
//!
//! 1. the mailbox (queries, subscriptions, sources, routes, samples, enable/disable, stop);
//! 2. its tick (every [`ServiceConfig::tick`], 2 s): clock, tailer supervision, snapshot
//!    publication, attribution (10 s) and flush (60 s);
//! 3. the batches of the tailers (one bounded channel shared by all of them).
//!
//! A query never waits behind a backlog of batches: a batch costs a few milliseconds at most
//! (the lines were decoded by the tailer on the blocking pool), and the mailbox is looked at
//! before every batch. The tailers never wait for the actor either: a full channel makes them
//! keep their batch and sample (ADR-0047).
//!
//! The actor never runs SQLite itself: flushes go through the store writer (encoding in
//! `spawn_blocking`), rollup and prune run in their own tasks, and ranges older than the
//! in-memory retention are completed *outside* the actor ([`MetricsHandle::query`]): the actor
//! answers the in-memory part with its histograms and the caller merges the stored part read
//! through the [`StoreReader`].
//!
//! # Subscriptions
//!
//! Samples are state, not history (ADR-0044 section 9): [`MetricsHandle::subscribe`] hands out a
//! `watch` of the latest [`Snapshot`] of a window, published every `cadence` while somebody
//! listens. Numbering, filtering and the drop-oldest queue are the subscriber's (the daemon's
//! stream task), so a slow client never slows the service.
//!
//! # Tail positions
//!
//! A tailer starts, in this order, from the position after the last batch the actor consumed
//! in this run (a restarted or re-added source: nothing is read twice), from the checkpoint
//! stored by the last flush (boot; the re-read records older than its `closed_edge` are
//! dropped, [`crate::persist::Resume::keeps`]), or, for a file never seen, from its first byte
//! when it is at most [`TailConfig::resume_max_lag`] long (else from its end).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::ids::SiteId;
use forge_core::store::{StoreReader, StoreWriter};
use jiff::Timestamp;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::agg::{AggConfig, AggStats, Aggregator, Counters, Summary};
use crate::attribution::{AppSample, Bucket, PoolRoute, PoolSample, SiteResources, attribute};
use crate::hist::LatencyHist;
use crate::parse::AccessParser;
use crate::persist::{self, FlushReport, Persister, Resume};
use crate::record::{AccessFormat, AccessRecord};
use crate::router::{HostRouter, SiteHosts};
use crate::tail::{self, Checkpoint, StartAt, TailBatch, TailConfig, TailSpec, TailStatus};

/// Source of the current time (tests inject a fake one).
pub type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// Seconds of an hour.
const HOUR: i64 = 3600;
/// Width of an attribution bucket (the 10 s points of tier 1).
const BUCKET_SECS: i64 = 10;
/// Records of a batch aggregated between two looks at the mailbox (a 4 MiB batch holds
/// tens of thousands of lines).
const BATCH_CHUNK: usize = 1024;

/// Tuning of the service. The defaults are the v1 design (docs/14 section 3).
#[derive(Clone)]
pub struct ServiceConfig {
    /// Aggregator knobs (`minute_outbox` is always turned on: persistence needs it).
    pub agg: AggConfig,
    /// Tailer knobs.
    pub tail: TailConfig,
    /// Base tick: clock, supervision and snapshot cadence granularity (2 s).
    pub tick: Duration,
    /// How often closed minutes and checkpoints are flushed (1 min).
    pub flush_interval: Duration,
    /// How often CPU/RAM is attributed to the sites (10 s, the tier 1 points).
    pub attribution_interval: Duration,
    /// Mailbox capacity.
    pub mailbox: usize,
    /// Batches queued between the tailers and the actor.
    pub batch_queue: usize,
    /// Writer timeout of a flush (`None`: the store default).
    pub write_timeout: Option<Duration>,
    /// How much of the stored history goes back to memory at start (24 h).
    pub rehydrate: Duration,
    /// Clock (default: the system clock).
    pub clock: Option<Clock>,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            agg: AggConfig::default(),
            tail: TailConfig::default(),
            tick: Duration::from_secs(2),
            flush_interval: Duration::from_secs(60),
            attribution_interval: Duration::from_secs(10),
            mailbox: 256,
            batch_queue: 64,
            write_timeout: None,
            rehydrate: persist::REHYDRATE_WINDOW,
            clock: None,
        }
    }
}

impl std::fmt::Debug for ServiceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceConfig")
            .field("tick", &self.tick)
            .field("flush_interval", &self.flush_interval)
            .field("attribution_interval", &self.attribution_interval)
            .finish_non_exhaustive()
    }
}

/// One access log to follow: the file of a running web server and how to read it (the
/// `logs[].access` block of its manifest, converted by the daemon).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogSource {
    /// Stable name of the file (the persisted checkpoint's `path_key`).
    pub key: String,
    pub path: PathBuf,
    pub format: AccessFormat,
}

/// A site as the router and the samples see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteRoute {
    pub id: SiteId,
    /// Lowercase primary host (also a `*.host` wildcard).
    pub host: String,
    pub aliases: Vec<String>,
    /// The site's switch (`sites.metrics`); off: its lines are dropped, its samples are zero.
    pub metrics_enabled: bool,
}

/// The latest sample of a pool and the sites routed to it.
#[derive(Debug, Clone, PartialEq)]
pub struct PoolFeed {
    /// `from_ms`/`to_ms` are ignored: the pool counts as alive for the whole bucket.
    pub sample: PoolSample,
    pub sites: Vec<SiteId>,
}

/// What to aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    /// The last `span` up to now.
    Window(Duration),
    /// `[from, to)`, `to` defaulting to now; aligned outward, limited by the retention in
    /// memory and by the stored history beyond it.
    Range {
        from: Timestamp,
        to: Option<Timestamp>,
    },
}

/// Which sites a query reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sites {
    /// Every site of the last [`MetricsHandle::set_routes`].
    All,
    /// One site (host and switch from the routes; unknown: empty host, switch on).
    One(SiteId),
    /// Exactly these, with their host and switch: the caller's fresher view of the sites
    /// (a site created a moment ago is reported before the routes reach the service).
    These(Vec<SiteRoute>),
}

/// Metrics of one site over a span.
#[derive(Debug, Clone, PartialEq)]
pub struct SiteSample {
    pub site: SiteId,
    pub host: String,
    /// Global and per-site switches together. Off: `summary` is empty.
    pub metrics_enabled: bool,
    pub summary: Summary,
    pub last_request_at: Option<Timestamp>,
    /// Latest attribution (10 s bucket) when the span ends now; `None` for past ranges or
    /// sites without pools or processes.
    pub resources: Option<SiteResources>,
}

/// Samples of every known site (or one), at the service clock `at`.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub at: Timestamp,
    pub samples: Vec<SiteSample>,
}

/// One followed file.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceStatus {
    pub key: String,
    pub path: PathBuf,
    /// `None` while no tailer runs for it (disabled, waiting for the old one to stop).
    pub tail: Option<TailStatus>,
    /// Why it is not followed (invalid format).
    pub error: Option<String>,
}

/// Diagnostics of the service.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceStatus {
    pub enabled: bool,
    /// Tailers running right now.
    pub tailers: usize,
    pub sources: Vec<SourceStatus>,
    pub sites: usize,
    pub agg: AggStats,
    /// Records whose host matched no site (weighted).
    pub unrouted: u64,
    /// Records of sites whose metrics are off (weighted).
    pub disabled: u64,
    /// Re-read records dropped by a resume (already stored).
    pub resumed_skipped: u64,
    pub batches: u64,
    pub flushes: u64,
    pub last_flush_error: Option<String>,
    /// Estimated heap of the aggregator.
    pub heap_bytes: usize,
    pub rehydrated: usize,
}

/// What the stop did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StopReport {
    pub flush: Option<FlushReport>,
    pub error: Option<String>,
}

type Reply<T> = oneshot::Sender<T>;
type SnapshotRx = watch::Receiver<Option<Arc<Snapshot>>>;

enum Msg {
    SetEnabled(bool),
    SetRoutes(Vec<SiteRoute>),
    SetSources(Vec<LogSource>),
    SetPools(Vec<PoolFeed>),
    SetApps(Vec<AppSample>),
    Query {
        span: Span,
        sites: Sites,
        reply: Reply<Result<Partial>>,
    },
    Subscribe {
        span: Duration,
        cadence: Duration,
        reply: Reply<SnapshotRx>,
    },
    Status {
        reply: Reply<ServiceStatus>,
    },
    Flush {
        reply: Reply<Result<FlushReport>>,
    },
    Stop {
        timeout: Duration,
        reply: Reply<StopReport>,
    },
}

/// The in-memory part of a query and the span still to read from SQLite.
#[derive(Debug)]
struct Partial {
    at: Timestamp,
    /// `[from, split)` to read through the store (older than the in-memory retention).
    store: Option<(Timestamp, Timestamp)>,
    samples: Vec<(SiteSample, Option<(LatencyHist, LatencyHist)>)>,
}

/// Handle on the service. Cheap to clone; the actor stops when [`Self::stop`] is called or
/// every handle is dropped.
#[derive(Clone)]
pub struct MetricsHandle {
    tx: mpsc::Sender<Msg>,
    reader: StoreReader,
}

impl std::fmt::Debug for MetricsHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricsHandle")
            .field("closed", &self.tx.is_closed())
            .finish()
    }
}

fn stopped() -> ForgeError {
    ForgeError::Internal("the metrics service is stopped".into())
}

impl MetricsHandle {
    async fn ask<R>(&self, make: impl FnOnce(Reply<R>) -> Msg, timeout: Duration) -> Result<R> {
        let (reply, answer) = oneshot::channel();
        match self.tx.try_send(make(reply)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                return Err(ForgeError::Timeout {
                    operation: "request to the metrics service (mailbox full)".into(),
                    after: Duration::ZERO,
                });
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return Err(stopped()),
        }
        match tokio::time::timeout(timeout, answer).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err(stopped()),
            Err(_) => Err(ForgeError::Timeout {
                operation: "request to the metrics service".into(),
                after: timeout,
            }),
        }
    }

    /// Fire-and-forget; `false` when the mailbox is full or the service stopped (the
    /// caller sends the full state again later: every `set_*` replaces what it had).
    fn tell(&self, msg: Msg) -> bool {
        self.tx.try_send(msg).is_ok()
    }

    /// Turns collection on or off (`[metrics] enabled`): off stops every tailer; the data
    /// in memory and on disk stays.
    pub fn set_enabled(&self, enabled: bool) -> bool {
        self.tell(Msg::SetEnabled(enabled))
    }

    /// Replaces the sites (router and per-site switches).
    pub fn set_routes(&self, routes: Vec<SiteRoute>) -> bool {
        self.tell(Msg::SetRoutes(routes))
    }

    /// Replaces the followed files (access logs of the running web servers).
    pub fn set_sources(&self, sources: Vec<LogSource>) -> bool {
        self.tell(Msg::SetSources(sources))
    }

    /// Replaces the latest pool samples.
    pub fn set_pools(&self, pools: Vec<PoolFeed>) -> bool {
        self.tell(Msg::SetPools(pools))
    }

    /// Replaces the latest samples of the sites' processes (`site` = the site id).
    pub fn set_apps(&self, apps: Vec<AppSample>) -> bool {
        self.tell(Msg::SetApps(apps))
    }

    /// Samples of `sites` over `span`. The part older than the in-memory retention is read
    /// from SQLite here, outside the actor, and merged exactly.
    pub async fn query(&self, span: Span, sites: Sites, timeout: Duration) -> Result<Snapshot> {
        let partial = self
            .ask(|reply| Msg::Query { span, sites, reply }, timeout)
            .await??;
        complete(partial, &self.reader).await
    }

    /// The snapshots of the window `span`, published every `cadence` while the receiver
    /// lives (`None` until the first publication).
    pub async fn subscribe(
        &self,
        span: Duration,
        cadence: Duration,
        timeout: Duration,
    ) -> Result<SnapshotRx> {
        self.ask(
            |reply| Msg::Subscribe {
                span,
                cadence,
                reply,
            },
            timeout,
        )
        .await
    }

    /// Diagnostics.
    pub async fn status(&self, timeout: Duration) -> Result<ServiceStatus> {
        self.ask(|reply| Msg::Status { reply }, timeout).await
    }

    /// Flushes now (tests and diagnostics; the service flushes every minute by itself).
    pub async fn flush(&self, timeout: Duration) -> Result<FlushReport> {
        self.ask(|reply| Msg::Flush { reply }, timeout).await?
    }

    /// Stops the tailers, consumes what they delivered and flushes, within `timeout`.
    pub async fn stop(&self, timeout: Duration) -> Result<StopReport> {
        self.ask(
            |reply| Msg::Stop { timeout, reply },
            timeout + Duration::from_secs(1),
        )
        .await
    }

    /// Whether the actor is gone.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// Merges the stored part of a query into the in-memory one.
async fn complete(partial: Partial, reader: &StoreReader) -> Result<Snapshot> {
    let Partial { at, store, samples } = partial;
    let mut out = Vec::with_capacity(samples.len());
    for (mut sample, hists) in samples {
        if let (Some((from, split)), true) = (store, sample.metrics_enabled) {
            let (stored, srt, sut) =
                persist::query_range_hists(reader, sample.site, from, split, at).await?;
            let (mrt, mut_) = hists.unwrap_or_default();
            sample.summary = merge(
                &stored,
                &sample.summary,
                &srt.merge(&mrt),
                &sut.merge(&mut_),
            );
        }
        out.push(sample);
    }
    Ok(Snapshot { at, samples: out })
}

fn add_counters(a: &mut Counters, b: &Counters) {
    a.requests += b.requests;
    for (x, y) in a.status.iter_mut().zip(b.status) {
        *x += y;
    }
    a.bytes += b.bytes;
    a.rt_sum_us += b.rt_sum_us;
    a.ut_sum_us += b.ut_sum_us;
    a.ut_count += b.ut_count;
}

/// `older` then `newer` (adjacent spans), with the merged histograms of both.
fn merge(older: &Summary, newer: &Summary, rt: &LatencyHist, ut: &LatencyHist) -> Summary {
    let mut counters = older.counters;
    add_counters(&mut counters, &newer.counters);
    Summary {
        from: older.from.min(newer.from),
        to: older.to.max(newer.to),
        counters,
        rt: rt.percentiles(),
        ut: ut.percentiles(),
        sampled: older.sampled || newer.sampled,
    }
}

/// Starts the service: rehydrates the last [`ServiceConfig::rehydrate`] from the store and
/// loads the stored tail checkpoints (in the actor, before its first message), then serves.
/// `enabled` is the initial global switch.
pub fn spawn(
    config: ServiceConfig,
    reader: StoreReader,
    writer: StoreWriter,
    enabled: bool,
) -> MetricsHandle {
    let (tx, rx) = mpsc::channel(config.mailbox.max(1));
    let handle = MetricsHandle {
        tx,
        reader: reader.clone(),
    };
    let service = Service::new(config, reader, writer, enabled);
    tokio::spawn(service.run(rx));
    handle
}

struct SourceState {
    spec: LogSource,
    handle: Option<tail::TailerHandle>,
    /// Drops re-read records already stored (first tailer after a boot resume).
    filter: Option<Resume>,
    /// Started without any position (see `Service::seed_start`).
    seed: bool,
    error: Option<String>,
}

struct Publisher {
    span: Duration,
    every: u64,
    tx: Arc<watch::Sender<Option<Arc<Snapshot>>>>,
    /// Completion of a snapshot that needs the store (single flight).
    completing: Option<JoinHandle<()>>,
}

struct Service {
    cfg: ServiceConfig,
    clock: Clock,
    reader: StoreReader,
    writer: StoreWriter,
    enabled: bool,
    agg: Aggregator,
    persister: Persister,
    router: HostRouter,
    routes: Vec<SiteRoute>,
    route_index: HashMap<SiteId, usize>,
    sources: BTreeMap<String, SourceState>,
    /// Tailers being stopped (a key is started again only once its old tailer ended).
    stopping: HashMap<String, JoinHandle<TailStatus>>,
    /// Position after the last batch consumed, per key.
    consumed: HashMap<String, Checkpoint>,
    /// Checkpoints stored by earlier runs, used once per key.
    boot_resumes: HashMap<String, Resume>,
    batch_tx: mpsc::Sender<TailBatch<AccessRecord>>,
    batch_rx: mpsc::Receiver<TailBatch<AccessRecord>>,
    /// The batch being aggregated and the index of its next record.
    current: Option<(TailBatch<AccessRecord>, usize)>,
    pools: Vec<PoolFeed>,
    apps: Vec<AppSample>,
    resources: HashMap<SiteId, SiteResources>,
    publishers: HashMap<u64, Publisher>,
    maintenance: Option<JoinHandle<()>>,
    last_rollup_hour: Option<i64>,
    last_prune_day: Option<i64>,
    ticks: u64,
    unrouted: u64,
    disabled: u64,
    resumed_skipped: u64,
    batches: u64,
    flushes: u64,
    last_flush_error: Option<String>,
    rehydrated: usize,
}

fn ticks_of(interval: Duration, tick: Duration) -> u64 {
    let tick = tick.as_millis().max(1);
    u64::try_from((interval.as_millis() + tick / 2) / tick)
        .unwrap_or(u64::MAX)
        .max(1)
}

fn floor_secs(ts: Timestamp, step: i64) -> i64 {
    ts.as_second().div_euclid(step) * step
}

fn at_secs(secs: i64) -> Timestamp {
    Timestamp::from_second(secs).unwrap_or(Timestamp::UNIX_EPOCH)
}

fn span_of(d: Duration) -> jiff::SignedDuration {
    jiff::SignedDuration::from_secs(i64::try_from(d.as_secs()).unwrap_or(i64::MAX / 4))
}

impl Service {
    fn new(
        mut cfg: ServiceConfig,
        reader: StoreReader,
        writer: StoreWriter,
        enabled: bool,
    ) -> Self {
        cfg.agg.minute_outbox = true;
        let clock: Clock = cfg
            .clock
            .clone()
            .unwrap_or_else(|| Arc::new(Timestamp::now));
        let agg = Aggregator::new(cfg.agg.clone(), clock());
        let (batch_tx, batch_rx) = mpsc::channel(cfg.batch_queue.max(1));
        let mut persister = Persister::new();
        if let Some(t) = cfg.write_timeout {
            persister = persister.with_write_timeout(t);
        }
        Self {
            clock,
            reader,
            writer,
            enabled,
            agg,
            persister,
            router: HostRouter::new(&[]),
            routes: Vec::new(),
            route_index: HashMap::new(),
            sources: BTreeMap::new(),
            stopping: HashMap::new(),
            consumed: HashMap::new(),
            boot_resumes: HashMap::new(),
            batch_tx,
            batch_rx,
            current: None,
            pools: Vec::new(),
            apps: Vec::new(),
            resources: HashMap::new(),
            publishers: HashMap::new(),
            maintenance: None,
            last_rollup_hour: None,
            last_prune_day: None,
            ticks: 0,
            unrouted: 0,
            disabled: 0,
            resumed_skipped: 0,
            batches: 0,
            flushes: 0,
            last_flush_error: None,
            rehydrated: 0,
            cfg,
        }
    }

    /// Retention of the aggregator, clamped like the aggregator does.
    fn retention_secs(&self) -> i64 {
        i64::try_from(self.cfg.agg.retention.as_secs())
            .unwrap_or(i64::MAX / 4)
            .clamp(
                HOUR,
                i64::try_from(crate::agg::MAX_RETENTION.as_secs()).unwrap_or(HOUR),
            )
    }

    async fn run(mut self, mut mailbox: mpsc::Receiver<Msg>) {
        self.start().await;
        let mut tick = tokio::time::interval(self.cfg.tick.max(Duration::from_millis(10)));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                msg = mailbox.recv() => {
                    let Some(msg) = msg else { break };
                    if let Some(stop) = self.handle(msg).await {
                        let (timeout, reply) = stop;
                        let report = self.shutdown(timeout).await;
                        let _ = reply.send(report);
                        return;
                    }
                }
                _ = tick.tick() => self.on_tick().await,
                () = std::future::ready(()), if self.current.is_some() => self.continue_batch(),
                Some(batch) = self.batch_rx.recv(), if self.current.is_none() => {
                    self.begin_batch(batch);
                }
            }
        }
        // Every handle dropped without a stop: flush what can be flushed.
        let _ = self.shutdown(Duration::from_secs(5)).await;
    }

    /// Rehydration and stored checkpoints, before the first message.
    async fn start(&mut self) {
        let now = (self.clock)();
        if !self.cfg.rehydrate.is_zero() {
            match persist::rehydrate(&mut self.agg, &self.reader, now, self.cfg.rehydrate).await {
                Ok(report) => {
                    self.rehydrated = report.restored;
                    tracing::info!(
                        restored = report.restored,
                        skipped = report.skipped,
                        "metrics: history rehydrated"
                    );
                }
                Err(err) => tracing::warn!(%err, "metrics: history not rehydrated"),
            }
        }
        match persist::load_checkpoints(&self.reader).await {
            Ok(list) => self.boot_resumes = list.into_iter().collect(),
            Err(err) => tracing::warn!(%err, "metrics: tail checkpoints not loaded"),
        }
    }

    /// Handles one message; `Some` for a stop.
    async fn handle(&mut self, msg: Msg) -> Option<(Duration, Reply<StopReport>)> {
        match msg {
            Msg::SetEnabled(enabled) => self.set_enabled(enabled).await,
            Msg::SetRoutes(routes) => self.set_routes(routes),
            Msg::SetSources(sources) => self.set_sources(sources).await,
            Msg::SetPools(pools) => self.pools = pools,
            Msg::SetApps(apps) => self.apps = apps,
            Msg::Query { span, sites, reply } => {
                let _ = reply.send(self.query(span, sites));
            }
            Msg::Subscribe {
                span,
                cadence,
                reply,
            } => {
                let _ = reply.send(self.subscribe(span, cadence));
            }
            Msg::Status { reply } => {
                let _ = reply.send(self.status());
            }
            Msg::Flush { reply } => {
                let result = self.flush().await;
                let _ = reply.send(result);
            }
            Msg::Stop { timeout, reply } => return Some((timeout, reply)),
        }
        None
    }

    fn advance(&mut self) -> Timestamp {
        let now = (self.clock)();
        self.agg.advance(now);
        self.agg.now()
    }

    // ----------------------------------------------------------------------- batches

    /// Takes a batch; its records go in by chunks of [`BATCH_CHUNK`], the mailbox and the
    /// tick being looked at between two chunks.
    fn begin_batch(&mut self, batch: TailBatch<AccessRecord>) {
        self.batches += 1;
        self.seed_start(&batch);
        self.current = Some((batch, 0));
        self.continue_batch();
    }

    /// A file read from its first byte without any stored position: the start of the file
    /// is a safe position from the beginning, so the first flush stores it (with its edge)
    /// even when the first batch straddles a minute that closes. A restart then re-reads the
    /// file and drops what the stored minutes have (never counted twice, never lost).
    fn seed_start(&mut self, batch: &TailBatch<AccessRecord>) {
        let Some(state) = self.sources.get_mut(&*batch.key) else {
            return;
        };
        if !std::mem::take(&mut state.seed) {
            return;
        }
        let from_start = state.handle.as_ref().is_some_and(|h| {
            matches!(
                h.status().start,
                tail::StartOutcome::Beginning | tail::StartOutcome::Missing
            )
        });
        if let (true, Some(cp)) = (from_start, batch.checkpoint) {
            self.persister.observe(
                &batch.key,
                Some(Checkpoint::start_of(cp.creation_time)),
                None,
            );
        }
    }

    /// The next chunk of the current batch; the batch is noted for persistence once all its
    /// records are in (its position is only safe after them).
    fn continue_batch(&mut self) {
        let Some((batch, start)) = self.current.take() else {
            return;
        };
        self.advance();
        let filter = self.sources.get(&*batch.key).and_then(|s| s.filter);
        let weight = u64::from(batch.weight);
        let end = (start + BATCH_CHUNK).min(batch.items.len());
        for rec in &batch.items[start..end] {
            if filter.is_some_and(|f| !f.keeps(rec)) {
                self.resumed_skipped += weight;
                continue;
            }
            match self.router.route(&rec.host).site() {
                Some(site) if self.site_enabled(site) => self.agg.record(site, rec, batch.weight),
                Some(_) => self.disabled += weight,
                None => self.unrouted += weight,
            }
        }
        if end < batch.items.len() {
            self.current = Some((batch, end));
            return;
        }
        if let Some(cp) = batch.checkpoint {
            self.consumed.insert(batch.key.to_string(), cp);
        }
        self.persister.observe_batch(&batch);
    }

    /// Consumes the current batch and every batch already queued (before a tailer of the
    /// same file starts again, and at the stop).
    fn drain_batches(&mut self) {
        while self.current.is_some() {
            self.continue_batch();
        }
        while let Ok(batch) = self.batch_rx.try_recv() {
            self.begin_batch(batch);
            while self.current.is_some() {
                self.continue_batch();
            }
        }
    }

    fn site_enabled(&self, site: SiteId) -> bool {
        self.route_index
            .get(&site)
            .is_none_or(|&i| self.routes[i].metrics_enabled)
    }

    // ----------------------------------------------------------------------- routes

    fn set_routes(&mut self, mut routes: Vec<SiteRoute>) {
        routes.sort_by(|a, b| a.host.cmp(&b.host));
        let hosts: Vec<SiteHosts> = routes
            .iter()
            .map(|r| SiteHosts {
                id: r.id,
                host: r.host.clone(),
                aliases: r.aliases.clone(),
            })
            .collect();
        self.router = self.router.rebuild(&hosts);
        let keep: HashSet<SiteId> = routes.iter().map(|r| r.id).collect();
        let gone: Vec<SiteId> = self.agg.sites().filter(|s| !keep.contains(s)).collect();
        for site in gone {
            // A removed site: its stored history stays (it ages out with the retention).
            self.agg.remove_site(site);
        }
        self.route_index = routes.iter().enumerate().map(|(i, r)| (r.id, i)).collect();
        self.routes = routes;
    }

    // ----------------------------------------------------------------------- sources

    async fn set_enabled(&mut self, enabled: bool) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        tracing::info!(enabled, "metrics: collection switched");
        if enabled {
            self.supervise().await;
        } else {
            let keys: Vec<String> = self.sources.keys().cloned().collect();
            for key in keys {
                self.stop_tailer(&key);
            }
        }
    }

    async fn set_sources(&mut self, sources: Vec<LogSource>) {
        let wanted: BTreeMap<String, LogSource> =
            sources.into_iter().map(|s| (s.key.clone(), s)).collect();
        let gone: Vec<String> = self
            .sources
            .keys()
            .filter(|k| !wanted.contains_key(*k))
            .cloned()
            .collect();
        for key in gone {
            self.stop_tailer(&key);
            self.sources.remove(&key);
        }
        for (key, spec) in wanted {
            match self.sources.get_mut(&key) {
                Some(state) if state.spec == spec => {}
                Some(state) => {
                    state.spec = spec;
                    state.error = None;
                    self.stop_tailer(&key);
                }
                None => {
                    self.sources.insert(
                        key,
                        SourceState {
                            spec,
                            handle: None,
                            filter: None,
                            seed: false,
                            error: None,
                        },
                    );
                }
            }
        }
        self.supervise().await;
    }

    fn stop_tailer(&mut self, key: &str) {
        let Some(state) = self.sources.get_mut(key) else {
            return;
        };
        if let Some(handle) = state.handle.take() {
            let grace = self.cfg.tail.stop_grace + Duration::from_millis(500);
            self.stopping
                .insert(key.to_owned(), tokio::spawn(handle.stop(grace)));
        }
        state.filter = None;
    }

    /// Starts the tailers that should run and restarts the ones that ended.
    async fn supervise(&mut self) {
        // Stopped tailers whose task ended are forgotten.
        self.stopping.retain(|_, join| !join.is_finished());
        let keys: Vec<String> = self.sources.keys().cloned().collect();
        for key in keys {
            let Some(state) = self.sources.get_mut(&key) else {
                continue;
            };
            if let Some(handle) = &state.handle {
                if !handle.is_finished() {
                    continue;
                }
                let status = handle.status();
                tracing::warn!(key = %key, state = ?status.state, error = ?status.last_error,
                    "metrics: tailer ended; restarting from the last batch consumed");
                state.handle = None;
                state.filter = None;
            }
            if !self.enabled || state.error.is_some() || self.stopping.contains_key(&key) {
                continue;
            }
            // Every batch of an earlier tailer of this file is in the channel already.
            self.drain_batches();
            self.start_tailer(&key).await;
        }
    }

    async fn start_tailer(&mut self, key: &str) {
        let Some(spec) = self.sources.get(key).map(|s| s.spec.clone()) else {
            return;
        };
        let parser = match AccessParser::new(&spec.format) {
            Ok(parser) => parser,
            Err(err) => {
                tracing::warn!(key, %err, "metrics: access log format not usable");
                if let Some(state) = self.sources.get_mut(key) {
                    state.error = Some(err.to_string());
                }
                return;
            }
        };
        let (start, filter, seed) = if let Some(cp) = self.consumed.get(key) {
            (StartAt::Checkpoint(*cp), None, false)
        } else if let Some(resume) = self.boot_resumes.remove(key) {
            (StartAt::Checkpoint(resume.checkpoint), Some(resume), false)
        } else {
            (self.first_start(&spec.path).await, None, true)
        };
        tracing::info!(key, path = %spec.path.display(), ?start, "metrics: following access log");
        let handle = tail::spawn(
            TailSpec {
                key: Arc::from(key),
                path: spec.path.clone(),
                start,
            },
            self.cfg.tail.clone(),
            parser,
            self.batch_tx.clone(),
        );
        if let Some(state) = self.sources.get_mut(key) {
            state.handle = Some(handle);
            state.filter = filter;
            state.seed = seed;
        }
    }

    /// A file never seen: from its first byte unless it is longer than a resume may lag.
    async fn first_start(&self, path: &std::path::Path) -> StartAt {
        let path = path.to_path_buf();
        let max = self.cfg.tail.resume_max_lag;
        let len = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::task::spawn_blocking(move || std::fs::metadata(&path).map(|m| m.len()).ok()),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
        match len {
            Some(len) if len > max => StartAt::End,
            _ => StartAt::Beginning,
        }
    }

    // ----------------------------------------------------------------------- tick

    async fn on_tick(&mut self) {
        self.ticks += 1;
        let now = self.advance();
        self.supervise().await;
        self.publish();
        let tick = self.cfg.tick;
        if self
            .ticks
            .is_multiple_of(ticks_of(self.cfg.attribution_interval, tick))
        {
            self.attribute(now);
        }
        if self
            .ticks
            .is_multiple_of(ticks_of(self.cfg.flush_interval, tick))
        {
            if let Err(err) = self.flush().await {
                tracing::warn!(%err, "metrics: flush failed; retried at the next one");
            }
            self.maintain(now);
        }
    }

    /// CPU/RAM of the last complete bucket, attributed to the sites (docs/14 section 3.3).
    fn attribute(&mut self, now: Timestamp) {
        if self.pools.is_empty() && self.apps.is_empty() {
            self.resources.clear();
            return;
        }
        let to = floor_secs(now, BUCKET_SECS);
        let from = to - BUCKET_SECS;
        let length_ms = u64::try_from(BUCKET_SECS * 1000).unwrap_or(10_000);
        let mut loads = BTreeMap::new();
        let mut routes = Vec::new();
        let mut pools = Vec::new();
        for feed in &self.pools {
            let mut sample = feed.sample.clone();
            sample.from_ms = 0;
            sample.to_ms = length_ms;
            for site in &feed.sites {
                let key = site.to_string();
                if !loads.contains_key(&key) {
                    let load = self.agg.sums(*site, at_secs(from), at_secs(to)).site_load();
                    loads.insert(key.clone(), load);
                }
                routes.push(PoolRoute {
                    site: key,
                    pool: sample.pool.clone(),
                    from_ms: 0,
                    to_ms: length_ms,
                });
            }
            pools.push(sample);
        }
        let bucket = Bucket {
            length_ms,
            pools,
            apps: self.apps.clone(),
            routes,
            loads,
        };
        let attribution = attribute(&bucket);
        self.resources = attribution
            .sites
            .into_iter()
            .filter_map(|(site, res)| Some((site.parse::<SiteId>().ok()?, res)))
            .collect();
        for (site, res) in &self.resources {
            self.agg.set_resources(*site, at_secs(from), res);
        }
    }

    async fn flush(&mut self) -> Result<FlushReport> {
        let now = (self.clock)();
        let result = self.persister.flush(&mut self.agg, &self.writer, now).await;
        self.flushes += 1;
        self.last_flush_error = result.as_ref().err().map(ToString::to_string);
        result
    }

    /// Hourly rollup and daily prune in their own task (they read and write the store only).
    fn maintain(&mut self, now: Timestamp) {
        if self.maintenance.as_ref().is_some_and(|j| !j.is_finished()) {
            return;
        }
        let hour = floor_secs(now, HOUR);
        let day = floor_secs(now, 24 * HOUR);
        let rollup = self.last_rollup_hour != Some(hour);
        let prune = self.last_prune_day != Some(day);
        if !(rollup || prune) {
            return;
        }
        self.last_rollup_hour = Some(hour);
        self.last_prune_day = Some(day);
        let (reader, writer) = (self.reader.clone(), self.writer.clone());
        self.maintenance = Some(tokio::spawn(async move {
            if rollup {
                match persist::rollup(&reader, &writer, now).await {
                    Ok(report) => tracing::debug!(?report, "metrics: rollup"),
                    Err(err) => tracing::warn!(%err, "metrics: rollup failed"),
                }
            }
            if prune {
                match persist::prune(&writer, now).await {
                    Ok(report) => tracing::debug!(?report, "metrics: prune"),
                    Err(err) => tracing::warn!(%err, "metrics: prune failed"),
                }
            }
        }));
    }

    // ----------------------------------------------------------------------- queries

    /// The in-memory part of `span` and what is left for the store.
    fn query(&mut self, span: Span, sites: Sites) -> Result<Partial> {
        let now = self.advance();
        let (from, to) = match span {
            Span::Window(d) => (now - span_of(d), now),
            Span::Range { from, to } => (from, to.unwrap_or(now).min(now)),
        };
        if from > to {
            return Err(ForgeError::InvalidInput(
                "the start of the range is after its end".into(),
            ));
        }
        let retention = self.retention_secs();
        let horizon = (now.as_second() - retention).div_euclid(HOUR) * HOUR;
        let (store, mem_from) = if from.as_second() >= horizon {
            (None, from)
        } else {
            // First whole hour the memory holds: older time comes from SQLite.
            let split = at_secs(-((retention - now.as_second()).div_euclid(HOUR) * HOUR));
            (Some((from, split.min(to))), split.min(to))
        };
        let live = to >= now - jiff::SignedDuration::from_secs(BUCKET_SECS);
        let routes: Vec<SiteRoute> = match sites {
            Sites::All => self.routes.clone(),
            Sites::One(id) => vec![
                self.route_index
                    .get(&id)
                    .map(|&i| self.routes[i].clone())
                    .unwrap_or(SiteRoute {
                        id,
                        host: String::new(),
                        aliases: Vec::new(),
                        metrics_enabled: true,
                    }),
            ],
            Sites::These(routes) => routes,
        };
        let with_hists = store.is_some();
        let samples = routes
            .iter()
            .map(|r| self.sample(r, mem_from, to, with_hists, live))
            .collect();
        Ok(Partial {
            at: now,
            store,
            samples,
        })
    }

    fn sample(
        &mut self,
        route: &SiteRoute,
        from: Timestamp,
        to: Timestamp,
        with_hists: bool,
        live: bool,
    ) -> (SiteSample, Option<(LatencyHist, LatencyHist)>) {
        let enabled = self.enabled && route.metrics_enabled;
        let (summary, hists) = if !enabled {
            (
                Summary {
                    from,
                    to,
                    counters: Counters::default(),
                    rt: None,
                    ut: None,
                    sampled: false,
                },
                None,
            )
        } else if with_hists {
            let (summary, rt, ut) = self.agg.range_hists(route.id, from, to);
            (summary, Some((rt, ut)))
        } else {
            (self.agg.range(route.id, from, to), None)
        };
        let sample = SiteSample {
            site: route.id,
            host: route.host.clone(),
            metrics_enabled: enabled,
            summary,
            last_request_at: enabled
                .then(|| self.agg.last_request_at(route.id))
                .flatten(),
            resources: (enabled && live)
                .then(|| self.resources.get(&route.id).cloned())
                .flatten(),
        };
        (sample, hists)
    }

    // ----------------------------------------------------------------------- subscriptions

    fn subscribe(&mut self, span: Duration, cadence: Duration) -> SnapshotRx {
        let every = ticks_of(cadence, self.cfg.tick);
        let publisher = self
            .publishers
            .entry(span.as_secs())
            .or_insert_with(|| Publisher {
                span,
                every,
                tx: Arc::new(watch::channel(None).0),
                completing: None,
            });
        publisher.every = publisher.every.min(every);
        publisher.tx.subscribe()
    }

    /// Publishes the snapshot of every window that has listeners and is due.
    fn publish(&mut self) {
        self.publishers.retain(|_, p| {
            p.tx.receiver_count() > 0 || p.completing.as_ref().is_some_and(|j| !j.is_finished())
        });
        let due: Vec<u64> = self
            .publishers
            .iter()
            .filter(|(_, p)| self.ticks.is_multiple_of(p.every) && p.tx.receiver_count() > 0)
            .map(|(k, _)| *k)
            .collect();
        for key in due {
            let span = self.publishers[&key].span;
            let partial = match self.query(Span::Window(span), Sites::All) {
                Ok(partial) => partial,
                Err(err) => {
                    tracing::debug!(%err, "metrics: snapshot not built");
                    continue;
                }
            };
            let Some(publisher) = self.publishers.get_mut(&key) else {
                continue;
            };
            if partial.store.is_none() {
                let samples = partial.samples.into_iter().map(|(s, _)| s).collect();
                publisher.tx.send_replace(Some(Arc::new(Snapshot {
                    at: partial.at,
                    samples,
                })));
                continue;
            }
            if publisher
                .completing
                .as_ref()
                .is_some_and(|j| !j.is_finished())
            {
                continue; // the previous one is still reading the store
            }
            let (tx, reader) = (Arc::clone(&publisher.tx), self.reader.clone());
            publisher.completing = Some(tokio::spawn(async move {
                match complete(partial, &reader).await {
                    Ok(snapshot) => {
                        tx.send_replace(Some(Arc::new(snapshot)));
                    }
                    Err(err) => tracing::debug!(%err, "metrics: stored part of a snapshot"),
                }
            }));
        }
    }

    // ----------------------------------------------------------------------- status, stop

    fn status(&self) -> ServiceStatus {
        let sources = self
            .sources
            .iter()
            .map(|(key, s)| SourceStatus {
                key: key.clone(),
                path: s.spec.path.clone(),
                tail: s.handle.as_ref().map(tail::TailerHandle::status),
                error: s.error.clone(),
            })
            .collect();
        ServiceStatus {
            enabled: self.enabled,
            tailers: self
                .sources
                .values()
                .filter(|s| s.handle.as_ref().is_some_and(|h| !h.is_finished()))
                .count(),
            sources,
            sites: self.routes.len(),
            agg: self.agg.stats(),
            unrouted: self.unrouted,
            disabled: self.disabled,
            resumed_skipped: self.resumed_skipped,
            batches: self.batches,
            flushes: self.flushes,
            last_flush_error: self.last_flush_error.clone(),
            heap_bytes: self.agg.heap_bytes(),
            rehydrated: self.rehydrated,
        }
    }

    /// Stops the tailers, consumes their last batches and flushes, within `timeout`.
    async fn shutdown(&mut self, timeout: Duration) -> StopReport {
        let deadline = tokio::time::Instant::now() + timeout;
        let grace = (timeout / 2).max(Duration::from_millis(100));
        let handles: Vec<tail::TailerHandle> = self
            .sources
            .values_mut()
            .filter_map(|s| s.handle.take())
            .collect();
        // Every tailer is asked first, so they wind down together.
        for handle in &handles {
            handle.control().request_stop();
        }
        for handle in handles {
            let _ = tokio::time::timeout_at(deadline, handle.stop(grace)).await;
        }
        for (_, join) in self.stopping.drain() {
            let _ = tokio::time::timeout_at(deadline, join).await;
        }
        self.drain_batches();
        if let Some(job) = self.maintenance.take() {
            job.abort();
        }
        for p in self.publishers.values_mut() {
            if let Some(job) = p.completing.take() {
                job.abort();
            }
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left.max(Duration::from_millis(200)), self.flush()).await {
            Ok(Ok(report)) => StopReport {
                flush: Some(report),
                error: None,
            },
            Ok(Err(err)) => {
                tracing::warn!(%err, "metrics: final flush failed");
                StopReport {
                    flush: None,
                    error: Some(err.to_string()),
                }
            }
            Err(_) => {
                tracing::warn!("metrics: final flush did not finish in time");
                StopReport {
                    flush: None,
                    error: Some("final flush timed out".into()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_counts_round_to_the_nearest_tick() {
        let tick = Duration::from_secs(2);
        assert_eq!(ticks_of(Duration::from_secs(2), tick), 1);
        assert_eq!(ticks_of(Duration::from_secs(10), tick), 5);
        assert_eq!(ticks_of(Duration::from_secs(60), tick), 30);
        assert_eq!(ticks_of(Duration::from_millis(100), tick), 1);
    }

    #[test]
    fn merged_spans_add_counters_and_merge_histograms() {
        let a = Summary {
            from: at_secs(0),
            to: at_secs(60),
            counters: Counters {
                requests: 2,
                status: [0, 1, 0, 1, 0],
                ..Counters::default()
            },
            rt: None,
            ut: None,
            sampled: false,
        };
        let b = Summary {
            from: at_secs(60),
            to: at_secs(120),
            counters: Counters {
                requests: 1,
                status: [0, 0, 0, 0, 1],
                ..Counters::default()
            },
            rt: None,
            ut: None,
            sampled: true,
        };
        let rt = LatencyHist::from_values([1000, 2000, 3000]);
        let m = merge(&a, &b, &rt, &LatencyHist::new());
        assert_eq!((m.from, m.to), (at_secs(0), at_secs(120)));
        assert_eq!(m.counters.requests, 3);
        assert_eq!(m.counters.status, [0, 1, 0, 1, 1]);
        assert!(m.sampled);
        assert_eq!(m.rt.unwrap().p50_us, rt.percentiles().unwrap().p50_us);
        assert!(m.ut.is_none());
    }
}
