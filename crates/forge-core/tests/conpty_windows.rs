//! Pseudo consoles on the real Windows ConPTY (M2-E3-T3, docs/03 §1.6), through the public
//! traits only: `PtySpawner`, `PtySession`, `ProcessGroup`. The Win32-level checks (no
//! visible console window, descendants found by a process snapshot, no console host left
//! behind when the owner dies) are unit tests in `src/platform/windows/conpty.rs`, the only
//! place allowed to call Win32.
//!
//! Every shell gets a generous deadline: antivirus and a loaded machine (other worktrees
//! compiling) can make `CreatePseudoConsole` and a PowerShell start take seconds.

#![cfg(windows)]

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use forge_core::error::{ForgeError, Result};
use forge_core::platform::{
    GroupOptions, ProcessCommand, ProcessControl, ProcessControlOptions, ProcessGroup, PtyCommand,
    PtyReader, PtySession, PtySize, SpawnedProcess, process_control, pty_spawner,
};

/// Deadline for a shell to start, answer or exit.
const WAIT: Duration = Duration::from_secs(60);

/// Output kept for assertions; past it, bytes are only counted.
const KEEP: usize = 8 * 1024 * 1024;

fn control() -> Arc<dyn ProcessControl> {
    process_control(ProcessControlOptions::default())
}

fn system32() -> PathBuf {
    control().system_path().remove(0)
}

/// Group nested in a root group, as the daemon does for a session.
fn session_group(control: &dyn ProcessControl) -> (Arc<dyn ProcessGroup>, Arc<dyn ProcessGroup>) {
    let root = control.create_group(GroupOptions::ROOT).unwrap();
    let group = root.create_child(GroupOptions::INSTANCE).unwrap();
    (root, group)
}

/// `exe` (in System32) with the minimal environment plus `PATH` = System32 and the
/// Windows PowerShell folder.
fn command(exe: &str, args: &[&str], cols: u16, rows: u16) -> PtyCommand {
    let system = system32();
    let mut cmd = PtyCommand::new(system.join(exe), &system);
    cmd.args = args.iter().map(OsString::from).collect();
    cmd.env = control().base_env();
    let path =
        std::env::join_paths([system.clone(), system.join(r"WindowsPowerShell\v1.0")]).unwrap();
    cmd.env.push(("PATH".into(), path));
    cmd.cols = cols;
    cmd.rows = rows;
    cmd
}

fn spawn(group: &dyn ProcessGroup, cmd: &PtyCommand) -> PtySession {
    pty_spawner()
        .spawn(group, cmd)
        .unwrap_or_else(|err| panic!("spawning {} in a pseudo console: {err}", cmd.exe.display()))
}

/// What the reading thread saw.
#[derive(Default)]
struct Captured {
    bytes: Vec<u8>,
    total: u64,
    eof: bool,
    error: Option<String>,
}

/// Reads a session's output on its own thread until EOF, as the daemon will.
struct Drain {
    shared: Arc<(Mutex<Captured>, Condvar)>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drain {
    fn start(mut reader: PtyReader) -> Self {
        let shared = Arc::new((Mutex::new(Captured::default()), Condvar::new()));
        let state = Arc::clone(&shared);
        let thread = thread::spawn(move || {
            let mut buf = vec![0_u8; 64 * 1024];
            loop {
                let read = reader.read(&mut buf);
                let (lock, changed) = &*state;
                let mut captured = lock.lock().unwrap();
                match read {
                    Ok(0) => captured.eof = true,
                    Ok(n) => {
                        captured.total += n as u64;
                        let room = KEEP.saturating_sub(captured.bytes.len());
                        captured.bytes.extend_from_slice(&buf[..n.min(room)]);
                    }
                    Err(err) => {
                        captured.error = Some(err.to_string());
                        captured.eof = true;
                    }
                }
                let done = captured.eof;
                drop(captured);
                changed.notify_all();
                if done {
                    return;
                }
            }
        });
        Self {
            shared,
            thread: Some(thread),
        }
    }

    /// Waits until `done` holds for what was captured; `false` on timeout.
    fn wait_until(&self, timeout: Duration, done: impl Fn(&Captured) -> bool) -> bool {
        let (lock, changed) = &*self.shared;
        let captured = lock.lock().unwrap();
        let (captured, result) = changed
            .wait_timeout_while(captured, timeout, |c| !done(c))
            .unwrap();
        drop(captured);
        !result.timed_out()
    }

    /// The screen text seen so far (VT sequences removed).
    fn text(&self) -> String {
        strip_vt(&self.shared.0.lock().unwrap().bytes)
    }

    fn total(&self) -> u64 {
        self.shared.0.lock().unwrap().total
    }

    /// Waits for `needle` in the screen text; panics with the text on timeout.
    fn expect(&self, needle: &str) {
        let found = self.wait_until(WAIT, |c| strip_vt(&c.bytes).contains(needle));
        assert!(found, "{needle:?} never appeared in:\n{}", self.text());
    }

    /// Waits for EOF and returns the screen text.
    fn finish(mut self, timeout: Duration) -> String {
        let eof = self.wait_until(timeout, |c| c.eof);
        assert!(eof, "no EOF after {timeout:?}; output:\n{}", self.text());
        self.thread.take().unwrap().join().unwrap();
        let captured = self.shared.0.lock().unwrap();
        assert_eq!(captured.error, None, "reading the output");
        strip_vt(&captured.bytes)
    }
}

/// Removes VT sequences from the console rendering: CSI (`ESC [ … final`), OSC
/// (`ESC ] … BEL|ST`) and two-byte escapes. Cursor-forward (`ESC [ n C`) becomes `n`
/// spaces and cursor positioning (`ESC [ … H`) a line break, so words stay apart.
fn strip_vt(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            if c != '\r' {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                let mut params = String::new();
                for n in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&n) {
                        match n {
                            'C' => {
                                let count = params.parse::<usize>().unwrap_or(1);
                                out.extend(std::iter::repeat_n(' ', count));
                            }
                            'H' => out.push('\n'),
                            _ => {}
                        }
                        break;
                    }
                    params.push(n);
                }
            }
            Some(']') => {
                while let Some(n) = chars.next() {
                    if n == '\x07' {
                        break;
                    }
                    if n == '\x1b' {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

async fn exit_code(session: &mut PtySession) -> u32 {
    session
        .exit
        .wait_timeout(WAIT)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("process {} did not exit within {WAIT:?}", session.pid))
}

fn type_line(session: &mut PtySession, line: &str) {
    session.writer.write_all(line.as_bytes()).unwrap();
    session.writer.write_all(b"\r").unwrap();
    session.writer.flush().unwrap();
}

#[tokio::test]
async fn echo_reaches_the_output_and_exit_zero_is_reported() {
    let control = control();
    let (_root, group) = session_group(&*control);
    let mut session = spawn(
        &*group,
        &command("cmd.exe", &["/d", "/c", "echo hello"], 80, 24),
    );
    let drain = Drain::start(std::mem::replace(
        &mut session.reader,
        Box::new(std::io::empty()),
    ));
    assert_eq!(exit_code(&mut session).await, 0);
    // The output only ends once the console is closed, even though the process exited.
    session.close().unwrap();
    assert!(session.console.is_closed());
    let text = drain.finish(WAIT);
    assert!(text.contains("hello"), "{text:?}");
}

#[tokio::test]
async fn exit_code_is_reported() {
    let control = control();
    let (_root, group) = session_group(&*control);
    let mut session = spawn(
        &*group,
        &command("cmd.exe", &["/d", "/c", "exit 7"], 80, 24),
    );
    let drain = Drain::start(std::mem::replace(
        &mut session.reader,
        Box::new(std::io::empty()),
    ));
    assert_eq!(exit_code(&mut session).await, 7);
    session.close().unwrap();
    drain.finish(WAIT);
}

/// Typed UTF-8 reaches the program as the right characters, and what it prints comes
/// back as UTF-8, whatever the console code page: `[ação ✓]` only appears as cmd's own
/// output (the typed line has `%X%`).
#[tokio::test]
async fn utf8_round_trip() {
    let control = control();
    let (_root, group) = session_group(&*control);
    let mut session = spawn(&*group, &command("cmd.exe", &["/d"], 100, 30));
    let drain = Drain::start(std::mem::replace(
        &mut session.reader,
        Box::new(std::io::empty()),
    ));
    drain.expect(">");
    type_line(&mut session, "set X=ação ✓");
    type_line(&mut session, "echo [%X%]");
    drain.expect("[ação ✓]");
    type_line(&mut session, "exit 3");
    assert_eq!(exit_code(&mut session).await, 3);
    session.close().unwrap();
    let text = drain.finish(WAIT);
    assert!(text.contains("[ação ✓]"), "{text:?}");
}

/// The size is the one asked for at spawn and, after a resize, the new one, as PowerShell
/// itself reports it.
#[tokio::test]
async fn resize_is_reflected_in_powershell() {
    let control = control();
    let (_root, group) = session_group(&*control);
    let mut session = spawn(
        &*group,
        &command(
            "WindowsPowerShell\\v1.0\\powershell.exe",
            &["-NoLogo", "-NoProfile"],
            80,
            25,
        ),
    );
    let drain = Drain::start(std::mem::replace(
        &mut session.reader,
        Box::new(std::io::empty()),
    ));
    let query = r#"$s = $Host.UI.RawUI.WindowSize; "SIZE=$($s.Width)x$($s.Height)""#;
    drain.expect("PS ");
    type_line(&mut session, query);
    drain.expect("SIZE=80x25");

    session.resize(120, 40).unwrap();
    assert_eq!(
        session.console.size(),
        PtySize {
            cols: 120,
            rows: 40
        }
    );
    type_line(&mut session, query);
    drain.expect("SIZE=120x40");

    type_line(&mut session, "exit 5");
    assert_eq!(exit_code(&mut session).await, 5);
    session.close().unwrap();
    drain.finish(WAIT);
}

/// A shell and what it starts (`ping -t`, which never ends by itself) are members of the
/// group from the start and all die with `terminate_all`.
#[tokio::test]
async fn shell_descendants_die_with_the_group() {
    let control = control();
    let (_root, group) = session_group(&*control);
    let mut session = spawn(&*group, &command("cmd.exe", &["/d"], 80, 24));
    assert_eq!(
        group.pids().unwrap(),
        vec![session.pid],
        "a member before it ran"
    );
    let drain = Drain::start(std::mem::replace(
        &mut session.reader,
        Box::new(std::io::empty()),
    ));
    drain.expect(">");
    type_line(&mut session, "ping -t 127.0.0.1");

    let started = Instant::now();
    let members = loop {
        let pids = group.pids().unwrap();
        if pids.len() >= 2 {
            break pids;
        }
        assert!(
            started.elapsed() < WAIT,
            "ping never joined the group: {pids:?}"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert!(members.contains(&session.pid), "{members:?}");
    let mut watches: Vec<_> = members
        .iter()
        .map(|&pid| control.watch(pid).unwrap())
        .collect();

    group.terminate_all(9).unwrap();
    for (pid, watch) in members.iter().zip(&mut watches) {
        let code = watch.wait_timeout(Duration::from_secs(5)).await.unwrap();
        assert_eq!(code, Some(9), "process {pid}");
        assert!(!control.is_alive(*pid).unwrap(), "process {pid}");
    }
    assert!(group.pids().unwrap().is_empty());
    assert_eq!(exit_code(&mut session).await, 9);
    session.close().unwrap();
    drain.finish(WAIT);
}

fn flood_file(dir: &Path, bytes: usize) -> PathBuf {
    let path = dir.join("flood.txt");
    let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let mut written = 0;
    let mut n = 0_u64;
    while written < bytes {
        let line = format!("flood line {n:08} {}\r\n", "x".repeat(78));
        file.write_all(line.as_bytes()).unwrap();
        written += line.len();
        n += 1;
    }
    file.flush().unwrap();
    path
}

/// Closing while the program floods the console (~20 MB) returns fast, with the output
/// being drained as the daemon does. ConPTY renders such a flood at ~1 MB/s on the dev
/// machine (Windows 10 19045 over RDP: ~19 s for the whole file), so the close lands
/// mid-flood after the first MiB.
#[tokio::test]
async fn close_under_a_flood_returns_quickly() {
    let dir = tempfile::tempdir().unwrap();
    let flood = flood_file(dir.path(), 20 * 1024 * 1024);
    let control = control();
    let (_root, group) = session_group(&*control);
    let mut session = spawn(
        &*group,
        &command(
            "cmd.exe",
            &["/d", "/c", "type", flood.to_str().unwrap()],
            120,
            30,
        ),
    );
    let drain = Drain::start(std::mem::replace(
        &mut session.reader,
        Box::new(std::io::empty()),
    ));
    let flooding = drain.wait_until(WAIT, |c| c.total >= 1024 * 1024);
    assert!(flooding, "the flood never started: {} bytes", drain.total());
    assert_eq!(
        session.exit.try_exit_code().unwrap(),
        None,
        "still flooding"
    );

    let started = Instant::now();
    session.close().unwrap();
    let elapsed = started.elapsed();
    println!(
        "close under flood: {elapsed:?} after {} bytes of output",
        drain.total()
    );
    assert!(elapsed < Duration::from_secs(2), "close took {elapsed:?}");
    drain.finish(Duration::from_secs(10));
    // The console's processes end with it.
    let code = session
        .exit
        .wait_timeout(Duration::from_secs(10))
        .await
        .unwrap();
    assert!(code.is_some(), "the flooding process outlived its console");
    group.terminate_all(1).unwrap();
}

/// A close while nobody reads a full output pipe (the Windows 10 hang) gives up after its
/// timeout instead of blocking the caller, and completes once the output is read.
#[tokio::test]
async fn close_without_draining_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let flood = flood_file(dir.path(), 4 * 1024 * 1024);
    let control = control();
    let (_root, group) = session_group(&*control);
    let mut session = spawn(
        &*group,
        &command(
            "cmd.exe",
            &["/d", "/c", "type", flood.to_str().unwrap()],
            120,
            30,
        ),
    );
    // Let the console fill the pipe while nothing reads it.
    thread::sleep(Duration::from_secs(1));
    let limit = Duration::from_millis(1500);
    let started = Instant::now();
    let closed = session.console.close_within(limit);
    let elapsed = started.elapsed();
    println!("close without draining: {closed:?} after {elapsed:?}");
    assert!(elapsed < limit + Duration::from_millis(500), "{elapsed:?}");
    match closed {
        Ok(()) => assert!(session.console.is_closed()),
        Err(ForgeError::Timeout { .. }) => assert!(!session.console.is_closed()),
        Err(err) => panic!("{err}"),
    }
    // Draining lets the pending close finish; a second close waits for the same one.
    let drain = Drain::start(std::mem::replace(
        &mut session.reader,
        Box::new(std::io::empty()),
    ));
    session.console.close_within(WAIT).unwrap();
    assert!(session.console.is_closed());
    drain.finish(WAIT);
    group.terminate_all(1).unwrap();
}

#[tokio::test]
async fn closed_console_refuses_resize_and_closes_idempotently() {
    let control = control();
    let (_root, group) = session_group(&*control);
    let mut session = spawn(&*group, &command("cmd.exe", &["/d"], 80, 24));
    let drain = Drain::start(std::mem::replace(
        &mut session.reader,
        Box::new(std::io::empty()),
    ));
    assert_eq!(session.console.size(), PtySize { cols: 80, rows: 24 });
    assert_eq!(
        session.resize(0, 10).unwrap_err().code(),
        "invalid_input",
        "size validated before reaching the console"
    );
    session.resize(100, 30).unwrap();
    session.close().unwrap();
    session.close().unwrap();
    assert_eq!(session.resize(90, 30).unwrap_err().code(), "conflict");
    assert_eq!(
        session.console.size(),
        PtySize {
            cols: 100,
            rows: 30
        }
    );
    drain.finish(WAIT);
    // cmd was attached to the closed console: it ends with it.
    assert!(
        session
            .exit
            .wait_timeout(Duration::from_secs(10))
            .await
            .unwrap()
            .is_some()
    );
    group.terminate_all(1).unwrap();
}

#[test]
fn bad_commands_are_refused() {
    let control = control();
    let (_root, group) = session_group(&*control);
    let system = system32();
    let spawner = pty_spawner();
    let check = |cmd: PtyCommand, code: &str| {
        let err = spawner.spawn(&*group, &cmd).err().unwrap();
        assert_eq!(err.code(), code, "{}: {err}", cmd.exe.display());
    };
    check(
        PtyCommand::new(r"relative\cmd.exe", &system),
        "invalid_input",
    );
    check(
        PtyCommand::new(system.join("x.cmd"), &system),
        "invalid_input",
    );
    check(
        PtyCommand::new(system.join("x.BAT"), &system),
        "invalid_input",
    );
    check(
        PtyCommand::new(system.join("no-such-forgebox.exe"), &system),
        "not_found",
    );
    check(
        PtyCommand::new(system.join("cmd.exe"), system.join("no-such-dir")),
        "not_found",
    );
    let mut cmd = command("cmd.exe", &[], 80, 24);
    cmd.rows = 0;
    check(cmd, "invalid_input");
    let mut cmd = command("cmd.exe", &[], PtySize::MAX_COLS + 1, 24);
    check(cmd.clone(), "invalid_input");
    cmd.cols = 80;
    cmd.args = vec![OsString::from("bad\0arg")];
    check(cmd, "invalid_input");
    assert!(group.pids().unwrap().is_empty());
}

/// A group that refuses the new process: the spawn fails and the process never ran (its
/// command would have created a file), so the assignment happened before `ResumeThread`.
#[derive(Debug, Default)]
struct RefusingGroup {
    asked: Mutex<Vec<u32>>,
}

impl ProcessGroup for RefusingGroup {
    fn create_child(&self, _: GroupOptions) -> Result<Arc<dyn ProcessGroup>> {
        unreachable!()
    }
    fn spawn(&self, _: &ProcessCommand) -> Result<SpawnedProcess> {
        unreachable!()
    }
    fn assign(&self, pid: u32) -> Result<()> {
        self.asked.lock().unwrap().push(pid);
        Err(ForgeError::Conflict("this group takes no members".into()))
    }
    fn pids(&self) -> Result<Vec<u32>> {
        Ok(Vec::new())
    }
    fn terminate_all(&self, _: u32) -> Result<()> {
        Ok(())
    }
}

#[test]
fn a_refused_process_never_runs() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran.txt");
    let group = RefusingGroup::default();
    let cmd = command(
        "cmd.exe",
        &["/d", "/c", &format!("echo ran> \"{}\"", marker.display())],
        80,
        24,
    );
    let err = pty_spawner().spawn(&group, &cmd).err().unwrap();
    assert_eq!(err.code(), "conflict", "{err}");
    let asked = group.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1, "{asked:?}");
    let control = control();
    let started = Instant::now();
    while control.is_alive(asked[0]).unwrap() {
        assert!(started.elapsed() < Duration::from_secs(5), "never killed");
        thread::sleep(Duration::from_millis(20));
    }
    thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists(), "the refused process ran");
}
