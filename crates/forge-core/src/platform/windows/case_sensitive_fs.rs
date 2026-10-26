//! NTFS per-directory case sensitivity (docs/15 §3, ADR-0010).
//!
//! Uses `NtQueryInformationFile` / `NtSetInformationFile` with
//! `FileCaseSensitiveInformation` (class 71) and `FILE_CS_FLAG_CASE_SENSITIVE_DIR`, the
//! same mechanism as `fsutil file setCaseSensitiveInfo` and WSL. Behaviour observed on
//! Windows 10 22H2 (19045), NTFS:
//! - enabling works on empty and non-empty directories, without elevation;
//! - only the directory itself changes; existing subdirectories keep their flag and
//!   new subdirectories inherit it at creation;
//! - disabling fails with `STATUS_CASE_DIFFERING_NAMES_IN_DIR` while the directory
//!   holds names that differ only by case.

use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use ::windows::Wdk::Storage::FileSystem::{
    FILE_CASE_SENSITIVE_INFORMATION, FileCaseSensitiveInformation, NtQueryInformationFile,
    NtSetInformationFile,
};
use ::windows::Win32::Foundation::{
    HANDLE, NTSTATUS, RtlNtStatusToDosError, STATUS_ACCESS_DENIED,
    STATUS_CASE_DIFFERING_NAMES_IN_DIR, STATUS_DIRECTORY_NOT_EMPTY, STATUS_INVALID_DEVICE_REQUEST,
    STATUS_INVALID_INFO_CLASS, STATUS_INVALID_PARAMETER, STATUS_NOT_SUPPORTED,
};
use ::windows::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, GetVolumeInformationByHandleW,
};
use ::windows::Win32::System::IO::IO_STATUS_BLOCK;

use crate::error::{ForgeError, IoResultExt, Result};
use crate::platform::{CaseSensitiveFs, CaseSensitivitySupport};

/// `FILE_CS_FLAG_CASE_SENSITIVE_DIR` (ntifs.h), kept local to avoid pulling the large
/// `Win32_System_SystemServices` feature for one constant.
const FILE_CS_FLAG_CASE_SENSITIVE_DIR: u32 = 0x0000_0001;

/// [`CaseSensitiveFs`] backed by the NTFS per-directory flag.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsCaseSensitiveFs;

/// Opens `dir` (a directory, following junctions) with attribute access only.
fn open_dir(dir: &Path, write: bool) -> Result<File> {
    let meta = std::fs::metadata(dir).at("inspecting", dir)?;
    if !meta.is_dir() {
        return Err(ForgeError::InvalidInput(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    let mut access = FILE_READ_ATTRIBUTES.0;
    if write {
        access |= FILE_WRITE_ATTRIBUTES.0;
    }
    OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
        // Required to obtain a handle to a directory.
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(dir)
        .at("opening", dir)
}

fn handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

/// File system name of the volume holding `file` (`NTFS`, `FAT32`, `ReFS`...).
fn file_system_name(file: &File) -> Option<String> {
    let mut name = [0u16; 64];
    // SAFETY: the handle is valid for the lifetime of `file`; `name` is a writable
    // buffer whose length is passed by the slice; other outputs are not requested.
    let ok = unsafe {
        GetVolumeInformationByHandleW(handle(file), None, None, None, None, Some(&mut name))
    };
    ok.ok()?;
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    Some(String::from_utf16_lossy(&name[..len]))
}

fn is_unsupported(status: NTSTATUS) -> bool {
    [
        STATUS_NOT_SUPPORTED,
        STATUS_INVALID_INFO_CLASS,
        STATUS_INVALID_PARAMETER,
        STATUS_INVALID_DEVICE_REQUEST,
    ]
    .contains(&status)
}

fn unsupported_reason(file: &File, status: NTSTATUS) -> String {
    let fs = file_system_name(file).unwrap_or_else(|| "unknown".to_owned());
    format!(
        "per-directory case sensitivity is not available on this volume \
         (file system {fs}, NTSTATUS 0x{:08X}); NTFS on Windows 10 1803+ is required",
        status.0 as u32
    )
}

fn status_error(action: &str, dir: &Path, status: NTSTATUS) -> ForgeError {
    // SAFETY: pure conversion function without pointers.
    let win32 = unsafe { RtlNtStatusToDosError(status) };
    ForgeError::io(
        format!(
            "{action} {} (NTSTATUS 0x{:08X})",
            dir.display(),
            status.0 as u32
        ),
        std::io::Error::from_raw_os_error(win32 as i32),
    )
}

fn query(file: &File) -> std::result::Result<bool, NTSTATUS> {
    let mut iosb = IO_STATUS_BLOCK::default();
    let mut info = FILE_CASE_SENSITIVE_INFORMATION::default();
    // SAFETY: the handle is valid while `file` lives; `iosb` and `info` are valid,
    // writable and live for the whole call; the length matches the type of `info`,
    // which is what the kernel expects for class 71.
    let status = unsafe {
        NtQueryInformationFile(
            handle(file),
            &mut iosb,
            (&raw mut info).cast(),
            size_of::<FILE_CASE_SENSITIVE_INFORMATION>() as u32,
            FileCaseSensitiveInformation,
        )
    };
    if status.is_ok() {
        Ok(info.Flags & FILE_CS_FLAG_CASE_SENSITIVE_DIR != 0)
    } else {
        Err(status)
    }
}

impl CaseSensitiveFs for WindowsCaseSensitiveFs {
    fn support(&self, dir: &Path) -> CaseSensitivitySupport {
        let file = match open_dir(dir, false) {
            Ok(file) => file,
            Err(err) => return CaseSensitivitySupport::Unsupported(err.to_string()),
        };
        match query(&file) {
            Ok(_) => CaseSensitivitySupport::Supported,
            Err(status) if is_unsupported(status) => {
                CaseSensitivitySupport::Unsupported(unsupported_reason(&file, status))
            }
            Err(status) => CaseSensitivitySupport::Unsupported(
                status_error("querying", dir, status).to_string(),
            ),
        }
    }

    fn get(&self, dir: &Path) -> Result<bool> {
        let file = open_dir(dir, false)?;
        query(&file).map_err(|status| {
            if is_unsupported(status) {
                ForgeError::UnsupportedPlatform(unsupported_reason(&file, status))
            } else {
                status_error("querying case sensitivity of", dir, status)
            }
        })
    }

    fn set(&self, dir: &Path, enabled: bool) -> Result<()> {
        let file = open_dir(dir, true)?;
        let info = FILE_CASE_SENSITIVE_INFORMATION {
            Flags: if enabled {
                FILE_CS_FLAG_CASE_SENSITIVE_DIR
            } else {
                0
            },
        };
        let mut iosb = IO_STATUS_BLOCK::default();
        // SAFETY: the handle (opened with FILE_WRITE_ATTRIBUTES) is valid while `file`
        // lives; `info` is a readable buffer of exactly the length passed; `iosb` is
        // writable for the whole call.
        let status = unsafe {
            NtSetInformationFile(
                handle(&file),
                &mut iosb,
                (&raw const info).cast(),
                size_of::<FILE_CASE_SENSITIVE_INFORMATION>() as u32,
                FileCaseSensitiveInformation,
            )
        };
        if status.is_ok() {
            return Ok(());
        }
        Err(match status {
            s if is_unsupported(s) => ForgeError::UnsupportedPlatform(unsupported_reason(&file, s)),
            STATUS_ACCESS_DENIED => {
                ForgeError::NeedsElevation(format!("fs.set_case_sensitive {}", dir.display()))
            }
            STATUS_CASE_DIFFERING_NAMES_IN_DIR => ForgeError::Conflict(format!(
                "{} contains names that differ only by case",
                dir.display()
            )),
            STATUS_DIRECTORY_NOT_EMPTY => ForgeError::Conflict(format!(
                "{} must be empty to change case sensitivity",
                dir.display()
            )),
            s => status_error("setting case sensitivity of", dir, s),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Returns the temp dir, or `None` (test skipped with a message) when the volume of
    /// `%TEMP%` cannot use the flag. `FORGEBOX_REQUIRE_CASE_SENSITIVE=1` turns the skip
    /// into a failure (NTFS dev machines and CI).
    fn supported_tempdir() -> Option<tempfile::TempDir> {
        let dir = tempfile::tempdir().unwrap();
        match WindowsCaseSensitiveFs.support(dir.path()) {
            CaseSensitivitySupport::Supported => Some(dir),
            CaseSensitivitySupport::Unsupported(reason) => {
                if std::env::var("FORGEBOX_REQUIRE_CASE_SENSITIVE").as_deref() == Ok("1") {
                    panic!("case sensitivity required but unsupported: {reason}");
                }
                eprintln!("SKIPPED: case sensitivity unsupported here: {reason}");
                None
            }
        }
    }

    #[test]
    fn flagged_directory_distinguishes_case() {
        let Some(base) = supported_tempdir() else {
            return;
        };
        let cs = WindowsCaseSensitiveFs;
        let dir = base.path().join("cs");
        fs::create_dir(&dir).unwrap();
        assert!(!cs.get(&dir).unwrap(), "new dirs in %TEMP% are insensitive");

        cs.set(&dir, true).unwrap();
        assert!(cs.get(&dir).unwrap());
        fs::write(dir.join("A.txt"), "upper").unwrap();
        fs::write(dir.join("a.txt"), "lower").unwrap();
        assert_eq!(entries(&dir), vec!["A.txt", "a.txt"]);
        assert_eq!(fs::read_to_string(dir.join("A.txt")).unwrap(), "upper");
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "lower");
        assert!(!dir.join("A.TXT").exists(), "lookups are exact");

        // New subdirectories inherit the flag.
        fs::create_dir(dir.join("Sub")).unwrap();
        assert!(cs.get(&dir.join("Sub")).unwrap());

        // Cannot go back while names differ only by case.
        let err = cs.set(&dir, false).unwrap_err();
        assert_eq!(err.code(), "conflict", "{err}");
        fs::remove_file(dir.join("A.txt")).unwrap();
        fs::remove_dir(dir.join("Sub")).unwrap();
        fs::remove_file(dir.join("a.txt")).unwrap();

        cs.set(&dir, false).unwrap();
        assert!(!cs.get(&dir).unwrap());
        fs::write(dir.join("A.txt"), "x").unwrap();
        assert!(dir.join("a.txt").exists(), "insensitive again");
    }

    #[test]
    fn errors_are_typed() {
        let base = tempfile::tempdir().unwrap();
        let cs = WindowsCaseSensitiveFs;
        let missing = base.path().join("missing");
        assert_eq!(cs.get(&missing).unwrap_err().code(), "not_found");
        assert!(matches!(
            cs.support(&missing),
            CaseSensitivitySupport::Unsupported(_)
        ));
        let file = base.path().join("f.txt");
        fs::write(&file, "x").unwrap();
        assert_eq!(cs.set(&file, true).unwrap_err().code(), "invalid_input");
    }

    #[test]
    fn temp_volume_reports_its_file_system() {
        let base = tempfile::tempdir().unwrap();
        let file = open_dir(base.path(), false).unwrap();
        let name = file_system_name(&file).unwrap();
        assert!(!name.is_empty());
    }
}
