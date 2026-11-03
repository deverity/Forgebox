//! `logs.read` / `logs.subscribe` (`forge logs`, `forge daemon logs`). Targets:
//!
//! * `daemon` — the daemon's own JSON lines (`logs/forgeboxd.log.<date>`, every day
//!   kept);
//! * `<tool>` / `<tool>/<instance>` / instance ULID — the captured stdout/stderr of the
//!   instance (`logs/<tool>/<name>.out.log` and its rotated files; live lines come from
//!   the instance actor, numbered like the files);
//! * `<tool>.<file>` — a log the manifest declares (`nginx.error` → the `logs[]` entry
//!   whose file name starts with `error`);
//! * `<pool tool>@<series>` — the newest worker log of that pool;
//! * `site:<host>` — the web server's JSON access log (the manifest `logs[].access`
//!   mapping, read by `forge-metrics`' parser), only the requests whose host is the
//!   site's or one of its aliases, with `LogLineDto.access` filled and the level from the
//!   status (5xx `error`, 4xx `warn`). Its `seq` is the line's index in the shared log, so
//!   a site's lines have gaps;
//! * `app:<host>/<process>` — the log file of the AppManager instance of a site process
//!   (ADR-0028), followed for live lines, with `Authorization:` / `password=` masked
//!   (docs/11 section 6); an unknown process is `not_found`.
//!
//! The level and time of each line come from the parser of the manifest `format`
//! ([`crate::log_format`]). `logs.targets` lists everything above that exists.
//! `logs.read {before_seq}` pages back: the newest `limit` lines below that `seq`.
//!
//! `seq` is one monotonic domain per target (ADR-0046, [`forge_supervisor::logs::set`]):
//! the line's absolute index in the target's file set, rotated files included (the
//! daemon's daily files: `day << 32 | line`). The tail backlog, the replay of a resumed
//! subscription and the live lines use it, across daemon restarts and rotations.
//!
//! * `logs.subscribe` answers `last_seq` = the newest line when the subscription was
//!   taken. The handler does no file I/O for instances: the follower task replays (`tail`
//!   or `resume_from`, at most [`REPLAY_LIMIT`] lines, waiting for queue room) and then
//!   streams the live lines; the lines a resume can no longer get (rotated away, or
//!   beyond the limit) are one `logs.lagged{missed}` before the replay.
//! * Instances: the actor's broadcast is taken first, then its next `seq`; lines below
//!   it come from the files (waiting up to [`DISK_WAIT`] for the writer), the rest live.
//!   A slow client lags on the broadcast (drop-oldest) and nothing waits for it. When
//!   the actor goes away, the follower keeps polling the files.
//! * Other files (and instances without an actor) are polled every [`POLL`] in
//!   `spawn_blocking`; rotation, a new day or a truncation never repeats a line nor
//!   makes `seq` go back.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forge_core::Timestamp;
use forge_core::error::{ForgeError, Result};
use forge_core::store::Site;
use forge_ipc::api::methods::{logs_read, logs_subscribe, logs_targets};
use forge_ipc::api::{
    AccessLineDto, LogChunkDto, LogEnvelope, LogLineDto, LogSource, LogTargetDto, LogTargetKind,
    LogTargetListDto, LogsReadParams, LogsSubscribeParams, NoParams, SubscriptionDto,
};
use forge_ipc::server::{Ctx, Router, StreamSender};
use forge_manifest::schema::tool::{AccessMulti, AccessSpec, AccessUnit, LogSpec};
use forge_metrics::{
    AccessFormat, AccessParser, FieldMap, Multi, Unit, UpstreamMap, normalize_host,
};
use forge_supervisor::logs::set::{Follower, LogSet, Snapshot};
use forge_supervisor::{LogItem, LogStream, LogSubscription, QUICK_ASK_TIMEOUT};

use super::route;
use crate::catalog;
use crate::daemon::Daemon;
use crate::services::Entry;

/// Lines returned by `logs.read` without `limit`.
const DEFAULT_LIMIT: u32 = 200;
/// Newest lines `logs.read` filters with `grep` / `since`.
const SCAN_LIMIT: usize = 100_000;
/// Most lines a subscription replays (`tail`, `resume_from`), like the events replay
/// limit of ADR-0015 §8: a resume further behind gets `logs.lagged` for the rest.
const REPLAY_LIMIT: u64 = 10_000;
/// File polling interval of `logs.subscribe`.
const POLL: Duration = Duration::from_millis(500);
/// How long a replay waits for an instance's writer to put its lines on disk (it is at
/// most one queue behind the broadcast), polling from [`DISK_POLL`], doubling up to
/// [`DISK_POLL_MAX`] (each poll recounts the set: a writer that keeps failing must not
/// turn a subscribe into a busy loop).
const DISK_WAIT: Duration = Duration::from_secs(3);
const DISK_POLL: Duration = Duration::from_millis(25);
const DISK_POLL_MAX: Duration = Duration::from_millis(400);
/// How long a replayed line waits for room in the subscriber's queue (ADR-0015 §8).
const REPLAY_WAIT: Duration = Duration::from_secs(2);
/// How long `logs.subscribe` waits for an instance's first count of its files; after
/// that the files are polled instead of the actor's broadcast.
const START_WAIT: Duration = Duration::from_millis(600);
/// Deadline of one blocking read of a set.
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// Answer deadline of `logs.read` / `logs.subscribe` (the default 1 s of ADR-0015 §6 is
/// too short for counting a full set, up to 50 MiB, on a cold disk): the actor ask
/// ([`QUICK_ASK_TIMEOUT`]), [`START_WAIT`] and one [`IO_TIMEOUT`] read, with slack. Below
/// the client's 10 s call timeout.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(8);

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, logs_read, read);
    route!(router, daemon, logs_subscribe, subscribe);
    route!(router, daemon, logs_targets, targets);
    router
        .timeout::<logs_read>(ANSWER_TIMEOUT)
        .timeout::<logs_subscribe>(ANSWER_TIMEOUT)
        .timeout::<logs_targets>(ANSWER_TIMEOUT);
}

/// What a target reads and how its lines are shaped.
struct Target {
    set: LogSet,
    /// The instance whose actor feeds the live lines.
    entry: Option<Box<Entry>>,
    shaper: Shaper,
}

impl Target {
    fn set(set: LogSet, format: Option<String>) -> Self {
        Self {
            set,
            entry: None,
            shaper: Shaper::plain(format),
        }
    }
}

/// Turns the raw lines of a target into [`LogLineDto`]s, or drops them: the line format
/// parsers ([`crate::log_format`], chosen by the manifest `format`), the access-log
/// filter of a site and the redaction of app output.
#[derive(Clone)]
struct Shaper {
    kind: ShapeKind,
    format: Option<String>,
}

#[derive(Clone)]
enum ShapeKind {
    Plain,
    /// Output of an app process: credentials are masked (docs/11 section 6).
    App,
    /// The access log of a web server, only the requests of the site's hosts.
    Site(Arc<SiteFilter>),
}

/// The access-log parser of a `site:<host>` target.
struct SiteFilter {
    parser: AccessParser,
    /// Primary host and aliases, normalized.
    hosts: BTreeSet<String>,
    /// JSON pointers of the request URI (as logged) and of the upstream address.
    uri: Option<String>,
    upstream: Option<String>,
}

impl Shaper {
    fn plain(format: Option<String>) -> Self {
        Self {
            kind: ShapeKind::Plain,
            format,
        }
    }

    /// Whether lines can be dropped, so a read has to scan more than `limit` lines.
    fn filters(&self) -> bool {
        matches!(self.kind, ShapeKind::Site(_))
    }

    fn shape(
        &self,
        seq: u64,
        text: String,
        source: LogSource,
        ts: Option<Timestamp>,
    ) -> Option<LogLineDto> {
        match &self.kind {
            ShapeKind::Plain => {
                let parsed = crate::log_format::parse(self.format.as_deref(), &text);
                Some(LogLineDto {
                    seq,
                    ts: ts.or(parsed.ts),
                    source,
                    level: parsed.level,
                    text,
                    access: None,
                })
            }
            ShapeKind::App => {
                let text = crate::log_format::redact(&text);
                let parsed = crate::log_format::parse(None, &text);
                Some(LogLineDto {
                    seq,
                    ts: ts.or(parsed.ts),
                    source,
                    level: parsed.level,
                    text,
                    access: None,
                })
            }
            ShapeKind::Site(filter) => {
                let (record, access) = filter.access(&text)?;
                let level = match record.status {
                    500..=599 => "error",
                    400..=499 => "warn",
                    _ => "info",
                };
                Some(LogLineDto {
                    seq,
                    ts: Some(record.end_ts),
                    source,
                    level: Some(level.to_owned()),
                    text,
                    access: Some(access),
                })
            }
        }
    }
}

impl SiteFilter {
    /// The request of an access line, when it is one of this site's.
    fn access(&self, text: &str) -> Option<(forge_metrics::AccessRecord, AccessLineDto)> {
        let record = self.parser.parse_str(text).ok()?;
        if !self.hosts.contains(&record.host) {
            return None;
        }
        // The record's route is normalized for aggregation; the viewer wants the URI as
        // the server logged it.
        let value = serde_json::from_str::<serde_json::Value>(text).ok();
        let text_at = |pointer: &Option<String>| -> Option<String> {
            let value = value.as_ref()?.pointer(pointer.as_deref()?)?;
            match value {
                serde_json::Value::String(s) => Some(s.clone()),
                other => Some(other.to_string()),
            }
        };
        let uri = text_at(&self.uri).unwrap_or_else(|| record.route.clone());
        let upstream = text_at(&self.upstream).filter(|u| !u.is_empty() && u != "-");
        let access = AccessLineDto {
            host: record.host.clone(),
            status: record.status,
            method: record.method.clone(),
            uri,
            rt_ms: Some(record.duration_us as f64 / 1000.0),
            upstream,
        };
        Some((record, access))
    }
}

/// The manifest's `logs[].access` mapping as the parser's format.
fn access_format(spec: &AccessSpec) -> AccessFormat {
    let unit = |u: AccessUnit| match u {
        AccessUnit::S => Unit::Seconds,
        AccessUnit::Ms => Unit::Millis,
        AccessUnit::Us => Unit::Micros,
        AccessUnit::Ns => Unit::Nanos,
    };
    AccessFormat {
        ts: FieldMap::new(spec.ts.pointer.clone(), unit(spec.ts.unit)),
        ts_is_start: spec.ts.start,
        host: spec.host.clone(),
        status: spec.status.clone(),
        duration: FieldMap::new(spec.duration.pointer.clone(), unit(spec.duration.unit)),
        upstream_duration: spec.upstream_duration.as_ref().map(|u| UpstreamMap {
            field: FieldMap::new(u.pointer.clone(), unit(u.unit)),
            multi: match u.multi {
                AccessMulti::Sum => Multi::Sum,
                AccessMulti::Last => Multi::Last,
            },
        }),
        bytes: spec.bytes.clone(),
        method: spec.method.clone(),
        uri: spec.uri.as_ref().map(|u| u.pointer.clone()),
        lenient_escapes: spec.lenient_escapes,
    }
}

/// The filter of a site over the access log described by `spec`.
fn site_filter(spec: &AccessSpec, site: &Site) -> Result<SiteFilter> {
    let parser = AccessParser::new(&access_format(spec))
        .map_err(|e| ForgeError::InvalidInput(format!("access log mapping: {e}")))?;
    let hosts = std::iter::once(site.host.as_str())
        .chain(site.aliases.iter().map(String::as_str))
        .map(|h| normalize_host(h).into_owned())
        .collect();
    Ok(SiteFilter {
        parser,
        hosts,
        uri: spec.uri.as_ref().map(|u| u.pointer.clone()),
        upstream: spec.upstream.clone(),
    })
}

/// Whether `path` is the newest file of a set (not a rotated `.<n>`, a `.seq` sidecar or
/// a temporary file).
fn is_current_log(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return true;
    };
    !(ext.bytes().all(|b| b.is_ascii_digit())
        || ext == forge_supervisor::logs::set::SIDECAR_SUFFIX.trim_start_matches('.')
        || ext == "tmp")
}

fn newest(files: Vec<PathBuf>) -> Option<PathBuf> {
    files
        .into_iter()
        .filter(|p| is_current_log(p))
        .max_by_key(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        })
}

/// The key of a manifest log in `<tool>.<key>` targets: the start of its file name
/// (`error.log` gives `error`).
fn log_key(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
        .split('.')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// The log of `tool` (its first instance with resolved variables) the manifest declares
/// and `pick` selects: the spec and its path.
fn manifest_log(
    daemon: &Daemon,
    tool: &str,
    pick: impl Fn(&LogSpec) -> bool,
) -> Result<Option<(LogSpec, PathBuf)>> {
    let tool_id = forge_core::ids::ToolId::new(tool)
        .map_err(|e| ForgeError::InvalidInput(format!("{tool}: {e}")))?;
    let entry = catalog::require_tool(&daemon.registry, &tool_id)?;
    let instance = daemon.services.resolve(tool)?.into_iter().next();
    let specs = entry
        .manifest
        .spec
        .service
        .daemon
        .as_ref()
        .map(|d| d.logs.clone())
        .unwrap_or_default();
    for spec in specs {
        if !pick(&spec) {
            continue;
        }
        let Some(vars) = instance.as_ref().and_then(|i| i.vars.as_ref()) else {
            continue;
        };
        let path = PathBuf::from(vars.substitute(&spec.path)?);
        return Ok(Some((spec, path)));
    }
    Ok(None)
}

async fn resolve(daemon: &Daemon, target: &str) -> Result<Target> {
    let target = target.trim();
    if target == "daemon" {
        let logs = daemon.root.logs();
        let dir = logs.clone();
        tokio::task::spawn_blocking(move || crate::logging::log_files(&logs).pop())
            .await
            .ok()
            .flatten()
            .ok_or_else(|| ForgeError::NotFound("no daemon log file yet".into()))?;
        return Ok(Target::set(
            LogSet::Daily {
                dir,
                prefix: crate::logging::LOG_PREFIX.to_owned(),
            },
            Some("json".into()),
        ));
    }
    if let Some(host) = target.strip_prefix("site:") {
        return resolve_site(daemon, host).await;
    }
    if let Some(rest) = target.strip_prefix("app:") {
        return resolve_app(daemon, rest).await;
    }
    if let Some((tool, version)) = target.split_once('@') {
        let (tool_id, _) = crate::install::parse_spec(target)?;
        let entry = catalog::require_tool(&daemon.registry, &tool_id)?;
        let rows = catalog::installed(&daemon.reader, &tool_id, Some(&entry.manifest)).await?;
        let row = catalog::pick(&entry.manifest, &rows, version)
            .ok_or_else(|| ForgeError::NotFound(format!("{tool}@{version} is not installed")))?;
        let series = catalog::series_of(&entry.manifest, &row.version);
        let dir = daemon.root.logs().join(tool).join(series);
        let file = tokio::task::spawn_blocking(move || {
            let files = std::fs::read_dir(&dir)
                .map(|e| {
                    e.flatten()
                        .map(|e| e.path())
                        .filter(|p| p.is_file())
                        .collect()
                })
                .unwrap_or_default();
            newest(files)
        })
        .await
        .ok()
        .flatten()
        .ok_or_else(|| ForgeError::NotFound(format!("no log of {target} yet")))?;
        return Ok(Target::set(LogSet::rotated(file), None));
    }
    if let Some((tool, file)) = target.split_once('.') {
        if !tool.contains('/') {
            let file = file.to_ascii_lowercase();
            let found = manifest_log(daemon, tool, |spec| {
                Path::new(&spec.path)
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().starts_with(&file))
            })?;
            return match found {
                Some((spec, path)) => Ok(Target::set(LogSet::rotated(path), spec.format)),
                None => Err(ForgeError::NotFound(format!("log {target}"))),
            };
        }
    }
    let entries = daemon.services.resolve(target)?;
    let entry = entries
        .into_iter()
        .next()
        .ok_or_else(|| ForgeError::NotFound(format!("log target {target}")))?;
    let file =
        forge_supervisor::instance_log_path(&daemon.root, &entry.row.tool_id, &entry.row.name)?;
    Ok(Target {
        set: LogSet::rotated(file),
        entry: Some(Box::new(entry)),
        shaper: Shaper::plain(None),
    })
}

/// `site:<host>`: the access log of the site's web server, only its hosts.
async fn resolve_site(daemon: &Daemon, host: &str) -> Result<Target> {
    let site = super::sites::find(daemon, &forge_ipc::api::SiteRef::from(host)).await?;
    let server = site
        .server
        .clone()
        .unwrap_or_else(|| daemon.config().default_server.to_string());
    let found = manifest_log(daemon, &server, |spec| spec.access.is_some())?;
    let Some((spec, path)) = found else {
        return Err(ForgeError::NotFound(format!(
            "no access log of {server} for site:{}",
            site.host
        )));
    };
    let access = spec.access.as_ref().expect("picked by `access`");
    let filter = site_filter(access, &site)?;
    Ok(Target {
        set: LogSet::rotated(path),
        entry: None,
        shaper: Shaper {
            kind: ShapeKind::Site(Arc::new(filter)),
            format: spec.format,
        },
    })
}

/// `app:<host>/<process>`: the AppManager's log of a site process (the file its
/// instance writes, ADR-0028), followed for the live lines.
async fn resolve_app(daemon: &Daemon, rest: &str) -> Result<Target> {
    let (host, process) = rest
        .split_once('/')
        .filter(|(h, p)| !h.is_empty() && !p.is_empty())
        .ok_or_else(|| {
            ForgeError::InvalidInput(format!("app:{rest}: expected app:<host>/<process>"))
        })?;
    let site = super::sites::find(daemon, &forge_ipc::api::SiteRef::from(host)).await?;
    let status = daemon.apps.status(&site.host).await.map_err(|e| match e {
        ForgeError::NotFound(_) => {
            ForgeError::NotFound(format!("site {} has no processes", site.host))
        }
        other => other,
    })?;
    let process = status
        .process(process)
        .ok_or_else(|| ForgeError::NotFound(format!("process {process} of {}", site.host)))?;
    Ok(Target {
        set: LogSet::rotated(process.log_file.clone()),
        entry: None,
        shaper: Shaper {
            kind: ShapeKind::App,
            format: None,
        },
    })
}

fn envelope(sender: &StreamSender, line: LogLineDto) -> LogEnvelope {
    LogEnvelope {
        subscription_id: sender.id(),
        seq: line.seq,
        line,
    }
}

/// What a subscription or read does with each raw line: shaping (and dropping) plus the
/// case-sensitive `grep`, which looks at the shaped (redacted) text.
#[derive(Clone)]
struct Filter {
    shaper: Shaper,
    grep: Option<String>,
}

impl Filter {
    fn line(
        &self,
        seq: u64,
        text: String,
        source: LogSource,
        ts: Option<Timestamp>,
    ) -> Option<LogLineDto> {
        let dto = self.shaper.shape(seq, text, source, ts)?;
        self.grep
            .as_deref()
            .is_none_or(|g| dto.text.contains(g))
            .then_some(dto)
    }

    fn file_line(&self, seq: u64, text: String) -> Option<LogLineDto> {
        self.line(seq, text, LogSource::File, None)
    }
}

/// Runs blocking set I/O off the runtime, with the read deadline.
async fn blocking<T: Send + 'static>(
    what: &str,
    f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T> {
    let what_owned = what.to_owned();
    forge_core::runtime::spawn_blocking_timeout(what, IO_TIMEOUT, move || {
        f().map_err(|e| ForgeError::io(what_owned, e))
    })
    .await
}

/// The newest `n` lines with `seq < before` (`None`: the newest of all), with the snapshot
/// they come from. Blocking.
fn read_before(
    set: &LogSet,
    before: Option<u64>,
    n: usize,
) -> std::io::Result<(Snapshot, Vec<(u64, String)>)> {
    let Some(before) = before else {
        return set.tail(n);
    };
    for _ in 0..10 {
        let snapshot = set.snapshot()?;
        let upto = before.saturating_sub(1).min(snapshot.last_seq);
        if upto == 0 || upto < snapshot.first_seq {
            return Ok((snapshot, Vec::new()));
        }
        let from = set.tail_start(&snapshot, n as u64, upto)?;
        if let Some(lines) = set.read_in(&snapshot, from, upto, n)? {
            return Ok((snapshot, lines));
        }
    }
    Err(std::io::Error::other("the log kept rotating"))
}

async fn read(daemon: Daemon, params: LogsReadParams, _ctx: Ctx) -> Result<LogChunkDto> {
    let target = resolve(&daemon, &params.target).await?;
    let filter = Filter {
        shaper: target.shaper.clone(),
        grep: params.grep.clone(),
    };
    let set = target.set.clone();
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).max(1) as usize;
    let since = params
        .since_secs
        .map(|s| Timestamp::now() - jiff::SignedDuration::from_secs(i64::from(s)));
    // Lines that can be dropped (grep, since, a site's other hosts) need a wider scan to
    // fill `limit`; the next page starts at the oldest line returned, so pages never
    // overlap nor skip a kept line.
    let scan = if params.grep.is_some() || since.is_some() || filter.shaper.filters() {
        SCAN_LIMIT
    } else {
        limit
    };
    let before = params.before_seq;
    let (snapshot, lines) =
        blocking("reading a log", move || read_before(&set, before, scan)).await?;
    let mut out: Vec<LogLineDto> = lines
        .into_iter()
        .filter_map(|(seq, text)| filter.file_line(seq, text))
        .filter(|l| since.is_none_or(|s| l.ts.is_none_or(|ts| ts >= s)))
        .collect();
    if out.len() > limit {
        out.drain(..out.len() - limit);
    }
    Ok(LogChunkDto {
        target: params.target,
        lines: out,
        last_seq: snapshot.last_seq,
    })
}

/// Which lines a subscription wants before the live ones.
#[derive(Debug, Clone, Copy)]
enum Start {
    /// The last N lines (`tail`).
    Tail(u64),
    /// Every line after this `seq` (`resume_from`).
    After(u64),
    Live,
}

impl Start {
    fn of(params: &LogsSubscribeParams) -> Self {
        match (params.resume_from, params.tail) {
            (Some(seq), _) => Self::After(seq),
            (None, Some(n)) if n > 0 => Self::Tail(u64::from(n).min(REPLAY_LIMIT)),
            _ => Self::Live,
        }
    }
}

/// What a replay read from the files.
struct Replay {
    /// Lines the client will never get, reported before `lines`: deleted by rotation, or
    /// beyond [`REPLAY_LIMIT`].
    lost: u64,
    lines: Vec<(u64, String)>,
    /// Lines up to `upto` not on disk (yet): reported after `lines`.
    missing: u64,
    snapshot: Snapshot,
}

/// Reads what `start` asks for up to `upto`. Blocking.
fn read_replay(set: &LogSet, start: Start, upto: u64) -> std::io::Result<Replay> {
    for _ in 0..10 {
        let snapshot = set.snapshot()?;
        let (mut from, mut lost) = match start {
            Start::Live => (upto + 1, 0),
            Start::Tail(n) => (set.tail_start(&snapshot, n, upto)?, 0),
            Start::After(seq) => {
                let from = seq.saturating_add(1);
                (from, set.lost_before(&snapshot, from))
            }
        };
        from = from.max(snapshot.first_seq);
        if from <= upto {
            // Only the newest REPLAY_LIMIT lines are replayed; older ones are lagged.
            let newest = set.tail_start(&snapshot, REPLAY_LIMIT, upto)?;
            if newest > from {
                lost += set.count_range(&snapshot, from, newest - 1)?;
                from = newest;
            }
        }
        let Some(lines) = set.read_in(&snapshot, from, upto, usize::MAX)? else {
            continue; // rotated meanwhile
        };
        let missing = match (set, lines.last()) {
            (LogSet::Rotated(_), Some((last, _))) => upto - last,
            (LogSet::Rotated(_), None) if from <= upto => upto - from + 1,
            _ => 0,
        };
        return Ok(Replay {
            lost,
            lines,
            missing,
            snapshot,
        });
    }
    Err(std::io::Error::other("the log kept rotating"))
}

/// Sends the lines `start` asks for, up to `upto` (inclusive). `wait_disk`: `upto` comes
/// from an instance whose writer may not have flushed yet; wait for it a little. `false`
/// once the stream is closed.
async fn replay(
    set: &LogSet,
    start: Start,
    upto: u64,
    wait_disk: bool,
    sender: &StreamSender,
    filter: &Filter,
) -> bool {
    if matches!(start, Start::Live) || upto == 0 {
        return !sender.is_closed();
    }
    let started = tokio::time::Instant::now();
    let mut pause = DISK_POLL;
    let found = loop {
        let s = set.clone();
        let found = blocking("replaying a log", move || read_replay(&s, start, upto)).await;
        match found {
            Ok(r) if wait_disk && r.missing > 0 && started.elapsed() < DISK_WAIT => {
                tokio::select! {
                    () = sender.closed() => return false,
                    () = tokio::time::sleep(pause) => {}
                }
                pause = (pause * 2).min(DISK_POLL_MAX);
            }
            Ok(r) => break r,
            Err(err) => {
                tracing::debug!(set = %set.display(), %err, "log replay failed");
                return !sender.is_closed();
            }
        }
    };
    tracing::trace!(
        set = %set.display(),
        first = found.snapshot.first_seq,
        last = found.snapshot.last_seq,
        lines = found.lines.len(),
        "log replay"
    );
    sender.lagged(found.lost);
    for (seq, text) in found.lines {
        let Some(dto) = filter.file_line(seq, text) else {
            continue;
        };
        if !sender.send_wait(&envelope(sender, dto), REPLAY_WAIT).await {
            return false;
        }
    }
    sender.lagged(found.missing);
    !sender.is_closed()
}

/// Follows a set by polling it (in `spawn_blocking`) every [`POLL`], from `after` (or
/// from now).
async fn follow_set(set: LogSet, sender: StreamSender, filter: Filter, after: Option<u64>) {
    let s = set.clone();
    let Ok(snapshot) = blocking("reading a log", move || s.snapshot()).await else {
        return;
    };
    if let Some(after) = after {
        if after < snapshot.last_seq
            && !replay(
                &set,
                Start::After(after),
                snapshot.last_seq,
                false,
                &sender,
                &filter,
            )
            .await
        {
            return;
        }
    }
    poll_set(Follower::new(set, &snapshot), sender, filter).await;
}

async fn poll_set(mut follower: Follower, sender: StreamSender, filter: Filter) {
    loop {
        tokio::select! {
            () = sender.closed() => return,
            () = tokio::time::sleep(POLL) => {}
        }
        let polled = tokio::task::spawn_blocking(move || {
            let polled = follower.poll();
            (follower, polled)
        })
        .await;
        let Ok((back, polled)) = polled else { return };
        follower = back;
        let polled = match polled {
            Ok(polled) => polled,
            Err(err) => {
                tracing::debug!(%err, "following a log failed; retrying");
                continue;
            }
        };
        sender.lagged(polled.lost);
        for (seq, text) in polled.lines {
            let Some(dto) = filter.file_line(seq, text) else {
                continue;
            };
            if !sender.send(&envelope(&sender, dto)) {
                return;
            }
        }
    }
}

/// The live lines of an instance after its replay; when the instance actor goes away,
/// the files are followed from the last line sent.
async fn follow_instance(
    mut sub: LogSubscription,
    set: LogSet,
    sender: StreamSender,
    filter: Filter,
    start: Start,
    upto: u64,
) {
    if !replay(&set, start, upto, true, &sender, &filter).await {
        return;
    }
    let mut last = upto;
    loop {
        let item = tokio::select! {
            () = sender.closed() => return,
            item = sub.recv_item() => item,
        };
        match item {
            Some(LogItem::Line(line)) => {
                // Lines up to `upto` were replayed from the files.
                if line.seq <= last {
                    continue;
                }
                last = line.seq;
                let source = match line.stream {
                    LogStream::Stdout => LogSource::Stdout,
                    LogStream::Stderr => LogSource::Stderr,
                };
                let Some(dto) = filter.line(line.seq, line.text, source, Some(line.ts)) else {
                    continue;
                };
                if !sender.send(&envelope(&sender, dto)) {
                    return;
                }
            }
            Some(LogItem::Lagged { missed }) => sender.lagged(missed),
            None => {
                drop(sub);
                return follow_set(set, sender, filter, Some(last)).await;
            }
        }
    }
}

async fn subscribe(
    daemon: Daemon,
    params: LogsSubscribeParams,
    ctx: Ctx,
) -> Result<SubscriptionDto> {
    let target = resolve(&daemon, &params.target).await?;
    let start = Start::of(&params);
    let filter = Filter {
        shaper: target.shaper.clone(),
        grep: params.grep.clone(),
    };
    // Live lines of an instance actor, numbered like its files.
    let live = match &target.entry {
        Some(entry) => match &entry.handle {
            Some(handle) => match handle.subscribe_logs(QUICK_ASK_TIMEOUT).await {
                Ok(sub) => tokio::time::timeout(START_WAIT, sub.start_seq())
                    .await
                    .ok()
                    .map(|next| (sub, next)),
                Err(_) => None,
            },
            None => None,
        },
        None => None,
    };
    let set = target.set.clone();
    if let Some((sub, next)) = live {
        let sender = ctx.open_stream(None)?;
        let id = sender.id();
        let upto = next.saturating_sub(1);
        tokio::spawn(follow_instance(sub, set, sender, filter, start, upto));
        return Ok(SubscriptionDto {
            subscription_id: id,
            stream: "logs".into(),
            last_seq: upto,
        });
    }
    // Files only (no actor, or its numbering is not ready yet): poll them.
    let s = set.clone();
    let snapshot = blocking("reading a log", move || s.snapshot()).await?;
    let sender = ctx.open_stream(None)?;
    let id = sender.id();
    let upto = snapshot.last_seq;
    tokio::spawn(async move {
        if replay(&set, start, upto, false, &sender, &filter).await {
            poll_set(Follower::new(set, &snapshot), sender, filter).await;
        }
    });
    Ok(SubscriptionDto {
        subscription_id: id,
        stream: "logs".into(),
        last_seq: upto,
    })
}
/// Root-relative path with `/` separators, for `fs.tail`.
fn rel(root: &Path, path: &Path) -> String {
    forge_core::paths::posix_string(path.strip_prefix(root).unwrap_or(path))
}

fn target_dto(
    target: String,
    label: String,
    kind: LogTargetKind,
    group: &str,
    format: Option<String>,
    live: bool,
    file: Option<String>,
) -> LogTargetDto {
    LogTargetDto {
        target,
        label,
        kind,
        group: group.to_owned(),
        format,
        live,
        file,
    }
}

/// `logs.targets`: everything `logs.read` accepts right now — the daemon, each
/// instance's output and the logs its manifest declares, the PHP pools with a log
/// folder, one `site:` per site whose web server has an access log, and one `app:` per
/// site process.
async fn targets(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<LogTargetListDto> {
    let root = daemon.root.path().to_path_buf();
    let mut out = Vec::new();

    // The daemon.
    let logs = daemon.root.logs();
    let newest_daemon = tokio::task::spawn_blocking(move || crate::logging::log_files(&logs).pop())
        .await
        .ok()
        .flatten();
    if let Some(file) = newest_daemon {
        out.push(target_dto(
            "daemon".into(),
            "Forgebox daemon".into(),
            LogTargetKind::Daemon,
            "daemon",
            Some("json".into()),
            true,
            Some(rel(&root, &file)),
        ));
    }

    // Instances and the logs their manifest declares (like `resolve`, the first
    // instance of a tool provides the paths).
    let mut tools_done = BTreeSet::new();
    for entry in daemon.services.entries() {
        let tool = entry.row.tool_id.to_string();
        let live = entry.handle.is_some();
        if let Ok(file) =
            forge_supervisor::instance_log_path(&daemon.root, &entry.row.tool_id, &entry.row.name)
        {
            out.push(target_dto(
                format!("{tool}/{}", entry.row.name),
                format!("{tool} · {}", entry.row.name),
                LogTargetKind::Instance,
                &tool,
                None,
                live,
                Some(rel(&root, &file)),
            ));
        }
        if !tools_done.insert(tool.clone()) {
            continue;
        }
        let (Some(manifest), Some(vars)) = (&entry.manifest, &entry.vars) else {
            continue;
        };
        let Some(service) = manifest.spec.service.daemon.as_ref() else {
            continue;
        };
        for (i, spec) in service.logs.iter().enumerate() {
            let key = log_key(&spec.path);
            // `<tool>.<key>` resolves to the first log whose file name starts with the
            // key: skip a log an earlier one would shadow.
            let shadowed = service.logs[..i].iter().any(|earlier| {
                Path::new(&earlier.path)
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().starts_with(&key))
            });
            let Ok(path) = vars.substitute(&spec.path) else {
                continue;
            };
            if key.is_empty() || shadowed {
                continue;
            }
            out.push(target_dto(
                format!("{tool}.{key}"),
                format!("{tool} · {}", spec.label.to_ascii_lowercase()),
                LogTargetKind::InstanceFile,
                &tool,
                spec.format.clone(),
                live,
                Some(rel(&root, Path::new(&path))),
            ));
        }
    }

    // PHP pools: `<tool>@<series>` for each installed series that has a log folder.
    let mut pools = Vec::new();
    for pool in catalog::pool_tools(&daemon.registry) {
        let id = pool.manifest.metadata.id.clone();
        let rows = catalog::installed(&daemon.reader, &id, Some(&pool.manifest))
            .await
            .unwrap_or_default();
        let series: BTreeSet<String> = rows
            .iter()
            .map(|r| catalog::series_of(&pool.manifest, &r.version))
            .collect();
        for series in series {
            pools.push((id.to_string(), series));
        }
    }
    let base = daemon.root.logs();
    let pools = tokio::task::spawn_blocking(move || {
        pools
            .into_iter()
            .filter(|(tool, series)| base.join(tool).join(series).is_dir())
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    for (tool, series) in pools {
        out.push(target_dto(
            format!("{tool}@{series}"),
            format!("{tool} {series} · pool"),
            LogTargetKind::Pool,
            &tool,
            None,
            true,
            Some(rel(&root, &daemon.root.logs().join(&tool).join(&series))),
        ));
    }

    // Sites (access log of their web server) and app processes.
    let default_server = daemon.config().default_server.to_string();
    let mut access_logs: std::collections::BTreeMap<String, Option<(LogSpec, PathBuf)>> =
        std::collections::BTreeMap::new();
    for site in daemon.sites.list().await.unwrap_or_default() {
        let server = site
            .server
            .clone()
            .unwrap_or_else(|| default_server.clone());
        let found = access_logs
            .entry(server.clone())
            .or_insert_with(|| {
                manifest_log(&daemon, &server, |spec| spec.access.is_some())
                    .ok()
                    .flatten()
            })
            .clone();
        if let Some((spec, path)) = found {
            out.push(target_dto(
                format!("site:{}", site.host),
                format!("{} · access", site.host),
                LogTargetKind::Site,
                &site.host,
                spec.format,
                true,
                Some(rel(&root, &path)),
            ));
        }
    }
    for host in daemon.apps.hosts().await.unwrap_or_default() {
        let Ok(status) = daemon.apps.status(&host).await else {
            continue;
        };
        for process in &status.processes {
            out.push(target_dto(
                format!("app:{}/{}", status.host, process.name),
                format!("{} · {}", status.host, process.name),
                LogTargetKind::App,
                &status.host,
                None,
                matches!(
                    process.instance.state,
                    forge_supervisor::InstanceState::Running
                ),
                Some(rel(&root, &process.log_file)),
            ));
        }
    }
    Ok(LogTargetListDto { targets: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `<dir>/a.log.1` with lines 1..=4 and `a.log` with 5..=10, 100 lines rotated away.
    fn rotated_set(dir: &Path) -> LogSet {
        let file = dir.join("a.log");
        forge_supervisor::logs::set::write_state(
            &file,
            forge_supervisor::logs::set::SeqState {
                dropped: 100,
                rotations: 2,
            },
        )
        .unwrap();
        let text = |r: std::ops::RangeInclusive<u64>| -> String {
            r.map(|i| format!("line {}\r\n", 100 + i)).collect()
        };
        std::fs::write(dir.join("a.log.1"), text(1..=4)).unwrap();
        std::fs::write(&file, text(5..=10)).unwrap();
        LogSet::rotated(file)
    }

    #[test]
    fn tails_are_numbered_across_the_rotated_files() {
        let dir = tempfile::tempdir().unwrap();
        let set = rotated_set(dir.path());
        let (snapshot, lines) = set.tail(8).unwrap();
        assert_eq!((snapshot.first_seq, snapshot.last_seq), (101, 110));
        assert_eq!(lines.len(), 8);
        assert!(
            lines
                .iter()
                .all(|(seq, text)| *text == format!("line {seq}"))
        );
        assert_eq!(lines[0].0, 103);
    }

    #[test]
    fn replays_resume_inside_the_files_or_count_what_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let set = rotated_set(dir.path());
        let seqs = |r: &Replay| r.lines.iter().map(|(s, _)| *s).collect::<Vec<_>>();
        // Inside the retained lines: exactly what follows the cursor.
        let r = read_replay(&set, Start::After(107), 110).unwrap();
        assert_eq!((r.lost, seqs(&r), r.missing), (0, vec![108, 109, 110], 0));
        // Before them: the rotated-away lines are counted.
        let r = read_replay(&set, Start::After(95), 110).unwrap();
        assert_eq!((r.lost, r.lines.len(), r.missing), (5, 10, 0));
        // An instance ahead of its file: the lines not written yet are missing.
        let r = read_replay(&set, Start::Tail(3), 112).unwrap();
        assert_eq!((r.lost, seqs(&r), r.missing), (0, vec![110], 2));
        // Nothing to replay.
        let r = read_replay(&set, Start::After(110), 110).unwrap();
        assert_eq!((r.lost, r.lines.len(), r.missing), (0, 0, 0));
        let r = read_replay(&set, Start::Live, 110).unwrap();
        assert_eq!((r.lost, r.lines.len(), r.missing), (0, 0, 0));
    }

    #[test]
    fn replays_keep_the_newest_lines_within_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.log");
        let total = REPLAY_LIMIT + 250;
        let text: String = (1..=total).map(|i| format!("{i}\n")).collect();
        std::fs::write(&file, text).unwrap();
        let set = LogSet::rotated(&file);
        let r = read_replay(&set, Start::After(0), total).unwrap();
        assert_eq!(r.lost, 250);
        assert_eq!(r.lines.len() as u64, REPLAY_LIMIT);
        assert_eq!(r.lines[0], (251, "251".to_owned()));
        let r = read_replay(&set, Start::Tail(REPLAY_LIMIT), total).unwrap();
        assert_eq!((r.lost, r.lines.len() as u64), (0, REPLAY_LIMIT));
    }

    #[test]
    fn subscribe_params_choose_the_start() {
        let params = |tail: Option<u32>, resume_from: Option<u64>| LogsSubscribeParams {
            target: "x".into(),
            grep: None,
            tail,
            resume_from,
        };
        assert!(matches!(
            Start::of(&params(Some(5), Some(9))),
            Start::After(9)
        ));
        assert!(matches!(Start::of(&params(Some(5), None)), Start::Tail(5)));
        assert!(matches!(
            Start::of(&params(Some(u32::MAX), None)),
            Start::Tail(REPLAY_LIMIT)
        ));
        assert!(matches!(Start::of(&params(Some(0), None)), Start::Live));
        assert!(matches!(Start::of(&params(None, None)), Start::Live));
    }

    #[test]
    fn only_current_files_are_pool_logs() {
        assert!(is_current_log(Path::new("worker-9001.out.log")));
        assert!(is_current_log(Path::new("php_errors")));
        assert!(!is_current_log(Path::new("worker-9001.out.log.1")));
        assert!(!is_current_log(Path::new("worker-9001.out.log.seq")));
        assert!(!is_current_log(Path::new("worker-9001.out.log.seq.tmp")));
    }

    /// `registry/tools/nginx.yaml` `logs[1].access`.
    fn nginx_access() -> AccessSpec {
        serde_json::from_value(serde_json::json!({
            "ts": {"pointer": "/t", "unit": "s"},
            "host": "/host",
            "status": "/s",
            "duration": {"pointer": "/rt", "unit": "s"},
            "upstream_duration": {"pointer": "/ut", "unit": "s", "multi": "sum"},
            "bytes": "/b",
            "method": "/m",
            "uri": {"pointer": "/u"},
            "upstream": "/up",
        }))
        .unwrap()
    }

    fn site(host: &str, aliases: &[&str]) -> Site {
        let mut site = Site::new(host, host.replace('.', "-"));
        site.aliases = aliases.iter().map(|a| (*a).to_owned()).collect();
        site
    }

    fn access_line(t: f64, host: &str, status: u16, uri: &str) -> String {
        format!(
            r#"{{"t":{t},"host":"{host}","s":{status},"rt":0.012,"ut":"0.010","b":512,"m":"GET","u":"{uri}","up":"127.0.0.1:19083"}}"#
        )
    }

    fn site_shaper(site: &Site) -> Shaper {
        Shaper {
            kind: ShapeKind::Site(Arc::new(site_filter(&nginx_access(), site).unwrap())),
            format: Some("forgebox-json".into()),
        }
    }

    fn filter_of(shaper: Shaper) -> Filter {
        Filter { shaper, grep: None }
    }

    #[test]
    fn a_site_keeps_only_its_hosts_and_fills_the_access_fields() {
        let filter = filter_of(site_shaper(&site("loja.test", &["www.loja.test"])));
        let lines = [
            access_line(1_761_652_800.5, "loja.test", 200, "/index.php"),
            access_line(1_761_652_801.0, "other.test", 200, "/x"),
            access_line(1_761_652_802.0, "WWW.Loja.test:8080", 404, "/missing"),
            "not json at all".to_owned(),
            access_line(1_761_652_803.0, "blog.test", 500, "/boom"),
            access_line(1_761_652_804.0, "loja.test", 503, "/down"),
        ];
        let got: Vec<LogLineDto> = lines
            .into_iter()
            .enumerate()
            .filter_map(|(i, text)| filter.file_line(i as u64 + 1, text))
            .collect();
        assert_eq!(got.iter().map(|l| l.seq).collect::<Vec<_>>(), vec![1, 3, 6]);
        let access = got[0].access.as_ref().unwrap();
        assert_eq!(
            (access.host.as_str(), access.status, access.method.as_str()),
            ("loja.test", 200, "GET")
        );
        assert_eq!(access.uri, "/index.php");
        assert_eq!(access.rt_ms, Some(12.0));
        assert_eq!(access.upstream.as_deref(), Some("127.0.0.1:19083"));
        assert_eq!(got[0].level.as_deref(), Some("info"));
        assert_eq!(got[0].ts, Some("2025-10-28T12:00:00.5Z".parse().unwrap()));
        // The alias, with a port and capitals: normalized; 4xx warns, 5xx fails.
        assert_eq!(got[1].access.as_ref().unwrap().host, "www.loja.test");
        assert_eq!(got[1].level.as_deref(), Some("warn"));
        assert_eq!(got[2].level.as_deref(), Some("error"));
    }

    #[test]
    fn a_site_follows_appends_and_survives_a_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("access.json");
        let filter = filter_of(site_shaper(&site("loja.test", &[])));
        let write = |lines: &[String]| {
            let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
            std::fs::write(&file, text).unwrap();
        };
        let first = [
            access_line(1.0, "loja.test", 200, "/a"),
            access_line(2.0, "other.test", 200, "/b"),
            access_line(3.0, "loja.test", 200, "/c"),
        ];
        write(&first);
        let set = LogSet::rotated(&file);
        let snapshot = set.snapshot().unwrap();
        assert_eq!(snapshot.last_seq, 3);
        let mut follower = Follower::new(set, &snapshot);
        let shape = |lines: Vec<(u64, String)>| -> Vec<(u64, String)> {
            lines
                .into_iter()
                .filter_map(|(seq, text)| filter.file_line(seq, text))
                .map(|l| (l.seq, l.access.unwrap().uri))
                .collect()
        };

        // Appends: the other host's line is dropped, the site's arrives.
        let mut lines = first.to_vec();
        lines.push(access_line(4.0, "other.test", 200, "/d"));
        lines.push(access_line(5.0, "loja.test", 200, "/e"));
        write(&lines);
        assert_eq!(
            shape(follower.poll().unwrap().lines),
            vec![(5, "/e".to_owned())]
        );
        assert!(follower.poll().unwrap().lines.is_empty());

        // Truncation (someone emptied the file): no error, nothing repeated, and the
        // `seq` of the next line is still higher than everything seen.
        write(&[access_line(6.0, "loja.test", 200, "/f")]);
        let after = shape(follower.poll().unwrap().lines);
        assert_eq!(after.len(), 1, "{after:?}");
        assert_eq!(after[0].1, "/f");
        assert!(after[0].0 > 5, "seq went back: {after:?}");
    }

    #[test]
    fn before_seq_pages_back_without_gaps_or_overlaps() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.log");
        let text: String = (1..=25).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&file, text).unwrap();
        let set = LogSet::rotated(&file);
        let seqs = |before: Option<u64>, n: usize| -> Vec<u64> {
            read_before(&set, before, n)
                .unwrap()
                .1
                .into_iter()
                .map(|(seq, _)| seq)
                .collect()
        };
        let page1 = seqs(None, 10);
        assert_eq!((page1[0], *page1.last().unwrap()), (16, 25));
        let page2 = seqs(Some(page1[0]), 10);
        assert_eq!((page2[0], *page2.last().unwrap()), (6, 15));
        let page3 = seqs(Some(page2[0]), 10);
        assert_eq!(page3, vec![1, 2, 3, 4, 5]);
        assert!(seqs(Some(page3[0]), 10).is_empty());
        // A cursor ahead of the file gives the newest lines.
        assert_eq!(seqs(Some(1000), 3), vec![23, 24, 25]);
    }

    #[test]
    fn before_seq_pages_over_a_filtered_target() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("access.json");
        let host = |i: u32| {
            if i.is_multiple_of(3) {
                "loja.test"
            } else {
                "other.test"
            }
        };
        let text: String = (1..=30)
            .map(|i| format!("{}\n", access_line(f64::from(i), host(i), 200, "/")))
            .collect();
        std::fs::write(&file, text).unwrap();
        let set = LogSet::rotated(&file);
        let filter = filter_of(site_shaper(&site("loja.test", &[])));
        let page = |before: Option<u64>, limit: usize| -> Vec<u64> {
            let (_, lines) = read_before(&set, before, SCAN_LIMIT).unwrap();
            let mut kept: Vec<u64> = lines
                .into_iter()
                .filter_map(|(seq, text)| filter.file_line(seq, text))
                .map(|l| l.seq)
                .collect();
            if kept.len() > limit {
                kept.drain(..kept.len() - limit);
            }
            kept
        };
        let p1 = page(None, 4);
        assert_eq!(p1, vec![21, 24, 27, 30]);
        let p2 = page(Some(p1[0]), 4);
        assert_eq!(p2, vec![9, 12, 15, 18]);
        let p3 = page(Some(p2[0]), 4);
        assert_eq!(p3, vec![3, 6]);
    }

    #[test]
    fn app_output_is_redacted_and_leveled() {
        let filter = filter_of(Shaper {
            kind: ShapeKind::App,
            format: None,
        });
        let line = filter
            .line(
                7,
                "[error] sent Authorization: Bearer abc123 password=hunter2".into(),
                LogSource::Stderr,
                None,
            )
            .unwrap();
        assert_eq!(line.text, "[error] sent Authorization: *** password=***");
        assert_eq!(line.level.as_deref(), Some("error"));
        assert_eq!(line.source, LogSource::Stderr);
        // `grep` looks at the redacted text: a secret cannot be probed for.
        let probing = Filter {
            grep: Some("hunter2".into()),
            ..filter
        };
        assert!(
            probing
                .line(8, "password=hunter2".into(), LogSource::Stdout, None)
                .is_none()
        );
    }

    #[test]
    fn manifest_logs_get_a_stable_key() {
        assert_eq!(log_key("C:/forgebox/logs/nginx/error.log"), "error");
        assert_eq!(log_key("C:/x/access.json"), "access");
        assert_eq!(log_key("C:/x/postgresql.log"), "postgresql");
    }
}
