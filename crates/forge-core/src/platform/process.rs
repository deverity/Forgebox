//! Process supervision primitives (docs/03 §1): groups of processes that die together
//! (Job Objects on Windows), spawning into a group with captured stdio, console control
//! events and window close requests.
//!
//! Every method here is **blocking** (`CreateProcess`, job queries, helper processes) and
//! belongs in `spawn_blocking` (ADR-0011). Two things are async and never hold a thread
//! while waiting: the pipe readers of [`SpawnedProcess`] (overlapped I/O on the tokio
//! reactor) and [`ProcessExit`] (an OS thread-pool wait that completes a channel).
//! Because captured pipes are registered with the tokio reactor,
//! [`ProcessGroup::spawn`] must be called inside a tokio runtime context; `spawn_blocking`
//! closures have one.
//!
//! Groups nest: [`ProcessGroup::create_child`] makes a group whose processes also belong
//! to the parent, so terminating (or losing) the parent takes every child group with it
//! (docs/03 §1.1).

use std::ffi::OsString;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::AsyncRead;

use crate::error::{ForgeError, Result};

/// File name (without extension) of the helper that delivers `CTRL_BREAK` to a
/// supervised process' console (docs/03 §1.3, ADR-0016). Shipped next to `forgeboxd`.
pub const CTRL_BREAK_HELPER_NAME: &str = "forgebox-ctrl";

/// How long [`ProcessGroup::terminate_all`] waits for the killed processes to be gone.
pub const TERMINATE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long adding a process to a group keeps retrying while the group refuses new
/// members because a termination is still settling (see [`ProcessGroup::spawn`]).
pub const ASSIGN_RETRY_LIMIT: Duration = Duration::from_secs(2);

/// Async reading end of a captured stdout/stderr pipe.
pub type PipeReader = Pin<Box<dyn AsyncRead + Send>>;

/// Limits of a process group (Job Object).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupOptions {
    /// Kill every member when the last handle to the group closes, i.e. when the daemon
    /// dies (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`).
    pub kill_on_close: bool,
    /// Members may start children outside the group with an explicit breakaway request
    /// (`JOB_OBJECT_LIMIT_BREAKAWAY_OK`).
    pub breakaway_ok: bool,
    /// A member that hits an unhandled exception is terminated at once instead of
    /// hanging on the Windows Error Reporting dialog
    /// (`JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION`).
    pub die_on_unhandled_exception: bool,
}

impl GroupOptions {
    /// Root group of the daemon (docs/03 §1.1): kill on close, breakaway allowed.
    pub const ROOT: Self = Self {
        kill_on_close: true,
        breakaway_ok: true,
        die_on_unhandled_exception: false,
    };

    /// Group of one service instance or pool: same as the root, plus no crash dialogs.
    pub const INSTANCE: Self = Self {
        kill_on_close: true,
        breakaway_ok: true,
        die_on_unhandled_exception: true,
    };
}

/// Where a child's stdout or stderr goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdioTarget {
    /// A pipe read asynchronously by the caller ([`SpawnedProcess::stdout`]).
    Pipe,
    /// The null device.
    Null,
    /// Appended to this file (created if missing).
    File(PathBuf),
}

/// Console of a child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConsoleMode {
    /// Own console without a window (`CREATE_NO_WINDOW`). Required for `CTRL_BREAK`.
    #[default]
    Hidden,
    /// No console at all (`DETACHED_PROCESS`); console control events cannot reach it.
    Detached,
}

/// Everything the OS needs to start a process. Nothing is inherited from the daemon:
/// `env` is the complete environment and stdin is always the null device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCommand {
    /// Absolute path of the executable (`.bat`/`.cmd` are refused: cmd.exe quoting).
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    /// Working directory (must exist).
    pub cwd: PathBuf,
    /// Complete environment. On Windows names are case-insensitive; the last duplicate
    /// wins and the block is sorted as `CreateProcess` requires.
    pub env: Vec<(OsString, OsString)>,
    pub stdout: StdioTarget,
    pub stderr: StdioTarget,
    /// Start a new process group whose id is the child's pid (`CREATE_NEW_PROCESS_GROUP`),
    /// so `CTRL_BREAK` can target it alone.
    pub new_process_group: bool,
    pub console: ConsoleMode,
}

impl ProcessCommand {
    /// Command for `exe` with no arguments, cwd `cwd`, empty environment, stdio to null,
    /// hidden console and a new process group.
    pub fn new(exe: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            exe: exe.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: Vec::new(),
            stdout: StdioTarget::Null,
            stderr: StdioTarget::Null,
            new_process_group: true,
            console: ConsoleMode::Hidden,
        }
    }
}

/// A process that was started (and resumed) inside a group.
pub struct SpawnedProcess {
    pub pid: u32,
    /// Present when stdout was [`StdioTarget::Pipe`].
    pub stdout: Option<PipeReader>,
    /// Present when stderr was [`StdioTarget::Pipe`].
    pub stderr: Option<PipeReader>,
    /// Resolves with the exit code.
    pub exit: ProcessExit,
}

impl fmt::Debug for SpawnedProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpawnedProcess")
            .field("pid", &self.pid)
            .field("stdout", &self.stdout.is_some())
            .field("stderr", &self.stderr.is_some())
            .finish_non_exhaustive()
    }
}

/// OS side of a [`ProcessExit`].
pub trait ExitSource: Send {
    /// Polls for the exit code; registers `cx` to be woken when the process exits.
    fn poll_exit(&mut self, cx: &mut Context<'_>) -> Poll<Result<u32>>;
    /// Exit code if the process has already exited (non-blocking).
    fn try_exit_code(&self) -> Result<Option<u32>>;
}

/// Exit of a process, awaitable without holding a thread. Dropping it only stops
/// watching; the process keeps running.
pub struct ProcessExit {
    pid: u32,
    source: Box<dyn ExitSource>,
}

impl ProcessExit {
    /// Wraps an OS-specific source.
    pub fn new(pid: u32, source: Box<dyn ExitSource>) -> Self {
        Self { pid, source }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Exit code if the process has already exited.
    pub fn try_exit_code(&self) -> Result<Option<u32>> {
        self.source.try_exit_code()
    }

    /// Waits at most `timeout`; `Ok(None)` if the process is still running.
    pub async fn wait_timeout(&mut self, timeout: Duration) -> Result<Option<u32>> {
        match tokio::time::timeout(timeout, self).await {
            Ok(code) => code.map(Some),
            Err(_) => Ok(None),
        }
    }
}

impl fmt::Debug for ProcessExit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessExit")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl Future for ProcessExit {
    type Output = Result<u32>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.source.poll_exit(cx)
    }
}

/// Group of processes that die together (Job Object on Windows, cgroup on Linux).
/// Dropping the last reference closes the group, which kills its members when it was
/// created with [`GroupOptions::kill_on_close`].
///
/// Termination is asynchronous in the OS: on Windows `TerminateJobObject` returns before
/// the processes are gone, and the job keeps refusing new members
/// (`ERROR_ACCESS_DENIED`) for a few milliseconds even after its last process left
/// (docs/03 §1.1). The implementations hide both: [`terminate_all`](Self::terminate_all)
/// returns only once the group is empty, and [`spawn`](Self::spawn)/[`assign`](Self::assign)
/// retry while a termination settles.
pub trait ProcessGroup: Send + Sync + fmt::Debug {
    /// New empty group nested in this one: its members are also members of `self`.
    fn create_child(&self, opts: GroupOptions) -> Result<Arc<dyn ProcessGroup>>;

    /// Starts `cmd` suspended, adds it to this group (and its ancestors) and only then
    /// lets it run, so no grandchild can escape. Needs a tokio runtime context when a
    /// stream is [`StdioTarget::Pipe`]. If the group refuses the new member because a
    /// termination is still settling, the assignment is retried for at most
    /// [`ASSIGN_RETRY_LIMIT`]; if it still fails the child is killed before it ran.
    fn spawn(&self, cmd: &ProcessCommand) -> Result<SpawnedProcess>;

    /// Adds the running process `pid` to the group (same retry as [`spawn`](Self::spawn)).
    fn assign(&self, pid: u32) -> Result<()>;

    /// PIDs currently in the group, nested groups included.
    fn pids(&self) -> Result<Vec<u32>>;

    /// Kills every process of the group (and of nested groups) and waits, at most
    /// [`TERMINATE_TIMEOUT`], until none is left: on `Ok` every member has exited.
    /// `Timeout` when some process outlives the deadline (stuck in the kernel). Calling
    /// it while another termination of the same group is in flight is fine.
    fn terminate_all(&self, exit_code: u32) -> Result<()>;
}

/// Process-level operations of the OS.
pub trait ProcessControl: Send + Sync + fmt::Debug {
    /// New top-level group.
    fn create_group(&self, opts: GroupOptions) -> Result<Arc<dyn ProcessGroup>>;

    /// Delivers `CTRL_BREAK` to the process group led by `group_leader` (the pid of a
    /// process spawned with [`ProcessCommand::new_process_group`] and a
    /// [`ConsoleMode::Hidden`] console). Waits at most `timeout` for the delivery.
    fn send_ctrl_break(&self, group_leader: u32, timeout: Duration) -> Result<()>;

    /// Posts `WM_CLOSE` to the top-level windows owned by `pids`; returns how many
    /// windows got it (0 = the processes have no window).
    fn close_windows(&self, pids: &[u32]) -> Result<usize>;

    /// Watches the exit of an existing process. `NotFound` if it does not exist.
    fn watch(&self, pid: u32) -> Result<ProcessExit>;

    /// Whether `pid` is a running process.
    fn is_alive(&self, pid: u32) -> Result<bool>;

    /// Directories always appended to a child's `PATH` (`%SystemRoot%\System32`).
    fn system_path(&self) -> Vec<PathBuf>;

    /// Minimal environment every child inherits from the daemon (allowlist: system
    /// folders, user profile, temp, processor info...). Never `PATH`.
    fn base_env(&self) -> Vec<(OsString, OsString)>;
}

/// Options of [`process_control`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessControlOptions {
    /// Path of the `forgebox-ctrl` helper. `None`: next to the running executable.
    pub ctrl_break_helper: Option<PathBuf>,
}

impl ProcessControlOptions {
    /// Uses `helper` as the `CTRL_BREAK` helper.
    pub fn with_ctrl_break_helper(helper: impl Into<PathBuf>) -> Self {
        Self {
            ctrl_break_helper: Some(helper.into()),
        }
    }

    /// The helper path: the explicit one, or `<dir of current exe>/forgebox-ctrl[.exe]`.
    pub fn resolved_ctrl_break_helper(&self) -> Result<PathBuf> {
        if let Some(path) = &self.ctrl_break_helper {
            return Ok(path.clone());
        }
        let exe = std::env::current_exe()
            .map_err(|e| ForgeError::io("locating the running executable", e))?;
        let dir = exe
            .parent()
            .ok_or_else(|| ForgeError::NotFound(format!("folder of {}", exe.display())))?;
        Ok(dir.join(format!(
            "{CTRL_BREAK_HELPER_NAME}{}",
            std::env::consts::EXE_SUFFIX
        )))
    }
}

/// Process operations for the running OS.
#[cfg(windows)]
pub fn process_control(opts: ProcessControlOptions) -> Arc<dyn ProcessControl> {
    Arc::new(super::windows::WindowsProcessControl::new(opts))
}

/// Process operations for the running OS (stubs until the Linux/macOS ports).
#[cfg(not(windows))]
pub fn process_control(_opts: ProcessControlOptions) -> Arc<dyn ProcessControl> {
    Arc::new(unsupported::Unsupported)
}

/// Entry point of the `forgebox-ctrl` helper binary: `forgebox-ctrl break <pid>`.
/// Returns the process exit code (see the module docs of the Windows implementation).
#[cfg(windows)]
pub fn ctrl_break_helper_main(args: impl IntoIterator<Item = OsString>) -> i32 {
    super::windows::console_ctrl::helper_main(args)
}

/// Entry point of the `forgebox-ctrl` helper binary (unsupported on this OS).
#[cfg(not(windows))]
pub fn ctrl_break_helper_main(_args: impl IntoIterator<Item = OsString>) -> i32 {
    eprintln!(
        "forgebox-ctrl is not implemented on {}",
        std::env::consts::OS
    );
    1
}

#[cfg(not(windows))]
mod unsupported {
    use super::*;

    #[derive(Debug)]
    pub struct Unsupported;

    fn err<T>() -> Result<T> {
        Err(ForgeError::UnsupportedPlatform(format!(
            "process supervision is not implemented on {}",
            std::env::consts::OS
        )))
    }

    impl ProcessControl for Unsupported {
        fn create_group(&self, _: GroupOptions) -> Result<Arc<dyn ProcessGroup>> {
            err()
        }
        fn send_ctrl_break(&self, _: u32, _: Duration) -> Result<()> {
            err()
        }
        fn close_windows(&self, _: &[u32]) -> Result<usize> {
            err()
        }
        fn watch(&self, _: u32) -> Result<ProcessExit> {
            err()
        }
        fn is_alive(&self, _: u32) -> Result<bool> {
            err()
        }
        fn system_path(&self) -> Vec<PathBuf> {
            Vec::new()
        }
        fn base_env(&self) -> Vec<(OsString, OsString)> {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_defaults_to_sibling_of_current_exe() {
        let opts = ProcessControlOptions::default();
        let helper = opts.resolved_ctrl_break_helper().unwrap();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(helper.parent(), exe.parent());
        let name = helper.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(CTRL_BREAK_HELPER_NAME), "{name}");

        let explicit = ProcessControlOptions::with_ctrl_break_helper("x/helper.exe");
        assert_eq!(
            explicit.resolved_ctrl_break_helper().unwrap(),
            PathBuf::from("x/helper.exe")
        );
    }

    #[test]
    fn command_defaults_are_safe_for_services() {
        let cmd = ProcessCommand::new("C:/bin/tool.exe", "C:/bin");
        assert!(cmd.new_process_group);
        assert_eq!(cmd.console, ConsoleMode::Hidden);
        assert_eq!(cmd.stdout, StdioTarget::Null);
        assert!(cmd.env.is_empty());
        const { assert!(GroupOptions::ROOT.kill_on_close && GroupOptions::ROOT.breakaway_ok) };
        const { assert!(GroupOptions::INSTANCE.die_on_unhandled_exception) };
    }
}
