//! Pseudo terminals (docs/03 §1.6): a process attached to a pseudo console whose screen
//! is read as a VT byte stream and whose keyboard is written as VT input. The daemon's
//! terminal sessions (docs/07 §5.10, ADR-0040) sit on top of it.
//!
//! [`PtySpawner::spawn`] starts the process **inside a [`ProcessGroup`]** and only lets it
//! run once it is a member (created suspended, assigned, resumed), so neither the shell nor
//! anything it starts can outlive the group (docs/03 §1.1).
//!
//! Every call is **blocking** (`CreatePseudoConsole` starts a console host process,
//! `CreateProcess`, pipe writes) and belongs in `spawn_blocking` (ADR-0011). The reader and
//! the writer of a [`PtySession`] are blocking byte streams meant for dedicated threads:
//! the output never pauses waiting for a slow consumer only if somebody keeps reading it.
//!
//! What a consumer must know (Windows ConPTY behavior, docs/03 §1.6):
//! - The output is a *rendering* of the console screen, not the bytes the program wrote:
//!   UTF-8 text mixed with VT sequences (cursor moves, colors). The first chunk carries
//!   initialization sequences (clear screen, cursor home, window title).
//! - The output does not end when the process exits: the console stays open until
//!   [`PtyConsole::close`]. Watch [`PtySession::exit`], then close; the reader sees EOF once
//!   the console is gone.
//! - Keep reading the output until EOF while closing (or drop the reader): the console
//!   flushes a final frame, and on Windows 10 closing waits until that frame was read. A
//!   close that cannot finish returns `Timeout` after the given time instead of hanging the
//!   caller (the console finishes closing in the background as soon as the output is read
//!   or the reader dropped).
//! - Ctrl+C typed into the input (`0x03`) reaches the console's processes as a console
//!   control event, as in a real terminal.

use std::ffi::OsString;
use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::error::{ForgeError, Result};
use crate::platform::process::{ProcessExit, ProcessGroup};

/// How long [`PtyConsole::close`] waits for the pseudo console to be gone.
pub const PTY_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long [`PtyConsole::resize`] waits for the console to take the new size.
pub const PTY_RESIZE_TIMEOUT: Duration = Duration::from_secs(2);

/// Blocking reading end of a pseudo console's output (UTF-8 + VT sequences). `Ok(0)` is
/// EOF: the console was closed.
pub type PtyReader = Box<dyn Read + Send>;

/// Blocking writing end of a pseudo console's input (UTF-8 + VT input sequences). Fails
/// with `BrokenPipe` once the console was closed; blocks while the console does not read
/// (its process does not consume input), like a real terminal.
pub type PtyWriter = Box<dyn Write + Send>;

/// Size of a pseudo console in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

impl PtySize {
    /// The classic VT size. The daemon passes the API's own default (`terminal.open`).
    pub const DEFAULT: Self = Self { cols: 80, rows: 24 };

    /// Largest width accepted. The console keeps a buffer of `cols × rows` cells, so the
    /// bound keeps a bad request from costing hundreds of MB in the console host.
    pub const MAX_COLS: u16 = 2000;

    /// Largest height accepted (see [`PtySize::MAX_COLS`]).
    pub const MAX_ROWS: u16 = 1000;

    /// A validated size: both dimensions in `1..=MAX`. `InvalidInput` otherwise.
    pub fn new(cols: u16, rows: u16) -> Result<Self> {
        Self { cols, rows }.validated()
    }

    /// `self` if both dimensions are in range, `InvalidInput` otherwise.
    pub fn validated(self) -> Result<Self> {
        if (1..=Self::MAX_COLS).contains(&self.cols) && (1..=Self::MAX_ROWS).contains(&self.rows) {
            Ok(self)
        } else {
            Err(ForgeError::InvalidInput(format!(
                "terminal size {}x{} is out of range (1x1 to {}x{})",
                self.cols,
                self.rows,
                Self::MAX_COLS,
                Self::MAX_ROWS
            )))
        }
    }
}

impl Default for PtySize {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for PtySize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.cols, self.rows)
    }
}

/// Everything the OS needs to start a process in a pseudo console. As with
/// [`ProcessCommand`](crate::platform::ProcessCommand), nothing is inherited from the
/// daemon: `env` is the complete environment, and no handle is inherited (the console
/// gives the process its standard handles).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyCommand {
    /// Absolute path of the executable (`.bat`/`.cmd` are refused: run them through
    /// `cmd.exe /d /c`).
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    /// Working directory (must exist).
    pub cwd: PathBuf,
    /// Complete environment (Windows: case-insensitive names, last duplicate wins).
    pub env: Vec<(OsString, OsString)>,
    /// Initial size in cells, `1..=PtySize::MAX_COLS`.
    pub cols: u16,
    /// Initial size in cells, `1..=PtySize::MAX_ROWS`.
    pub rows: u16,
}

impl PtyCommand {
    /// Command for `exe` with no arguments, cwd `cwd`, empty environment and
    /// [`PtySize::DEFAULT`].
    pub fn new(exe: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            exe: exe.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: Vec::new(),
            cols: PtySize::DEFAULT.cols,
            rows: PtySize::DEFAULT.rows,
        }
    }

    /// The initial size, validated.
    pub fn size(&self) -> Result<PtySize> {
        PtySize::new(self.cols, self.rows)
    }
}

/// The pseudo console of a session: resize and close. Shared (`Arc`) so a resize or a
/// close can move into `spawn_blocking` while the session keeps it. Every method is
/// bounded in time: none waits on the console host forever.
pub trait PtyConsole: Send + Sync + fmt::Debug {
    /// Gives the console a new size; the process sees it as a window resize. `InvalidInput`
    /// for a size out of range, `Conflict` once the console is closed (or closing),
    /// `Timeout` after [`PTY_RESIZE_TIMEOUT`].
    fn resize(&self, size: PtySize) -> Result<()>;

    /// Size the console has now (the last successful resize, or the initial size).
    fn size(&self) -> PtySize;

    /// Closes the console and waits at most `timeout` until it is gone. Processes still
    /// attached receive a console close event and end (the group they run in is the hard
    /// stop). The output must keep being read meanwhile (see the module docs); on `Ok` the
    /// reader is at, or about to reach, EOF. `Timeout` when the console host has not
    /// finished in time: it goes on closing in the background. Calling it again, also
    /// concurrently, is fine: later calls wait for the same close.
    fn close_within(&self, timeout: Duration) -> Result<()>;

    /// [`PtyConsole::close_within`] with [`PTY_CLOSE_TIMEOUT`].
    fn close(&self) -> Result<()> {
        self.close_within(PTY_CLOSE_TIMEOUT)
    }

    /// Whether the console is gone (a close finished).
    fn is_closed(&self) -> bool;
}

/// A process running in a new pseudo console. The parts are public so the consumer can
/// move them where they belong: the reader to a reading thread, the writer to a writing
/// thread, the exit into a task, the console wherever resize and close are decided.
///
/// Dropping the console (every clone of the `Arc`) closes the pseudo console in the
/// background without waiting; the process ends with it, or with its group.
pub struct PtySession {
    pub pid: u32,
    /// Output of the console. EOF only after the console is closed.
    pub reader: PtyReader,
    /// Input of the console.
    pub writer: PtyWriter,
    /// Exit of the process (not of the console). Dropping it only stops watching.
    pub exit: ProcessExit,
    pub console: Arc<dyn PtyConsole>,
}

impl PtySession {
    /// Shorthand for `self.console.resize(PtySize::new(cols, rows)?)`.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.console.resize(PtySize::new(cols, rows)?)
    }

    /// Shorthand for [`PtyConsole::close`].
    pub fn close(&self) -> Result<()> {
        self.console.close()
    }
}

impl fmt::Debug for PtySession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PtySession")
            .field("pid", &self.pid)
            .field("console", &self.console)
            .finish_non_exhaustive()
    }
}

/// Starts processes in pseudo consoles.
pub trait PtySpawner: Send + Sync + fmt::Debug {
    /// Creates a pseudo console of `cmd`'s size and starts `cmd` in it: suspended, then
    /// added to `group` ([`ProcessGroup::assign`]), then resumed, so the process and its
    /// descendants run only as members of the group. If the group refuses it, the process
    /// is killed before it ran and the console is closed.
    ///
    /// `InvalidInput` for a relative or batch executable, a NUL in an argument or variable,
    /// or a size out of range; `NotFound` for a missing executable or working directory;
    /// `UnsupportedPlatform` where the OS has no pseudo consoles.
    fn spawn(&self, group: &dyn ProcessGroup, cmd: &PtyCommand) -> Result<PtySession>;
}

/// Pseudo consoles of the running OS (ConPTY on Windows 10 1809+).
#[cfg(windows)]
pub fn pty_spawner() -> Arc<dyn PtySpawner> {
    Arc::new(super::windows::WindowsPtySpawner)
}

/// Pseudo consoles of the running OS (stub until the Linux/macOS ports).
#[cfg(not(windows))]
pub fn pty_spawner() -> Arc<dyn PtySpawner> {
    Arc::new(unsupported::Unsupported)
}

#[cfg(not(windows))]
mod unsupported {
    use super::*;

    #[derive(Debug)]
    pub struct Unsupported;

    impl PtySpawner for Unsupported {
        fn spawn(&self, _: &dyn ProcessGroup, _: &PtyCommand) -> Result<PtySession> {
            Err(ForgeError::UnsupportedPlatform(format!(
                "pseudo terminals are not implemented on {}",
                std::env::consts::OS
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_validated() {
        assert_eq!(
            PtySize::new(120, 40).unwrap(),
            PtySize {
                cols: 120,
                rows: 40
            }
        );
        assert_eq!(PtySize::new(1, 1).unwrap().to_string(), "1x1");
        let max = PtySize::new(PtySize::MAX_COLS, PtySize::MAX_ROWS).unwrap();
        assert_eq!(max.validated().unwrap(), max);
        for (cols, rows) in [
            (0, 24),
            (80, 0),
            (PtySize::MAX_COLS + 1, 24),
            (80, PtySize::MAX_ROWS + 1),
            (u16::MAX, u16::MAX),
        ] {
            let err = PtySize::new(cols, rows).unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{cols}x{rows}: {err}");
        }
        assert_eq!(PtySize::default(), PtySize::DEFAULT);
    }

    #[test]
    fn command_defaults() {
        let mut cmd = PtyCommand::new("C:/Windows/System32/cmd.exe", "C:/");
        assert!(cmd.args.is_empty() && cmd.env.is_empty());
        assert_eq!(cmd.size().unwrap(), PtySize::DEFAULT);
        cmd.cols = 0;
        assert_eq!(cmd.size().unwrap_err().code(), "invalid_input");
    }

    /// The daemon moves the reader and the writer to threads, the exit into a task and the
    /// console into `spawn_blocking`.
    #[test]
    fn session_parts_move_across_threads() {
        fn sendable<T: Send + 'static>() {}
        sendable::<PtySession>();
        sendable::<PtyReader>();
        sendable::<PtyWriter>();
        sendable::<ProcessExit>();
        sendable::<Arc<dyn PtyConsole>>();
        sendable::<Arc<dyn PtySpawner>>();
    }

    #[cfg(not(windows))]
    #[test]
    fn stub_is_unsupported() {
        /// The process stub cannot create groups, so the PTY stub is called with a fake.
        #[derive(Debug)]
        struct NoGroup;
        impl ProcessGroup for NoGroup {
            fn create_child(
                &self,
                _: crate::platform::GroupOptions,
            ) -> Result<Arc<dyn ProcessGroup>> {
                unreachable!()
            }
            fn spawn(
                &self,
                _: &crate::platform::ProcessCommand,
            ) -> Result<crate::platform::SpawnedProcess> {
                unreachable!()
            }
            fn assign(&self, _: u32) -> Result<()> {
                unreachable!()
            }
            fn pids(&self) -> Result<Vec<u32>> {
                Ok(Vec::new())
            }
            fn terminate_all(&self, _: u32) -> Result<()> {
                Ok(())
            }
        }
        let err = pty_spawner()
            .spawn(&NoGroup, &PtyCommand::new("/bin/sh", "/"))
            .unwrap_err();
        assert_eq!(err.code(), "unsupported_platform");
    }
}
