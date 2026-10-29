//! Per-process counters (docs/03 §6, docs/14 §2): `GetProcessTimes` (CPU and creation
//! time), `GetProcessMemoryInfo` with `PROCESS_MEMORY_COUNTERS_EX` (working set and
//! private commit) and `GetProcessHandleCount`, through one `PROCESS_QUERY_LIMITED_INFORMATION`
//! handle. These are the counters Task Manager and `Get-Process` show. The executable of
//! a process ([`ProcessStats::info`]) comes from `QueryFullProcessImageNameW` on the same
//! kind of handle ([`image_path`], shared with the TCP table of `net_table.rs`).

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ::windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, FILETIME, HANDLE};
use ::windows::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use ::windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use ::windows::Win32::System::Threading::{
    ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, GetProcessHandleCount, GetProcessTimes,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use ::windows::core::PWSTR;

use super::process::{open_process, os_error, win32_code};
use crate::error::{ForgeError, Result};
use crate::platform::process_stats::{ProcStats, ProcessInfo, ProcessStats};

/// 100 ns intervals between 1601-01-01 (FILETIME epoch) and 1970-01-01.
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

/// Longest image path asked for (UTF-16 units): far beyond `MAX_PATH`, within the 32 767
/// of a `\\?\` path.
const MAX_IMAGE_PATH: usize = 32 * 1024;

/// [`ProcessStats`] for Windows.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsProcessStats;

/// The 100 ns ticks of a `FILETIME`.
pub(super) fn ticks(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

/// Duration of `n` 100 ns ticks.
pub(super) fn from_ticks(n: u64) -> Duration {
    Duration::new(
        n / 10_000_000,
        u32::try_from((n % 10_000_000) * 100).unwrap_or(0),
    )
}

fn system_time(time: FILETIME) -> SystemTime {
    let n = ticks(time);
    if n >= FILETIME_UNIX_EPOCH {
        UNIX_EPOCH + from_ticks(n - FILETIME_UNIX_EPOCH)
    } else {
        UNIX_EPOCH - from_ticks(FILETIME_UNIX_EPOCH - n)
    }
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

/// Logical processors of the machine, every processor group included
/// (`GetActiveProcessorCount(ALL_PROCESSOR_GROUPS)`), or what the standard library sees
/// when that fails.
pub(super) fn logical_processors() -> u32 {
    // SAFETY: plain query.
    let count = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) };
    if count > 0 {
        count
    } else {
        std::thread::available_parallelism()
            .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
            .unwrap_or(1)
    }
}

/// Creation time of the process behind `process` (a handle with query access).
fn creation_of(process: &OwnedHandle, pid: u32) -> Result<SystemTime> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: valid handle with query access; the four out-pointers are writable.
    unsafe {
        GetProcessTimes(
            raw(process),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    }
    .map_err(|e| {
        ForgeError::io(
            format!("reading the creation time of process {pid}"),
            os_error(&e),
        )
    })?;
    Ok(system_time(creation))
}

/// Full Win32 path of the executable of the process behind `process` (a handle with
/// `PROCESS_QUERY_LIMITED_INFORMATION`); `None` when the OS does not tell it (the System
/// process, some protected processes).
pub(super) fn image_path(process: &OwnedHandle) -> Option<PathBuf> {
    let mut buffer = vec![0_u16; 1024];
    loop {
        let mut len = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // SAFETY: valid handle with query access; `buffer` holds `len` u16s.
        let result = unsafe {
            QueryFullProcessImageNameW(
                raw(process),
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut len,
            )
        };
        match result {
            Ok(()) => {
                buffer.truncate(len as usize);
                return Some(PathBuf::from(OsString::from_wide(&buffer)));
            }
            Err(err)
                if win32_code(&err) == Some(ERROR_INSUFFICIENT_BUFFER.0)
                    && buffer.len() < MAX_IMAGE_PATH =>
            {
                let bigger = buffer.len() * 2;
                buffer.resize(bigger, 0);
            }
            Err(_) => return None,
        }
    }
}

impl ProcessStats for WindowsProcessStats {
    fn sample(&self, pid: u32) -> Result<ProcStats> {
        let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let handle = raw(&process);

        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: valid handle with query access; the four out-pointers are writable.
        unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }
            .map_err(|e| {
                ForgeError::io(format!("reading CPU times of process {pid}"), os_error(&e))
            })?;
        // SAFETY: no arguments; returns the current time.
        let now = unsafe { GetSystemTimeAsFileTime() };

        let mut memory = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        // SAFETY: `memory` is a writable PROCESS_MEMORY_COUNTERS_EX whose size is passed;
        // the API accepts the _EX layout through a PROCESS_MEMORY_COUNTERS pointer.
        let ok = unsafe {
            K32GetProcessMemoryInfo(
                handle,
                (&raw mut memory).cast::<PROCESS_MEMORY_COUNTERS>(),
                size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            )
        };
        if !ok.as_bool() {
            return Err(ForgeError::io(
                format!("reading memory counters of process {pid}"),
                std::io::Error::last_os_error(),
            ));
        }

        let mut handles = 0_u32;
        // SAFETY: valid handle with query access; `handles` is writable.
        unsafe { GetProcessHandleCount(handle, &mut handles) }.map_err(|e| {
            ForgeError::io(
                format!("reading the handle count of process {pid}"),
                os_error(&e),
            )
        })?;

        Ok(ProcStats {
            pid,
            cpu_total: from_ticks(ticks(kernel).saturating_add(ticks(user))),
            rss_bytes: memory.WorkingSetSize as u64,
            private_bytes: memory.PrivateUsage as u64,
            handles,
            started_at: system_time(creation),
            uptime: from_ticks(ticks(now).saturating_sub(ticks(creation))),
        })
    }

    fn creation_time(&self, pid: u32) -> Result<SystemTime> {
        let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        creation_of(&process, pid)
    }

    fn logical_cpus(&self) -> u32 {
        logical_processors()
    }

    fn info(&self, pid: u32) -> Result<ProcessInfo> {
        // One handle for both answers: the image is the one of the process created at
        // `started_at`, even if the PID is reused right after.
        let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let started_at = creation_of(&process, pid)?;
        Ok(ProcessInfo::new(pid, started_at, image_path(&process)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_conversions() {
        let unix = FILETIME {
            dwLowDateTime: (FILETIME_UNIX_EPOCH & 0xFFFF_FFFF) as u32,
            dwHighDateTime: (FILETIME_UNIX_EPOCH >> 32) as u32,
        };
        assert_eq!(system_time(unix), UNIX_EPOCH);
        assert_eq!(from_ticks(15_000_000), Duration::from_millis(1500));
        assert_eq!(from_ticks(1), Duration::from_nanos(100));
    }

    #[test]
    fn counts_every_logical_processor() {
        let count = WindowsProcessStats.logical_cpus();
        let std_count = std::thread::available_parallelism().unwrap().get();
        assert!(count as usize >= std_count, "{count} < {std_count}");
    }

    #[test]
    fn info_of_the_current_process_names_its_executable() {
        let pid = std::process::id();
        let info = WindowsProcessStats.info(pid).unwrap();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(info.pid, pid);
        assert_eq!(info.image_path.as_deref(), Some(exe.as_path()));
        assert_eq!(
            info.image_name.as_deref(),
            exe.file_name().and_then(|n| n.to_str())
        );
        assert_eq!(
            info.started_at,
            WindowsProcessStats.creation_time(pid).unwrap()
        );
    }

    #[test]
    fn info_of_kernel_and_missing_processes() {
        // The System process (PID 4) has no image file, and a non-elevated caller cannot
        // even open it (access denied): either way, no name and no `not_found`.
        let system = WindowsProcessStats.info(4);
        println!("info(4) = {system:?}");
        match system {
            Ok(info) => assert_eq!(info.image_path, None, "{info:?}"),
            Err(err) => assert_ne!(err.code(), "not_found", "{err}"),
        }
        assert_eq!(
            WindowsProcessStats.info(u32::MAX - 3).unwrap_err().code(),
            "not_found"
        );
    }
}
