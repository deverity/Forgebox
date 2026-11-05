//! Log tailer (M2-E5-T4, docs/14 section 3.2) against real files: writers in this process and
//! in another one, rotations, truncation, checkpoints and a 50 MiB backlog.
//!
//! The separate-process writer is this test binary started again on `child_process_entry`
//! (`FORGE_TAIL_CHILD` says what to do), so no fixture project has to be built. Like nginx, it
//! opens files with std's default share mode (`READ | WRITE | DELETE`) and appends.

use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{self, BufWriter, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use forge_metrics::tail::{self, ResumeReject, TailState};
use forge_metrics::{
    AccessFormat, AccessParser, Checkpoint, FieldMap, Multi, StartAt, StartOutcome, TailBatch,
    TailConfig, TailSpec, TailStatus, Tailer, TailerHandle, Unit, UpstreamMap,
};
use tokio::sync::{mpsc, watch};

const CHILD_ENV: &str = "FORGE_TAIL_CHILD";
const WAIT: Duration = Duration::from_secs(60);
const STOP: Duration = Duration::from_secs(10);
/// Time given to a late duplicate to show up before the final count.
const SETTLE: Duration = Duration::from_millis(300);

/// Fast ticks; the grace periods keep their defaults (a loaded machine must not make the
/// tailer let go of a rotated file before its last lines are written).
fn fast() -> TailConfig {
    TailConfig {
        tick: Duration::from_millis(10),
        idle_tick: Duration::from_millis(40),
        catch_up_tick: Duration::from_millis(2),
        ..TailConfig::default()
    }
}

fn spec(path: &Path, start: StartAt) -> TailSpec {
    TailSpec {
        key: Arc::from("test"),
        path: path.to_path_buf(),
        start,
    }
}

/// Test line `seq`: varying length, every third one ends with CRLF (Apache on Windows).
fn line(seq: u64) -> String {
    let pad: String = "abcdefghijklmnopqrstuvwxyz0123456789"
        .chars()
        .cycle()
        .skip((seq % 36) as usize)
        .take((seq % 7 * 23) as usize)
        .collect();
    let eol = if seq.is_multiple_of(3) { "\r\n" } else { "\n" };
    format!("L{seq:06} {pad}{eol}")
}

/// Decoder of the test lines: the sequence number, only when the whole line is intact.
fn decode(raw: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(raw).ok()?;
    let seq: u64 = text.get(1..7)?.parse().ok()?;
    (line(seq).trim_end_matches(['\r', '\n']) == text).then_some(seq)
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().append(true).create(true).open(path)
}

/// Creates the file, retrying while the name is still taken (legacy delete semantics keep a
/// deleted name until the last handle, the tailer's, is closed).
fn create_retrying(path: &Path) -> io::Result<File> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match open_append(path) {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            other => return other,
        }
    }
}

fn write_lines(file: &mut File, seqs: Range<u64>) -> io::Result<()> {
    for seq in seqs {
        file.write_all(line(seq).as_bytes())?;
    }
    Ok(())
}

/// Writes a line in two halves with a pause between them: the tailer sees a partial line.
fn write_split(file: &mut File, seq: u64, pause: Duration) -> io::Result<()> {
    let text = line(seq);
    let (a, b) = text.as_bytes().split_at(text.len() / 2);
    file.write_all(a)?;
    std::thread::sleep(pause);
    file.write_all(b)
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Lines written by [`writer_script`].
const SCRIPT_TOTAL: u64 = 450;
/// Lines the tailer must have delivered before each `sync` point of the script returns.
const SYNC_POINTS: [u64; 3] = [0, 100, 210];

/// The writer of the exactly-once tests, run in a thread or in another process. Every way a
/// web server or a rotation tool changes the file, in sequence. `sync(i)` returns once the
/// tailer delivered `SYNC_POINTS[i]` lines.
fn writer_script(path: &Path, sync: &dyn Fn(usize)) -> io::Result<()> {
    let pause = Duration::from_millis(25);
    sync(0);

    // Late creation (the tailer started first) and partial lines.
    let mut w1 = open_append(path)?;
    for seq in 0..100u64 {
        if seq.is_multiple_of(10) {
            write_split(&mut w1, seq, pause)?;
        } else {
            write_lines(&mut w1, seq..seq + 1)?;
        }
    }
    sync(1);

    // nginx rotation: rename while the tailer and the writer hold the file; the writer keeps
    // writing to the renamed file until it reopens; one worker writes after the reopen.
    fs::rename(path, sibling(path, ".1"))?;
    write_lines(&mut w1, 100..150)?;
    let mut w2 = open_append(path)?;
    write_lines(&mut w2, 150..200)?;
    write_lines(&mut w1, 200..210)?;
    drop(w1);
    sync(2);

    // copytruncate: truncated in place while the writer keeps appending; the new content
    // quickly grows past the old read position.
    OpenOptions::new().write(true).open(path)?.set_len(0)?;
    write_lines(&mut w2, 210..310)?;

    // Delete and recreate; lines go to the deleted file while the writer still holds it.
    write_lines(&mut w2, 310..330)?;
    fs::remove_file(path)?;
    write_lines(&mut w2, 330..350)?;
    drop(w2);
    let mut w3 = create_retrying(path)?;
    write_lines(&mut w3, 350..SCRIPT_TOTAL)?;
    Ok(())
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + WAIT;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Entry point of the separate-process writer (module docs); does nothing in a normal run.
#[test]
fn child_process_entry() {
    let Ok(job) = std::env::var(CHILD_ENV) else {
        return;
    };
    let mut parts = job.splitn(3, '|');
    let mode = parts.next().unwrap().to_owned();
    let path = PathBuf::from(parts.next().unwrap());
    let sync = PathBuf::from(parts.next().unwrap());
    match mode.as_str() {
        "script" => writer_script(&path, &|i| wait_for_file(&sync.join(format!("go-{i}"))))
            .expect("writer script"),
        "rename" => fs::rename(&path, sibling(&path, ".1")).expect("rename by another process"),
        "delete" => fs::remove_file(&path).expect("delete by another process"),
        other => panic!("unknown child mode {other}"),
    }
}

fn spawn_child(mode: &str, path: &Path, sync: &Path) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "child_process_entry",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(
            CHILD_ENV,
            format!("{mode}|{}|{}", path.display(), sync.display()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning the child writer")
}

async fn wait_child(child: std::process::Child) -> Output {
    tokio::task::spawn_blocking(move || child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}

fn assert_child_ok(output: &Output) {
    assert!(
        output.status.success(),
        "child failed ({:?})\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// What the consumer received.
#[derive(Default)]
struct Seen {
    seqs: Mutex<Vec<u64>>,
    count: AtomicU64,
    lines: AtomicU64,
    rejected: AtomicU64,
    scaled: AtomicU64,
    sampled: AtomicU64,
    /// While set, the consumer does not read (a stalled aggregator).
    paused: AtomicBool,
}

impl Seen {
    fn count(&self) -> u64 {
        self.count.load(Ordering::SeqCst)
    }

    fn diagnostics(&self) -> String {
        format!(
            "items={} lines={} rejected={}",
            self.count(),
            self.lines.load(Ordering::SeqCst),
            self.rejected.load(Ordering::SeqCst)
        )
    }

    async fn wait(&self, n: u64, status: &watch::Receiver<TailStatus>) {
        let deadline = Instant::now() + WAIT;
        while self.count() < n {
            assert!(
                Instant::now() < deadline,
                "waited for {n} lines: {}; tailer {:?}",
                self.diagnostics(),
                *status.borrow()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn wait_blocking(&self, n: u64) {
        let deadline = Instant::now() + WAIT;
        while self.count() < n {
            assert!(
                Instant::now() < deadline,
                "waited for {n} lines: {}",
                self.diagnostics()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Every sequence number of `range` exactly once, nothing else, nothing rejected.
    fn assert_exactly(&self, range: Range<u64>) {
        let mut seqs = self.seqs.lock().unwrap().clone();
        seqs.sort_unstable();
        let mut duplicates: Vec<u64> = seqs
            .windows(2)
            .filter(|w| w[0] == w[1])
            .map(|w| w[0])
            .collect();
        duplicates.dedup();
        let missing: Vec<u64> = range
            .clone()
            .filter(|s| seqs.binary_search(s).is_err())
            .collect();
        let outside: Vec<u64> = seqs
            .iter()
            .copied()
            .filter(|s| !range.contains(s))
            .collect();
        assert!(
            duplicates.is_empty() && missing.is_empty() && outside.is_empty(),
            "duplicates {duplicates:?}, missing {missing:?}, unexpected {outside:?} ({})",
            self.diagnostics()
        );
        assert_eq!(self.rejected.load(Ordering::SeqCst), 0, "rejected lines");
        assert_eq!(self.lines.load(Ordering::SeqCst), range.end - range.start);
    }
}

/// Consumes batches on its own task.
fn collect(rx: mpsc::Receiver<TailBatch<u64>>) -> Arc<Seen> {
    let seen = Arc::new(Seen::default());
    collect_into(rx, seen.clone());
    seen
}

fn collect_into(mut rx: mpsc::Receiver<TailBatch<u64>>, sink: Arc<Seen>) {
    tokio::spawn(async move {
        loop {
            while sink.paused.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let Some(batch) = rx.recv().await else { break };
            sink.lines.fetch_add(batch.lines, Ordering::SeqCst);
            sink.rejected.fetch_add(batch.rejected, Ordering::SeqCst);
            sink.scaled.fetch_add(batch.scaled(), Ordering::SeqCst);
            if batch.sampled() {
                sink.sampled.fetch_add(1, Ordering::SeqCst);
            }
            let n = batch.items.len() as u64;
            sink.seqs.lock().unwrap().extend(batch.items);
            sink.count.fetch_add(n, Ordering::SeqCst);
        }
    });
}

fn start(path: &Path, at: StartAt, config: TailConfig) -> (TailerHandle, Arc<Seen>) {
    let (tx, rx) = mpsc::channel(64);
    let handle = tail::spawn(spec(path, at), config, decode, tx);
    (handle, collect(rx))
}

/// Waits until the status matches. The `Ref` of `wait_for` is a read guard of the channel:
/// it is dropped at once, or the tailer's next status update would wait for it.
async fn wait_status(
    rx: &mut watch::Receiver<TailStatus>,
    f: impl Fn(&TailStatus) -> bool,
) -> bool {
    matches!(
        tokio::time::timeout(WAIT, rx.wait_for(|s| f(s))).await,
        Ok(Ok(_))
    )
}

async fn wait_state(handle: &TailerHandle, what: &str, f: impl Fn(&TailStatus) -> bool) {
    let found = wait_status(&mut handle.watch(), f).await;
    assert!(found, "{what}: tailer {:?}", handle.status());
}

async fn finish(handle: TailerHandle) -> TailStatus {
    tokio::time::sleep(SETTLE).await;
    let status = handle.stop(STOP).await;
    assert_eq!(status.state, TailState::Stopped, "{status:?}");
    status
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_process_writer_every_line_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.log");
    let (handle, seen) = start(&path, StartAt::End, fast());
    let watch = handle.watch();
    wait_state(&handle, "waiting for the file", |s| {
        s.state == TailState::Waiting
    })
    .await;

    let (s, p) = (seen.clone(), path.clone());
    tokio::task::spawn_blocking(move || writer_script(&p, &|i| s.wait_blocking(SYNC_POINTS[i])))
        .await
        .unwrap()
        .unwrap();
    seen.wait(SCRIPT_TOTAL, &watch).await;
    let status = finish(handle).await;

    seen.assert_exactly(0..SCRIPT_TOTAL);
    assert_eq!(status.start, StartOutcome::Missing);
    assert!(status.totals.rotations >= 2, "{status:?}");
    assert!(status.totals.truncations >= 1, "{status:?}");
    assert_eq!(status.totals.sampled_batches, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn separate_process_writer_every_line_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.log");
    let sync = dir.path().join("sync");
    fs::create_dir(&sync).unwrap();
    let (handle, seen) = start(&path, StartAt::End, fast());
    let watch = handle.watch();
    wait_state(&handle, "waiting for the file", |s| {
        s.state == TailState::Waiting
    })
    .await;

    let child = spawn_child("script", &path, &sync);
    for (i, n) in SYNC_POINTS.iter().enumerate() {
        seen.wait(*n, &watch).await;
        fs::write(sync.join(format!("go-{i}")), b"").unwrap();
    }
    let output = wait_child(child).await;
    assert_child_ok(&output);
    seen.wait(SCRIPT_TOTAL, &watch).await;
    let status = finish(handle).await;

    seen.assert_exactly(0..SCRIPT_TOTAL);
    assert_eq!(status.start, StartOutcome::Missing);
    assert!(status.totals.rotations >= 2, "{status:?}");
    assert!(status.totals.truncations >= 1, "{status:?}");
}

/// A rename (and a delete) by another process succeeds while the tailer holds the file open:
/// std's share mode includes DELETE. The tailer then follows the new file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_and_delete_by_another_process_succeed_while_tailed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.log");
    write_lines(&mut open_append(&path).unwrap(), 0..10).unwrap();
    let (handle, seen) = start(&path, StartAt::Beginning, fast());
    let watch = handle.watch();
    seen.wait(10, &watch).await;
    assert_eq!(handle.status().state, TailState::Following);

    let output = wait_child(spawn_child("rename", &path, dir.path())).await;
    assert_child_ok(&output);
    assert!(!path.exists() && sibling(&path, ".1").exists());

    write_lines(&mut open_append(&path).unwrap(), 10..20).unwrap();
    seen.wait(20, &watch).await;

    let output = wait_child(spawn_child("delete", &path, dir.path())).await;
    assert_child_ok(&output);
    assert!(!path.exists());

    write_lines(&mut create_retrying(&path).unwrap(), 20..30).unwrap();
    seen.wait(30, &watch).await;
    let status = finish(handle).await;

    seen.assert_exactly(0..30);
    assert!(status.totals.rotations >= 2, "{status:?}");
}

/// nginx-style rotations (rename, stragglers to the old file, reopen) every 700 ms while a
/// writer appends as fast as it can, with no synchronization with the tailer. Sampling is off:
/// the point is that no line is lost or read twice across the switches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rotations_under_continuous_writes_lose_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.log");
    let config = TailConfig {
        sample_backlog: u64::MAX,
        max_sample: 1,
        ..fast()
    };
    let (handle, seen) = start(&path, StartAt::Beginning, config);
    let watch = handle.watch();
    wait_state(&handle, "waiting for the file", |s| {
        s.state == TailState::Waiting
    })
    .await;

    let p = path.clone();
    let total = tokio::task::spawn_blocking(move || -> io::Result<u64> {
        let rotations = 6;
        let mut seq = 0;
        let mut file = open_append(&p)?;
        for n in 1..=rotations + 1 {
            let until = Instant::now() + Duration::from_millis(700);
            while Instant::now() < until {
                write_lines(&mut file, seq..seq + 50)?;
                seq += 50;
                std::thread::sleep(Duration::from_millis(2));
            }
            if n > rotations {
                break;
            }
            fs::rename(&p, sibling(&p, &format!(".{n}")))?;
            write_lines(&mut file, seq..seq + 20)?;
            seq += 20;
            let old = std::mem::replace(&mut file, open_append(&p)?);
            write_lines(&mut file, seq..seq + 20)?;
            seq += 20;
            let mut old = old;
            write_lines(&mut old, seq..seq + 5)?;
            seq += 5;
        }
        Ok(seq)
    })
    .await
    .unwrap()
    .unwrap();

    seen.wait(total, &watch).await;
    let status = finish(handle).await;
    seen.assert_exactly(0..total);
    assert_eq!(status.totals.rotations, 6, "{status:?}");
    eprintln!(
        "{total} lines across 6 rotations, {} ticks",
        status.totals.ticks
    );
}

/// Stops a tailer and returns the checkpoint of what it delivered.
async fn delivered(handle: TailerHandle) -> Checkpoint {
    let status = finish(handle).await;
    status.delivered.expect("a delivered checkpoint")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_resume_delivers_exactly_the_appended_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.log");
    write_lines(&mut open_append(&path).unwrap(), 0..500).unwrap();

    let (first, seen) = start(&path, StartAt::Beginning, fast());
    seen.wait(500, &first.watch()).await;
    let checkpoint = delivered(first).await;
    seen.assert_exactly(0..500);
    assert_eq!(checkpoint.offset, fs::metadata(&path).unwrap().len());
    assert_eq!(checkpoint.head_len, 256);

    let before = fs::metadata(&path).unwrap().len();
    write_lines(&mut open_append(&path).unwrap(), 500..1500).unwrap();
    let appended = fs::metadata(&path).unwrap().len() - before;

    let (second, seen) = start(&path, StartAt::Checkpoint(checkpoint), fast());
    seen.wait(1000, &second.watch()).await;
    let status = finish(second).await;
    seen.assert_exactly(500..1500);
    assert_eq!(status.start, StartOutcome::Resumed { lag: appended });
}

/// A line half written when the tailer stopped is read whole after the resume.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_resume_completes_a_partial_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.log");
    let mut writer = open_append(&path).unwrap();
    write_lines(&mut writer, 0..10).unwrap();
    let ten = line(10);
    let (a, b) = ten.as_bytes().split_at(5);
    writer.write_all(a).unwrap();

    let (first, seen) = start(&path, StartAt::Beginning, fast());
    seen.wait(10, &first.watch()).await;
    let checkpoint = delivered(first).await;
    seen.assert_exactly(0..10);
    assert_eq!(checkpoint.offset, fs::metadata(&path).unwrap().len() - 5);

    writer.write_all(b).unwrap();
    write_lines(&mut writer, 11..20).unwrap();
    let (second, seen) = start(&path, StartAt::Checkpoint(checkpoint), fast());
    seen.wait(10, &second.watch()).await;
    finish(second).await;
    seen.assert_exactly(10..20);
}

#[cfg(windows)]
fn creation_time(path: &Path) -> u64 {
    std::os::windows::fs::MetadataExt::creation_time(&fs::metadata(path).unwrap())
}

/// Checkpoints of another file never resume: creation time, or (same creation time, NTFS
/// tunneling) the first bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_of_another_file_starts_at_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.log");
    write_lines(&mut open_append(&path).unwrap(), 0..100).unwrap();
    let (first, seen) = start(&path, StartAt::Beginning, fast());
    seen.wait(100, &first.watch()).await;
    let checkpoint = delivered(first).await;

    // Another creation time.
    let other = Checkpoint {
        creation_time: checkpoint.creation_time + 1,
        ..checkpoint
    };
    let (second, seen) = start(&path, StartAt::Checkpoint(other), fast());
    wait_state(&second, "started", |s| s.state == TailState::Following).await;
    assert_eq!(
        second.status().start,
        StartOutcome::Rejected(ResumeReject::CreationTime)
    );
    write_lines(&mut open_append(&path).unwrap(), 100..103).unwrap();
    seen.wait(3, &second.watch()).await;
    finish(second).await;
    seen.assert_exactly(100..103);

    // Rotated while "the daemon was down": a new, longer file under the same name. NTFS gives
    // it the creation time of the old one; only the first bytes tell them apart.
    fs::rename(&path, sibling(&path, ".1")).unwrap();
    write_lines(&mut open_append(&path).unwrap(), 1000..1400).unwrap();
    assert!(fs::metadata(&path).unwrap().len() > checkpoint.offset);
    #[cfg(windows)]
    let tunneled = creation_time(&path) == checkpoint.creation_time;
    #[cfg(not(windows))]
    let tunneled = false;
    let (third, seen) = start(&path, StartAt::Checkpoint(checkpoint), fast());
    wait_state(&third, "started", |s| s.state == TailState::Following).await;
    let expected = if tunneled {
        ResumeReject::Content
    } else {
        ResumeReject::CreationTime
    };
    eprintln!("creation time tunneled to the new file: {tunneled}");
    assert_eq!(third.status().start, StartOutcome::Rejected(expected));
    write_lines(&mut open_append(&path).unwrap(), 1400..1403).unwrap();
    seen.wait(3, &third.watch()).await;
    finish(third).await;
    seen.assert_exactly(1400..1403);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_too_far_behind_starts_at_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.log");
    write_lines(&mut open_append(&path).unwrap(), 0..10).unwrap();
    let (first, seen) = start(&path, StartAt::Beginning, fast());
    seen.wait(10, &first.watch()).await;
    let checkpoint = delivered(first).await;

    write_lines(&mut open_append(&path).unwrap(), 10..1010).unwrap();
    let lag = fs::metadata(&path).unwrap().len() - checkpoint.offset;
    let config = TailConfig {
        resume_max_lag: lag - 1,
        ..fast()
    };
    let (second, seen) = start(&path, StartAt::Checkpoint(checkpoint), config);
    wait_state(&second, "started", |s| s.state == TailState::Following).await;
    assert_eq!(
        second.status().start,
        StartOutcome::Rejected(ResumeReject::Lag { lag })
    );
    write_lines(&mut open_append(&path).unwrap(), 1010..1012).unwrap();
    seen.wait(2, &second.watch()).await;
    finish(second).await;
    seen.assert_exactly(1010..1012);
}

/// A deleted file that nobody recreates is let go once drained, and a file created later is
/// read from its first byte.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_file_is_released_and_its_successor_read_from_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.log");
    write_lines(&mut open_append(&path).unwrap(), 0..10).unwrap();
    let config = TailConfig {
        orphan_grace: Duration::from_millis(200),
        ..fast()
    };
    let (handle, seen) = start(&path, StartAt::Beginning, config);
    seen.wait(10, &handle.watch()).await;
    fs::remove_file(&path).unwrap();
    wait_state(&handle, "orphaned", |s| s.state == TailState::Orphaned).await;
    wait_state(&handle, "released", |s| s.state == TailState::Waiting).await;

    write_lines(&mut create_retrying(&path).unwrap(), 10..20).unwrap();
    seen.wait(20, &handle.watch()).await;
    finish(handle).await;
    seen.assert_exactly(0..20);
}

/// A consumer that stops reading never blocks the tailer: it defers, keeps the unread data in
/// the file, samples once it reads again and loses no line count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_channel_never_blocks_the_tailer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.log");
    let total = 20_000;
    write_lines(&mut open_append(&path).unwrap(), 0..total).unwrap();

    let (tx, rx) = mpsc::channel(1);
    let seen = Arc::new(Seen::default());
    seen.paused.store(true, Ordering::SeqCst);
    let config = TailConfig {
        max_read: 64 * 1024,
        ..fast()
    };
    let handle = tail::spawn(spec(&path, StartAt::Beginning), config, decode, tx);
    collect_into(rx, seen.clone());
    let mut watch = handle.watch();

    // Stalled consumer: the tailer keeps ticking (deferring), never waiting on the channel.
    let stalled = wait_status(&mut watch, |s| s.totals.deferred_ticks >= 20).await;
    assert!(stalled, "{:?}", handle.status());
    let deferred = handle.status();
    assert!(deferred.totals.batches <= 2, "{deferred:?}");

    seen.paused.store(false, Ordering::SeqCst);
    let deadline = Instant::now() + WAIT;
    while seen.lines.load(Ordering::SeqCst) < total {
        assert!(
            Instant::now() < deadline,
            "{} {:?}",
            seen.diagnostics(),
            handle.status()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let status = finish(handle).await;

    assert_eq!(seen.lines.load(Ordering::SeqCst), total);
    assert_eq!(seen.rejected.load(Ordering::SeqCst), 0);
    assert!(seen.sampled.load(Ordering::SeqCst) > 0, "{status:?}");
    let mut seqs = seen.seqs.lock().unwrap().clone();
    let decoded = seqs.len();
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(seqs.len(), decoded, "sampling never duplicates a line");
    let scaled = seen.scaled.load(Ordering::SeqCst) as f64;
    let error = (scaled - total as f64).abs() / total as f64;
    assert!(error <= 0.02, "scaled {scaled} vs {total}");
}

/// Dropping the handle stops the task; `stop` returns the final status.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_handle_stops_the_tailer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.log");
    let (tx, mut rx) = mpsc::channel::<TailBatch<u64>>(4);
    let handle = tail::spawn(spec(&path, StartAt::End), fast(), decode, tx);
    let mut watch = handle.watch();
    wait_state(&handle, "waiting", |s| s.state == TailState::Waiting).await;
    drop(handle);
    assert!(wait_status(&mut watch, |s| s.state == TailState::Stopped).await);
    // The task ended, so its sender is gone.
    assert!(
        tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .is_none()
    );
}

// --- 50 MiB backlog ---------------------------------------------------------------------

fn nginx() -> AccessFormat {
    AccessFormat {
        ts: FieldMap::new("/t", Unit::Seconds),
        ts_is_start: false,
        host: "/host".into(),
        status: "/s".into(),
        duration: FieldMap::new("/rt", Unit::Seconds),
        upstream_duration: Some(UpstreamMap {
            field: FieldMap::new("/ut", Unit::Seconds),
            multi: Multi::Sum,
        }),
        bytes: Some("/b".into()),
        method: Some("/m".into()),
        uri: Some("/u".into()),
        lenient_escapes: false,
    }
}

/// A line of the nginx `log_format forgebox` (docs/14 section 2).
fn nginx_line(i: u64) -> String {
    let status = [200, 200, 200, 304, 404, 500][(i % 6) as usize];
    format!(
        "{{\"t\":\"{}.{:03}\",\"host\":\"site{}.test\",\"s\":{status},\"rt\":0.{:03},\"ut\":\"0.{:03}\",\"b\":{},\"m\":\"GET\",\"u\":\"/api/items/{i}\",\"up\":\"127.0.0.1:18082\"}}\n",
        1_793_167_380 + i / 1000,
        i % 1000,
        i % 5,
        i % 997,
        i % 991,
        i % 4096,
    )
}

/// Writes at least `bytes` of nginx lines; returns the number of lines.
fn write_backlog(path: &Path, bytes: u64) -> u64 {
    let mut out = BufWriter::with_capacity(1 << 20, File::create(path).unwrap());
    let (mut written, mut lines) = (0u64, 0u64);
    while written < bytes {
        let text = nginx_line(lines);
        out.write_all(text.as_bytes()).unwrap();
        written += text.len() as u64;
        lines += 1;
    }
    out.flush().unwrap();
    lines
}

/// Longest single `poll` of the wrapped future: how long it held a runtime thread.
struct MaxPoll<F> {
    inner: Pin<Box<F>>,
    max_ns: Arc<AtomicU64>,
    polls: Arc<AtomicU64>,
}

impl<F: Future> Future for MaxPoll<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let start = Instant::now();
        let out = self.inner.as_mut().poll(cx);
        let ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.max_ns.fetch_max(ns, Ordering::Relaxed);
        self.polls.fetch_add(1, Ordering::Relaxed);
        out
    }
}

/// docs/14 section 3.2 acceptance: 50 MiB already in the file, production settings, a
/// single-threaded runtime (anything blocking in the tailer would hold it).
#[test]
fn backlog_of_50_mib_is_sampled_without_holding_the_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.log");
    let total = write_backlog(&path, 50 * 1024 * 1024);
    let size = fs::metadata(&path).unwrap().len();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let max_poll = Arc::new(AtomicU64::new(0));
    let polls = Arc::new(AtomicU64::new(0));
    let probe_max = Arc::new(AtomicU64::new(0));
    let (lines, scaled, rejected, sampled, max_weight, elapsed, status) = runtime.block_on(async {
        let (tx, mut rx) = mpsc::channel(16);
        let parser = AccessParser::new(&nginx()).unwrap();
        let (tailer, control) = Tailer::new(
            spec(&path, StartAt::Beginning),
            TailConfig::default(),
            parser,
            tx,
        );
        let task = tokio::spawn(MaxPoll {
            inner: Box::pin(tailer.run()),
            max_ns: max_poll.clone(),
            polls: polls.clone(),
        });
        // Timer lateness on the same (only) runtime thread.
        let probing = Arc::new(AtomicBool::new(true));
        let probe = {
            let (probing, probe_max) = (probing.clone(), probe_max.clone());
            tokio::spawn(async move {
                let period = Duration::from_millis(2);
                while probing.load(Ordering::Relaxed) {
                    let t = Instant::now();
                    tokio::time::sleep(period).await;
                    let late = t.elapsed().saturating_sub(period);
                    probe_max.fetch_max(late.as_nanos() as u64, Ordering::Relaxed);
                }
            })
        };

        let started = Instant::now();
        let (mut lines, mut scaled, mut rejected, mut sampled, mut max_weight) = (0, 0, 0, 0, 1);
        while lines < total {
            let batch = tokio::time::timeout(WAIT, rx.recv())
                .await
                .unwrap_or_else(|_| {
                    panic!("no progress at {lines}/{total}: {:?}", control.status())
                })
                .expect("tailer alive");
            lines += batch.lines;
            scaled += batch.scaled();
            rejected += batch.rejected;
            if batch.sampled() {
                sampled += 1;
            }
            max_weight = max_weight.max(batch.weight);
        }
        let elapsed = started.elapsed();
        control.request_stop();
        let status = task.await.unwrap();
        probing.store(false, Ordering::Relaxed);
        probe.await.unwrap();
        (
            lines, scaled, rejected, sampled, max_weight, elapsed, status,
        )
    });

    let max_poll = Duration::from_nanos(max_poll.load(Ordering::Relaxed));
    let probe_late = Duration::from_nanos(probe_max.load(Ordering::Relaxed));
    let error = (scaled as f64 - total as f64).abs() / total as f64;
    eprintln!(
        "backlog: {size} bytes, {total} lines in {elapsed:?}; {} ticks, {} polls, longest poll \
         {max_poll:?}, timer probe max lateness {probe_late:?}; {sampled} sampled batches \
         (max k {max_weight}), scaled count {scaled} ({:.3} % off)",
        status.totals.ticks,
        polls.load(Ordering::Relaxed),
        error * 100.0
    );
    assert!(
        max_poll < Duration::from_millis(50),
        "a poll held the runtime {max_poll:?}"
    );
    assert!(sampled > 0, "a 50 MiB backlog is sampled");
    assert_eq!(lines, total, "every line is counted");
    assert_eq!(rejected, 0);
    assert!(error <= 0.02, "scaled {scaled} vs {total}");
    assert_eq!(status.state, TailState::Stopped);
    assert_eq!(status.totals.bytes, size);
}
