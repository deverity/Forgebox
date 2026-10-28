//! Visual C++ v14 Redistributable (x64) detection (docs/03 §1.5).
//!
//! Two sources, the newest wins:
//! 1. The registration of the redistributable,
//!    `HKLM\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64` (`Installed` = 1,
//!    `Major`/`Minor`/`Bld`/`Rbld`), read in the 64-bit and then the 32-bit registry
//!    view (the x64 package has written to either over the years).
//! 2. The file versions of `vcruntime140.dll` and `msvcp140.dll` in the system folder
//!    (the oldest of the two counts). Runtimes deployed without the redistributable
//!    package (merge modules, OS images, other installers) have no registry entry but
//!    load just the same; without this a working machine would be told to install it.
//!
//! Registry and file reads are blocking: callers use `spawn_blocking`.

use std::ffi::c_void;
use std::path::Path;

use ::windows::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};
use ::windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
};
use ::windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, REG_ROUTINE_FLAGS, RRF_RT_REG_DWORD, RRF_SUBKEY_WOW6432KEY,
    RRF_SUBKEY_WOW6464KEY, RegGetValueW,
};
use ::windows::core::{PCWSTR, w};

use super::process::{system_directory, wide};
use crate::error::Result;
use crate::platform::runtime_deps::{RuntimeDeps, VcRuntimeVersion};

/// Registration key of the x64 v14 runtime.
const RUNTIME_KEY: PCWSTR = w!(r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64");

/// The DLLs every v14 runtime installs into the system folder.
const RUNTIME_DLLS: [&str; 2] = ["vcruntime140.dll", "msvcp140.dll"];

/// [`RuntimeDeps`] for Windows.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsRuntimeDeps;

/// A `REG_DWORD` value of [`RUNTIME_KEY`] in the `view` (`RRF_SUBKEY_WOW64*KEY`).
/// `None` when the key or the value is missing (or is not a DWORD).
fn dword(view: REG_ROUTINE_FLAGS, value: PCWSTR) -> Option<u32> {
    let mut data = 0_u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: valid NUL-terminated strings; `data` holds `size` bytes.
    let status: WIN32_ERROR = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            RUNTIME_KEY,
            value,
            RRF_RT_REG_DWORD | view,
            None,
            Some((&raw mut data).cast::<c_void>()),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS).then_some(data)
}

/// The runtime registered in one registry view, if `Installed` is 1.
fn registered(view: REG_ROUTINE_FLAGS) -> Option<VcRuntimeVersion> {
    if dword(view, w!("Installed"))? != 1 {
        return None;
    }
    Some(VcRuntimeVersion::new(
        dword(view, w!("Major"))?,
        dword(view, w!("Minor"))?,
        dword(view, w!("Bld")).unwrap_or(0),
        dword(view, w!("Rbld")).unwrap_or(0),
    ))
}

/// File version (`VS_FIXEDFILEINFO`) of `path`; `None` when the file is missing or has
/// no version resource.
fn file_version(path: &Path) -> Option<VcRuntimeVersion> {
    let name = wide(path.as_os_str());
    // SAFETY: NUL-terminated path.
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(name.as_ptr()), None) };
    if size == 0 {
        return None;
    }
    let mut block = vec![0_u8; size as usize];
    // SAFETY: `block` holds `size` bytes.
    unsafe {
        GetFileVersionInfoW(
            PCWSTR(name.as_ptr()),
            None,
            size,
            block.as_mut_ptr().cast::<c_void>(),
        )
    }
    .ok()?;
    let mut info: *mut c_void = std::ptr::null_mut();
    let mut len = 0_u32;
    // SAFETY: `block` is a version resource filled above; `\` selects VS_FIXEDFILEINFO,
    // returned as a pointer into `block`.
    let found = unsafe {
        VerQueryValueW(
            block.as_ptr().cast::<c_void>(),
            w!("\\"),
            &mut info,
            &mut len,
        )
    };
    if !found.as_bool() || info.is_null() || (len as usize) < size_of::<VS_FIXEDFILEINFO>() {
        return None;
    }
    // SAFETY: VerQueryValueW returned a VS_FIXEDFILEINFO of `len` bytes inside `block`,
    // which is still alive.
    let fixed = unsafe { info.cast::<VS_FIXEDFILEINFO>().read_unaligned() };
    Some(VcRuntimeVersion::new(
        fixed.dwFileVersionMS >> 16,
        fixed.dwFileVersionMS & 0xFFFF,
        fixed.dwFileVersionLS >> 16,
        fixed.dwFileVersionLS & 0xFFFF,
    ))
}

/// The runtime deployed in the system folder: every runtime DLL present, the oldest
/// version of them.
fn deployed(system: &Path) -> Option<VcRuntimeVersion> {
    RUNTIME_DLLS
        .iter()
        .map(|dll| file_version(&system.join(dll)))
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .min()
        .filter(|v| v.major == 14)
}

impl RuntimeDeps for WindowsRuntimeDeps {
    fn vcredist_version(&self) -> Result<Option<VcRuntimeVersion>> {
        let registered = [RRF_SUBKEY_WOW6464KEY, RRF_SUBKEY_WOW6432KEY]
            .into_iter()
            .filter_map(registered)
            .max();
        Ok(registered.max(deployed(&system_directory())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_file_versions_of_system_dlls() {
        // kernel32.dll always exists and carries a version resource (10.0.x on Win10+).
        let kernel32 = file_version(&system_directory().join("kernel32.dll")).unwrap();
        assert!(kernel32.major >= 6, "{kernel32}");
        assert_eq!(file_version(Path::new(r"C:\no\such\file.dll")), None);
    }

    #[test]
    fn missing_dlls_mean_no_deployed_runtime() {
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(deployed(empty.path()), None);
    }

    #[test]
    fn the_newest_source_wins() {
        let registry = [RRF_SUBKEY_WOW6464KEY, RRF_SUBKEY_WOW6432KEY]
            .into_iter()
            .filter_map(registered)
            .max();
        let files = deployed(&system_directory());
        eprintln!("registered: {registry:?}, deployed: {files:?}");
        let best = WindowsRuntimeDeps.vcredist_version().unwrap();
        assert_eq!(best, registry.max(files));
    }
}
