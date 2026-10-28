//! `WM_CLOSE` for the top-level windows of a set of processes (docs/03 §1.3 step 3).
//!
//! `EnumWindows` lists the top-level windows of the caller's desktop (hidden ones
//! included, message-only windows excluded); `GetWindowThreadProcessId` gives the owner.
//! The IME helper windows that every GUI thread owns (`IME`, `MSCTFIME UI`) are skipped:
//! closing them does not ask the application to quit. `PostMessageW` never waits for the
//! target, so a hung GUI cannot block the caller.

use ::windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use ::windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
};
use ::windows::core::BOOL;

use super::process::os_error;
use crate::error::{ForgeError, Result};

/// Window classes owned by the input method framework, never closed.
const IGNORED_CLASSES: &[&str] = &["IME", "MSCTFIME UI"];

struct Search<'a> {
    pids: &'a [u32],
    found: Vec<HWND>,
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `Search` passed by `close_windows`, alive and exclusively
    // borrowed for the whole (synchronous) enumeration.
    let search = unsafe { &mut *(lparam.0 as *mut Search<'_>) };
    let mut pid = 0_u32;
    // SAFETY: `hwnd` comes from the enumeration; `pid` is writable.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if search.pids.contains(&pid) && !is_ignored(hwnd) {
        search.found.push(hwnd);
    }
    true.into()
}

fn is_ignored(hwnd: HWND) -> bool {
    let mut class = [0_u16; 64];
    // SAFETY: writable buffer whose length is passed by the slice.
    let len = unsafe { GetClassNameW(hwnd, &mut class) };
    let Ok(len) = usize::try_from(len) else {
        return false;
    };
    let name = String::from_utf16_lossy(&class[..len.min(class.len())]);
    IGNORED_CLASSES.contains(&name.as_str())
}

/// Posts `WM_CLOSE` to every top-level window owned by `pids`; returns how many windows
/// got the message.
pub(crate) fn close_windows(pids: &[u32]) -> Result<usize> {
    if pids.is_empty() {
        return Ok(0);
    }
    let mut search = Search {
        pids,
        found: Vec::new(),
    };
    // SAFETY: `collect` only dereferences `lparam` as the `Search` above, which outlives
    // the synchronous enumeration.
    unsafe { EnumWindows(Some(collect), LPARAM((&raw mut search) as isize)) }
        .map_err(|e| ForgeError::io("enumerating top-level windows", os_error(&e)))?;
    let mut posted = 0;
    for hwnd in search.found {
        // SAFETY: posting never dereferences anything of ours; a window that vanished in
        // the meantime only makes the call fail.
        if unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_ok() {
            posted += 1;
        }
    }
    Ok(posted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processes_without_windows_get_nothing() {
        assert_eq!(close_windows(&[]).unwrap(), 0);
        // PID 4 is the System process: it owns no top-level window.
        assert_eq!(close_windows(&[4]).unwrap(), 0);
    }
}
