//! Per-process counters (docs/03 §6, docs/14 §2): `GetProcessTimes` (CPU and creation
//! time), `GetProcessMemoryInfo` with `PROCESS_MEMORY_COUNTERS_EX` (working set and
//! private commit) and `GetProcessHandleCount`, through one `PROCESS_QUERY_LIMITED_INFORMATION`
//! handle. These are the counters Task Manager and `Get-Process` show.

use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ::windows::Win32::Foundation::{FILETIME, HANDLE};
use ::windows::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use ::windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use ::windows::Win32::System::Threading::{
    ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, GetProcessHandleCount, GetProcessTimes,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::process::{open_process, os_error};
use crate::error::{ForgeError, Result};
use crate::platform::process_stats::{ProcStats, ProcessStats};

/// 100 ns intervals between 1601-01-01 (FILETIME epoch) and 1970-01-01.
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

/// [`ProcessStats`] for Windows.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsProcessStats;

fn ticks(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

/// Duration of `n` 100 ns ticks.
fn from_ticks(n: u64) -> Duration {
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

    fn logical_cpus(&self) -> u32 {
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
}
