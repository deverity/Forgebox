//! Machine-wide counters (docs/03 §6, docs/14 §4.2): `GetSystemTimes` (idle, kernel and
//! user time summed over every logical processor; the kernel time **includes** the idle
//! time, so busy = kernel − idle + user), `GlobalMemoryStatusEx` (physical memory: the
//! total `Win32_ComputerSystem` reports and Task Manager's "Available") and
//! `GetDiskFreeSpaceExW` (size and free space of the volume holding a path, as
//! `Get-PSDrive` shows them). All three are cheap kernel queries; only the volume one can
//! stall (sleeping disk, network share), which is why callers run them in
//! `spawn_blocking`.

use std::ffi::OsString;
use std::path::Path;

use ::windows::Win32::Foundation::{
    ERROR_BAD_NETPATH, ERROR_DIRECTORY, ERROR_FILE_NOT_FOUND, ERROR_INVALID_NAME,
    ERROR_PATH_NOT_FOUND, FILETIME,
};
use ::windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use ::windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use ::windows::Win32::System::Threading::GetSystemTimes;
use ::windows::core::PCWSTR;

use super::process::{os_error, wide, win_err, win32_code};
use super::process_stats::{from_ticks, logical_processors, ticks};
use crate::error::{ForgeError, Result};
use crate::platform::system_stats::{MachineSample, SystemStats, VolumeSpace};

/// [`SystemStats`] for Windows.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsSystemStats;

/// `(busy, idle)` ticks from the three `GetSystemTimes` values: the kernel time includes
/// the idle time, so busy = kernel − idle + user.
fn busy_and_idle(idle: u64, kernel: u64, user: u64) -> (u64, u64) {
    (kernel.saturating_sub(idle).saturating_add(user), idle)
}

/// The folder handed to `GetDiskFreeSpaceExW`: `path` with a trailing separator (required
/// for UNC shares, harmless elsewhere; `C:` becomes the root `C:\`, not the current
/// folder of drive C).
fn directory_arg(path: &Path) -> OsString {
    let mut dir = path.as_os_str().to_owned();
    let text = dir.to_string_lossy();
    if !(text.ends_with('\\') || text.ends_with('/')) {
        dir.push("\\");
    }
    dir
}

/// Free space query of one folder: `(available to the caller, total, free)`.
fn disk_free_space(dir: &Path) -> std::result::Result<(u64, u64, u64), ::windows::core::Error> {
    let arg = wide(&directory_arg(dir));
    let (mut available, mut total, mut free) = (0_u64, 0_u64, 0_u64);
    // SAFETY: `arg` is NUL-terminated UTF-16 alive for the call; the three out-pointers
    // are writable u64s.
    unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(arg.as_ptr()),
            Some(&mut available),
            Some(&mut total),
            Some(&mut free),
        )
    }?;
    Ok((available, total, free))
}

impl SystemStats for WindowsSystemStats {
    fn sample(&self) -> Result<MachineSample> {
        let mut idle = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: the three out-pointers are writable FILETIMEs.
        unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }
            .map_err(|e| win_err("reading the CPU times of the machine", &e))?;
        let (busy, idle) = busy_and_idle(ticks(idle), ticks(kernel), ticks(user));

        let mut memory = MEMORYSTATUSEX {
            dwLength: size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        // SAFETY: `memory` is a writable MEMORYSTATUSEX with `dwLength` set.
        unsafe { GlobalMemoryStatusEx(&mut memory) }
            .map_err(|e| win_err("reading the memory status of the machine", &e))?;

        Ok(MachineSample {
            cpu_busy_total: from_ticks(busy),
            cpu_idle_total: from_ticks(idle),
            logical_cpus: logical_processors(),
            mem_total: memory.ullTotalPhys,
            mem_avail: memory.ullAvailPhys,
        })
    }

    fn volume(&self, path: &Path) -> Result<VolumeSpace> {
        let answer = match disk_free_space(path) {
            // A file: ask for the folder holding it.
            Err(err) if win32_code(&err) == Some(ERROR_DIRECTORY.0) => match path.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => disk_free_space(parent),
                _ => Err(err),
            },
            other => other,
        };
        match answer {
            Ok((available, total, free)) => Ok(VolumeSpace {
                total,
                free,
                available,
            }),
            Err(err)
                if [
                    ERROR_FILE_NOT_FOUND.0,
                    ERROR_PATH_NOT_FOUND.0,
                    ERROR_INVALID_NAME.0,
                    ERROR_BAD_NETPATH.0,
                ]
                .map(Some)
                .contains(&win32_code(&err)) =>
            {
                Err(ForgeError::NotFound(format!("{} (volume)", path.display())))
            }
            Err(err) => Err(ForgeError::io(
                format!("reading the free space of the volume of {}", path.display()),
                os_error(&err),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_excludes_the_idle_part_of_kernel_time() {
        // 4 CPUs over 1 s (40 M ticks): 25 M idle, kernel 30 M (idle included), user 10 M.
        assert_eq!(
            busy_and_idle(25_000_000, 30_000_000, 10_000_000),
            (15_000_000, 25_000_000)
        );
        // Never underflows on odd values.
        assert_eq!(busy_and_idle(10, 5, 1), (1, 10));
    }

    #[test]
    fn a_file_answers_for_its_folder() {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let direct = disk_free_space(&file).map_err(|e| win32_code(&e));
        println!("GetDiskFreeSpaceExW on a file: {direct:?}");
        assert_eq!(direct, Err(Some(ERROR_DIRECTORY.0)));
        let of_file = WindowsSystemStats.volume(&file).unwrap();
        let of_dir = WindowsSystemStats.volume(file.parent().unwrap()).unwrap();
        assert_eq!(of_file.total, of_dir.total);
    }

    #[test]
    fn directory_argument_ends_with_a_separator() {
        assert_eq!(directory_arg(Path::new("C:")), OsString::from("C:\\"));
        assert_eq!(directory_arg(Path::new("C:\\")), OsString::from("C:\\"));
        assert_eq!(
            directory_arg(Path::new("C:/apps/forgebox")),
            OsString::from("C:/apps/forgebox\\")
        );
        assert_eq!(
            directory_arg(Path::new(r"\\server\share")),
            OsString::from(r"\\server\share\")
        );
        assert_eq!(
            directory_arg(Path::new("C:/apps/")),
            OsString::from("C:/apps/")
        );
    }
}
