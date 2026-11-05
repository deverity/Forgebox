//! Rotation of an access log by the daemon's logic (`forge_metrics::rotate`, M2-E5-T12,
//! docs/14 section 3.8) against a fake server and the real tailer.
//!
//! The fake server behaves like nginx: it holds the log open (std's default share mode, as
//! nginx and Apache do), appends 2,000 numbered lines per second and, when it is told to
//! reopen, closes the renamed file and creates a new one a little later (until then it keeps
//! writing to the renamed file, like the workers that have not seen the signal yet). The
//! rotator renames at `max_size` and asks for the reopen; the tailer must deliver every
//! number exactly once, in order, across three or more rotations.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use forge_metrics::rotate::{self, Check, RotatePolicy, Rotator, Strategy, rotated_name};
use forge_metrics::{StartAt, TailBatch, TailConfig, TailSpec, tail};
use tokio::sync::mpsc;

const RATE: u64 = 2_000;
const TOTAL: u64 = 9_000;
/// 128 KiB: a rotation about every 1,000 lines (0.5 s).
const MAX_SIZE: u64 = 128 * 1024;

fn line(seq: u64) -> String {
    // ~130 bytes, like a real nginx access line.
    format!(
        "{{\"t\":\"1761652800.{:03}\",\"host\":\"loja.test\",\"s\":200,\"rt\":0.002,\"seq\":{seq:08},\"pad\":\"{}\"}}\n",
        seq % 1000,
        "x".repeat(60)
    )
}

fn decode(raw: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(raw).ok()?;
    let at = text.find("\"seq\":")? + 6;
    let seq: u64 = text.get(at..at + 8)?.parse().ok()?;
    (line(seq).trim_end() == text).then_some(seq)
}

fn open_append(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().append(true).create(true).open(path)
}

struct Fake {
    reopen: Arc<AtomicBool>,
    written: Arc<AtomicU64>,
    thread: std::thread::JoinHandle<()>,
}

/// The fake server: `TOTAL` lines at `RATE` per second.
fn fake_server(path: PathBuf) -> Fake {
    let reopen = Arc::new(AtomicBool::new(false));
    let written = Arc::new(AtomicU64::new(0));
    let (r, w) = (Arc::clone(&reopen), Arc::clone(&written));
    let thread = std::thread::spawn(move || {
        let mut file = open_append(&path).unwrap();
        let started = Instant::now();
        let mut seq = 0u64;
        let mut reopen_at: Option<Instant> = None;
        while seq < TOTAL {
            // Lines due by now, in small bursts like concurrent workers.
            let due = (started.elapsed().as_millis() as u64 * RATE / 1000).min(TOTAL);
            while seq < due {
                file.write_all(line(seq).as_bytes()).unwrap();
                seq += 1;
            }
            w.store(seq, Ordering::SeqCst);
            // The signal reaches the workers 40 ms later.
            if r.load(Ordering::SeqCst) {
                let at =
                    *reopen_at.get_or_insert_with(|| Instant::now() + Duration::from_millis(40));
                if Instant::now() >= at {
                    file = open_append(&path).unwrap();
                    reopen_at = None;
                    r.store(false, Ordering::SeqCst);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        w.store(seq, Ordering::SeqCst);
    });
    Fake {
        reopen,
        written,
        thread,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_rotations_at_2000_lines_per_second_lose_and_repeat_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("access.json");
    let policy = RotatePolicy::new(Strategy::RenameReopen, Some(MAX_SIZE), Some(1));

    let (tx, mut rx) = mpsc::channel::<TailBatch<u64>>(256);
    let tailer = tail::spawn(
        TailSpec {
            key: Arc::from("access"),
            path: log.clone(),
            start: StartAt::Beginning,
        },
        TailConfig {
            tick: Duration::from_millis(10),
            idle_tick: Duration::from_millis(40),
            catch_up_tick: Duration::from_millis(2),
            ..TailConfig::default()
        },
        decode,
        tx,
    );
    let collector = tokio::spawn(async move {
        let mut seen: Vec<u64> = Vec::new();
        while let Some(batch) = rx.recv().await {
            assert_eq!(batch.weight, 1, "nothing is sampled at 2,000 lines/s");
            assert_eq!(batch.rejected, 0);
            seen.extend(batch.items);
        }
        seen
    });

    let fake = fake_server(log.clone());
    let mut rotator = Rotator::new(Duration::from_millis(300));
    let mut rotations = 0u32;
    // The rotation loop of the daemon: look at the size, rename, ask for the reopen, wait for
    // it to happen.
    while fake.written.load(Ordering::SeqCst) < TOTAL {
        match rotator.check(&log, &policy, Instant::now()) {
            Check::Rotate { .. } => {
                rotate::rotate_files(&log, policy.keep).unwrap();
                rotator.rotated(&log, Instant::now());
                fake.reopen.store(true, Ordering::SeqCst);
                rotations += 1;
            }
            Check::Reopen if !fake.reopen.load(Ordering::SeqCst) => {
                rotator.reopened(&log);
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    fake.thread.join().unwrap();
    // The last lines, and the grace the tailer gives a rotated file.
    tokio::time::sleep(Duration::from_millis(500)).await;
    tailer.stop(Duration::from_secs(10)).await;
    let seen = collector.await.unwrap();

    assert!(rotations >= 3, "only {rotations} rotations");
    assert!(
        rotated_name(&log, 1).exists(),
        "access.json.1 must exist after a rotation"
    );
    assert!(
        !rotated_name(&log, 2).exists(),
        "keep = 1: nothing older than .1"
    );
    assert_eq!(
        seen.len() as u64,
        TOTAL,
        "delivered {} of {TOTAL}",
        seen.len()
    );
    assert!(
        seen.iter().copied().eq(0..TOTAL),
        "every number exactly once and in order (first difference at {:?})",
        seen.iter().zip(0..TOTAL).position(|(a, b)| *a != b)
    );
    println!("{rotations} rotations, {TOTAL} lines delivered exactly once");
}
