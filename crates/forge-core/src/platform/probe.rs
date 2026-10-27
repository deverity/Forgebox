//! One-shot probes: run a short command (`php.exe -v`, `nginx.exe -v`) and capture its
//! output, with a hard timeout and no console window. Used by discovery of manually
//! installed tools (docs/03 §5.3) and anything else that asks a binary for its version.
//!
//! This is **blocking** (`CreateProcess`, pipe reads): call it from `spawn_blocking`
//! (ADR-0011). Long-running services do not use this: they go through
//! [`super::ProcessGroup`].

use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{ForgeError, Result};

/// Default timeout of a probe.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Bytes kept from each of stdout and stderr; the rest is read and discarded so the child
/// never blocks on a full pipe.
pub const PROBE_MAX_OUTPUT: usize = 1024 * 1024;

/// A command to run once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeCommand {
    /// Executable (absolute or relative to the process, never searched in `PATH` by the
    /// caller's intent: pass an absolute path).
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    /// Working directory.
    pub cwd: PathBuf,
}

/// What a probe printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutput {
    /// `None` when the process was terminated by a signal.
    pub exit_code: Option<i32>,
    /// Lossy UTF-8, at most [`PROBE_MAX_OUTPUT`] bytes.
    pub stdout: String,
    pub stderr: String,
}

impl ProbeOutput {
    /// stdout followed by stderr (some tools print their version on stderr: `nginx -v`).
    pub fn combined(&self) -> String {
        if self.stderr.is_empty() {
            self.stdout.clone()
        } else if self.stdout.is_empty() {
            self.stderr.clone()
        } else {
            format!("{}\n{}", self.stdout, self.stderr)
        }
    }
}

fn read_capped(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = PROBE_MAX_OUTPUT.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
        kept
    })
}

/// Runs `cmd` with stdin on the null device and both outputs captured. If it does not exit
/// within `timeout` the process is killed and the result is `ForgeError::Timeout`.
pub fn run_probe(cmd: &ProbeCommand, timeout: Duration) -> Result<ProbeOutput> {
    let mut command = Command::new(&cmd.exe);
    command
        .args(&cmd.args)
        .current_dir(&cmd.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    super::windows::hide_console(&mut command);

    let mut child = command
        .spawn()
        .map_err(|e| ForgeError::io_at("starting", &cmd.exe, e))?;
    let out = child.stdout.take().map(read_capped);
    let err = child.stderr.take().map(read_capped);

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= timeout => break None,
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ForgeError::io_at("waiting for", &cmd.exe, e));
            }
        }
    };
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        // The pipes close with the process; grandchildren holding them would stall the
        // readers, so they are detached instead of joined.
        return Err(ForgeError::Timeout {
            operation: format!("running {}", cmd.exe.display()),
            after: timeout,
        });
    };
    let join = |h: Option<thread::JoinHandle<Vec<u8>>>| {
        h.and_then(|h| h.join().ok())
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    };
    Ok(ProbeOutput {
        exit_code: status.code(),
        stdout: join(out),
        stderr: join(err),
    })
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn cmd_exe(args: &[&str]) -> ProbeCommand {
        let system = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        ProbeCommand {
            exe: system.join("System32").join("cmd.exe"),
            args: args.iter().map(OsString::from).collect(),
            cwd: system,
        }
    }

    #[test]
    fn captures_output_and_exit_code() {
        let out = run_probe(&cmd_exe(&["/c", "echo hello & exit 3"]), PROBE_TIMEOUT).unwrap();
        assert_eq!(out.exit_code, Some(3));
        assert_eq!(out.stdout.trim(), "hello");
    }

    #[test]
    fn times_out_and_kills() {
        let started = Instant::now();
        let err = run_probe(
            &cmd_exe(&["/c", "ping -n 30 127.0.0.1 >nul"]),
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert_eq!(err.code(), "timeout");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn missing_executable_is_an_error() {
        let mut cmd = cmd_exe(&[]);
        cmd.exe = cmd.cwd.join("no-such-binary.exe");
        assert!(run_probe(&cmd, PROBE_TIMEOUT).is_err());
    }
}
