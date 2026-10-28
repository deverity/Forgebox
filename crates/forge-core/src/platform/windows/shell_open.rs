//! [`ShellOpener`] for Windows (docs/07 §4, ADR-0039): `ShellExecuteExW` with the default
//! verb opens a URL in the default browser and a folder in Explorer. The calling thread
//! (a `spawn_blocking` one) gets a single-threaded COM apartment for the call, as the
//! shell expects; `SEE_MASK_NOASYNC` makes the call finish before that apartment goes
//! away. Inputs are checked first ([`crate::platform::shell_open`]): the shell would run
//! anything it is handed.

use std::ffi::OsStr;
use std::path::Path;

use ::windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use ::windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use ::windows::core::PCWSTR;

use super::elevation::Com;
use super::process::{os_error, wide};
use crate::error::{ForgeError, Result};
use crate::platform::ShellOpener;
use crate::platform::shell_open::{validate_path, validate_url};

/// [`ShellOpener`] through `ShellExecuteExW`.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsShellOpener;

/// Runs `target` with the default verb.
fn execute(target: &OsStr, what: &str) -> Result<()> {
    let file = wide(target);
    let _com = Com::init();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` is fully initialized and `file` outlives the call.
    unsafe { ShellExecuteExW(&mut info) }
        .map_err(|e| ForgeError::io(format!("opening {what}"), os_error(&e)))
}

impl ShellOpener for WindowsShellOpener {
    fn open_url(&self, url: &str) -> Result<()> {
        let url = validate_url(url)?;
        execute(OsStr::new(url), url)
    }

    fn open_path(&self, path: &Path) -> Result<()> {
        validate_path(path)?;
        execute(path.as_os_str(), &path.display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the refusals: a successful call would open a window on the desktop.
    #[test]
    fn bad_input_is_refused_before_the_shell_sees_it() {
        let opener = WindowsShellOpener;
        assert_eq!(
            opener
                .open_url("file:///C:/Windows/notepad.exe")
                .unwrap_err()
                .code(),
            "invalid_input"
        );
        assert_eq!(
            opener.open_url("calc.exe").unwrap_err().code(),
            "invalid_input"
        );
        assert_eq!(
            opener
                .open_path(Path::new(r"C:\definitely-not-here"))
                .unwrap_err()
                .code(),
            "not_found"
        );
        assert_eq!(
            opener.open_path(Path::new("relative")).unwrap_err().code(),
            "invalid_input"
        );
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("tool.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        assert_eq!(opener.open_path(&exe).unwrap_err().code(), "invalid_input");
    }
}
