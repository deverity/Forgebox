//! Streaming one-shot runner (`forge_supervisor::oneshot`, M2-E3-T6) with real Windows
//! programs: stdin and stdout streamed at the same time (no deadlock with a child that
//! echoes as it reads), the stderr tail and the exit code, cancellation that leaves no
//! process behind, and a stdout receiver that goes away without stalling the child.
#![cfg(windows)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use forge_core::platform::{ProcessControlOptions, process_control};
use forge_supervisor::oneshot::{self, CHUNK, Oneshot};
use forge_supervisor::{JobTree, SpawnSpec};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn system32() -> PathBuf {
    let root = std::env::var_os("SystemRoot").map_or_else(|| r"C:\Windows".into(), PathBuf::from);
    root.join("System32")
}

fn tree() -> JobTree {
    JobTree::new(process_control(ProcessControlOptions::default())).unwrap()
}

fn spec(exe: &str, args: &[&str]) -> SpawnSpec {
    SpawnSpec::new(system32().join(exe), system32()).args(args.iter().copied())
}

/// `findstr` echoes every line while it reads: 8 MiB in, the same 8 MiB out, with both
/// pipes far smaller than the data, so stdin and stdout must flow at the same time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdin_and_stdout_stream_concurrently() {
    let mut input = Vec::new();
    let mut i = 0u64;
    while input.len() < 8 * 1024 * 1024 {
        input.extend_from_slice(format!("row {i:010} {}\r\n", "x".repeat(60)).as_bytes());
        i += 1;
    }
    let (in_tx, in_rx) = mpsc::channel(4);
    let (out_tx, mut out_rx) = mpsc::channel(4);
    let run = Oneshot::new(spec("findstr.exe", &["/r", "^"]))
        .stdin(in_rx)
        .stdout(out_tx);
    let data = input.clone();
    let producer = tokio::spawn(async move {
        for chunk in data.chunks(CHUNK) {
            in_tx.send(chunk.to_vec()).await.unwrap();
        }
    });
    let consumer = tokio::spawn(async move {
        let mut out = Vec::new();
        while let Some(chunk) = out_rx.recv().await {
            out.extend_from_slice(&chunk);
        }
        out
    });
    let tree = tree();
    let cancel = CancellationToken::new();
    let outcome = tokio::time::timeout(Duration::from_secs(60), oneshot::run(&tree, run, &cancel))
        .await
        .expect("no deadlock")
        .unwrap();
    producer.await.unwrap();
    let out = consumer.await.unwrap();
    assert_eq!(outcome.exit_code, 0, "{:?}", outcome.stderr_tail);
    assert_eq!(outcome.stdin_bytes, input.len() as u64);
    assert_eq!(outcome.stdout_bytes, input.len() as u64);
    assert!(
        out == input,
        "stdout differs from stdin ({} bytes)",
        out.len()
    );
    assert!(tree.pids().unwrap().is_empty());
}

#[tokio::test]
async fn exit_code_and_the_last_stderr_lines() {
    let run = Oneshot::new(spec(
        "cmd.exe",
        &[
            "/d",
            "/c",
            "(for /l %i in (1,1,50) do @echo problem %i 1>&2) & exit 3",
        ],
    ));
    let cancel = CancellationToken::new();
    let outcome = oneshot::run(&tree(), run, &cancel).await.unwrap();
    assert_eq!(outcome.exit_code, 3);
    assert!(!outcome.success());
    let expected: Vec<String> = (31..=50).map(|i| format!("problem {i}")).collect();
    let tail: Vec<String> = outcome
        .stderr_tail
        .iter()
        .map(|l| l.trim_end().to_owned())
        .collect();
    assert_eq!(tail, expected);
}

/// Cancel while the child runs and while it waits on stdin: `Cancelled` well under 2 s,
/// the process is gone and the job is empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_kills_the_job_at_once() {
    let control = process_control(ProcessControlOptions::default());
    let tree = JobTree::new(std::sync::Arc::clone(&control)).unwrap();
    // `sort` reads stdin until the end of file, which never comes.
    let (in_tx, in_rx) = mpsc::channel(4);
    in_tx.send(b"b\r\na\r\n".to_vec()).await.unwrap();
    let run = Oneshot::new(spec("sort.exe", &[])).stdin(in_rx);
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let pids = tree.clone();
    let watcher = tokio::spawn(async move {
        // Wait until the child is in the job, then cancel.
        let deadline = Instant::now() + Duration::from_secs(10);
        let pid = loop {
            if let Some(pid) = pids.pids().unwrap().first().copied() {
                break pid;
            }
            assert!(Instant::now() < deadline, "the child never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        trigger.cancel();
        (pid, Instant::now())
    });
    let err = oneshot::run(&tree, run, &cancel).await.unwrap_err();
    let (pid, cancelled_at) = watcher.await.unwrap();
    let took = cancelled_at.elapsed();
    assert_eq!(err.code(), "cancelled", "{err}");
    assert!(took < Duration::from_secs(2), "cancel took {took:?}");
    assert!(!control.is_alive(pid).unwrap_or(false), "sort.exe survived");
    assert!(tree.pids().unwrap().is_empty());
    drop(in_tx);

    // A token cancelled before the start never spawns anything.
    let err = oneshot::run(&tree, Oneshot::new(spec("sort.exe", &[])), &cancel)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "cancelled");
}

/// The consumer of stdout goes away: the rest is drained, the child ends normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_stdout_receiver_does_not_stall_the_child() {
    let (out_tx, out_rx) = mpsc::channel(1);
    drop(out_rx);
    let run = Oneshot::new(spec(
        "cmd.exe",
        &["/d", "/c", "for /l %i in (1,1,20000) do @echo line %i"],
    ))
    .stdout(out_tx);
    let cancel = CancellationToken::new();
    let outcome =
        tokio::time::timeout(Duration::from_secs(60), oneshot::run(&tree(), run, &cancel))
            .await
            .expect("the child was not stalled")
            .unwrap();
    assert_eq!(outcome.exit_code, 0);
    assert!(outcome.stdout_bytes > 100_000, "{}", outcome.stdout_bytes);
}
