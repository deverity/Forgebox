//! One-shot probes: run a short command (`php.exe -v`, `nginx.exe -v`) and capture its
//! output, with a hard timeout covering exit and output, no console window, and the
//! whole process tree killed afterwards (Job Object, via [`super::ProcessGroup`]). Used by discovery of manually
//! installed tools (docs/03 §5.3) and anything else that asks a binary for its version.
//!
//! This is **blocking** (`CreateProcess`, job termination): call it from `spawn_blocking`
//! (ADR-0011). Long-running services do not use this: they go through
//! [`super::ProcessGroup`].

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::AsyncReadExt;

use super::process::{
    GroupOptions, PipeReader, ProcessCommand, ProcessControlOptions, StdioTarget, process_control,
};
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

async fn read_capped(pipe: Option<PipeReader>) -> Vec<u8> {
    let Some(mut pipe) = pipe else {
        return Vec::new();
    };
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = PROBE_MAX_OUTPUT.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
    kept
}

/// Runs `cmd` in its own kill-on-close group with an explicit environment
/// (`ProcessControl::base_env` plus the system `PATH`, never the daemon's `PATH`), stdin on
/// the null device and both outputs captured.
///
/// `timeout` covers the whole probe: the exit of the process **and** the end of both
/// outputs. When it passes the whole group is terminated and the result is
/// `ForgeError::Timeout`. When the process exits in time, the group is terminated as well,
/// so a background process it started cannot keep the pipes open or outlive the probe.
///
/// Blocking: call it from `spawn_blocking`. It drives its own small runtime, so it works
/// with or without a caller runtime.
pub fn run_probe(cmd: &ProbeCommand, timeout: Duration) -> Result<ProbeOutput> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ForgeError::io("starting the probe runtime", e))?;
    runtime.block_on(probe_async(cmd, timeout))
}

async fn probe_async(cmd: &ProbeCommand, timeout: Duration) -> Result<ProbeOutput> {
    let control = process_control(ProcessControlOptions::default());
    let group = control.create_group(GroupOptions {
        kill_on_close: true,
        breakaway_ok: false,
        die_on_unhandled_exception: true,
    })?;
    let mut env = control.base_env();
    if let Ok(path) = std::env::join_paths(control.system_path()) {
        env.push((OsString::from("PATH"), path));
    }
    let mut process = ProcessCommand::new(&cmd.exe, &cmd.cwd);
    process.args.clone_from(&cmd.args);
    process.env = env;
    process.stdout = StdioTarget::Pipe;
    process.stderr = StdioTarget::Pipe;
    process.new_process_group = false;
    let mut spawned = group.spawn(&process)?;

    let out = read_capped(spawned.stdout.take());
    let err = read_capped(spawned.stderr.take());
    let exit = async {
        let code = (&mut spawned.exit).await;
        // Background processes of the tool must not keep the pipes open.
        let _ = group.terminate_all(0);
        code
    };
    let result = tokio::time::timeout(timeout, async { tokio::join!(exit, out, err) }).await;
    match result {
        Ok((code, out, err)) => {
            let code = code?;
            Ok(ProbeOutput {
                // Exit codes are bit patterns (NTSTATUS); keep them as the signed value.
                exit_code: Some(code as i32),
                stdout: String::from_utf8_lossy(&out).into_owned(),
                stderr: String::from_utf8_lossy(&err).into_owned(),
            })
        }
        Err(_) => {
            let _ = group.terminate_all(1);
            Err(ForgeError::Timeout {
                operation: format!("running {}", cmd.exe.display()),
                after: timeout,
            })
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::time::Instant;

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

    /// Number of running processes whose command line contains `needle`.
    fn running_with_arg(needle: &str) -> usize {
        let script = format!(
            "(Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -like '*{needle}*'              -and $_.Name -eq 'PING.EXE' }} | Measure-Object).Count"
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }

    #[test]
    fn background_process_does_not_outlive_or_block_the_probe() {
        // 127.0.0.77 makes this ping recognizable among other tests' ping.exe.
        let started = Instant::now();
        let res = run_probe(
            &cmd_exe(&["/c", "start /b ping -n 30 127.0.0.77 >nul & echo x"]),
            Duration::from_millis(300),
        );
        assert!(started.elapsed() < Duration::from_secs(2), "{res:?}");
        assert_eq!(running_with_arg("127.0.0.77"), 0, "ping.exe left running");
    }

    #[test]
    fn missing_executable_is_an_error() {
        let mut cmd = cmd_exe(&[]);
        cmd.exe = cmd.cwd.join("no-such-binary.exe");
        assert!(run_probe(&cmd, PROBE_TIMEOUT).is_err());
    }
}
