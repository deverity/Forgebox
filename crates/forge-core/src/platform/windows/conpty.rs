//! ConPTY pseudo consoles (docs/03 §1.6): [`WindowsPtySpawner`].
//!
//! * Two anonymous pipes (`CreatePipe`, not inheritable): the console reads our input from
//!   one and writes its rendering to the other. `CreatePseudoConsole` starts the console
//!   host (`conhost.exe`, a child of this process and of no Forgebox Job) with duplicates
//!   of its ends, so ours are closed right away: EOF on the output depends on the console
//!   host being the only writer.
//! * `CreateProcessW(CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT |
//!   CREATE_UNICODE_ENVIRONMENT)` with `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` →
//!   [`ProcessGroup::assign`] → `ResumeThread`: the process, and anything it starts, never
//!   runs outside the group. The assignment goes through the trait (by pid; the suspended
//!   process cannot exit, so its pid cannot be reused meanwhile), which keeps the group
//!   opaque and lets tests pass a fake.
//!   - No handle is inherited (`bInheritHandles = FALSE`), and the standard handles in
//!     `STARTUPINFO` are explicitly null (`STARTF_USESTDHANDLES`): without that, a process
//!     whose own standard handles are redirected (the daemon's are `NUL`, a test runner's
//!     are pipes) passes their values on and the child writes there instead of to the
//!     console. Null handles make the child open the console's own.
//!   - No `CREATE_NEW_PROCESS_GROUP` (it would disable Ctrl+C in the shell) and no
//!     `CREATE_NO_WINDOW`/`DETACHED_PROCESS` (they would give the process a console of its
//!     own instead of the pseudo console). The console host runs headless: no window.
//! * The `HPCON` belongs to a console thread (`forgebox-pty`): resize and close are
//!   messages to it, answered within a timeout, so no caller ever waits on the console host
//!   forever and the handle is never used after it was closed. `ClosePseudoConsole` on
//!   Windows 10 waits for the console host to exit, and the host first flushes a final
//!   frame to the output: if nobody reads the output and its pipe is full, it never
//!   returns. Running it on that thread lets the caller keep draining the output (or give
//!   up with `Timeout`) instead of deadlocking.
//! * The console thread also closes the console when the last [`PtyConsole`] reference is
//!   dropped, without blocking the dropping thread. If the daemon dies, the kernel closes
//!   its end of the console's signal pipe and the console host exits on its own.

use std::ffi::c_void;
use std::io::{PipeReader, PipeWriter};
use std::os::windows::io::OwnedHandle;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::{fmt, io};

use ::windows::Win32::Foundation::HANDLE;
use ::windows::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use ::windows::Win32::System::Pipes::CreatePipe;
use ::windows::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, EXTENDED_STARTUPINFO_PRESENT,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
};
use ::windows::core::{PCWSTR, PWSTR};

use super::process::{
    AttributeList, ExitWatch, check_launch, command_line, environment_block, launch_error, owned,
    raw, wide, win_err,
};
use crate::error::{ForgeError, Result};
use crate::platform::process::{ProcessExit, ProcessGroup};
use crate::platform::pty::{
    PTY_RESIZE_TIMEOUT, PtyCommand, PtyConsole, PtySession, PtySize, PtySpawner,
};

/// Buffer size requested for each pipe. A larger output buffer lets the console host
/// render a flood in fewer, larger writes.
const PIPE_BUFFER: u32 = 64 * 1024;

/// Stack of the console thread: it only makes two small kernel32 calls.
const CONSOLE_THREAD_STACK: usize = 256 * 1024;

/// First and longest pause while waiting for a close to finish (the backoff doubles).
const CLOSE_POLL_FIRST: Duration = Duration::from_micros(250);
const CLOSE_POLL_MAX: Duration = Duration::from_millis(10);

/// [`PtySpawner`] backed by ConPTY (Windows 10 1809+).
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsPtySpawner;

impl PtySpawner for WindowsPtySpawner {
    fn spawn(&self, group: &dyn ProcessGroup, cmd: &PtyCommand) -> Result<PtySession> {
        spawn(group, cmd)
    }
}

/// Anonymous pipe, both ends not inheritable: `(read, write)`.
fn anonymous_pipe() -> Result<(OwnedHandle, OwnedHandle)> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    // SAFETY: both out-pointers are writable; default (non-inheritable) security.
    unsafe { CreatePipe(&mut read, &mut write, None, PIPE_BUFFER) }
        .map_err(|e| win_err("creating a pseudo console pipe", &e))?;
    // SAFETY: CreatePipe succeeded; both handles are fresh and ours.
    Ok(unsafe { (owned(read), owned(write)) })
}

fn coord(size: PtySize) -> COORD {
    // PtySize keeps both dimensions far below i16::MAX.
    COORD {
        X: size.cols as i16,
        Y: size.rows as i16,
    }
}

fn spawn(group: &dyn ProcessGroup, cmd: &PtyCommand) -> Result<PtySession> {
    check_launch(&cmd.exe, &cmd.cwd)?;
    let size = cmd.size()?;
    let app = wide(cmd.exe.as_os_str());
    let mut line = command_line(&cmd.exe, &cmd.args)?;
    let env = environment_block(&cmd.env)?;
    let cwd = wide(cmd.cwd.as_os_str());

    // `input_*`: we write, the console reads. `output_*`: the console writes, we read.
    let (input_read, input_write) = anonymous_pipe()?;
    let (output_read, output_write) = anonymous_pipe()?;
    // SAFETY: both handles are valid pipe ends; the console host duplicates them.
    let hpc = unsafe { CreatePseudoConsole(coord(size), raw(&input_read), raw(&output_write), 0) }
        .map_err(|e| win_err("creating a pseudo console", &e))?;
    // The console host has its own duplicates (see the module docs).
    drop((input_read, output_write));
    // From here on the console thread owns `hpc`; dropping `console` closes it.
    let console = match ConPtyConsole::start(hpc, size) {
        Ok(console) => console,
        Err(err) => {
            // No thread: close it here. Nothing is attached and our output end goes
            // first, so the console host cannot block on a full pipe.
            drop(output_read);
            // SAFETY: `hpc` is valid and used by nobody else (the thread never started).
            unsafe { ClosePseudoConsole(hpc) };
            return Err(err);
        }
    };

    let mut attributes = AttributeList::new(1)?;
    // SAFETY: for this attribute the value *is* the HPCON (not a pointer to it), as
    // documented for PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE; `hpc` stays open until
    // `console` is dropped, after CreateProcessW returned.
    unsafe {
        UpdateProcThreadAttribute(
            attributes.ptr(),
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            Some(hpc.0 as *const c_void),
            size_of::<HPCON>(),
            None,
            None,
        )
    }
    .map_err(|e| win_err("attaching a process to a pseudo console", &e))?;

    let mut info = STARTUPINFOEXW::default();
    info.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    // Null standard handles: the child opens the pseudo console's (see the module docs).
    info.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    info.StartupInfo.hStdInput = HANDLE::default();
    info.StartupInfo.hStdOutput = HANDLE::default();
    info.StartupInfo.hStdError = HANDLE::default();
    info.lpAttributeList = attributes.ptr();
    let flags = CREATE_SUSPENDED.0 | EXTENDED_STARTUPINFO_PRESENT.0 | CREATE_UNICODE_ENVIRONMENT.0;

    let mut pi = PROCESS_INFORMATION::default();
    // SAFETY: `app`, `cwd` are NUL-terminated; `line` is writable and NUL-terminated
    // (CreateProcessW may modify it); `env` is a valid Unicode block; `info` is a
    // STARTUPINFOEXW (EXTENDED_STARTUPINFO_PRESENT) whose attribute list outlives the call.
    let created = unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            PROCESS_CREATION_FLAGS(flags),
            Some(env.as_ptr().cast()),
            PCWSTR(cwd.as_ptr()),
            (&raw const info).cast(),
            &mut pi,
        )
    };
    drop(attributes);
    created.map_err(|e| launch_error(&cmd.exe, &cmd.cwd, &e))?;

    // SAFETY: CreateProcessW succeeded; both handles are ours.
    let process = unsafe { owned(pi.hProcess) };
    // SAFETY: as above.
    let thread = unsafe { owned(pi.hThread) };
    let pid = pi.dwProcessId;
    let kill = |reason: ForgeError| {
        // SAFETY: valid process handle; the child is still suspended.
        let _ = unsafe { TerminateProcess(raw(&process), 1) };
        reason
    };
    // The handle we hold keeps the suspended process (and so its pid) alive.
    group.assign(pid).map_err(kill)?;
    // SAFETY: valid thread handle of the suspended primary thread.
    if unsafe { ResumeThread(raw(&thread)) } == u32::MAX {
        let err = io::Error::last_os_error();
        return Err(kill(ForgeError::io("resuming a new process", err)));
    }
    drop(thread);
    let exit = ProcessExit::new(pid, Box::new(ExitWatch::new(process)?));
    Ok(PtySession {
        pid,
        reader: Box::new(PipeReader::from(output_read)),
        writer: Box::new(PipeWriter::from(input_write)),
        exit,
        console: Arc::new(console),
    })
}

/// Requests to the console thread.
enum Request {
    Resize(COORD, mpsc::SyncSender<Result<()>>),
    Close,
}

/// [`PtyConsole`] of one ConPTY. The `HPCON` lives on the console thread (module docs).
struct ConPtyConsole {
    requests: mpsc::Sender<Request>,
    /// `cols << 16 | rows`.
    size: AtomicU32,
    closed: Arc<AtomicBool>,
}

impl fmt::Debug for ConPtyConsole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConPtyConsole")
            .field("size", &self.size())
            .field("closed", &self.is_closed())
            .finish()
    }
}

/// Owned `HPCON` that moves to the console thread.
struct ConsoleHandle(HPCON);

// SAFETY: an HPCON is a process-wide handle, usable from any thread; only the console
// thread uses it once moved there.
unsafe impl Send for ConsoleHandle {}

fn pack(size: PtySize) -> u32 {
    (u32::from(size.cols) << 16) | u32::from(size.rows)
}

fn unpack(value: u32) -> PtySize {
    PtySize {
        cols: (value >> 16) as u16,
        rows: (value & 0xFFFF) as u16,
    }
}

fn closed_error() -> ForgeError {
    ForgeError::Conflict("the pseudo console is closed".to_owned())
}

impl ConPtyConsole {
    /// Starts the console thread that owns `hpc` from now on.
    fn start(hpc: HPCON, size: PtySize) -> Result<Self> {
        let (requests, inbox) = mpsc::channel();
        let closed = Arc::new(AtomicBool::new(false));
        let handle = ConsoleHandle(hpc);
        let done = Arc::clone(&closed);
        std::thread::Builder::new()
            .name("forgebox-pty".to_owned())
            .stack_size(CONSOLE_THREAD_STACK)
            .spawn(move || console_thread(handle, inbox, &done))
            .map_err(|e| ForgeError::io("starting a pseudo console thread", e))?;
        Ok(Self {
            requests,
            size: AtomicU32::new(pack(size)),
            closed,
        })
    }
}

fn console_thread(handle: ConsoleHandle, inbox: mpsc::Receiver<Request>, closed: &AtomicBool) {
    let ConsoleHandle(hpc) = handle;
    // Ends on `Close` or when every sender is gone (the console was dropped).
    while let Ok(request) = inbox.recv() {
        match request {
            Request::Resize(size, reply) => {
                // SAFETY: `hpc` is open: only this thread closes it, below.
                let result = unsafe { ResizePseudoConsole(hpc, size) }
                    .map_err(|e| win_err("resizing a pseudo console", &e));
                let _ = reply.send(result);
            }
            Request::Close => break,
        }
    }
    // Later requests fail at once instead of waiting for a reply that never comes.
    drop(inbox);
    let started = Instant::now();
    // SAFETY: last use of `hpc`, which nothing else uses.
    unsafe { ClosePseudoConsole(hpc) };
    closed.store(true, Ordering::Release);
    tracing::trace!(
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "pseudo console closed"
    );
}

impl PtyConsole for ConPtyConsole {
    fn resize(&self, size: PtySize) -> Result<()> {
        let size = size.validated()?;
        if self.is_closed() {
            return Err(closed_error());
        }
        let (reply, answer) = mpsc::sync_channel(1);
        self.requests
            .send(Request::Resize(coord(size), reply))
            .map_err(|_| closed_error())?;
        match answer.recv_timeout(PTY_RESIZE_TIMEOUT) {
            Ok(Ok(())) => {
                self.size.store(pack(size), Ordering::Release);
                Ok(())
            }
            Ok(Err(err)) => Err(err),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(closed_error()),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ForgeError::Timeout {
                operation: "resizing a pseudo console".to_owned(),
                after: PTY_RESIZE_TIMEOUT,
            }),
        }
    }

    fn size(&self) -> PtySize {
        unpack(self.size.load(Ordering::Acquire))
    }

    fn close_within(&self, timeout: Duration) -> Result<()> {
        // Fails only when the thread already left its loop: a close is under way or done.
        let _ = self.requests.send(Request::Close);
        let started = Instant::now();
        let mut pause = CLOSE_POLL_FIRST;
        loop {
            if self.is_closed() {
                return Ok(());
            }
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Err(ForgeError::Timeout {
                    operation: "closing a pseudo console (is its output still being read?)"
                        .to_owned(),
                    after: timeout,
                });
            }
            std::thread::sleep(pause.min(timeout - elapsed));
            pause = (pause * 2).min(CLOSE_POLL_MAX);
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    //! Win32-level checks of the ConPTY implementation; the behavior through the public
    //! traits is covered by `tests/conpty_windows.rs`.

    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use ::windows::Win32::Foundation::{HWND, LPARAM};
    use ::windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use ::windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible,
    };
    use ::windows::core::BOOL;

    use super::*;
    use crate::platform::process::{GroupOptions, ProcessControl, ProcessControlOptions};
    use crate::platform::windows::WindowsProcessControl;

    const WAIT: Duration = Duration::from_secs(60);

    /// Env var that turns [`owner_helper`] into the owner process of
    /// [`owner_death_leaves_no_console_host`].
    const OWNER_HELPER_ENV: &str = "FORGEBOX_TEST_PTY_OWNER";

    struct Proc {
        pid: u32,
        parent: u32,
        name: String,
    }

    /// Every process of the machine (Toolhelp snapshot).
    fn processes() -> Vec<Proc> {
        // SAFETY: plain call; the snapshot handle is owned below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.unwrap();
        // SAFETY: fresh handle.
        let snapshot = unsafe { owned(snapshot) };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut out = Vec::new();
        // SAFETY: valid snapshot; `entry` is writable with `dwSize` set.
        let mut more = unsafe { Process32FirstW(raw(&snapshot), &mut entry) }.is_ok();
        while more {
            let name = &entry.szExeFile;
            let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            out.push(Proc {
                pid: entry.th32ProcessID,
                parent: entry.th32ParentProcessID,
                name: String::from_utf16_lossy(&name[..len]),
            });
            // SAFETY: as above.
            more = unsafe { Process32NextW(raw(&snapshot), &mut entry) }.is_ok();
        }
        out
    }

    /// Descendants of `root` by parent pid (children, grandchildren...).
    fn descendants(procs: &[Proc], root: u32) -> Vec<&Proc> {
        let mut seen = BTreeSet::from([root]);
        let mut out = Vec::new();
        let mut frontier = vec![root];
        while let Some(parent) = frontier.pop() {
            for proc in procs.iter().filter(|p| p.parent == parent) {
                if seen.insert(proc.pid) {
                    frontier.push(proc.pid);
                    out.push(proc);
                }
            }
        }
        out
    }

    /// Console hosts started by `owner` (ConPTY starts one per pseudo console).
    fn console_hosts(procs: &[Proc], owner: u32) -> Vec<u32> {
        procs
            .iter()
            .filter(|p| {
                p.parent == owner
                    && (p.name.eq_ignore_ascii_case("conhost.exe")
                        || p.name.eq_ignore_ascii_case("OpenConsole.exe"))
            })
            .map(|p| p.pid)
            .collect()
    }

    unsafe extern "system" fn collect_visible(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the Vec of `visible_windows`, exclusively borrowed for the
        // synchronous enumeration.
        let found = unsafe { &mut *(lparam.0 as *mut Vec<(isize, u32)>) };
        // SAFETY: `hwnd` comes from the enumeration.
        if unsafe { IsWindowVisible(hwnd) }.as_bool() {
            let mut pid = 0_u32;
            // SAFETY: as above; `pid` is writable.
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            found.push((hwnd.0 as isize, pid));
        }
        true.into()
    }

    /// Visible top-level windows of the desktop: `(hwnd, owner pid)`.
    fn visible_windows() -> Vec<(isize, u32)> {
        let mut found: Vec<(isize, u32)> = Vec::new();
        // SAFETY: the callback only uses `lparam` as `found`, which outlives the call.
        unsafe { EnumWindows(Some(collect_visible), LPARAM((&raw mut found) as isize)) }.unwrap();
        found
    }

    fn cmd_in(group: &dyn ProcessGroup, control: &WindowsProcessControl) -> PtySession {
        let system = super::super::process::system_directory();
        let mut cmd = PtyCommand::new(system.join("cmd.exe"), &system);
        cmd.args = vec![OsString::from("/d")];
        cmd.env = control.base_env();
        cmd.env.push(("PATH".into(), system.into_os_string()));
        WindowsPtySpawner.spawn(group, &cmd).unwrap()
    }

    /// Reads the output on a thread; chunks arrive on the channel until EOF.
    fn drain(session: &mut PtySession) -> mpsc::Receiver<Vec<u8>> {
        let mut reader = std::mem::replace(&mut session.reader, Box::new(std::io::empty()));
        let (sender, chunks) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = vec![0_u8; 64 * 1024];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                if sender.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        chunks
    }

    /// Waits until the output contains `needle`.
    fn wait_output(chunks: &mpsc::Receiver<Vec<u8>>, seen: &mut Vec<u8>, needle: &str) {
        let started = Instant::now();
        while !String::from_utf8_lossy(seen).contains(needle) {
            let left = WAIT.saturating_sub(started.elapsed());
            match chunks.recv_timeout(left) {
                Ok(chunk) => seen.extend(chunk),
                Err(err) => panic!(
                    "{needle:?} never appeared ({err}): {:?}",
                    String::from_utf8_lossy(seen)
                ),
            }
        }
    }

    /// No window appears: the console host of a pseudo console is headless and the shell
    /// is attached to it instead of getting a console window of its own.
    #[tokio::test]
    async fn no_console_window_is_shown() {
        let control = WindowsProcessControl::new(ProcessControlOptions::default());
        let root = control.create_group(GroupOptions::ROOT).unwrap();
        let group = root.create_child(GroupOptions::INSTANCE).unwrap();
        let mut session = cmd_in(&*group, &control);
        let chunks = drain(&mut session);
        let mut seen = Vec::new();
        wait_output(&chunks, &mut seen, ">");

        let procs = processes();
        let hosts = console_hosts(&procs, std::process::id());
        assert!(!hosts.is_empty(), "no console host child of this process");
        let mut ours: BTreeSet<u32> = hosts.into_iter().collect();
        ours.insert(session.pid);
        ours.extend(descendants(&procs, session.pid).iter().map(|p| p.pid));
        let shown: Vec<_> = visible_windows()
            .into_iter()
            .filter(|(_, pid)| ours.contains(pid))
            .collect();
        assert!(shown.is_empty(), "visible windows {shown:?} of {ours:?}");

        group.terminate_all(1).unwrap();
        session.close().unwrap();
    }

    /// The PID scan: a snapshot of the process table, independent of the Job, finds the
    /// shell's `ping -t` child; after `terminate_all` none of them is alive.
    #[tokio::test]
    async fn descendants_in_a_process_snapshot_die_with_the_group() {
        let control = WindowsProcessControl::new(ProcessControlOptions::default());
        let root = control.create_group(GroupOptions::ROOT).unwrap();
        let group = root.create_child(GroupOptions::INSTANCE).unwrap();
        let mut session = cmd_in(&*group, &control);
        let chunks = drain(&mut session);
        let mut seen = Vec::new();
        wait_output(&chunks, &mut seen, ">");
        session.writer.write_all(b"ping -t 127.0.0.1\r").unwrap();

        let started = Instant::now();
        let tree: Vec<(u32, String)> = loop {
            let procs = processes();
            let tree: Vec<_> = descendants(&procs, session.pid)
                .into_iter()
                .map(|p| (p.pid, p.name.clone()))
                .collect();
            if tree
                .iter()
                .any(|(_, name)| name.eq_ignore_ascii_case("PING.EXE"))
            {
                break tree;
            }
            assert!(
                started.elapsed() < WAIT,
                "no ping under the shell: {tree:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let members = group.pids().unwrap();
        for (pid, name) in &tree {
            assert!(members.contains(pid), "{name} ({pid}) escaped the group");
        }
        let mut watches = vec![control.watch(session.pid).unwrap()];
        for (pid, _) in &tree {
            watches.push(control.watch(*pid).unwrap());
        }

        group.terminate_all(9).unwrap();
        for watch in &mut watches {
            let code = watch.wait_timeout(Duration::from_secs(5)).await.unwrap();
            assert_eq!(code, Some(9), "process {}", watch.pid());
        }
        let after = processes();
        let left: Vec<_> = descendants(&after, session.pid)
            .into_iter()
            .map(|p| (p.pid, p.name.clone()))
            .collect();
        assert!(left.is_empty(), "left behind: {left:?}");
        session.close().unwrap();
    }

    /// Owner side of [`owner_death_leaves_no_console_host`]: holds a session (shell in a
    /// kill-on-close group, output never read) until its stdin closes. Does nothing
    /// unless started by that test.
    #[test]
    #[ignore = "helper process of owner_death_leaves_no_console_host"]
    fn owner_helper() {
        if std::env::var_os(OWNER_HELPER_ENV).is_none() {
            return;
        }
        let control = WindowsProcessControl::new(ProcessControlOptions::default());
        let root = control.create_group(GroupOptions::ROOT).unwrap();
        let group = root.create_child(GroupOptions::INSTANCE).unwrap();
        let session = cmd_in(&*group, &control);
        println!("pty-owner shell={}", session.pid);
        std::io::stdout().flush().unwrap();
        let _ = std::io::stdin().read_to_end(&mut Vec::new());
    }

    /// If the owner of a session dies (a daemon crash), nothing survives it: the shell dies
    /// with its group, and the console host exits once the kernel closes the owner's end
    /// of the console.
    #[tokio::test]
    async fn owner_death_leaves_no_console_host() {
        let mut owner = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::windows::conpty::tests::owner_helper",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(OWNER_HELPER_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = owner.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout)
                .lines()
                .map_while(std::result::Result::ok)
            {
                let _ = sender.send(line);
            }
        });
        let shell: u32 = loop {
            let line = lines.recv_timeout(WAIT).expect("the owner never reported");
            // libtest prints `test <name> ... ` first, on the same line.
            if let Some((_, pid)) = line.split_once("pty-owner shell=") {
                break pid.trim().parse().unwrap();
            }
        };
        let hosts = console_hosts(&processes(), owner.id());
        assert_eq!(hosts.len(), 1, "console hosts of the owner: {hosts:?}");
        let control = WindowsProcessControl::new(ProcessControlOptions::default());
        let mut shell_exit = control.watch(shell).unwrap();
        let mut host_exit = control.watch(hosts[0]).unwrap();

        owner.kill().unwrap();
        owner.wait().unwrap();
        let shell_code = shell_exit
            .wait_timeout(Duration::from_secs(10))
            .await
            .unwrap();
        assert!(shell_code.is_some(), "the shell outlived its owner");
        let host_code = host_exit
            .wait_timeout(Duration::from_secs(10))
            .await
            .unwrap();
        assert!(host_code.is_some(), "the console host outlived its owner");
    }
}
