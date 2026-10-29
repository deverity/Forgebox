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
//! * `site:<host>` / `app:<host>/<process>` — documented targets whose capture is the
//!   Logs screen (roadmap M2-E3): unknown sites are `not_found`, known ones answer
//!   `not_implemented`.
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

use std::path::{Path, PathBuf};
use std::time::Duration;

use forge_core::Timestamp;
use forge_core::error::{ForgeError, Result};
use forge_ipc::api::methods::{logs_read, logs_subscribe};
use forge_ipc::api::{
    LogChunkDto, LogEnvelope, LogLineDto, LogSource, LogsReadParams, LogsSubscribeParams,
    SubscriptionDto,
};
use forge_ipc::server::{Ctx, Router, StreamSender};
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
    router
        .timeout::<logs_read>(ANSWER_TIMEOUT)
        .timeout::<logs_subscribe>(ANSWER_TIMEOUT);
}

enum Target {
    /// Files only.
    Set(LogSet),
    /// An instance's files and, while it has an actor, its live lines.
    Instance(Box<Entry>, LogSet),
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
        return Ok(Target::Set(LogSet::Daily {
            dir,
            prefix: crate::logging::LOG_PREFIX.to_owned(),
        }));
    }
    if let Some(rest) = target
        .strip_prefix("site:")
        .or_else(|| target.strip_prefix("app:"))
    {
        let host = rest.split('/').next().unwrap_or(rest);
        super::sites::find(daemon, &forge_ipc::api::SiteRef::from(host)).await?;
        return Err(ForgeError::NotImplemented(format!(
            "log target {target}: site and app logs are not implemented yet (roadmap M2-E3)"
        )));
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
        return Ok(Target::Set(LogSet::rotated(file)));
    }
    if let Some((tool, file)) = target.split_once('.') {
        if !tool.contains('/') {
            let tool_id = forge_core::ids::ToolId::new(tool)
                .map_err(|e| ForgeError::InvalidInput(format!("{target}: {e}")))?;
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
                let path = match instance.as_ref().and_then(|i| i.vars.as_ref()) {
                    Some(vars) => vars.substitute(&spec.path)?,
                    None => continue,
                };
                let name = Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                if name.starts_with(&file.to_ascii_lowercase()) {
                    return Ok(Target::Set(LogSet::rotated(PathBuf::from(path))));
                }
            }
            return Err(ForgeError::NotFound(format!("log {target}")));
        }
    }
    let entries = daemon.services.resolve(target)?;
    let entry = entries
        .into_iter()
        .next()
        .ok_or_else(|| ForgeError::NotFound(format!("log target {target}")))?;
    let file =
        forge_supervisor::instance_log_path(&daemon.root, &entry.row.tool_id, &entry.row.name)?;
    Ok(Target::Instance(Box::new(entry), LogSet::rotated(file)))
}

fn set_of(target: &Target) -> &LogSet {
    match target {
        Target::Set(set) | Target::Instance(_, set) => set,
    }
}

fn level_of(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(level) = value.get("level").and_then(|l| l.as_str()) {
            return Some(level.to_ascii_lowercase());
        }
    }
    for level in ["error", "warn", "info", "debug"] {
        if lower.contains(&format!("[{level}")) {
            return Some(level.to_owned());
        }
    }
    None
}

fn ts_of(text: &str) -> Option<Timestamp> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()?
        .get("timestamp")?
        .as_str()?
        .parse()
        .ok()
}

fn line_dto(seq: u64, text: String) -> LogLineDto {
    LogLineDto {
        seq,
        ts: ts_of(&text),
        source: LogSource::File,
        level: level_of(&text),
        text,
        access: None,
    }
}

fn envelope(sender: &StreamSender, line: LogLineDto) -> LogEnvelope {
    LogEnvelope {
        subscription_id: sender.id(),
        seq: line.seq,
        line,
    }
}

fn matches(grep: Option<&str>, text: &str) -> bool {
    grep.is_none_or(|g| text.contains(g))
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

async fn read(daemon: Daemon, params: LogsReadParams, _ctx: Ctx) -> Result<LogChunkDto> {
    if params.before_seq.is_some() {
        // Ignoring it would answer the newest lines again: a client paging back would
        // repeat them instead of seeing older ones.
        return Err(ForgeError::NotImplementedYet {
            method: "logs.read".into(),
            task: super::not_implemented::LOGS_BEFORE_SEQ_TASK.into(),
            what: "paging back with before_seq".into(),
        });
    }
    let target = resolve(&daemon, &params.target).await?;
    let set = set_of(&target).clone();
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).max(1) as usize;
    let since = params
        .since_secs
        .map(|s| Timestamp::now() - jiff::SignedDuration::from_secs(i64::from(s)));
    let scan = if params.grep.is_some() || since.is_some() {
        SCAN_LIMIT
    } else {
        limit
    };
    let (snapshot, lines) = blocking("reading a log", move || set.tail(scan)).await?;
    let mut out: Vec<LogLineDto> = lines
        .into_iter()
        .map(|(seq, text)| line_dto(seq, text))
        .filter(|l| matches(params.grep.as_deref(), &l.text))
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
    grep: Option<&str>,
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
        if !matches(grep, &text) {
            continue;
        }
        if !sender
            .send_wait(&envelope(sender, line_dto(seq, text)), REPLAY_WAIT)
            .await
        {
            return false;
        }
    }
    sender.lagged(found.missing);
    !sender.is_closed()
}

/// Follows a set by polling it (in `spawn_blocking`) every [`POLL`], from `after` (or
/// from now).
async fn follow_set(set: LogSet, sender: StreamSender, grep: Option<String>, after: Option<u64>) {
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
                grep.as_deref(),
            )
            .await
        {
            return;
        }
    }
    poll_set(Follower::new(set, &snapshot), sender, grep).await;
}

async fn poll_set(mut follower: Follower, sender: StreamSender, grep: Option<String>) {
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
            if !matches(grep.as_deref(), &text) {
                continue;
            }
            if !sender.send(&envelope(&sender, line_dto(seq, text))) {
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
    grep: Option<String>,
    start: Start,
    upto: u64,
) {
    if !replay(&set, start, upto, true, &sender, grep.as_deref()).await {
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
                if !matches(grep.as_deref(), &line.text) {
                    continue;
                }
                let dto = LogLineDto {
                    seq: line.seq,
                    ts: Some(line.ts),
                    source: match line.stream {
                        LogStream::Stdout => LogSource::Stdout,
                        LogStream::Stderr => LogSource::Stderr,
                    },
                    level: level_of(&line.text),
                    text: line.text,
                    access: None,
                };
                if !sender.send(&envelope(&sender, dto)) {
                    return;
                }
            }
            Some(LogItem::Lagged { missed }) => sender.lagged(missed),
            None => {
                drop(sub);
                return follow_set(set, sender, grep, Some(last)).await;
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
    // Live lines of an instance actor, numbered like its files.
    let live = match &target {
        Target::Instance(entry, _) => match &entry.handle {
            Some(handle) => match handle.subscribe_logs(QUICK_ASK_TIMEOUT).await {
                Ok(sub) => tokio::time::timeout(START_WAIT, sub.start_seq())
                    .await
                    .ok()
                    .map(|next| (sub, next)),
                Err(_) => None,
            },
            None => None,
        },
        Target::Set(_) => None,
    };
    let set = set_of(&target).clone();
    if let Some((sub, next)) = live {
        let sender = ctx.open_stream(None)?;
        let id = sender.id();
        let upto = next.saturating_sub(1);
        tokio::spawn(follow_instance(sub, set, sender, params.grep, start, upto));
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
        if replay(&set, start, upto, false, &sender, params.grep.as_deref()).await {
            poll_set(Follower::new(set, &snapshot), sender, params.grep).await;
        }
    });
    Ok(SubscriptionDto {
        subscription_id: id,
        stream: "logs".into(),
        last_seq: upto,
    })
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

    #[test]
    fn json_lines_give_level_and_time() {
        let line =
            r#"{"timestamp":"2026-10-27T10:00:00Z","level":"WARN","fields":{"message":"x"}}"#;
        assert_eq!(level_of(line).as_deref(), Some("warn"));
        assert!(ts_of(line).is_some());
        assert_eq!(
            level_of("2026/10/27 [error] 1#1: boom").as_deref(),
            Some("error")
        );
    }
}
