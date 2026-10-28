//! The `seq` domain of a log target (docs/03 §7.3, ADR-0046).
//!
//! A target's lines live in a *set* of files, and a line's `seq` is its absolute index in
//! that set. It is the same number for the tail backlog, the replay of a resumed
//! subscription and the live lines, it survives daemon restarts (it is derived from the
//! files) and it never goes back when files rotate:
//!
//! * [`LogSet::Rotated`]: `<file>` plus its rotated siblings `<file>.1` … `<file>.N`
//!   (`.N` is the oldest), numbered contiguously. The lines of the files that rotation
//!   deleted are counted in the sidecar `<file>.seq` (`{"dropped":…,"rotations":…}`), so
//!   the first retained line is `dropped + 1`. Whoever rotates the set calls
//!   [`rotating`]: `rotations` is odd while files are being renamed, and readers retry
//!   until it is even and unchanged across their read (a seqlock).
//! * [`LogSet::Daily`]: `<dir>/<prefix>.<YYYY-MM-DD>` (the daemon's own log, rotated
//!   and pruned by `tracing-appender`, which does not tell anyone). Here
//!   `seq = (day << 32) + line`, with `day` the days since 1970-01-01 of the file name:
//!   deleting an old day never renumbers the others. The values stay below 2^53, so
//!   JavaScript clients read them exactly.
//!
//! A line ends with `\n`; an unterminated tail is still being written and is not a line
//! yet. Everything here is blocking file I/O: call it from `spawn_blocking`.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Suffix of the sidecar of a [`LogSet::Rotated`] set: `default.out.log.seq`.
pub const SIDECAR_SUFFIX: &str = ".seq";

/// `seq` bits of the line number inside one day of a [`LogSet::Daily`] set.
pub const DAY_SHIFT: u32 = 32;

/// Snapshot attempts while a rotation is in progress (each waits [`RETRY_WAIT`]).
const ATTEMPTS: u32 = 100;
const RETRY_WAIT: Duration = Duration::from_millis(5);
/// Read buffer of the line scanners.
const CHUNK: usize = 256 * 1024;

/// The files of one log target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogSet {
    /// `<file>`, `<file>.1` … `<file>.N` and the sidecar `<file>.seq`.
    Rotated(PathBuf),
    /// `<dir>/<prefix>.<YYYY-MM-DD>`.
    Daily { dir: PathBuf, prefix: String },
}

/// Content of the sidecar of a [`LogSet::Rotated`] set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeqState {
    /// Lines that are no longer retained (deleted by rotation).
    pub dropped: u64,
    /// Rotations so far; odd while one is renaming files.
    pub rotations: u64,
}

/// One file of a set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub path: PathBuf,
    /// `seq` of its first line.
    pub first_seq: u64,
    /// Complete lines; `None` for the older files of a [`LogSet::Daily`] set (not
    /// needed to number them, counted on demand).
    pub lines: Option<u64>,
}

/// The state of a set at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Oldest first.
    pub segments: Vec<Segment>,
    /// `seq` of the oldest retained line (`last_seq + 1` when there is none).
    pub first_seq: u64,
    /// `seq` of the newest complete line (`0` when there never was one).
    pub last_seq: u64,
    /// Bytes of the newest segment up to the end of its last complete line.
    pub end: u64,
    /// [`SeqState::rotations`] when it was taken (`0` for [`LogSet::Daily`]).
    pub rotations: u64,
}

impl Snapshot {
    /// The newest file.
    pub fn newest(&self) -> Option<&Segment> {
        self.segments.last()
    }
}

/// `<file>.seq`.
pub fn sidecar_path(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(SIDECAR_SUFFIX);
    PathBuf::from(name)
}

/// The sidecar of `file` (default when missing or unreadable).
pub fn read_state(file: &Path) -> SeqState {
    std::fs::read(sidecar_path(file))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Replaces the sidecar of `file` (temporary file + rename).
pub fn write_state(file: &Path, state: SeqState) -> io::Result<()> {
    let target = sidecar_path(file);
    let mut tmp = target.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let text = serde_json::to_vec(&state).map_err(io::Error::other)?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &target)
}

/// Runs `op`, which renames/deletes the files of the [`LogSet::Rotated`] set of `file`
/// and adds to its `&mut u64` the lines it deleted, between the two halves of the
/// seqlock. The count is recorded even when `op` fails halfway. Sidecar write errors are
/// logged (readers then see a stale count until the next rotation).
pub fn rotating<T>(file: &Path, op: impl FnOnce(&mut u64) -> io::Result<T>) -> io::Result<T> {
    let mut state = read_state(file);
    // Odd while renaming (an odd value left by a crash moves to the next odd one).
    state.rotations += if state.rotations.is_multiple_of(2) {
        1
    } else {
        2
    };
    if let Err(err) = write_state(file, state) {
        tracing::warn!(path = %file.display(), %err, "cannot mark the log rotation");
    }
    let mut dropped = 0;
    let result = op(&mut dropped);
    state.dropped += dropped;
    state.rotations += 1;
    if let Err(err) = write_state(file, state) {
        tracing::warn!(path = %file.display(), %err, "cannot record the log rotation");
    }
    result
}

/// Ends an unterminated last line of `file` (left by a crash) so that it counts as a
/// line and the next one starts on its own; completes a rotation a crash interrupted.
/// Returns the last `seq` of the set: where a new writer continues.
pub(crate) fn prepare_append(file: &Path) -> io::Result<u64> {
    let state = read_state(file);
    if state.rotations % 2 == 1 {
        write_state(
            file,
            SeqState {
                rotations: state.rotations + 1,
                ..state
            },
        )?;
    }
    match OpenOptions::new().read(true).append(true).open(file) {
        Ok(mut handle) => {
            let len = handle.metadata()?.len();
            if len > 0 {
                let mut last = [0_u8; 1];
                handle.seek(SeekFrom::Start(len - 1))?;
                handle.read_exact(&mut last)?;
                if last[0] != b'\n' {
                    handle.write_all(b"\n")?;
                }
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    Ok(LogSet::Rotated(file.to_path_buf()).snapshot()?.last_seq)
}

/// Complete lines of `path` and the byte offset right after the last one.
pub fn count_lines(path: &Path) -> io::Result<(u64, u64)> {
    let mut file = File::open(path)?;
    let mut buf = vec![0_u8; CHUNK];
    let (mut lines, mut end, mut offset) = (0_u64, 0_u64, 0_u64);
    loop {
        let read = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        let chunk = &buf[..read];
        let found = chunk.iter().filter(|b| **b == b'\n').count() as u64;
        if found > 0 {
            lines += found;
            let last = chunk.iter().rposition(|b| *b == b'\n').unwrap_or(0);
            end = offset + last as u64 + 1;
        }
        offset += read as u64;
    }
    Ok((lines, end))
}

/// Calls `f(index, text)` for each complete line of `path` (index from 0; `\r` before
/// the `\n` removed; invalid UTF-8 replaced), reading at most `limit` bytes, until `f`
/// returns `false`.
fn scan(path: &Path, limit: Option<u64>, mut f: impl FnMut(u64, String) -> bool) -> io::Result<()> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(CHUNK, file.take(limit.unwrap_or(u64::MAX)));
    let mut line = Vec::new();
    let mut index = 0;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 || line.last() != Some(&b'\n') {
            return Ok(());
        }
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if !f(index, String::from_utf8_lossy(&line).into_owned()) {
            return Ok(());
        }
        index += 1;
    }
}

/// Complete lines of `bytes` (which starts at a line start) as `(text, bytes used)`.
pub fn split_complete(bytes: &[u8]) -> (Vec<String>, usize) {
    let mut lines = Vec::new();
    let mut start = 0;
    while let Some(pos) = bytes[start..].iter().position(|b| *b == b'\n') {
        let mut line = &bytes[start..start + pos];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        lines.push(String::from_utf8_lossy(line).into_owned());
        start += pos + 1;
    }
    (lines, start)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-10-27` → days since the epoch (dates before it are refused).
fn parse_day(text: &str) -> Option<u64> {
    let mut parts = text.splitn(3, '-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let (y, m, d): (i64, u32, u32) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    u64::try_from(days_from_civil(y, m, d)).ok()
}

impl LogSet {
    /// A [`LogSet::Rotated`] set.
    pub fn rotated(file: impl Into<PathBuf>) -> Self {
        Self::Rotated(file.into())
    }

    /// The file a writer appends to / the newest file name pattern, for messages.
    pub fn display(&self) -> String {
        match self {
            Self::Rotated(file) => file.display().to_string(),
            Self::Daily { dir, prefix } => dir.join(format!("{prefix}.*")).display().to_string(),
        }
    }

    /// `(index, path)` of the rotated siblings of `file`, oldest (highest index) first.
    pub(crate) fn siblings(file: &Path) -> io::Result<Vec<(u32, PathBuf)>> {
        let (Some(dir), Some(name)) = (file.parent(), file.file_name()) else {
            return Ok(Vec::new());
        };
        let prefix = format!("{}.", name.to_string_lossy());
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let mut out: Vec<(u32, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let index = name.to_str()?.strip_prefix(&prefix)?;
                if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                Some((index.parse().ok()?, entry.path()))
            })
            .collect();
        out.sort_by_key(|b| std::cmp::Reverse(b.0));
        Ok(out)
    }

    /// `(day, path)` of the files of a daily set, oldest first.
    fn days(dir: &Path, prefix: &str) -> io::Result<Vec<(u64, PathBuf)>> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let prefix = format!("{prefix}.");
        let mut out: Vec<(u64, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let day = parse_day(name.to_str()?.strip_prefix(&prefix)?)?;
                entry.path().is_file().then(|| (day, entry.path()))
            })
            .collect();
        out.sort();
        Ok(out)
    }

    /// Counts the set. Retries while a rotation is renaming files.
    pub fn snapshot(&self) -> io::Result<Snapshot> {
        match self {
            Self::Rotated(file) => {
                let mut attempt = 0;
                loop {
                    attempt += 1;
                    let before = read_state(file);
                    if before.rotations % 2 == 1 && attempt < ATTEMPTS {
                        std::thread::sleep(RETRY_WAIT);
                        continue;
                    }
                    let snapshot = Self::count_rotated(file, before);
                    let stable = read_state(file) == before;
                    match snapshot {
                        Ok(snapshot) if stable || attempt >= ATTEMPTS => return Ok(snapshot),
                        Err(err) if attempt >= ATTEMPTS => return Err(err),
                        _ => std::thread::sleep(RETRY_WAIT),
                    }
                }
            }
            Self::Daily { dir, prefix } => {
                let days = Self::days(dir, prefix)?;
                let mut segments: Vec<Segment> = days
                    .into_iter()
                    .map(|(day, path)| Segment {
                        path,
                        first_seq: (day << DAY_SHIFT) + 1,
                        lines: None,
                    })
                    .collect();
                let (mut last_seq, mut end) = (0, 0);
                if let Some(newest) = segments.last_mut() {
                    let (lines, bytes) = count_lines(&newest.path).or_else(|err| {
                        if err.kind() == io::ErrorKind::NotFound {
                            Ok((0, 0))
                        } else {
                            Err(err)
                        }
                    })?;
                    newest.lines = Some(lines);
                    last_seq = newest.first_seq + lines - 1;
                    end = bytes;
                }
                let first_seq = segments.first().map_or(last_seq + 1, |s| s.first_seq);
                Ok(Snapshot {
                    segments,
                    first_seq,
                    last_seq,
                    end,
                    rotations: 0,
                })
            }
        }
    }

    fn count_rotated(file: &Path, state: SeqState) -> io::Result<Snapshot> {
        let mut paths: Vec<PathBuf> = Self::siblings(file)?.into_iter().map(|(_, p)| p).collect();
        paths.push(file.to_path_buf());
        let mut segments = Vec::with_capacity(paths.len());
        let (mut next, mut end) = (state.dropped + 1, 0);
        for path in paths {
            match count_lines(&path) {
                Ok((lines, bytes)) => {
                    segments.push(Segment {
                        path,
                        first_seq: next,
                        lines: Some(lines),
                    });
                    next += lines;
                    end = bytes;
                }
                // Renamed or deleted meanwhile: the seqlock check catches it.
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        Ok(Snapshot {
            segments,
            first_seq: state.dropped + 1,
            last_seq: next - 1,
            end,
            rotations: state.rotations,
        })
    }

    /// Whether `snapshot` still describes the files (no rotation since).
    fn unchanged(&self, snapshot: &Snapshot) -> bool {
        match self {
            Self::Rotated(file) => read_state(file).rotations == snapshot.rotations,
            Self::Daily { .. } => true,
        }
    }

    /// Lines with `from <= seq <= to` (at most `max`, the oldest ones) as described by
    /// `snapshot`; `None` if the set rotated meanwhile (take a new snapshot). Lines whose
    /// file is gone are skipped (see [`LogSet::lost_before`]).
    pub fn read_in(
        &self,
        snapshot: &Snapshot,
        from: u64,
        to: u64,
        max: usize,
    ) -> io::Result<Option<Vec<(u64, String)>>> {
        let to = to.min(snapshot.last_seq);
        let mut out = Vec::new();
        let count = snapshot.segments.len();
        for (i, segment) in snapshot.segments.iter().enumerate() {
            if out.len() >= max || segment.first_seq > to {
                break;
            }
            let newest = i + 1 == count;
            let upper = if newest {
                snapshot.last_seq
            } else {
                snapshot.segments[i + 1].first_seq - 1
            };
            if upper < from {
                continue;
            }
            let limit = newest.then_some(snapshot.end);
            let scanned = scan(&segment.path, limit, |index, text| {
                let seq = segment.first_seq + index;
                if seq > to || out.len() >= max {
                    return false;
                }
                if seq >= from {
                    out.push((seq, text));
                }
                true
            });
            match scanned {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    if matches!(self, Self::Rotated(_)) {
                        return Ok(None);
                    }
                }
                Err(err) => return Err(err),
            }
        }
        Ok(self.unchanged(snapshot).then_some(out))
    }

    /// Lines with `from <= seq <= to` (at most `max`), with the snapshot they were read
    /// from (`to` is capped at its `last_seq`).
    pub fn range(
        &self,
        from: u64,
        to: u64,
        max: usize,
    ) -> io::Result<(Snapshot, Vec<(u64, String)>)> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let snapshot = self.snapshot()?;
            if let Some(lines) = self.read_in(&snapshot, from, to, max)? {
                return Ok((snapshot, lines));
            }
            if attempt >= ATTEMPTS {
                return Ok((snapshot, Vec::new()));
            }
        }
    }

    /// `seq` of the first of the last `n` lines at or below `upto` (which may be ahead of
    /// the files: a writer that has not flushed yet).
    pub fn tail_start(&self, snapshot: &Snapshot, n: u64, upto: u64) -> io::Result<u64> {
        if n == 0 {
            return Ok(upto + 1);
        }
        match self {
            Self::Rotated(_) => Ok((upto + 1).saturating_sub(n).max(snapshot.first_seq)),
            Self::Daily { .. } => {
                let mut need = n;
                let mut start = upto + 1;
                for segment in snapshot.segments.iter().rev() {
                    if segment.first_seq > upto {
                        continue;
                    }
                    let lines = match segment.lines {
                        Some(lines) => lines,
                        None => match count_lines(&segment.path) {
                            Ok((lines, _)) => lines,
                            Err(err) if err.kind() == io::ErrorKind::NotFound => 0,
                            Err(err) => return Err(err),
                        },
                    };
                    if lines == 0 {
                        continue;
                    }
                    let high = upto.min(segment.first_seq + lines - 1);
                    let available = high - segment.first_seq + 1;
                    if available >= need {
                        return Ok(high + 1 - need);
                    }
                    need -= available;
                    start = segment.first_seq;
                }
                Ok(start)
            }
        }
    }

    /// The last `n` lines (the snapshot they come from, the lines).
    pub fn tail(&self, n: usize) -> io::Result<(Snapshot, Vec<(u64, String)>)> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let snapshot = self.snapshot()?;
            let from = self.tail_start(&snapshot, n as u64, snapshot.last_seq)?;
            if let Some(lines) = self.read_in(&snapshot, from, snapshot.last_seq, n)? {
                return Ok((snapshot, lines));
            }
            if attempt >= ATTEMPTS {
                return Ok((snapshot, Vec::new()));
            }
        }
    }

    /// Lines with `from <= seq <= to` that `snapshot` retains (older daily files are
    /// counted on demand).
    pub fn count_range(&self, snapshot: &Snapshot, from: u64, to: u64) -> io::Result<u64> {
        let to = to.min(snapshot.last_seq);
        let mut total = 0;
        for segment in &snapshot.segments {
            if segment.first_seq > to {
                break;
            }
            let lines = match segment.lines {
                Some(lines) => lines,
                None => match count_lines(&segment.path) {
                    Ok((lines, _)) => lines,
                    Err(err) if err.kind() == io::ErrorKind::NotFound => 0,
                    Err(err) => return Err(err),
                },
            };
            if lines == 0 {
                continue;
            }
            let (low, high) = (
                from.max(segment.first_seq),
                to.min(segment.first_seq + lines - 1),
            );
            if high >= low {
                total += high - low + 1;
            }
        }
        Ok(total)
    }

    /// Lines with `seq >= from` that the set no longer has (deleted by rotation). For a
    /// [`LogSet::Daily`] set the count of a deleted day is unknown: at least one.
    pub fn lost_before(&self, snapshot: &Snapshot, from: u64) -> u64 {
        if from >= snapshot.first_seq {
            return 0;
        }
        match self {
            Self::Rotated(_) => snapshot.first_seq - from,
            Self::Daily { .. } => 1,
        }
    }
}

/// Incremental reader of a set for followers: [`Follower::poll`] returns the lines
/// appended since the last call, following rotations without repeating or skipping
/// lines and without ever going back in `seq`.
#[derive(Debug, Clone)]
pub struct Follower {
    set: LogSet,
    /// Newest file at the last poll and the offset after its last line read.
    path: Option<PathBuf>,
    pos: u64,
    /// `seq` (set numbering) of the next line.
    next: u64,
    rotations: u64,
    /// Added to the set numbering. Grows when the set shrank under us (a file truncated
    /// or deleted by someone else): the lines that follow keep increasing `seq`s.
    shift: u64,
}

/// What [`Follower::poll`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Polled {
    /// Lines the set lost before they could be read (reported as lagged first).
    pub lost: u64,
    pub lines: Vec<(u64, String)>,
}

impl Follower {
    /// Follows `set` from right after `snapshot`.
    pub fn new(set: LogSet, snapshot: &Snapshot) -> Self {
        Self {
            path: snapshot.newest().map(|s| s.path.clone()),
            pos: snapshot.end,
            next: snapshot.last_seq + 1,
            rotations: snapshot.rotations,
            shift: 0,
            set,
        }
    }

    /// `seq` the next line will have.
    pub fn next_seq(&self) -> u64 {
        self.next + self.shift
    }

    /// The newest file now, if it is still the one being read (fast path).
    fn same_newest(&self) -> io::Result<Option<(PathBuf, u64)>> {
        let newest = match &self.set {
            LogSet::Rotated(file) => {
                if read_state(file).rotations != self.rotations {
                    return Ok(None);
                }
                Some(file.clone())
            }
            LogSet::Daily { dir, prefix } => LogSet::days(dir, prefix)?.pop().map(|(_, p)| p),
        };
        let (Some(newest), Some(current)) = (newest, self.path.as_ref()) else {
            return Ok(None);
        };
        if &newest != current {
            return Ok(None);
        }
        match std::fs::metadata(&newest) {
            Ok(meta) if meta.len() >= self.pos => Ok(Some((newest, meta.len()))),
            Ok(_) => Ok(None),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// New lines since the last call.
    pub fn poll(&mut self) -> io::Result<Polled> {
        if let Some((path, len)) = self.same_newest()? {
            if len == self.pos {
                return Ok(Polled::default());
            }
            let mut file = File::open(&path)?;
            file.seek(SeekFrom::Start(self.pos))?;
            let mut bytes = Vec::new();
            file.take(len - self.pos).read_to_end(&mut bytes)?;
            if let LogSet::Rotated(file) = &self.set {
                if read_state(file).rotations != self.rotations {
                    // Rotated between the check and the read: `path` may be the new file.
                    return self.resync();
                }
            }
            let (texts, used) = split_complete(&bytes);
            self.pos += used as u64;
            let lines = texts
                .into_iter()
                .map(|text| {
                    let seq = self.next + self.shift;
                    self.next += 1;
                    (seq, text)
                })
                .collect();
            return Ok(Polled { lost: 0, lines });
        }
        self.resync()
    }

    /// The set changed shape (rotation, new day, truncation): continue from a snapshot.
    fn resync(&mut self) -> io::Result<Polled> {
        let snapshot = self.set.snapshot()?;
        let mut polled = Polled::default();
        if snapshot.last_seq + 1 >= self.next {
            // The numbering still covers our position: read what follows it.
            polled.lost = self.set.lost_before(&snapshot, self.next);
            let Some(lines) =
                self.set
                    .read_in(&snapshot, self.next, snapshot.last_seq, usize::MAX)?
            else {
                return Ok(Polled::default()); // rotated again: next poll
            };
            polled.lines = lines
                .into_iter()
                .map(|(seq, text)| (seq + self.shift, text))
                .collect();
        } else {
            // The set shrank under us (truncated, or files deleted by someone else): its
            // newest file is new content, numbered after what was already sent.
            let start = snapshot
                .newest()
                .map_or(snapshot.last_seq + 1, |s| s.first_seq);
            let shift = (self.next + self.shift) - start;
            let Some(lines) = self
                .set
                .read_in(&snapshot, start, snapshot.last_seq, usize::MAX)?
            else {
                return Ok(Polled::default());
            };
            tracing::debug!(set = %self.set.display(), "log set shrank; numbering continues");
            self.shift = shift;
            polled.lines = lines
                .into_iter()
                .map(|(seq, text)| (seq + shift, text))
                .collect();
        }
        self.path = snapshot.newest().map(|s| s.path.clone());
        self.pos = snapshot.end;
        self.next = snapshot.last_seq + 1;
        self.rotations = snapshot.rotations;
        Ok(polled)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::logs::{RotatingFile, Rotation};

    fn line(i: u64) -> Vec<u8> {
        format!("line {i:04} {}\n", "x".repeat(28)).into_bytes() // 39 bytes
    }

    /// `(seq, text)` → the number written in the text must be `seq - 1`.
    fn assert_numbered(lines: &[(u64, String)]) {
        for (seq, text) in lines {
            assert!(
                text.starts_with(&format!("line {:04}", seq - 1)),
                "seq {seq} is {text:?}"
            );
        }
    }

    #[test]
    fn days_are_counted_from_the_epoch() {
        assert_eq!(parse_day("1970-01-01"), Some(0));
        assert_eq!(parse_day("2000-03-01"), Some(11_017));
        assert_eq!(parse_day("2026-10-27"), Some(20_753));
        assert_eq!(parse_day("2026-13-01"), None);
        assert_eq!(parse_day("1969-12-31"), None);
        assert_eq!(parse_day("26-10-27"), None);
    }

    #[test]
    fn prepare_append_ends_a_line_left_by_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.out.log");
        assert_eq!(prepare_append(&file).unwrap(), 0);
        assert!(!file.exists(), "nothing created");
        std::fs::write(&file, "a\r\nb").unwrap();
        assert_eq!(prepare_append(&file).unwrap(), 2);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "a\r\nb\n");
        assert_eq!(prepare_append(&file).unwrap(), 2, "idempotent");
        // An interrupted rotation (odd counter) is closed.
        write_state(
            &file,
            SeqState {
                dropped: 7,
                rotations: 3,
            },
        )
        .unwrap();
        assert_eq!(prepare_append(&file).unwrap(), 9);
        assert_eq!(read_state(&file).rotations, 4);
    }

    #[test]
    fn unterminated_lines_are_not_lines_yet() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.log");
        std::fs::write(&file, "one\ntwo\nthr").unwrap();
        let set = LogSet::rotated(&file);
        let (snapshot, lines) = set.tail(10).unwrap();
        assert_eq!(
            lines,
            [(1, "one".to_owned()), (2, "two".to_owned())],
            "{snapshot:?}"
        );
        assert_eq!((snapshot.last_seq, snapshot.end), (2, 8));
    }

    #[test]
    fn rotated_sets_number_lines_across_files_and_deletions() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("svc.out.log");
        let rotation = Rotation {
            max_bytes: 100,
            max_files: 3,
        };
        let set = LogSet::rotated(&file);
        let mut writer = RotatingFile::open(&file, rotation, 0).unwrap();
        let mut seen_first = Vec::new();
        for i in 0..50 {
            writer.write_lines(&[line(i)]).unwrap();
            let snapshot = set.snapshot().unwrap();
            assert_eq!(snapshot.last_seq, i + 1);
            seen_first.push(snapshot.first_seq);
            let (_, tail) = set.tail(4).unwrap();
            assert_numbered(&tail);
            assert_eq!(tail.last().unwrap().0, i + 1);
        }
        assert!(
            seen_first.windows(2).all(|w| w[0] <= w[1]),
            "the oldest retained line only moves forward: {seen_first:?}"
        );
        // 2 lines per file, 3 files: lines 45..=50 are retained.
        let (snapshot, all) = set.range(1, 100, 1000).unwrap();
        assert_eq!(snapshot.first_seq, 45);
        assert_eq!(all.first().unwrap().0, 45);
        assert_numbered(&all);
        assert_eq!(set.lost_before(&snapshot, 40), 5);
        assert_eq!(set.lost_before(&snapshot, 45), 0);
        // A larger `max_files` earlier left `.5`: it is dropped (and counted) too.
        writer.close().unwrap();
        // It is the oldest file: its line comes right before the retained ones.
        std::fs::write(file.with_extension("log.5"), "stale\n").unwrap();
        let state = read_state(&file);
        write_state(
            &file,
            SeqState {
                dropped: state.dropped - 1,
                ..state
            },
        )
        .unwrap();
        assert_eq!(set.range(44, 44, 1).unwrap().1, [(44, "stale".to_owned())]);
        let mut writer = RotatingFile::open(&file, rotation, 50).unwrap();
        writer.write_lines(&[line(50), line(51), line(52)]).unwrap();
        writer.close().unwrap();
        assert!(!file.with_extension("log.5").exists());
        let (snapshot, tail) = set.tail(10).unwrap();
        assert_numbered(&tail);
        assert_eq!(snapshot.last_seq, 53);
    }

    #[test]
    fn followers_never_repeat_skip_or_go_back_across_rotations() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("svc.out.log");
        let rotation = Rotation {
            max_bytes: 200, // 5 lines per file
            max_files: 3,
        };
        let set = LogSet::rotated(&file);
        let mut writer = RotatingFile::open(&file, rotation, 0).unwrap();
        writer.write_lines(&[line(0), line(1)]).unwrap();
        let mut follower = Follower::new(set.clone(), &set.snapshot().unwrap());
        assert_eq!(follower.next_seq(), 3);
        let mut got: Vec<(u64, String)> = Vec::new();
        let mut lost = 0;
        let mut written = 2;
        // Bursts of 1..9 lines between polls: some cross one rotation, some two.
        for burst in [1_u64, 3, 7, 2, 9, 4, 1, 8, 5, 6, 2, 9] {
            for _ in 0..burst {
                writer.write_lines(&[line(written)]).unwrap();
                written += 1;
            }
            let polled = follower.poll().unwrap();
            lost += polled.lost;
            got.extend(polled.lines);
        }
        assert_numbered(&got);
        let seqs: Vec<u64> = got.iter().map(|(s, _)| *s).collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
        assert_eq!(seqs.first(), Some(&3));
        assert_eq!(*seqs.last().unwrap(), written);
        assert_eq!(
            seqs.len() as u64 + lost,
            written - 2,
            "every line read or counted"
        );

        // A long pause: three rotations happen, the oldest unread lines are deleted.
        for _ in 0..20 {
            writer.write_lines(&[line(written)]).unwrap();
            written += 1;
        }
        let polled = follower.poll().unwrap();
        assert!(polled.lost > 0, "{polled:?}");
        assert_numbered(&polled.lines);
        assert_eq!(polled.lines.last().unwrap().0, written);
        assert_eq!(
            polled.lost + polled.lines.len() as u64,
            20,
            "lost lines are counted exactly"
        );
    }

    #[test]
    fn a_set_truncated_by_someone_else_keeps_increasing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("error.log");
        std::fs::write(&file, "a\nb\nc\n").unwrap();
        let set = LogSet::rotated(&file);
        let mut follower = Follower::new(set.clone(), &set.snapshot().unwrap());
        std::fs::write(&file, "d\n").unwrap(); // truncated and rewritten
        let polled = follower.poll().unwrap();
        assert_eq!(polled.lines, [(4, "d".to_owned())]);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(b"e\n")
            .unwrap();
        assert_eq!(follower.poll().unwrap().lines, [(5, "e".to_owned())]);
        assert_eq!(follower.next_seq(), 6);
    }

    #[test]
    fn daily_sets_number_by_day_and_line() {
        let dir = tempfile::tempdir().unwrap();
        let set = LogSet::Daily {
            dir: dir.path().to_path_buf(),
            prefix: "forgeboxd.log".into(),
        };
        let day = |d: &str| dir.path().join(format!("forgeboxd.log.{d}"));
        std::fs::write(day("2026-10-26"), "a1\na2\na3\n").unwrap();
        std::fs::write(day("2026-10-27"), "b1\nb2\n").unwrap();
        std::fs::write(dir.path().join("forgeboxd.log.bogus"), "x\n").unwrap();
        let base = |d: u64| d << DAY_SHIFT;
        let (d26, d27) = (20_752, 20_753);
        let (snapshot, tail) = set.tail(4).unwrap();
        assert_eq!(snapshot.last_seq, base(d27) + 2);
        assert_eq!(
            tail,
            [
                (base(d26) + 2, "a2".to_owned()),
                (base(d26) + 3, "a3".to_owned()),
                (base(d27) + 1, "b1".to_owned()),
                (base(d27) + 2, "b2".to_owned()),
            ]
        );
        assert!(snapshot.last_seq < 1 << 53, "exact in JavaScript");

        // Follow into a new day; an old day pruned does not renumber the others.
        let mut follower = Follower::new(set.clone(), &snapshot);
        std::fs::OpenOptions::new()
            .append(true)
            .open(day("2026-10-27"))
            .unwrap()
            .write_all(b"b3\n")
            .unwrap();
        std::fs::write(day("2026-10-28"), "c1\n").unwrap();
        std::fs::remove_file(day("2026-10-26")).unwrap();
        let polled = follower.poll().unwrap();
        assert_eq!(
            polled.lines,
            [
                (base(d27) + 3, "b3".to_owned()),
                (base(d27 + 1) + 1, "c1".to_owned()),
            ]
        );
        let (snapshot, _) = set.tail(1).unwrap();
        assert_eq!(set.lost_before(&snapshot, base(d26) + 3), 1);
        assert_eq!(set.lost_before(&snapshot, base(d27) + 1), 0);
        let (_, replay) = set.range(base(d27) + 2, u64::MAX, 10).unwrap();
        assert_eq!(
            replay.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>(),
            ["b2", "b3", "c1"]
        );
    }
}
