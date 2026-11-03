//! Streaming one-shot runner (docs/03 §1.2, §7.3): runs a command once inside its own Job
//! Object with stdin, stdout and stderr pumped **concurrently**, and cancellation that
//! kills the whole job. `db.export` reads a dump client's stdout through it and
//! `db.import` feeds a restore client's stdin (M2-E3-T6).
//!
//! * **Data flows through bounded channels** of `Vec<u8>` chunks (at most [`CHUNK`]
//!   bytes): the caller owns the other ends and does its own I/O (compression, files) on
//!   blocking threads. A slow consumer of stdout makes the pump wait, the 64 KiB pipe
//!   fills and the child blocks in `WriteFile`; memory stays bounded by the channel
//!   capacity. A child that stops reading stdin makes the feeder wait the same way.
//! * **No deadlock**: the three pumps and the exit wait run in one `join`, so a child that
//!   fills stderr while we wait on stdout (or the reverse) never stalls. stderr is drained
//!   into a tail of the last `stderr_tail` lines (each cut at 4 KiB).
//! * **Exit**: when the process exits, the job is terminated (a background process it
//!   started cannot keep the pipes open), the pumps drain what is left in the pipes and
//!   the outcome carries the exit code and the stderr tail. A non-zero exit is not an
//!   error here; the caller decides what it means.
//! * **Cancellation**: the token fires → the job is terminated **while our end of stdin
//!   is still open**, so a restore client never sees an end of file (and never commits a
//!   half import) before it dies; the run returns `ForgeError::Cancelled` once the job is
//!   empty and the process object is signaled (at most 2 s more), with nothing sent to
//!   the stdout receiver after the cancellation. Callers that fail
//!   on their side (a corrupt input, a full disk) cancel a child token to stop the child
//!   the same way.
//! * The job is nested in the daemon's root job ([`JobTree::instance_job`]): nothing
//!   survives the daemon. `CreateProcess` runs in `spawn_blocking` (ADR-0011).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use forge_core::platform::{
    PipeReader, PipeWriter, ProcessGroup, StdinSource, StdioTarget, TERMINATE_TIMEOUT,
};
use forge_core::runtime::spawn_blocking_timeout;
use forge_core::{ForgeError, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::job::JobTree;
use crate::process::SpawnSpec;
use crate::stop::TERMINATED_EXIT_CODE;

/// Largest chunk read from stdout (and the size callers should feed stdin with).
pub const CHUNK: usize = 64 * 1024;
/// Default number of stderr lines kept.
pub const STDERR_TAIL_LINES: usize = 20;
/// Longest stderr line kept (the rest of the line is dropped).
const STDERR_LINE_MAX: usize = 4096;
/// Budget of creating the job and the process.
const SPAWN_BUDGET: Duration = Duration::from_secs(15);
/// How long a cancelled run waits for the killed process object to be signaled.
const EXIT_AFTER_KILL: Duration = Duration::from_secs(2);

/// What to run and where its streams go.
#[derive(Debug)]
pub struct Oneshot {
    /// Program, arguments, environment and `PATH` (the deterministic environment of
    /// [`SpawnSpec::environment`]). Its `stdio` is ignored: the runner owns the streams.
    pub spec: SpawnSpec,
    /// Chunks written to the child's stdin, in order; when every sender is dropped the
    /// pipe is closed (end of file). `None`: stdin is the null device.
    pub stdin: Option<mpsc::Receiver<Vec<u8>>>,
    /// Receives what the child prints on stdout. `None`: stdout goes to the null device.
    /// If the receiver goes away the rest of the output is read and discarded.
    pub stdout: Option<mpsc::Sender<Vec<u8>>>,
    /// stderr lines kept for the outcome.
    pub stderr_tail: usize,
}

impl Oneshot {
    /// `spec` with no stdin, no stdout and the default stderr tail.
    pub fn new(spec: SpawnSpec) -> Self {
        Self {
            spec,
            stdin: None,
            stdout: None,
            stderr_tail: STDERR_TAIL_LINES,
        }
    }

    /// Feeds stdin from `rx`.
    pub fn stdin(mut self, rx: mpsc::Receiver<Vec<u8>>) -> Self {
        self.stdin = Some(rx);
        self
    }

    /// Sends stdout to `tx`.
    pub fn stdout(mut self, tx: mpsc::Sender<Vec<u8>>) -> Self {
        self.stdout = Some(tx);
        self
    }
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneshotOutcome {
    pub pid: u32,
    /// Raw exit code (an NTSTATUS for a crash).
    pub exit_code: u32,
    /// The last stderr lines (lossy UTF-8, `\r` removed), oldest first.
    pub stderr_tail: Vec<String>,
    /// Bytes written to stdin.
    pub stdin_bytes: u64,
    /// Bytes read from stdout.
    pub stdout_bytes: u64,
}

impl OneshotOutcome {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Runs `run` to its end or until `cancel` fires (then `Err(Cancelled)` once the job is
/// empty). Needs a tokio runtime.
pub async fn run(
    jobs: &JobTree,
    run: Oneshot,
    cancel: &CancellationToken,
) -> Result<OneshotOutcome> {
    let Oneshot {
        spec,
        stdin,
        stdout,
        stderr_tail,
    } = run;
    let label = spec.exe.display().to_string();
    let mut command = spec.to_command(jobs.processes().as_ref())?;
    command.stdin = if stdin.is_some() {
        StdinSource::Pipe
    } else {
        StdinSource::Null
    };
    command.stdout = if stdout.is_some() {
        StdioTarget::Pipe
    } else {
        StdioTarget::Null
    };
    command.stderr = StdioTarget::Pipe;
    if cancel.is_cancelled() {
        return Err(ForgeError::Cancelled(format!("{label} was not started")));
    }
    let tree = jobs.clone();
    let (group, mut spawned) =
        spawn_blocking_timeout(&format!("starting {label}"), SPAWN_BUDGET, move || {
            let group = tree.instance_job()?;
            let spawned = group.spawn(&command)?;
            Ok((group, spawned))
        })
        .await?;
    let pid = spawned.pid;
    tracing::debug!(pid, exe = %label, "one-shot process started");

    // Our end of stdin lives here, outside the pumps: on cancellation the job dies first
    // and the pipe closes after (the child never reads a premature end of file).
    let mut writer = spawned.stdin.take();
    let reader = spawned.stdout.take();
    let errors = spawned.stderr.take();
    let exited = CancellationToken::new();

    let feed = feed_stdin(&mut writer, stdin, &exited);
    let pump = pump_stdout(reader, stdout, cancel);
    let tail = collect_stderr(errors, stderr_tail);
    let exit = async {
        let code = tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            code = &mut spawned.exit => Some(code),
        };
        // Stops the feeder without closing stdin (see above), then kills the job: after a
        // normal exit, the leftovers of the tool that could keep the pipes open.
        exited.cancel();
        terminate(&group, &label).await;
        match code {
            Some(code) => code.map(Some),
            None => {
                // The job is empty as soon as its processes are dying; the process object
                // is signaled a moment later. Return only once it is gone.
                let _ = tokio::time::timeout(EXIT_AFTER_KILL, &mut spawned.exit).await;
                Ok(None)
            }
        }
    };
    let (code, stdin_bytes, stdout_bytes, stderr_tail) = tokio::join!(exit, feed, pump, tail);
    match code? {
        Some(exit_code) => Ok(OneshotOutcome {
            pid,
            exit_code,
            stderr_tail,
            stdin_bytes,
            stdout_bytes,
        }),
        None => {
            tracing::debug!(pid, exe = %label, "one-shot process cancelled");
            Err(ForgeError::Cancelled(format!("{label} (pid {pid})")))
        }
    }
}

/// Kills what is left in the group and waits until it is empty (bounded).
async fn terminate(group: &Arc<dyn ProcessGroup>, label: &str) {
    let group = Arc::clone(group);
    let result = spawn_blocking_timeout(
        "terminating a one-shot job",
        TERMINATE_TIMEOUT + Duration::from_secs(1),
        move || group.terminate_all(TERMINATED_EXIT_CODE),
    )
    .await;
    if let Err(err) = result {
        tracing::warn!(exe = %label, %err, "terminating a one-shot job failed");
    }
}

/// Writes every chunk of `rx` to the child; closes stdin at the end of the input. Stops
/// (keeping the pipe open) when the process exits; stops and drops `rx` when the child
/// no longer reads (the producer sees its channel closed).
async fn feed_stdin(
    writer: &mut Option<PipeWriter>,
    rx: Option<mpsc::Receiver<Vec<u8>>>,
    exited: &CancellationToken,
) -> u64 {
    let mut written = 0u64;
    let (Some(pipe), Some(mut rx)) = (writer.as_mut(), rx) else {
        // No input: close stdin at once so the child is not left waiting.
        *writer = None;
        return 0;
    };
    loop {
        let chunk = tokio::select! {
            biased;
            () = exited.cancelled() => return written,
            chunk = rx.recv() => chunk,
        };
        let Some(chunk) = chunk else {
            let _ = pipe.flush().await;
            // End of the input: closing our end is the child's end of file.
            *writer = None;
            return written;
        };
        let wrote = tokio::select! {
            biased;
            () = exited.cancelled() => return written,
            wrote = pipe.write_all(&chunk) => wrote,
        };
        if let Err(err) = wrote {
            tracing::debug!(%err, "the child stopped reading stdin");
            return written;
        }
        written += chunk.len() as u64;
    }
}

/// Reads stdout to its end and forwards it; once the receiver is gone (or the run is
/// cancelled) the rest is read and dropped so the child never blocks on a full pipe.
async fn pump_stdout(
    reader: Option<PipeReader>,
    tx: Option<mpsc::Sender<Vec<u8>>>,
    cancel: &CancellationToken,
) -> u64 {
    let Some(mut reader) = reader else {
        return 0;
    };
    let mut tx = tx;
    let mut total = 0u64;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) | Err(_) => return total,
            Ok(n) => n,
        };
        total += n as u64;
        if let Some(sender) = &tx {
            let sent = tokio::select! {
                biased;
                () = cancel.cancelled() => false,
                sent = sender.send(buf[..n].to_vec()) => sent.is_ok(),
            };
            if !sent {
                tx = None;
            }
        }
    }
}

/// The last `keep` lines of stderr.
async fn collect_stderr(reader: Option<PipeReader>, keep: usize) -> Vec<String> {
    let Some(mut reader) = reader else {
        return Vec::new();
    };
    let mut tail = StderrTail::new(keep);
    let mut buf = vec![0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => tail.push(&buf[..n]),
        }
    }
    tail.finish()
}

/// Line splitter that keeps only the last lines.
#[derive(Debug)]
struct StderrTail {
    keep: usize,
    lines: VecDeque<String>,
    current: Vec<u8>,
    /// The current line passed [`STDERR_LINE_MAX`]: the rest of it is dropped.
    overflow: bool,
}

impl StderrTail {
    fn new(keep: usize) -> Self {
        Self {
            keep,
            lines: VecDeque::new(),
            current: Vec::new(),
            overflow: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if b == b'\n' {
                self.end_line();
            } else if self.current.len() < STDERR_LINE_MAX {
                self.current.push(b);
            } else {
                self.overflow = true;
            }
        }
    }

    fn end_line(&mut self) {
        if self.current.last() == Some(&b'\r') {
            self.current.pop();
        }
        let mut line = String::from_utf8_lossy(&self.current).into_owned();
        if self.overflow {
            line.push('…');
        }
        self.current.clear();
        self.overflow = false;
        if self.keep == 0 {
            return;
        }
        if self.lines.len() == self.keep {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    fn finish(mut self) -> Vec<String> {
        if !self.current.is_empty() {
            self.end_line();
        }
        self.lines.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_tail_keeps_the_last_lines() {
        let mut tail = StderrTail::new(3);
        tail.push(b"one\r\ntwo\nthr");
        tail.push(b"ee\nfour\r\nfive");
        assert_eq!(tail.finish(), ["three", "four", "five"]);
        let mut tail = StderrTail::new(2);
        let long = vec![b'x'; STDERR_LINE_MAX + 10];
        tail.push(&long);
        tail.push(b"\nend\n");
        let lines = tail.finish();
        assert_eq!(lines[0].chars().count(), STDERR_LINE_MAX + 1);
        assert!(lines[0].ends_with('…'));
        assert_eq!(lines[1], "end");
        assert!(StderrTail::new(0).finish().is_empty());
    }
}
