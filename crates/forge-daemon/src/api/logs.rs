//! `logs.read` / `logs.subscribe` (`forge logs`, `forge daemon logs`). Targets:
//!
//! * `daemon` — the daemon's own JSON lines (`logs/forgeboxd.log.<date>`, newest file);
//! * `<tool>` / `<tool>/<instance>` / instance ULID — the captured stdout/stderr of the
//!   instance (`logs/<tool>/<name>.out.log`; live lines come from the instance actor,
//!   with its own `seq`);
//! * `<tool>.<file>` — a log the manifest declares (`nginx.error` → the `logs[]` entry
//!   whose file name starts with `error`);
//! * `<pool tool>@<series>` — the newest worker log of that pool.
//!
//! File targets number their lines from the start of the file (`seq` = line number);
//! following a file polls it every 500 ms and starts over when it was rotated.

use std::io::{Read, Seek, SeekFrom};
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
use forge_supervisor::{LogItem, LogStream, QUICK_ASK_TIMEOUT};

use super::route;
use crate::catalog;
use crate::daemon::Daemon;
use crate::services::Entry;

/// Lines returned by `logs.read` without `limit`.
const DEFAULT_LIMIT: u32 = 200;
/// Largest tail read from a file.
const MAX_TAIL_BYTES: u64 = 4 * 1024 * 1024;
/// File polling interval of `logs.subscribe`.
const POLL: Duration = Duration::from_millis(500);

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, logs_read, read);
    route!(router, daemon, logs_subscribe, subscribe);
}

enum Target {
    /// A file, optionally a live instance too.
    File(PathBuf),
    Instance(Box<Entry>, PathBuf),
}

fn newest(files: Vec<PathBuf>) -> Option<PathBuf> {
    files.into_iter().max_by_key(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    })
}

async fn resolve(daemon: &Daemon, target: &str) -> Result<Target> {
    let target = target.trim();
    if target == "daemon" {
        let logs = daemon.root.logs();
        let file = tokio::task::spawn_blocking(move || crate::logging::log_files(&logs).pop())
            .await
            .ok()
            .flatten()
            .ok_or_else(|| ForgeError::NotFound("no daemon log file yet".into()))?;
        return Ok(Target::File(file));
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
        return Ok(Target::File(file));
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
                    return Ok(Target::File(PathBuf::from(path)));
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
    Ok(Target::Instance(Box::new(entry), file))
}

fn file_of(target: &Target) -> &Path {
    match target {
        Target::File(p) | Target::Instance(_, p) => p,
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

/// Last `limit` lines of `path` (from at most [`MAX_TAIL_BYTES`]), numbered from the
/// start of the file; also returns the number of lines. Blocking.
fn tail_file(path: &Path, limit: usize) -> Result<(Vec<(u64, String)>, u64)> {
    let mut file = std::fs::File::open(path).map_err(|e| ForgeError::io_at("opening", path, e))?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut text = String::new();
    let start = len.saturating_sub(MAX_TAIL_BYTES);
    let skipped_lines = if start > 0 {
        // Count the lines before the window so numbering stays absolute.
        let mut head = vec![0u8; usize::try_from(start).unwrap_or(usize::MAX)];
        file.read_exact(&mut head)
            .map_err(|e| ForgeError::io_at("reading", path, e))?;
        head.iter().filter(|b| **b == b'\n').count() as u64
    } else {
        0
    };
    file.seek(SeekFrom::Start(start))
        .map_err(|e| ForgeError::io_at("reading", path, e))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| ForgeError::io_at("reading", path, e))?;
    text.push_str(&String::from_utf8_lossy(&bytes));
    let lines: Vec<&str> = text.lines().collect();
    let total = skipped_lines + lines.len() as u64;
    let from = lines.len().saturating_sub(limit);
    let out = lines[from..]
        .iter()
        .enumerate()
        .map(|(i, l)| {
            (
                skipped_lines + (from + i) as u64 + 1,
                l.trim_end_matches('\r').to_owned(),
            )
        })
        .collect();
    Ok((out, total))
}

fn line_dto(seq: u64, text: String) -> LogLineDto {
    LogLineDto {
        seq,
        ts: ts_of(&text),
        source: LogSource::File,
        level: level_of(&text),
        text,
    }
}

async fn read(daemon: Daemon, params: LogsReadParams, _ctx: Ctx) -> Result<LogChunkDto> {
    let target = resolve(&daemon, &params.target).await?;
    let path = file_of(&target).to_path_buf();
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).max(1) as usize;
    let grep = params.grep.clone();
    let since = params
        .since_secs
        .map(|s| Timestamp::now() - jiff::SignedDuration::from_secs(i64::from(s)));
    let (lines, total) = forge_core::runtime::spawn_blocking_timeout(
        "reading a log",
        Duration::from_secs(5),
        move || match tail_file(
            &path,
            if grep.is_some() || since.is_some() {
                100_000
            } else {
                limit
            },
        ) {
            Ok(found) => Ok(found),
            Err(ForgeError::NotFound(_)) => Ok((Vec::new(), 0)),
            Err(e) => Err(e),
        },
    )
    .await?;
    let mut out: Vec<LogLineDto> = lines
        .into_iter()
        .map(|(seq, text)| line_dto(seq, text))
        .filter(|l| params.grep.as_deref().is_none_or(|g| l.text.contains(g)))
        .filter(|l| since.is_none_or(|s| l.ts.is_none_or(|ts| ts >= s)))
        .collect();
    if out.len() > limit {
        out.drain(..out.len() - limit);
    }
    Ok(LogChunkDto {
        target: params.target,
        lines: out,
        last_seq: total,
    })
}

async fn follow_file(
    path: PathBuf,
    sender: StreamSender,
    grep: Option<String>,
    mut offset: u64,
    mut seq: u64,
) {
    let mut pending = String::new();
    loop {
        tokio::select! {
            () = sender.closed() => return,
            () = tokio::time::sleep(POLL) => {}
        }
        let p = path.clone();
        let read = tokio::task::spawn_blocking(move || -> std::io::Result<(u64, Vec<u8>)> {
            let mut file = std::fs::File::open(&p)?;
            let len = file.metadata()?.len();
            Ok((len, {
                let mut buf = Vec::new();
                if len > offset {
                    file.seek(SeekFrom::Start(offset))?;
                    file.take(MAX_TAIL_BYTES).read_to_end(&mut buf)?;
                }
                buf
            }))
        })
        .await;
        let Ok(Ok((len, bytes))) = read else { continue };
        if len < offset {
            // Rotated or truncated: start over.
            offset = 0;
            seq = 0;
            pending.clear();
            continue;
        }
        offset += bytes.len() as u64;
        pending.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(pos) = pending.find('\n') {
            let line: String = pending.drain(..=pos).collect();
            seq += 1;
            let text = line.trim_end_matches(['\n', '\r']).to_owned();
            if grep.as_deref().is_some_and(|g| !text.contains(g)) {
                continue;
            }
            let envelope = LogEnvelope {
                subscription_id: sender.id(),
                seq,
                line: line_dto(seq, text),
            };
            if !sender.send(&envelope) {
                return;
            }
        }
    }
}

async fn follow_instance(
    entry: Entry,
    sender: StreamSender,
    grep: Option<String>,
    resume_from: Option<u64>,
) {
    let Some(handle) = entry.handle.clone() else {
        return;
    };
    let Ok(mut sub) = handle.subscribe_logs(QUICK_ASK_TIMEOUT).await else {
        return;
    };
    loop {
        let item = tokio::select! {
            () = sender.closed() => return,
            item = sub.recv_item() => item,
        };
        match item {
            Some(LogItem::Line(line)) => {
                if resume_from.is_some_and(|r| line.seq <= r) {
                    continue;
                }
                if grep.as_deref().is_some_and(|g| !line.text.contains(g)) {
                    continue;
                }
                let envelope = LogEnvelope {
                    subscription_id: sender.id(),
                    seq: line.seq,
                    line: LogLineDto {
                        seq: line.seq,
                        ts: Some(line.ts),
                        source: match line.stream {
                            LogStream::Stdout => LogSource::Stdout,
                            LogStream::Stderr => LogSource::Stderr,
                        },
                        level: level_of(&line.text),
                        text: line.text,
                    },
                };
                if !sender.send(&envelope) {
                    return;
                }
            }
            Some(LogItem::Lagged { missed }) => sender.lagged(missed),
            None => return,
        }
    }
}

async fn subscribe(
    daemon: Daemon,
    params: LogsSubscribeParams,
    ctx: Ctx,
) -> Result<SubscriptionDto> {
    let target = resolve(&daemon, &params.target).await?;
    let sender = ctx.open_stream(None)?;
    let tail = params.tail.unwrap_or(0) as usize;
    match target {
        Target::Instance(entry, path) => {
            // History from the file (unless resuming), then the live lines of the actor.
            if params.resume_from.is_none() && tail > 0 {
                let p = path.clone();
                if let Ok(Ok((lines, _))) =
                    tokio::task::spawn_blocking(move || tail_file(&p, tail)).await
                {
                    for (seq, text) in lines {
                        let _ = sender
                            .send_wait(
                                &LogEnvelope {
                                    subscription_id: sender.id(),
                                    seq,
                                    line: line_dto(seq, text),
                                },
                                Duration::from_millis(200),
                            )
                            .await;
                    }
                }
            }
            let id = sender.id();
            tokio::spawn(follow_instance(
                *entry,
                sender,
                params.grep,
                params.resume_from,
            ));
            Ok(SubscriptionDto {
                subscription_id: id,
                stream: "logs".into(),
                last_seq: params.resume_from.unwrap_or(0),
            })
        }
        Target::File(path) => {
            let p = path.clone();
            let ((lines, total), len) = tokio::task::spawn_blocking(move || {
                let len = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                (tail_file(&p, tail.max(1)).unwrap_or_default(), len)
            })
            .await
            .unwrap_or_default();
            if params.resume_from.is_none() {
                for (seq, text) in lines.into_iter().rev().take(tail).rev() {
                    let _ = sender
                        .send_wait(
                            &LogEnvelope {
                                subscription_id: sender.id(),
                                seq,
                                line: line_dto(seq, text),
                            },
                            Duration::from_millis(200),
                        )
                        .await;
                }
            }
            let id = sender.id();
            tokio::spawn(follow_file(path, sender, params.grep, len, total));
            Ok(SubscriptionDto {
                subscription_id: id,
                stream: "logs".into(),
                last_seq: total,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tails_are_numbered_from_the_file_start() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.log");
        let text: String = (1..=10).map(|i| format!("line {i}\r\n")).collect();
        std::fs::write(&file, text).unwrap();
        let (lines, total) = tail_file(&file, 3).unwrap();
        assert_eq!(total, 10);
        assert_eq!(
            lines,
            [
                (8, "line 8".to_owned()),
                (9, "line 9".to_owned()),
                (10, "line 10".to_owned())
            ]
        );
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
