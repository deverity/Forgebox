//! [`ProcessTerminator`] for Windows (ADR-0045): one `OpenProcess` handle with
//! `PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE` carries the whole
//! operation. From it: the image path (`QueryFullProcessImageNameW`), `IsProcessCritical`,
//! the token's user and session (`OpenProcessToken` + `TokenUser`/`TokenSessionId`); the
//! services hosted by the pid come from the service manager
//! (`EnumServicesStatusExW(SC_ENUM_PROCESS_INFO)`, per-user services included). Then the
//! caller decides, and `TerminateProcess` + `WaitForSingleObject` act on the same handle,
//! so a recycled pid can never be the one terminated.

use std::os::windows::io::OwnedHandle;

use ::windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, ERROR_MORE_DATA, HANDLE, WAIT_OBJECT_0,
};
use ::windows::Win32::Security::{TOKEN_QUERY, TokenSessionId};
use ::windows::Win32::System::Services::{
    CloseServiceHandle, ENUM_SERVICE_STATUS_PROCESSW, ENUM_SERVICE_TYPE, EnumServicesStatusExW,
    OpenSCManagerW, SC_ENUM_PROCESS_INFO, SC_MANAGER_ENUMERATE_SERVICE, SERVICE_ACTIVE,
    SERVICE_WIN32,
};
use ::windows::Win32::System::Threading::{
    IsProcessCritical, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};
use ::windows::core::{BOOL, PCWSTR};

use super::process::{owned, raw, win_err, win32_code};
use super::process_stats::image_path;
use super::security::token_user_sid;
use crate::error::{ForgeError, Result};
use crate::platform::process_kill::{KILL_WAIT, ProcessAccount, ProcessFacts, ProcessTerminator};

/// `SERVICE_USER_SERVICE | SERVICE_USERSERVICE_INSTANCE` (per-user services such as
/// `CDPUserSvc_1a2b3`, which run in the user's session as the user).
const USER_SERVICE_TYPES: u32 = 0x40 | 0x80;

/// Well-known SIDs of the service accounts.
const SYSTEM_SIDS: [&str; 3] = ["S-1-5-18", "S-1-5-19", "S-1-5-20"];

/// [`ProcessTerminator`] for Windows.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsProcessTerminator;

fn open(pid: u32) -> Result<OwnedHandle> {
    let access = PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    // SAFETY: plain call; the returned handle is owned below.
    match unsafe { OpenProcess(access, false, pid) } {
        // SAFETY: OpenProcess returned a fresh handle that nobody else closes.
        Ok(handle) => Ok(unsafe { owned(handle) }),
        Err(err) if win32_code(&err) == Some(ERROR_INVALID_PARAMETER.0) => {
            Err(ForgeError::NotFound(format!("process {pid}")))
        }
        Err(err) if win32_code(&err) == Some(ERROR_ACCESS_DENIED.0) => {
            Err(ForgeError::Forbidden(format!(
                "process {pid} cannot be opened for termination (protected, elevated or another user's)"
            )))
        }
        Err(err) => Err(win_err(format!("opening process {pid}"), &err)),
    }
}

fn critical(process: &OwnedHandle) -> bool {
    let mut flag = BOOL::default();
    // SAFETY: valid handle with query access; `flag` is writable. A failed query is
    // treated as critical (refused): only a positive answer lets the kill go on.
    unsafe { IsProcessCritical(raw(process), &mut flag) }.map_or(true, |()| flag.as_bool())
}

/// The account and session of the process, from its token.
fn account(process: &OwnedHandle) -> (ProcessAccount, Option<u32>) {
    let mut token = HANDLE::default();
    // SAFETY: valid process handle; `token` receives a handle owned below.
    if unsafe { OpenProcessToken(raw(process), TOKEN_QUERY, &mut token) }.is_err() {
        return (ProcessAccount::Unknown, None);
    }
    // SAFETY: OpenProcessToken returned a fresh handle.
    let token = unsafe { owned(token) };
    let mut session = 0_u32;
    let mut len = 0_u32;
    // SAFETY: `session` is a writable u32 and its size is passed.
    let session = unsafe {
        ::windows::Win32::Security::GetTokenInformation(
            raw(&token),
            TokenSessionId,
            Some((&mut session as *mut u32).cast()),
            size_of::<u32>() as u32,
            &mut len,
        )
    }
    .ok()
    .map(|()| session);
    let account = match (token_user_sid(raw(&token)), super::current_user_sid()) {
        (Ok(sid), _) if SYSTEM_SIDS.contains(&sid.as_str()) => ProcessAccount::System,
        (Ok(sid), Ok(me)) if sid == me => ProcessAccount::CurrentUser,
        (Ok(_), Ok(_)) => ProcessAccount::OtherUser,
        _ => ProcessAccount::Unknown,
    };
    (account, session)
}

/// One `EnumServicesStatusExW` pass over `types`.
fn enum_services(
    manager: ::windows::Win32::System::Services::SC_HANDLE,
    types: u32,
) -> Result<Vec<(String, u32)>> {
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut resume = 0_u32;
    let mut out = Vec::new();
    loop {
        let mut needed = 0_u32;
        let mut returned = 0_u32;
        // SAFETY: `buffer` is writable; the API fills it with ENUM_SERVICE_STATUS_PROCESSW
        // records whose strings point inside the same buffer.
        let result = unsafe {
            EnumServicesStatusExW(
                manager,
                SC_ENUM_PROCESS_INFO,
                ENUM_SERVICE_TYPE(types),
                SERVICE_ACTIVE,
                Some(&mut buffer),
                &mut needed,
                &mut returned,
                Some(&mut resume),
                PCWSTR::null(),
            )
        };
        let more = match &result {
            Ok(()) => false,
            Err(err) if win32_code(err) == Some(ERROR_MORE_DATA.0) => true,
            Err(err) => return Err(win_err("listing the running services", err)),
        };
        // u8 storage: read each record unaligned.
        let size = size_of::<ENUM_SERVICE_STATUS_PROCESSW>();
        for i in 0..returned as usize {
            let Some(bytes) = buffer.get(i * size..(i + 1) * size) else {
                break;
            };
            // SAFETY: the API wrote `returned` records at the start of `buffer`.
            let record = unsafe {
                std::ptr::read_unaligned(bytes.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>())
            };
            // SAFETY: the name points at a NUL-terminated string inside `buffer`.
            let name = unsafe { record.lpServiceName.to_string() }.unwrap_or_default();
            out.push((name, record.ServiceStatusProcess.dwProcessId));
        }
        if !more {
            return Ok(out);
        }
        if returned == 0 && (needed as usize) > buffer.len() {
            buffer.resize(needed as usize, 0);
        }
    }
}

/// Names of the running services hosted by `pid`.
fn services_of(pid: u32) -> Result<Vec<String>> {
    // SAFETY: plain call; the handle is closed below.
    let manager =
        unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ENUMERATE_SERVICE) }
            .map_err(|e| win_err("opening the service manager", &e))?;
    // Older builds refuse the per-user service bits: retry with the Win32 services only.
    let all = enum_services(manager, SERVICE_WIN32.0 | USER_SERVICE_TYPES)
        .or_else(|_| enum_services(manager, SERVICE_WIN32.0));
    // SAFETY: `manager` was opened above and is closed exactly once.
    let _ = unsafe { CloseServiceHandle(manager) };
    Ok(all?
        .into_iter()
        .filter(|(_, p)| *p == pid)
        .map(|(name, _)| name)
        .collect())
}

impl ProcessTerminator for WindowsProcessTerminator {
    fn terminate_checked(
        &self,
        pid: u32,
        exit_code: u32,
        decide: &mut dyn FnMut(&ProcessFacts) -> Result<()>,
    ) -> Result<ProcessFacts> {
        if pid == 0 || pid == 4 {
            return Err(ForgeError::Forbidden(format!(
                "PID {pid} is the Windows kernel"
            )));
        }
        let process = open(pid)?;
        let (account, session_id) = account(&process);
        let facts = ProcessFacts {
            pid,
            exe_path: image_path(&process),
            account,
            session_id,
            critical: critical(&process),
            services: services_of(pid)?,
        };
        decide(&facts)?;
        // SAFETY: valid handle with PROCESS_TERMINATE, the same object the facts describe.
        unsafe { TerminateProcess(raw(&process), exit_code) }
            .map_err(|e| win_err(format!("terminating process {pid}"), &e))?;
        let wait_ms = u32::try_from(KILL_WAIT.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: valid handle with SYNCHRONIZE.
        if unsafe { WaitForSingleObject(raw(&process), wait_ms) } != WAIT_OBJECT_0 {
            return Err(ForgeError::Internal(format!(
                "process {pid} was terminated but did not exit within {} s",
                KILL_WAIT.as_secs()
            )));
        }
        Ok(facts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn sleeper() -> std::process::Child {
        // `ping -n 60` runs for about a minute from System32, as the current user.
        Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[test]
    fn facts_of_a_child_and_the_decision_comes_first() {
        let mut child = sleeper();
        let pid = child.id();
        let refused = WindowsProcessTerminator
            .terminate_checked(pid, 1, &mut |facts| {
                assert_eq!(facts.pid, pid);
                assert_eq!(facts.account, ProcessAccount::CurrentUser);
                assert!(!facts.critical);
                assert!(facts.services.is_empty());
                assert_ne!(facts.session_id, Some(0));
                let exe = facts.exe_path.as_ref().unwrap();
                assert!(exe.to_string_lossy().to_lowercase().ends_with("ping.exe"));
                Err(ForgeError::Forbidden("test says no".into()))
            })
            .unwrap_err();
        assert_eq!(refused.code(), "forbidden");
        assert!(
            child.try_wait().unwrap().is_none(),
            "refused: still running"
        );

        let facts = WindowsProcessTerminator
            .terminate_checked(pid, 7, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(facts.pid, pid);
        assert_eq!(child.wait().unwrap().code(), Some(7));
    }

    #[test]
    fn missing_and_system_processes() {
        // A pid that cannot exist (pids are multiples of 4).
        let err = WindowsProcessTerminator
            .terminate_checked(0xFFFF_FFF1, 1, &mut |_| Ok(()))
            .unwrap_err();
        assert_eq!(err.code(), "not_found");
        let err = WindowsProcessTerminator
            .terminate_checked(4, 1, &mut |_| Ok(()))
            .unwrap_err();
        assert_eq!(err.code(), "forbidden");
    }

    #[test]
    fn the_service_manager_lists_running_services() {
        // Our own process hosts none; some process always hosts a running service, and
        // its pid maps back to that service.
        assert!(services_of(std::process::id()).unwrap().is_empty());
        // SAFETY: plain call; closed below.
        let manager =
            unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ENUMERATE_SERVICE) }
                .unwrap();
        let all = enum_services(manager, SERVICE_WIN32.0 | USER_SERVICE_TYPES).unwrap();
        // SAFETY: opened above, closed once.
        let _ = unsafe { CloseServiceHandle(manager) };
        let (name, pid) = all.iter().find(|(_, pid)| *pid != 0).unwrap().clone();
        assert!(services_of(pid).unwrap().contains(&name), "{name} in {pid}");
    }
}
