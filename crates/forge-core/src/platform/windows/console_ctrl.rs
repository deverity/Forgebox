//! `CTRL_BREAK` delivery to a supervised process (docs/03 §1.3 step 2, ADR-0016).
//!
//! `GenerateConsoleCtrlEvent` only reaches processes attached to the **caller's**
//! console. Every service runs with its own windowless console (`CREATE_NO_WINDOW`), so
//! the caller has to attach to that console first. Console attachment is process-wide
//! state: a process has at most one console, `FreeConsole` on the daemon's own console
//! (a developer terminal) cannot be undone, and every other thread writing to it would
//! race with the switch. The daemon therefore never attaches anywhere. It starts the
//! tiny `forgebox-ctrl` helper instead, without a console of its own
//! (`DETACHED_PROCESS`); the helper ignores every control event, attaches to the
//! target's console, generates `CTRL_BREAK_EVENT` for the target's process group and
//! exits.
//!
//! Limitations (documented in docs/03 §1.3):
//! * the target must have a console: [`ConsoleMode::Hidden`] (the default) and
//!   `new_process_group`; a [`ConsoleMode::Detached`] process cannot receive it;
//! * the event reaches the whole process group on that console (the service and the
//!   children that share its console), which is what a terminal's Ctrl+Break does;
//! * each delivery costs one short-lived process (tens of milliseconds);
//! * if the helper is missing the step fails and the stop cascade moves on.
//!
//! Helper protocol: `forgebox-ctrl break <pid>`. Exit code `0` = delivered;
//! `1` = usage error; `0x1000_0000 | win32` = `AttachConsole` failed;
//! `0x2000_0000 | win32` = `GenerateConsoleCtrlEvent` failed.
//!
//! [`ConsoleMode::Hidden`]: crate::platform::ConsoleMode::Hidden
//! [`ConsoleMode::Detached`]: crate::platform::ConsoleMode::Detached

use std::ffi::OsString;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::time::Duration;

use ::windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_GEN_FAILURE, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER,
    GetLastError, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use ::windows::Win32::System::Console::{
    AttachConsole, CTRL_BREAK_EVENT, FreeConsole, GenerateConsoleCtrlEvent, SetConsoleCtrlHandler,
};
use ::windows::Win32::System::Threading::{
    GetExitCodeProcess, TerminateProcess, WaitForSingleObject,
};
use ::windows::core::BOOL;

use super::process::{create_process, system_directory};
use crate::error::{ForgeError, Result};
use crate::platform::{ConsoleMode, ProcessCommand, StdioTarget};

/// Exit code of the helper on success.
pub const EXIT_OK: i32 = 0;
/// Exit code of the helper on a usage error.
pub const EXIT_USAGE: i32 = 1;
/// Flag OR-ed with the Win32 error when `AttachConsole` fails.
pub const EXIT_ATTACH_FAILED: i32 = 0x1000_0000;
/// Flag OR-ed with the Win32 error when `GenerateConsoleCtrlEvent` fails.
pub const EXIT_GENERATE_FAILED: i32 = 0x2000_0000;

/// Handler installed by the helper: swallow every control event (it must survive the
/// event it generates if the target turns out not to be a group leader).
unsafe extern "system" fn ignore_all(_ctrl_type: u32) -> BOOL {
    true.into()
}

fn last_error_code() -> i32 {
    // SAFETY: plain call.
    (unsafe { GetLastError() }.0 & 0x0FFF_FFFF) as i32
}

/// Body of the `forgebox-ctrl` binary.
pub fn helper_main(args: impl IntoIterator<Item = OsString>) -> i32 {
    let args: Vec<OsString> = args.into_iter().collect();
    let pid = match args.as_slice() {
        [command, pid] if command == "break" => pid.to_str().and_then(|p| p.parse::<u32>().ok()),
        _ => None,
    };
    let Some(pid) = pid.filter(|&pid| pid != 0) else {
        eprintln!(
            "forgebox-ctrl: internal helper of forgeboxd (delivers CTRL_BREAK to a supervised \
             process)\nusage: forgebox-ctrl break <pid>"
        );
        return EXIT_USAGE;
    };
    // SAFETY: plain Win32 calls on this (short-lived) process; `ignore_all` is a valid
    // handler for the whole life of the process.
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(ignore_all), true);
        // Normally started without a console (DETACHED_PROCESS); detach just in case.
        let _ = FreeConsole();
        if AttachConsole(pid).is_err() {
            return EXIT_ATTACH_FAILED | last_error_code();
        }
        let generated = GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid);
        let code = last_error_code();
        let _ = FreeConsole();
        if generated.is_err() {
            return EXIT_GENERATE_FAILED | code;
        }
    }
    EXIT_OK
}

/// Runs `helper break <group_leader>` and waits at most `timeout` for it.
pub(crate) fn send_ctrl_break(helper: &Path, group_leader: u32, timeout: Duration) -> Result<()> {
    if !helper.is_file() {
        return Err(ForgeError::NotFound(format!(
            "CTRL_BREAK helper {} (ships next to forgeboxd)",
            helper.display()
        )));
    }
    let system = system_directory();
    let mut cmd = ProcessCommand::new(helper, &system);
    cmd.args = vec!["break".into(), group_leader.to_string().into()];
    cmd.env = std::env::var_os("SystemRoot")
        .map(|root| vec![(OsString::from("SystemRoot"), root)])
        .unwrap_or_default();
    cmd.stdout = StdioTarget::Null;
    cmd.stderr = StdioTarget::Null;
    cmd.console = ConsoleMode::Detached;
    cmd.new_process_group = true;
    let created = create_process(None, &cmd)?;
    let process = HANDLE(created.process.as_raw_handle());
    let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: valid process handle; bounded wait (this function is blocking by contract).
    let state = unsafe { WaitForSingleObject(process, millis) };
    if state == WAIT_TIMEOUT {
        // SAFETY: valid process handle.
        let _ = unsafe { TerminateProcess(process, 1) };
        return Err(ForgeError::Timeout {
            operation: format!("delivering CTRL_BREAK to process {group_leader}"),
            after: timeout,
        });
    }
    if state != WAIT_OBJECT_0 {
        return Err(ForgeError::io(
            "waiting for the CTRL_BREAK helper",
            std::io::Error::last_os_error(),
        ));
    }
    let mut code = 0_u32;
    // SAFETY: valid process handle; `code` is writable.
    unsafe { GetExitCodeProcess(process, &mut code) }.map_err(|e| {
        ForgeError::io(
            "reading the CTRL_BREAK helper exit code",
            super::process::os_error(&e),
        )
    })?;
    decode_helper_exit(group_leader, code as i32)
}

/// Maps the helper exit code to a result.
fn decode_helper_exit(pid: u32, code: i32) -> Result<()> {
    if code == EXIT_OK {
        return Ok(());
    }
    let win32 = (code & 0x0FFF_FFFF) as u32;
    let os = std::io::Error::from_raw_os_error(win32 as i32);
    if code & EXIT_ATTACH_FAILED != 0 {
        // ERROR_INVALID_PARAMETER: no such process; ERROR_INVALID_HANDLE /
        // ERROR_GEN_FAILURE: the process has no console (detached or console gone).
        return Err(match win32 {
            w if w == ERROR_INVALID_PARAMETER.0 => ForgeError::NotFound(format!("process {pid}")),
            w if w == ERROR_INVALID_HANDLE.0 || w == ERROR_GEN_FAILURE.0 => {
                ForgeError::UnsupportedPlatform(format!(
                    "process {pid} has no console to receive CTRL_BREAK ({os})"
                ))
            }
            w if w == ERROR_ACCESS_DENIED.0 => {
                ForgeError::io(format!("attaching to the console of process {pid}"), os)
            }
            _ => ForgeError::io(format!("attaching to the console of process {pid}"), os),
        });
    }
    if code & EXIT_GENERATE_FAILED != 0 {
        return Err(ForgeError::io(
            format!("generating CTRL_BREAK for process group {pid}"),
            os,
        ));
    }
    Err(ForgeError::Internal(format!(
        "CTRL_BREAK helper exited with code {code:#x}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_errors_do_not_touch_consoles() {
        assert_eq!(helper_main(Vec::<OsString>::new()), EXIT_USAGE);
        assert_eq!(helper_main(["break".into(), "abc".into()]), EXIT_USAGE);
        assert_eq!(helper_main(["break".into(), "0".into()]), EXIT_USAGE);
        assert_eq!(helper_main(["kill".into(), "12".into()]), EXIT_USAGE);
    }

    #[test]
    fn helper_exit_codes_map_to_errors() {
        assert!(decode_helper_exit(1, EXIT_OK).is_ok());
        let not_found = decode_helper_exit(1, EXIT_ATTACH_FAILED | 87).unwrap_err();
        assert_eq!(not_found.code(), "not_found");
        let no_console = decode_helper_exit(1, EXIT_ATTACH_FAILED | 6).unwrap_err();
        assert_eq!(no_console.code(), "unsupported_platform");
        let generate = decode_helper_exit(1, EXIT_GENERATE_FAILED | 5).unwrap_err();
        assert_eq!(generate.code(), "io");
        assert_eq!(
            decode_helper_exit(1, 0xC000_013A_u32 as i32)
                .unwrap_err()
                .code(),
            "internal"
        );
    }

    #[test]
    fn missing_helper_is_not_found() {
        let err = send_ctrl_break(
            Path::new(r"C:\no\such\forgebox-ctrl.exe"),
            4,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(err.code(), "not_found");
    }
}
