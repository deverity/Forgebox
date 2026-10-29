//! Job Objects, process creation and exit watching (docs/03 §1.1–1.2).
//!
//! * A group is a Job Object. A nested group becomes a child of its parent the first
//!   time a process is assigned to both (nested jobs, Windows 8+): processes are always
//!   assigned from the root job down to the target job, so the hierarchy is the same
//!   whether or not the daemon itself already runs inside a job (cargo, terminals and IDEs
//!   put their children in jobs).
//! * `spawn` = `CreateProcessW(CREATE_SUSPENDED | …)` → `AssignProcessToJobObject` for
//!   every job of the chain → `ResumeThread`: the child, and anything it starts, never
//!   runs outside the job. If the assignment fails the child is terminated before it ran
//!   a single instruction.
//! * Job termination is asynchronous. `TerminateJobObject` returns before the processes
//!   exited, and the job then refuses `AssignProcessToJobObject` with
//!   `ERROR_ACCESS_DENIED` for a few milliseconds **even after** its last process left
//!   (measured on Windows 10 19045: `ActiveProcesses` reaches 0 within ~100 µs, the
//!   refusals last 1–8 ms when idle and tens of ms under load; nothing observable —
//!   the job's signaled state, its accounting, `SetInformationJobObject` — tells when
//!   they stop). So `terminate_all` waits until `ActiveProcesses == 0` (bounded by
//!   [`TERMINATE_TIMEOUT`]) and the assignment retries `ERROR_ACCESS_DENIED` with a
//!   short backoff for at most [`ASSIGN_RETRY_LIMIT`].
//! * Inheritance is limited to the three std handles with
//!   `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`: concurrent spawns never leak each other's pipe
//!   ends (a leaked write end would delay EOF on a service's stdout).
//! * Captured stdout/stderr are named pipes, like `std::process` does: our end is
//!   overlapped and registered with the tokio reactor (IOCP, no thread per pipe); the
//!   child's end is synchronous and inheritable. Pipe buffers are 64 KiB: a child writing
//!   faster than we read blocks in `WriteFile`, as on a terminal.
//! * Exit is watched with `RegisterWaitForSingleObject` (OS thread pool) completing a
//!   oneshot channel: no blocking thread per process.
//! * The launch helpers (checks, command line, environment block, attribute list, exit
//!   watch) are shared with the pseudo consoles of `conpty.rs`.

use std::ffi::{OsStr, OsString, c_void};
use std::future::Future as _;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use std::{fmt, io};

use ::windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_BAD_EXE_FORMAT, ERROR_DIRECTORY, ERROR_ELEVATION_REQUIRED,
    ERROR_FILE_NOT_FOUND, ERROR_INVALID_PARAMETER, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use ::windows::Win32::Security::SECURITY_ATTRIBUTES;
use ::windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_GENERIC_READ, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_ALWAYS, OPEN_EXISTING,
};
use ::windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_PROCESS_ID_LIST,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject,
};
use ::windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use ::windows::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROCESS_ACCESS_RIGHTS, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    RegisterWaitForSingleObject, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    TerminateProcess, UnregisterWaitEx, UpdateProcThreadAttribute, WORKER_THREAD_FLAGS,
    WT_EXECUTEINWAITTHREAD, WT_EXECUTEONLYONCE, WaitForSingleObject,
};
use ::windows::core::{PCWSTR, PWSTR};
use tokio::net::windows::named_pipe::{PipeMode, ServerOptions};
use tokio::sync::oneshot;

use crate::error::{ForgeError, Result};
use crate::platform::process::{
    ASSIGN_RETRY_LIMIT, ConsoleMode, ExitSource, GroupOptions, PipeReader, ProcessCommand,
    ProcessControl, ProcessControlOptions, ProcessExit, ProcessGroup, SpawnedProcess, StdioTarget,
    TERMINATE_TIMEOUT,
};

/// Size of each captured pipe buffer.
const PIPE_BUFFER: u32 = 64 * 1024;

/// First and longest pause between two checks of a settling job (the backoff doubles).
const SETTLE_POLL_FIRST: Duration = Duration::from_micros(250);
const SETTLE_POLL_MAX: Duration = Duration::from_millis(20);

/// Variables every child inherits from the daemon (docs/03 §1.2: minimal env, never PATH).
const BASE_ENV_VARS: &[&str] = &[
    "ALLUSERSPROFILE",
    "APPDATA",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "CommonProgramW6432",
    "COMPUTERNAME",
    "ComSpec",
    "HOMEDRIVE",
    "HOMEPATH",
    "LOCALAPPDATA",
    "NUMBER_OF_PROCESSORS",
    "OS",
    "PATHEXT",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "PROCESSOR_LEVEL",
    "PROCESSOR_REVISION",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "PUBLIC",
    "SystemDrive",
    "SystemRoot",
    "TEMP",
    "TMP",
    "USERDOMAIN",
    "USERNAME",
    "USERPROFILE",
    "windir",
];

// ---------------------------------------------------------------------------------------
// Errors and small helpers
// ---------------------------------------------------------------------------------------

/// `windows::core::Error` → `io::Error`, keeping the Win32 code when the HRESULT wraps one.
pub(crate) fn os_error(err: &::windows::core::Error) -> io::Error {
    match win32_code(err) {
        Some(code) => io::Error::from_raw_os_error(code as i32),
        None => io::Error::other(err.message()),
    }
}

/// Win32 error code inside an `HRESULT_FROM_WIN32` value.
pub(crate) fn win32_code(err: &::windows::core::Error) -> Option<u32> {
    let hr = err.code().0 as u32;
    (hr & 0xFFFF_0000 == 0x8007_0000).then_some(hr & 0xFFFF)
}

pub(crate) fn win_err(context: impl Into<String>, err: &::windows::core::Error) -> ForgeError {
    ForgeError::io(context, os_error(err))
}

/// NUL-terminated UTF-16.
pub(crate) fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

pub(crate) fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

/// Takes ownership of a handle returned by a successful Win32 call.
///
/// # Safety
/// `handle` must be a valid handle that nobody else closes.
pub(crate) unsafe fn owned(handle: HANDLE) -> OwnedHandle {
    // SAFETY: guaranteed by the caller.
    unsafe { OwnedHandle::from_raw_handle(handle.0) }
}

fn inheritable() -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    }
}

pub(crate) fn open_process(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> Result<OwnedHandle> {
    // SAFETY: plain call; the returned handle is owned below.
    match unsafe { OpenProcess(access, false, pid) } {
        // SAFETY: OpenProcess returned a fresh handle.
        Ok(handle) => Ok(unsafe { owned(handle) }),
        Err(err) if win32_code(&err) == Some(ERROR_INVALID_PARAMETER.0) => {
            Err(ForgeError::NotFound(format!("process {pid}")))
        }
        Err(err) => Err(win_err(format!("opening process {pid}"), &err)),
    }
}

// ---------------------------------------------------------------------------------------
// Job Objects
// ---------------------------------------------------------------------------------------

/// One Job Object and its ancestors.
pub(crate) struct Job {
    handle: OwnedHandle,
    parent: Option<Arc<Job>>,
    opts: GroupOptions,
}

impl fmt::Debug for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Job")
            .field("handle", &self.handle.as_raw_handle())
            .field("depth", &self.chain().len())
            .field("opts", &self.opts)
            .finish()
    }
}

impl Job {
    fn create(opts: GroupOptions, parent: Option<Arc<Job>>) -> Result<Arc<Self>> {
        // SAFETY: default security (handle not inheritable), unnamed job.
        let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
            .map_err(|e| win_err("creating a job object", &e))?;
        // SAFETY: fresh handle owned from here on.
        let handle = unsafe { owned(handle) };

        let mut flags = 0;
        if opts.kill_on_close {
            flags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE.0;
        }
        if opts.breakaway_ok {
            flags |= JOB_OBJECT_LIMIT_BREAKAWAY_OK.0;
        }
        if opts.die_on_unhandled_exception {
            flags |= JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION.0;
        }
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT(flags);
        // SAFETY: `info` is a valid JOBOBJECT_EXTENDED_LIMIT_INFORMATION of the size
        // passed, readable for the whole call.
        unsafe {
            SetInformationJobObject(
                raw(&handle),
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(|e| win_err("setting job object limits", &e))?;
        Ok(Arc::new(Self {
            handle,
            parent,
            opts,
        }))
    }

    /// Root first, `self` last.
    fn chain(&self) -> Vec<&Job> {
        let mut chain = vec![self];
        let mut cursor = self.parent.as_deref();
        while let Some(job) = cursor {
            chain.push(job);
            cursor = job.parent.as_deref();
        }
        chain.reverse();
        chain
    }

    /// Assigns `process` to every job of the chain, root first. A job that refuses it
    /// with `ERROR_ACCESS_DENIED` (a termination still settling, see the module docs) is
    /// retried with backoff for at most [`ASSIGN_RETRY_LIMIT`]. Returns how many attempts
    /// were refused before the process was accepted everywhere.
    fn assign_process(&self, process: HANDLE) -> Result<u32> {
        let started = Instant::now();
        let mut refused = 0_u32;
        let mut pause = SETTLE_POLL_FIRST;
        for job in self.chain() {
            loop {
                // SAFETY: both handles are valid for the duration of the call.
                match unsafe { AssignProcessToJobObject(raw(&job.handle), process) } {
                    Ok(()) => break,
                    Err(err)
                        if win32_code(&err) == Some(ERROR_ACCESS_DENIED.0)
                            && started.elapsed() < ASSIGN_RETRY_LIMIT =>
                    {
                        refused += 1;
                        std::thread::sleep(pause);
                        pause = (pause * 2).min(SETTLE_POLL_MAX);
                    }
                    Err(err) if refused > 0 => {
                        return Err(win_err(
                            format!(
                                "assigning a process to its job object (refused for {} ms: \
                                 the job may still be terminating its previous processes)",
                                started.elapsed().as_millis()
                            ),
                            &err,
                        ));
                    }
                    Err(err) => {
                        return Err(win_err("assigning a process to its job object", &err));
                    }
                }
            }
        }
        if refused > 0 {
            tracing::debug!(
                refused,
                waited_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                "job object accepted the process once its termination settled"
            );
        }
        Ok(refused)
    }

    /// Processes currently running in the job (nested jobs included).
    fn active_processes(&self) -> Result<u32> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: `info` is a writable JOBOBJECT_BASIC_ACCOUNTING_INFORMATION of the size
        // passed.
        unsafe {
            QueryInformationJobObject(
                Some(raw(&self.handle)),
                JobObjectBasicAccountingInformation,
                (&raw mut info).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                None,
            )
        }
        .map_err(|e| win_err("reading the accounting of a job object", &e))?;
        Ok(info.ActiveProcesses)
    }

    /// Waits at most `timeout` until no process runs in the job; `false` on timeout.
    fn wait_empty(&self, timeout: Duration) -> Result<bool> {
        let started = Instant::now();
        let mut pause = SETTLE_POLL_FIRST;
        loop {
            if self.active_processes()? == 0 {
                return Ok(true);
            }
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Ok(false);
            }
            std::thread::sleep(pause.min(timeout - elapsed));
            pause = (pause * 2).min(SETTLE_POLL_MAX);
        }
    }

    fn pids(&self) -> Result<Vec<u32>> {
        let mut capacity = 32_usize;
        loop {
            let bytes = size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()
                + capacity.saturating_sub(1) * size_of::<usize>();
            // usize words keep the struct alignment.
            let mut buf = vec![0_usize; bytes.div_ceil(size_of::<usize>())];
            let len = (buf.len() * size_of::<usize>()) as u32;
            // SAFETY: `buf` is writable, aligned for the struct and `len` bytes long.
            let result = unsafe {
                QueryInformationJobObject(
                    Some(raw(&self.handle)),
                    JobObjectBasicProcessIdList,
                    buf.as_mut_ptr().cast(),
                    len,
                    None,
                )
            };
            // SAFETY: `buf` starts with a JOBOBJECT_BASIC_PROCESS_ID_LIST header (zeroed or
            // filled by the call).
            let list = unsafe { &*buf.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() };
            let assigned = list.NumberOfAssignedProcesses as usize;
            match result {
                Ok(()) if assigned <= list.NumberOfProcessIdsInList as usize => {
                    let count = list.NumberOfProcessIdsInList as usize;
                    // SAFETY: the kernel wrote `count` ids starting at ProcessIdList, all
                    // inside `buf`.
                    let ids = unsafe {
                        std::slice::from_raw_parts(
                            (&raw const list.ProcessIdList).cast::<usize>(),
                            count,
                        )
                    };
                    return Ok(ids.iter().map(|&pid| pid as u32).collect());
                }
                Ok(()) => capacity = assigned + 16,
                Err(err) if win32_code(&err) == Some(ERROR_MORE_DATA.0) => {
                    capacity = assigned.max(capacity * 2) + 16;
                }
                Err(err) => return Err(win_err("listing the processes of a job object", &err)),
            }
        }
    }

    /// `TerminateJobObject` alone: returns while the processes may still be exiting.
    fn terminate(&self, exit_code: u32) -> Result<()> {
        // SAFETY: valid job handle.
        match unsafe { TerminateJobObject(raw(&self.handle), exit_code) } {
            Ok(()) => Ok(()),
            // A termination already in flight: the caller's wait covers it.
            Err(err) if win32_code(&err) == Some(ERROR_ACCESS_DENIED.0) => Ok(()),
            Err(err) => Err(win_err("terminating a job object", &err)),
        }
    }

    /// Terminates the job and waits until it is empty (see the module docs).
    fn terminate_all(&self, exit_code: u32) -> Result<()> {
        self.terminate(exit_code)?;
        if self.wait_empty(TERMINATE_TIMEOUT)? {
            return Ok(());
        }
        Err(ForgeError::Timeout {
            operation: format!(
                "waiting for the {} processes of a terminated job object to exit",
                self.active_processes().unwrap_or(0)
            ),
            after: TERMINATE_TIMEOUT,
        })
    }
}

/// [`ProcessGroup`] backed by a Job Object.
#[derive(Debug, Clone)]
struct JobGroup {
    job: Arc<Job>,
}

impl ProcessGroup for JobGroup {
    fn create_child(&self, opts: GroupOptions) -> Result<Arc<dyn ProcessGroup>> {
        let job = Job::create(opts, Some(Arc::clone(&self.job)))?;
        Ok(Arc::new(JobGroup { job }))
    }

    fn spawn(&self, cmd: &ProcessCommand) -> Result<SpawnedProcess> {
        let created = create_process(Some(&self.job), cmd)?;
        let exit = ExitWatch::new(created.process)?;
        Ok(SpawnedProcess {
            pid: created.pid,
            stdout: created.stdout,
            stderr: created.stderr,
            exit: ProcessExit::new(created.pid, Box::new(exit)),
        })
    }

    fn assign(&self, pid: u32) -> Result<()> {
        let process = open_process(
            pid,
            PROCESS_ACCESS_RIGHTS(PROCESS_SET_QUOTA.0 | PROCESS_TERMINATE.0),
        )?;
        self.job.assign_process(raw(&process)).map(drop)
    }

    fn pids(&self) -> Result<Vec<u32>> {
        self.job.pids()
    }

    fn terminate_all(&self, exit_code: u32) -> Result<()> {
        self.job.terminate_all(exit_code)
    }
}

// ---------------------------------------------------------------------------------------
// Process creation
// ---------------------------------------------------------------------------------------

/// A created, assigned and resumed process.
pub(crate) struct Created {
    pub pid: u32,
    pub process: OwnedHandle,
    pub stdout: Option<PipeReader>,
    pub stderr: Option<PipeReader>,
}

/// Child ends of the std handles (closed right after `CreateProcessW`) and our pipe ends.
struct Stdio {
    stdin: OwnedHandle,
    stdout: OwnedHandle,
    /// `None`: stderr shares the stdout handle (same file).
    stderr: Option<OwnedHandle>,
    our_stdout: Option<PipeReader>,
    our_stderr: Option<PipeReader>,
}

fn open_null(write: bool) -> Result<OwnedHandle> {
    let access = if write {
        FILE_APPEND_DATA.0 | FILE_READ_ATTRIBUTES.0
    } else {
        FILE_GENERIC_READ.0
    };
    let sa = inheritable();
    let name = wide(OsStr::new("NUL"));
    // SAFETY: `name` is NUL-terminated; `sa` outlives the call.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            access,
            FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0),
            Some(&raw const sa),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|e| win_err("opening the null device", &e))?;
    // SAFETY: fresh handle.
    Ok(unsafe { owned(handle) })
}

fn open_append(path: &Path) -> Result<OwnedHandle> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ForgeError::io_at("creating", parent, e))?;
    }
    let sa = inheritable();
    let name = wide(path.as_os_str());
    // SAFETY: `name` is NUL-terminated; `sa` outlives the call. FILE_APPEND_DATA without
    // FILE_WRITE_DATA makes every write an append.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            FILE_APPEND_DATA.0 | FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0),
            Some(&raw const sa),
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|e| ForgeError::io(format!("opening {}", path.display()), os_error(&e)))?;
    // SAFETY: fresh handle.
    Ok(unsafe { owned(handle) })
}

/// Named pipe: our overlapped reading end on the tokio reactor + the child's synchronous,
/// inheritable writing end.
fn pipe() -> Result<(PipeReader, OwnedHandle)> {
    if tokio::runtime::Handle::try_current().is_err() {
        return Err(ForgeError::Internal(
            "captured stdio needs a tokio runtime context (call spawn inside spawn_blocking)"
                .to_owned(),
        ));
    }
    let name = format!(
        r"\\.\pipe\forgebox-stdio-{}-{}",
        std::process::id(),
        ulid::Ulid::generate()
    );
    let server = ServerOptions::new()
        .access_inbound(true)
        .access_outbound(false)
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .pipe_mode(PipeMode::Byte)
        .max_instances(1)
        .in_buffer_size(PIPE_BUFFER)
        .out_buffer_size(PIPE_BUFFER)
        .create(&name)
        .map_err(|e| ForgeError::io("creating a stdio pipe", e))?;
    let sa = inheritable();
    let wide_name = wide(OsStr::new(&name));
    // SAFETY: `wide_name` is NUL-terminated; `sa` outlives the call. Synchronous handle
    // (no FILE_FLAG_OVERLAPPED): children expect blocking std handles.
    let client = unsafe {
        CreateFileW(
            PCWSTR(wide_name.as_ptr()),
            ::windows::Win32::Foundation::GENERIC_WRITE.0 | FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_MODE(0),
            Some(&raw const sa),
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .map_err(|e| win_err("opening the child end of a stdio pipe", &e))?;
    // SAFETY: fresh handle.
    let client = unsafe { owned(client) };
    // The reactor tried to start a background read when the server was registered,
    // before any client existed, and gave up (ERROR_PIPE_LISTENING). "Connecting" now
    // that the client end is open completes at once (ERROR_PIPE_CONNECTED) and starts
    // the reads; without it the reader would never see any data.
    match futures::FutureExt::now_or_never(server.connect()) {
        Some(Ok(())) | None => {}
        Some(Err(err)) => return Err(ForgeError::io("connecting a stdio pipe", err)),
    }
    Ok((Box::pin(server), client))
}

fn prepare_stdio(cmd: &ProcessCommand) -> Result<Stdio> {
    let stdin = open_null(false)?;
    let (stdout, our_stdout) = match &cmd.stdout {
        StdioTarget::Pipe => {
            let (ours, theirs) = pipe()?;
            (theirs, Some(ours))
        }
        StdioTarget::Null => (open_null(true)?, None),
        StdioTarget::File(path) => (open_append(path)?, None),
    };
    let (stderr, our_stderr) = match &cmd.stderr {
        StdioTarget::Pipe => {
            let (ours, theirs) = pipe()?;
            (Some(theirs), Some(ours))
        }
        StdioTarget::Null => (Some(open_null(true)?), None),
        StdioTarget::File(path) if cmd.stdout == StdioTarget::File(path.clone()) => (None, None),
        StdioTarget::File(path) => (Some(open_append(path)?), None),
    };
    Ok(Stdio {
        stdin,
        stdout,
        stderr,
        our_stdout,
        our_stderr,
    })
}

/// Appends `arg` quoted per the MSVCRT `CommandLineToArgvW` rules.
fn append_arg(line: &mut Vec<u16>, arg: &OsStr, force_quotes: bool) -> Result<()> {
    let units: Vec<u16> = arg.encode_wide().collect();
    if units.contains(&0) {
        return Err(ForgeError::InvalidInput(format!(
            "argument {arg:?} contains a NUL character"
        )));
    }
    let quote = force_quotes
        || units.is_empty()
        || units
            .iter()
            .any(|&c| c == u16::from(b' ') || c == u16::from(b'\t'));
    if quote {
        line.push(u16::from(b'"'));
    }
    let mut backslashes = 0_usize;
    for &unit in &units {
        if unit == u16::from(b'\\') {
            backslashes += 1;
        } else {
            if unit == u16::from(b'"') {
                // Double the backslashes that precede a quote, then escape the quote.
                line.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes + 1));
            }
            backslashes = 0;
        }
        line.push(unit);
    }
    if quote {
        // Backslashes before the closing quote must be doubled too.
        line.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
        line.push(u16::from(b'"'));
    }
    Ok(())
}

/// Full command line: quoted exe followed by the arguments. NUL-terminated.
pub(crate) fn command_line(exe: &Path, args: &[OsString]) -> Result<Vec<u16>> {
    let mut line = Vec::new();
    if exe.as_os_str().encode_wide().any(|c| c == u16::from(b'"')) {
        return Err(ForgeError::InvalidInput(format!(
            "executable path {} contains a quote",
            exe.display()
        )));
    }
    append_arg(&mut line, exe.as_os_str(), true)?;
    for arg in args {
        line.push(u16::from(b' '));
        append_arg(&mut line, arg, false)?;
    }
    line.push(0);
    Ok(line)
}

/// Environment block for `CREATE_UNICODE_ENVIRONMENT`: `NAME=value\0…\0\0`, names unique
/// (case-insensitive, last wins) and sorted case-insensitively.
pub(crate) fn environment_block(env: &[(OsString, OsString)]) -> Result<Vec<u16>> {
    let mut entries: Vec<(String, &OsString, &OsString)> = Vec::with_capacity(env.len());
    for (name, value) in env {
        let text = name.to_string_lossy();
        if text.is_empty() || text.contains('=') || text.contains('\0') {
            return Err(ForgeError::InvalidInput(format!(
                "invalid environment variable name {name:?}"
            )));
        }
        if value.encode_wide().any(|c| c == 0) {
            return Err(ForgeError::InvalidInput(format!(
                "environment variable {name:?} contains a NUL character"
            )));
        }
        let key = text.to_uppercase();
        entries.retain(|(existing, _, _)| *existing != key);
        entries.push((key, name, value));
    }
    entries.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
    let mut block = Vec::new();
    for (_, name, value) in entries {
        block.extend(name.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

/// Owns an initialized `PROC_THREAD_ATTRIBUTE_LIST`.
pub(crate) struct AttributeList {
    buf: Vec<u64>,
}

impl AttributeList {
    pub(crate) fn new(count: u32) -> Result<Self> {
        let mut size = 0_usize;
        // SAFETY: sizing call (expected to fail with ERROR_INSUFFICIENT_BUFFER).
        let _ = unsafe { InitializeProcThreadAttributeList(None, count, None, &mut size) };
        let mut list = Self {
            buf: vec![0; size.div_ceil(size_of::<u64>()).max(1)],
        };
        // SAFETY: `buf` has at least `size` writable bytes, 8-byte aligned.
        unsafe { InitializeProcThreadAttributeList(Some(list.ptr()), count, None, &mut size) }
            .map_err(|e| {
                list.buf.clear();
                win_err("initializing a process attribute list", &e)
            })?;
        Ok(list)
    }

    pub(crate) fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.buf.as_mut_ptr().cast())
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            // SAFETY: the list was initialized by InitializeProcThreadAttributeList.
            unsafe { DeleteProcThreadAttributeList(self.ptr()) };
        }
    }
}

/// Domain error for a failed `CreateProcessW` of `exe` in `cwd`.
pub(crate) fn launch_error(exe: &Path, cwd: &Path, err: &::windows::core::Error) -> ForgeError {
    match win32_code(err) {
        Some(code) if code == ERROR_FILE_NOT_FOUND.0 || code == ERROR_PATH_NOT_FOUND.0 => {
            ForgeError::NotFound(format!("executable {}", exe.display()))
        }
        Some(code) if code == ERROR_DIRECTORY.0 => {
            ForgeError::NotFound(format!("working directory {}", cwd.display()))
        }
        Some(code) if code == ERROR_ELEVATION_REQUIRED.0 => {
            ForgeError::NeedsElevation(format!("starting {}", exe.display()))
        }
        Some(code) if code == ERROR_BAD_EXE_FORMAT.0 => {
            ForgeError::InvalidInput(format!("{} is not a valid executable", exe.display()))
        }
        _ => win_err(format!("starting {}", exe.display()), err),
    }
}

/// Checks shared by every launch (supervised processes and pseudo consoles): `exe` is
/// absolute and not a batch file (cmd.exe has its own quoting rules, so arguments could
/// not be passed safely), `cwd` is an existing folder.
pub(crate) fn check_launch(exe: &Path, cwd: &Path) -> Result<()> {
    if !exe.is_absolute() {
        return Err(ForgeError::InvalidInput(format!(
            "executable path must be absolute: {}",
            exe.display()
        )));
    }
    let extension = exe
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    if matches!(extension.as_deref(), Some("bat" | "cmd")) {
        return Err(ForgeError::InvalidInput(format!(
            "batch files cannot be supervised directly (cmd.exe quoting): {}",
            exe.display()
        )));
    }
    if !cwd.is_dir() {
        return Err(ForgeError::NotFound(format!(
            "working directory {}",
            cwd.display()
        )));
    }
    Ok(())
}

/// `CreateProcessW` suspended → assign to `job` (and its ancestors) → resume.
pub(crate) fn create_process(job: Option<&Job>, cmd: &ProcessCommand) -> Result<Created> {
    check_launch(&cmd.exe, &cmd.cwd)?;

    let app = wide(cmd.exe.as_os_str());
    let mut line = command_line(&cmd.exe, &cmd.args)?;
    let env = environment_block(&cmd.env)?;
    let cwd = wide(cmd.cwd.as_os_str());
    let Stdio {
        stdin,
        stdout,
        stderr,
        our_stdout,
        our_stderr,
    } = prepare_stdio(cmd)?;

    let stderr_handle = stderr.as_ref().unwrap_or(&stdout);
    let mut inherit = vec![raw(&stdin), raw(&stdout)];
    if stderr.is_some() {
        inherit.push(raw(stderr_handle));
    }
    let mut attributes = AttributeList::new(1)?;
    // SAFETY: `inherit` holds valid inheritable handles and outlives CreateProcessW
    // (the list stores the pointer, not a copy).
    unsafe {
        UpdateProcThreadAttribute(
            attributes.ptr(),
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            Some(inherit.as_ptr().cast()),
            inherit.len() * size_of::<HANDLE>(),
            None,
            None,
        )
    }
    .map_err(|e| win_err("restricting inherited handles", &e))?;

    let mut info = STARTUPINFOEXW::default();
    info.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    info.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    info.StartupInfo.hStdInput = raw(&stdin);
    info.StartupInfo.hStdOutput = raw(&stdout);
    info.StartupInfo.hStdError = raw(stderr_handle);
    info.lpAttributeList = attributes.ptr();

    let mut flags =
        CREATE_SUSPENDED.0 | CREATE_UNICODE_ENVIRONMENT.0 | EXTENDED_STARTUPINFO_PRESENT.0;
    flags |= match cmd.console {
        ConsoleMode::Hidden => CREATE_NO_WINDOW.0,
        ConsoleMode::Detached => DETACHED_PROCESS.0,
    };
    if cmd.new_process_group {
        flags |= CREATE_NEW_PROCESS_GROUP.0;
    }

    let mut pi = PROCESS_INFORMATION::default();
    // SAFETY: every pointer is valid for the call: `app`, `cwd` NUL-terminated; `line`
    // writable and NUL-terminated (CreateProcessW may modify it); `env` is a valid
    // Unicode block; `info` is a STARTUPINFOEXW (EXTENDED_STARTUPINFO_PRESENT) whose
    // attribute list and handles outlive the call.
    let created = unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            true,
            PROCESS_CREATION_FLAGS(flags),
            Some(env.as_ptr().cast()),
            PCWSTR(cwd.as_ptr()),
            (&raw const info).cast(),
            &mut pi,
        )
    };
    drop(attributes);
    // Close the child's ends in this process right away: EOF on our pipe ends depends on
    // the child (and its descendants) being the only holders.
    drop((stdin, stdout, stderr));
    created.map_err(|e| launch_error(&cmd.exe, &cmd.cwd, &e))?;

    // SAFETY: CreateProcessW succeeded; both handles are ours.
    let process = unsafe { owned(pi.hProcess) };
    // SAFETY: as above.
    let thread = unsafe { owned(pi.hThread) };
    let kill = |reason: ForgeError| {
        // SAFETY: valid process handle; the child is still suspended.
        let _ = unsafe { TerminateProcess(raw(&process), 1) };
        reason
    };
    if let Some(job) = job {
        job.assign_process(raw(&process)).map_err(kill)?;
    }
    // SAFETY: valid thread handle of the suspended primary thread.
    if unsafe { ResumeThread(raw(&thread)) } == u32::MAX {
        let err = io::Error::last_os_error();
        return Err(kill(ForgeError::io("resuming a new process", err)));
    }
    Ok(Created {
        pid: pi.dwProcessId,
        process,
        stdout: our_stdout,
        stderr: our_stderr,
    })
}

/// Starts `exe` detached from the caller (no console, own process group, outside the
/// caller's Job Object when it allows breakaway) with stdin/stdout/stderr on the null
/// device and **only those handles inherited** (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`).
/// `std::process::Command` inherits every inheritable handle of the parent, including the
/// pipe a shell or a test harness gave `forge` as stdout: a daemon started that way would
/// keep the pipe open and `forge status | more` would never see EOF. The environment is the
/// caller's. Returns the pid; the caller never waits for it.
pub(crate) fn create_detached(exe: &Path, args: &[OsString], cwd: &Path) -> Result<u32> {
    create_detached_handle(exe, args, cwd).map(|(pid, _process)| pid)
}

/// [`create_detached`] keeping the process handle, so the caller can learn how the
/// process ended (an auto-started daemon that exits at once) without racing a pid that
/// may already be gone. Dropping the [`ProcessExit`] only stops watching.
pub(crate) fn create_detached_watched(
    exe: &Path,
    args: &[OsString],
    cwd: &Path,
) -> Result<ProcessExit> {
    let (pid, process) = create_detached_handle(exe, args, cwd)?;
    Ok(ProcessExit::new(pid, Box::new(ExitWatch::new(process)?)))
}

fn create_detached_handle(exe: &Path, args: &[OsString], cwd: &Path) -> Result<(u32, OwnedHandle)> {
    let app = wide(exe.as_os_str());
    let mut line = command_line(exe, args)?;
    let cwd_wide = wide(cwd.as_os_str());
    let stdin = open_null(false)?;
    let stdout = open_null(true)?;
    let inherit = [raw(&stdin), raw(&stdout)];
    let mut attributes = AttributeList::new(1)?;
    // SAFETY: `inherit` holds valid inheritable handles and outlives CreateProcessW.
    unsafe {
        UpdateProcThreadAttribute(
            attributes.ptr(),
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            Some(inherit.as_ptr().cast()),
            inherit.len() * size_of::<HANDLE>(),
            None,
            None,
        )
    }
    .map_err(|e| win_err("restricting inherited handles", &e))?;
    let mut info = STARTUPINFOEXW::default();
    info.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    info.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    info.StartupInfo.hStdInput = raw(&stdin);
    info.StartupInfo.hStdOutput = raw(&stdout);
    info.StartupInfo.hStdError = raw(&stdout);
    info.lpAttributeList = attributes.ptr();

    let base = DETACHED_PROCESS.0 | CREATE_NEW_PROCESS_GROUP.0 | EXTENDED_STARTUPINFO_PRESENT.0;
    let mut last = None;
    // First outside the caller's job; a job that forbids breakaway gets the child inside.
    for flags in [base | CREATE_BREAKAWAY_FROM_JOB.0, base] {
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: `app`, `cwd_wide` are NUL-terminated; `line` is writable and
        // NUL-terminated; `info` is a STARTUPINFOEXW (EXTENDED_STARTUPINFO_PRESENT) whose
        // attribute list and handles outlive the call; a null environment inherits ours.
        let created = unsafe {
            CreateProcessW(
                PCWSTR(app.as_ptr()),
                Some(PWSTR(line.as_mut_ptr())),
                None,
                None,
                true,
                PROCESS_CREATION_FLAGS(flags),
                None,
                PCWSTR(cwd_wide.as_ptr()),
                (&raw const info).cast(),
                &mut pi,
            )
        };
        match created {
            Ok(()) => {
                // SAFETY: CreateProcessW succeeded; both handles are ours to close.
                let process = unsafe { owned(pi.hProcess) };
                // SAFETY: as above.
                drop(unsafe { owned(pi.hThread) });
                return Ok((pi.dwProcessId, process));
            }
            Err(e) if win32_code(&e) == Some(ERROR_ACCESS_DENIED.0) => last = Some(e),
            Err(e) => {
                return Err(match win32_code(&e) {
                    Some(c) if c == ERROR_FILE_NOT_FOUND.0 || c == ERROR_PATH_NOT_FOUND.0 => {
                        ForgeError::NotFound(format!("executable {}", exe.display()))
                    }
                    _ => win_err(format!("starting {}", exe.display()), &e),
                });
            }
        }
    }
    Err(last.map_or_else(
        || ForgeError::Internal("process creation failed".into()),
        |e| win_err(format!("starting {}", exe.display()), &e),
    ))
}

// ---------------------------------------------------------------------------------------
// Exit watching
// ---------------------------------------------------------------------------------------

/// Registered thread-pool wait whose callback completes a oneshot.
struct WaitRegistration {
    wait: HANDLE,
    /// Owned by the registration; read by the callback at most once.
    slot: *mut Option<oneshot::Sender<()>>,
}

// SAFETY: the wait handle and the slot are only touched by the callback (at most once,
// WT_EXECUTEONLYONCE) and by Drop, which first waits for any running callback
// (UnregisterWaitEx with INVALID_HANDLE_VALUE).
unsafe impl Send for WaitRegistration {}

impl Drop for WaitRegistration {
    fn drop(&mut self) {
        // SAFETY: `wait` came from RegisterWaitForSingleObject; INVALID_HANDLE_VALUE makes
        // the call wait for a running callback, so `slot` is no longer used afterwards.
        let _ = unsafe { UnregisterWaitEx(self.wait, Some(INVALID_HANDLE_VALUE)) };
        // SAFETY: `slot` came from Box::into_raw and nothing else references it now.
        drop(unsafe { Box::from_raw(self.slot) });
    }
}

unsafe extern "system" fn on_process_exit(context: *mut c_void, _timed_out: bool) {
    // SAFETY: `context` is the slot of a live WaitRegistration (Drop waits for us).
    let slot = unsafe { &mut *context.cast::<Option<oneshot::Sender<()>>>() };
    if let Some(sender) = slot.take() {
        let _ = sender.send(());
    }
}

/// [`ExitSource`] for a process handle (also the exit watch of a pseudo console's process,
/// `conpty.rs`).
pub(crate) struct ExitWatch {
    // Declared first so it is dropped first: the wait must be unregistered before the
    // process handle it waits on is closed.
    _registration: WaitRegistration,
    process: OwnedHandle,
    receiver: oneshot::Receiver<()>,
    /// The wait completed (the receiver must not be polled again).
    signalled: bool,
    code: Option<u32>,
}

impl ExitWatch {
    /// Watches `process`, which needs `SYNCHRONIZE` and query access.
    pub(crate) fn new(process: OwnedHandle) -> Result<Self> {
        let (sender, receiver) = oneshot::channel();
        let slot = Box::into_raw(Box::new(Some(sender)));
        let mut wait = HANDLE::default();
        // SAFETY: `process` has SYNCHRONIZE access and outlives the registration (both
        // live in `ExitWatch`, which drops the registration first); `slot` stays valid
        // until the registration is dropped.
        let registered = unsafe {
            RegisterWaitForSingleObject(
                &mut wait,
                raw(&process),
                Some(on_process_exit),
                Some(slot.cast_const().cast()),
                INFINITE,
                WORKER_THREAD_FLAGS(WT_EXECUTEONLYONCE.0 | WT_EXECUTEINWAITTHREAD.0),
            )
        };
        if let Err(err) = registered {
            // SAFETY: the registration failed, so the callback will never see `slot`.
            drop(unsafe { Box::from_raw(slot) });
            return Err(win_err("watching a process exit", &err));
        }
        Ok(Self {
            _registration: WaitRegistration { wait, slot },
            process,
            receiver,
            signalled: false,
            code: None,
        })
    }

    fn exit_code(&self) -> Result<u32> {
        let mut code = 0_u32;
        // SAFETY: valid process handle with query access; `code` is writable.
        unsafe { GetExitCodeProcess(raw(&self.process), &mut code) }
            .map_err(|e| win_err("reading a process exit code", &e))?;
        Ok(code)
    }
}

impl ExitSource for ExitWatch {
    fn poll_exit(&mut self, cx: &mut Context<'_>) -> Poll<Result<u32>> {
        if let Some(code) = self.code {
            return Poll::Ready(Ok(code));
        }
        if !self.signalled {
            match std::pin::Pin::new(&mut self.receiver).poll(cx) {
                Poll::Pending => return Poll::Pending,
                // A dropped sender only happens after the callback ran.
                Poll::Ready(_) => self.signalled = true,
            }
        }
        let code = self.exit_code();
        if let Ok(code) = code {
            self.code = Some(code);
        }
        Poll::Ready(code)
    }

    fn try_exit_code(&self) -> Result<Option<u32>> {
        if let Some(code) = self.code {
            return Ok(Some(code));
        }
        // SAFETY: valid handle with SYNCHRONIZE access; zero timeout never blocks.
        let state = unsafe { WaitForSingleObject(raw(&self.process), 0) };
        if state == WAIT_OBJECT_0 {
            self.exit_code().map(Some)
        } else if state == WAIT_TIMEOUT {
            Ok(None)
        } else {
            Err(ForgeError::io(
                "polling a process exit",
                io::Error::last_os_error(),
            ))
        }
    }
}

// ---------------------------------------------------------------------------------------
// ProcessControl
// ---------------------------------------------------------------------------------------

/// [`ProcessControl`] for Windows: Job Objects, `CreateProcessW`, the `forgebox-ctrl`
/// helper for `CTRL_BREAK` and `WM_CLOSE` through `EnumWindows`.
#[derive(Debug, Clone)]
pub struct WindowsProcessControl {
    opts: ProcessControlOptions,
}

impl WindowsProcessControl {
    pub fn new(opts: ProcessControlOptions) -> Self {
        Self { opts }
    }
}

/// `%SystemRoot%\System32` as reported by the OS.
pub(crate) fn system_directory() -> PathBuf {
    let mut buf = [0_u16; 512];
    // SAFETY: writable buffer whose length is passed by the slice.
    let len = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
    if len == 0 || len > buf.len() {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
        return PathBuf::from(root).join("System32");
    }
    PathBuf::from(String::from_utf16_lossy(&buf[..len]))
}

impl ProcessControl for WindowsProcessControl {
    fn create_group(&self, opts: GroupOptions) -> Result<Arc<dyn ProcessGroup>> {
        Ok(Arc::new(JobGroup {
            job: Job::create(opts, None)?,
        }))
    }

    fn send_ctrl_break(&self, group_leader: u32, timeout: std::time::Duration) -> Result<()> {
        let helper = self.opts.resolved_ctrl_break_helper()?;
        super::console_ctrl::send_ctrl_break(&helper, group_leader, timeout)
    }

    fn close_windows(&self, pids: &[u32]) -> Result<usize> {
        super::window_close::close_windows(pids)
    }

    fn watch(&self, pid: u32) -> Result<ProcessExit> {
        let process = open_process(
            pid,
            PROCESS_ACCESS_RIGHTS(PROCESS_SYNCHRONIZE.0 | PROCESS_QUERY_LIMITED_INFORMATION.0),
        )?;
        Ok(ProcessExit::new(pid, Box::new(ExitWatch::new(process)?)))
    }

    fn is_alive(&self, pid: u32) -> Result<bool> {
        let process = match open_process(pid, PROCESS_SYNCHRONIZE) {
            Ok(process) => process,
            Err(ForgeError::NotFound(_)) => return Ok(false),
            Err(err) => return Err(err),
        };
        // SAFETY: valid handle with SYNCHRONIZE access; zero timeout.
        Ok(unsafe { WaitForSingleObject(raw(&process), 0) } == WAIT_TIMEOUT)
    }

    fn system_path(&self) -> Vec<PathBuf> {
        vec![system_directory()]
    }

    fn base_env(&self) -> Vec<(OsString, OsString)> {
        BASE_ENV_VARS
            .iter()
            .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(name), value)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn line(exe: &str, args: &[&str]) -> String {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let units = command_line(Path::new(exe), &args).unwrap();
        String::from_utf16(&units[..units.len() - 1]).unwrap()
    }

    #[test]
    fn command_line_follows_msvcrt_quoting() {
        assert_eq!(
            line(r"C:\b\php-cgi.exe", &["-b", "127.0.0.1:9000"]),
            r#""C:\b\php-cgi.exe" -b 127.0.0.1:9000"#
        );
        assert_eq!(line(r"C:\x.exe", &["a b", ""]), r#""C:\x.exe" "a b" """#);
        assert_eq!(
            line(r"C:\x.exe", &[r#"say "hi""#]),
            r#""C:\x.exe" "say \"hi\"""#
        );
        assert_eq!(
            line(r"C:\x.exe", &[r"C:\dir with space\"]),
            r#""C:\x.exe" "C:\dir with space\\""#
        );
        assert_eq!(line(r"C:\x.exe", &[r#"a\"b"#]), r#""C:\x.exe" a\\\"b"#);
        assert_eq!(line(r"C:\x.exe", &[r"a\\b"]), r#""C:\x.exe" a\\b"#);
        let args = vec![OsString::from("bad\0arg")];
        assert!(command_line(Path::new(r"C:\x.exe"), &args).is_err());
        assert!(command_line(Path::new("C:\\x\".exe"), &[]).is_err());
    }

    #[test]
    fn environment_block_is_sorted_unique_and_terminated() {
        let env = vec![
            (OsString::from("Path"), OsString::from("A")),
            (OsString::from("b"), OsString::from("2")),
            (OsString::from("PATH"), OsString::from("B")),
            (OsString::from("a"), OsString::from("1")),
        ];
        let block = environment_block(&env).unwrap();
        let text = String::from_utf16(&block).unwrap();
        assert_eq!(text, "a=1\0b=2\0PATH=B\0\0");
        assert_eq!(environment_block(&[]).unwrap(), vec![0, 0]);
        assert!(environment_block(&[(OsString::from("A=B"), OsString::new())]).is_err());
        assert!(environment_block(&[(OsString::new(), OsString::new())]).is_err());
    }

    #[test]
    fn base_env_never_contains_path() {
        let env = WindowsProcessControl::new(ProcessControlOptions::default()).base_env();
        assert!(env.iter().all(|(k, _)| !k.eq_ignore_ascii_case("PATH")));
        assert!(env.iter().any(|(k, _)| k == "SystemRoot"), "{env:?}");
        let system = system_directory();
        assert!(
            system.join("kernel32.dll").is_file(),
            "{}",
            system.display()
        );
    }

    #[tokio::test]
    async fn job_lists_and_terminates_processes() {
        let control = WindowsProcessControl::new(ProcessControlOptions::default());
        let root = control.create_group(GroupOptions::ROOT).unwrap();
        let child = root.create_child(GroupOptions::INSTANCE).unwrap();
        assert!(child.pids().unwrap().is_empty());
        let system = system_directory();
        // `ping -n 30` keeps running long enough and needs no console input.
        let mut cmd = ProcessCommand::new(system.join("PING.EXE"), &system);
        cmd.args = vec!["-n".into(), "30".into(), "127.0.0.1".into()];
        cmd.env = control.base_env();
        let mut spawned = child.spawn(&cmd).unwrap();
        let pid = spawned.pid;
        assert_eq!(child.pids().unwrap(), vec![pid]);
        assert!(
            root.pids().unwrap().contains(&pid),
            "nested: also in the root job"
        );
        assert!(control.is_alive(pid).unwrap());
        assert_eq!(spawned.exit.try_exit_code().unwrap(), None);
        let mut watch = control.watch(pid).unwrap();

        child.terminate_all(7).unwrap();
        let code = spawned
            .exit
            .wait_timeout(std::time::Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(code, Some(7));
        assert_eq!(spawned.exit.try_exit_code().unwrap(), Some(7));
        assert_eq!((&mut watch).await.unwrap(), 7);
        assert!(child.pids().unwrap().is_empty());
        assert!(!control.is_alive(pid).unwrap_or(false));
        assert_eq!(
            control.watch(u32::MAX - 3).err().unwrap().code(),
            "not_found"
        );
    }

    /// `ping -n <seconds> 127.0.0.1`: runs long enough and needs no console input.
    fn ping(control: &WindowsProcessControl, seconds: u32) -> ProcessCommand {
        let system = system_directory();
        let mut cmd = ProcessCommand::new(system.join("PING.EXE"), &system);
        cmd.args = vec!["-n".into(), seconds.to_string().into(), "127.0.0.1".into()];
        cmd.env = control.base_env();
        cmd
    }

    /// Regression (review of M1-E3-T1): `terminate_all` returned while the job was still
    /// terminating, and adding a process to the same job right after it failed with
    /// `ERROR_ACCESS_DENIED`. Each round terminates the job and, at once, assigns an
    /// already running process (so nothing slow such as `CreateProcessW` sits between the
    /// two calls: the race is hit every time) and spawns a new one.
    #[tokio::test]
    async fn job_takes_processes_right_after_terminate_all() {
        let control = WindowsProcessControl::new(ProcessControlOptions::default());
        let root = control.create_group(GroupOptions::ROOT).unwrap();
        let job = root.create_child(GroupOptions::INSTANCE).unwrap();
        let cmd = ping(&control, 30);
        for round in 1..=20 {
            let mut member = job.spawn(&cmd).unwrap();
            // Outside our jobs until assigned below.
            let mut newcomer = std::process::Command::new(&cmd.exe)
                .args(&cmd.args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();

            job.terminate_all(9).unwrap();
            let assigned = job.assign(newcomer.id());
            let spawned = job.spawn(&cmd);

            assigned
                .unwrap_or_else(|err| panic!("round {round}: assign after terminate_all: {err}"));
            let spawned = spawned
                .unwrap_or_else(|err| panic!("round {round}: spawn after terminate_all: {err}"));
            let pids = job.pids().unwrap();
            assert!(pids.contains(&newcomer.id()), "round {round}: {pids:?}");
            assert!(pids.contains(&spawned.pid), "round {round}: {pids:?}");
            assert!(!pids.contains(&member.pid), "round {round}: {pids:?}");
            let code = member
                .exit
                .wait_timeout(Duration::from_secs(2))
                .await
                .unwrap();
            assert_eq!(code, Some(9), "round {round}");

            job.terminate_all(9).unwrap();
            assert!(job.pids().unwrap().is_empty(), "round {round}: job emptied");
            assert_eq!(newcomer.wait().unwrap().code(), Some(9), "round {round}");
        }
        // Nothing left: returns at once.
        let started = Instant::now();
        job.terminate_all(9).unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// The race itself, without the wait of `terminate_all`: right after a bare
    /// `TerminateJobObject` the job refuses new members (even once empty), and the
    /// assignment retries until the termination settled.
    #[tokio::test]
    async fn assignment_retries_while_a_termination_settles() {
        let control = WindowsProcessControl::new(ProcessControlOptions::default());
        let root = Job::create(GroupOptions::ROOT, None).unwrap();
        let job = Job::create(GroupOptions::INSTANCE, Some(Arc::clone(&root))).unwrap();
        let cmd = ping(&control, 30);
        let mut refused_total = 0;
        for round in 1..=10 {
            let member = create_process(Some(&job), &cmd).unwrap();
            // Already running (outside our jobs) so the assignment follows the
            // termination within microseconds.
            let newcomer = create_process(None, &cmd).unwrap();
            job.terminate(1).unwrap();
            job.terminate(1).unwrap(); // a second termination in flight is fine
            let started = Instant::now();
            let refused = job
                .assign_process(raw(&newcomer.process))
                .unwrap_or_else(|err| panic!("round {round}: {err}"));
            println!(
                "round {round}: accepted after {refused} refusals in {:?}",
                started.elapsed()
            );
            refused_total += refused;
            assert!(job.pids().unwrap().contains(&newcomer.pid), "round {round}");
            assert!(!job.pids().unwrap().contains(&member.pid), "round {round}");
            job.terminate_all(1).unwrap();
        }
        assert!(
            refused_total > 0,
            "the job never refused a process right after TerminateJobObject: this test no \
             longer reproduces the race it guards against"
        );
    }

    #[test]
    fn missing_executable_and_cwd_are_not_found() {
        let system = system_directory();
        let cmd = ProcessCommand::new(system.join("does-not-exist-forgebox.exe"), &system);
        let err = create_process(None, &cmd).err().unwrap();
        assert_eq!(err.code(), "not_found", "{err}");
        let cmd = ProcessCommand::new(system.join("PING.EXE"), system.join("no-such-dir"));
        assert_eq!(
            create_process(None, &cmd).err().unwrap().code(),
            "not_found"
        );
        let cmd = ProcessCommand::new(r"relative\tool.exe", &system);
        assert_eq!(
            create_process(None, &cmd).err().unwrap().code(),
            "invalid_input"
        );
        let cmd = ProcessCommand::new(system.join("x.cmd"), &system);
        assert_eq!(
            create_process(None, &cmd).err().unwrap().code(),
            "invalid_input"
        );
    }

    async fn read_all(reader: Option<PipeReader>) -> String {
        use tokio::io::AsyncReadExt;
        let mut text = String::new();
        let mut reader = reader.expect("captured pipe");
        // EOF is signalled as BrokenPipe once every writer is gone.
        match reader.read_to_string(&mut text).await {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("{err}"),
        }
        text
    }

    #[tokio::test]
    async fn captured_pipes_deliver_output_and_eof() {
        let control = WindowsProcessControl::new(ProcessControlOptions::default());
        let job = control.create_group(GroupOptions::INSTANCE).unwrap();
        let system = system_directory();
        let mut cmd = ProcessCommand::new(system.join("cmd.exe"), &system);
        cmd.args = vec!["/d".into(), "/c".into(), "echo out& echo err 1>&2".into()];
        cmd.env = control.base_env();
        cmd.stdout = StdioTarget::Pipe;
        cmd.stderr = StdioTarget::Pipe;
        let spawned = job.spawn(&cmd).unwrap();
        let (out, err) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            futures::future::join(read_all(spawned.stdout), read_all(spawned.stderr)),
        )
        .await
        .expect("pipes reach EOF when the child exits");
        assert_eq!(out.trim_end(), "out");
        assert_eq!(err.trim_end(), "err");
        assert_eq!(spawned.exit.await.unwrap(), 0);
    }

    #[tokio::test]
    async fn file_stdio_appends_both_streams() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("logs").join("out.log");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, "before\r\n").unwrap();
        let control = WindowsProcessControl::new(ProcessControlOptions::default());
        let job = control.create_group(GroupOptions::INSTANCE).unwrap();
        let system = system_directory();
        let mut cmd = ProcessCommand::new(system.join("cmd.exe"), &system);
        cmd.args = vec!["/d".into(), "/c".into(), "echo out& echo err 1>&2".into()];
        cmd.env = control.base_env();
        cmd.stdout = StdioTarget::File(log.clone());
        cmd.stderr = StdioTarget::File(log.clone());
        let spawned = job.spawn(&cmd).unwrap();
        assert!(spawned.stdout.is_none() && spawned.stderr.is_none());
        assert_eq!(spawned.exit.await.unwrap(), 0);
        let text = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
        assert_eq!(lines, vec!["before", "out", "err"]);
    }
}
