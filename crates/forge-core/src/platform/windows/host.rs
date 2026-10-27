//! Windows side of [`crate::platform::host`]: named mutex (`CreateMutexW`), connection
//! cost (`INetworkCostManager`) and detached process launch.

use std::ffi::OsString;
use std::os::windows::process::CommandExt;
use std::path::Path;

use ::windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError, HANDLE,
};
use ::windows::Win32::Networking::NetworkListManager::{
    INetworkCostManager, NLM_CONNECTION_COST_APPROACHINGDATALIMIT, NLM_CONNECTION_COST_FIXED,
    NLM_CONNECTION_COST_OVERDATALIMIT, NLM_CONNECTION_COST_ROAMING, NLM_CONNECTION_COST_VARIABLE,
    NetworkListManager,
};
use ::windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use ::windows::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CreateMutexW, DETACHED_PROCESS,
};
use ::windows::core::HSTRING;

use super::com::ComGuard;
use crate::error::{ForgeError, Result};
use crate::platform::host::NetworkCost;

/// An open named mutex handle (not owned: only the name's existence matters).
pub struct NamedMutex(HANDLE);

// SAFETY: a kernel handle; closing it from any thread is fine.
unsafe impl Send for NamedMutex {}
// SAFETY: the handle is never used after creation except by `Drop`.
unsafe impl Sync for NamedMutex {}

impl NamedMutex {
    /// Creates `name`; `Conflict` if it already exists or belongs to another user.
    pub fn create_new(name: &str) -> Result<Self> {
        let wide = HSTRING::from(name);
        // SAFETY: valid NUL-terminated name; default security; not initially owned.
        let created = unsafe { CreateMutexW(None, false, &wide) };
        match created {
            Ok(handle) => {
                // SAFETY: reads the calling thread's last error right after the call.
                let last = unsafe { GetLastError() };
                if last == ERROR_ALREADY_EXISTS {
                    // SAFETY: the handle was just returned by CreateMutexW.
                    unsafe {
                        let _ = CloseHandle(handle);
                    }
                    return Err(ForgeError::Conflict(format!(
                        "another daemon already owns {name}"
                    )));
                }
                Ok(Self(handle))
            }
            Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => Err(ForgeError::Conflict(
                format!("{name} exists and belongs to another user: {e}"),
            )),
            Err(e) => Err(ForgeError::io(
                format!("creating mutex {name}"),
                std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF),
            )),
        }
    }
}

impl Drop for NamedMutex {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value and closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// `INetworkCostManager::GetCost` of the machine's internet connection.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsNetworkCost;

impl NetworkCost for WindowsNetworkCost {
    fn is_metered(&self) -> Result<bool> {
        let _com = ComGuard::init();
        // SAFETY: COM is initialized on this thread; the CLSID/IID pair is the documented
        // Network List Manager class.
        let manager: INetworkCostManager = unsafe {
            CoCreateInstance(&NetworkListManager, None, CLSCTX_ALL)
        }
        .map_err(|e| ForgeError::UnsupportedPlatform(format!("Network List Manager: {e}")))?;
        let mut cost = 0u32;
        // SAFETY: `cost` outlives the call; a null destination asks for the machine cost.
        unsafe { manager.GetCost(&mut cost, std::ptr::null()) }
            .map_err(|e| ForgeError::Internal(format!("INetworkCostManager::GetCost: {e}")))?;
        let metered = [
            NLM_CONNECTION_COST_FIXED,
            NLM_CONNECTION_COST_VARIABLE,
            NLM_CONNECTION_COST_OVERDATALIMIT,
            NLM_CONNECTION_COST_ROAMING,
            NLM_CONNECTION_COST_APPROACHINGDATALIMIT,
        ]
        .iter()
        .any(|flag| cost & (flag.0 as u32) != 0);
        Ok(metered)
    }
}

/// Detached start: own process group, no console, outside the caller's job when allowed.
pub fn spawn_detached(exe: &Path, args: &[OsString], cwd: &Path) -> Result<u32> {
    let base = DETACHED_PROCESS.0 | CREATE_NEW_PROCESS_GROUP.0;
    let attempt = |flags: u32| {
        std::process::Command::new(exe)
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    let child = match attempt(base | CREATE_BREAKAWAY_FROM_JOB.0) {
        Ok(child) => child,
        // The caller's job does not allow breakaway: start inside it.
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            attempt(base).map_err(|e| ForgeError::io_at("starting", exe, e))?
        }
        Err(e) => return Err(ForgeError::io_at("starting", exe, e)),
    };
    Ok(child.id())
}
