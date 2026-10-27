//! [`UserEnv`] for Windows: `HKCU\Environment\Path` (docs/08 §3, ADR-0025).
//!
//! * The value is read with `RegQueryValueExW`, which returns the data as stored: a
//!   `REG_EXPAND_SZ` keeps its `%USERPROFILE%`-style references, and the type is written
//!   back unchanged. A missing value is created as `REG_EXPAND_SZ` (what Windows uses for
//!   the user `Path`).
//! * Changes are a read-modify-write of the whole value through
//!   [`PathValue`](crate::platform::user_env::PathValue): only Forgebox's own entries are
//!   inserted or removed; everything else is written back byte for byte.
//! * After a change, `WM_SETTINGCHANGE` with `lParam = "Environment"` is broadcast with
//!   `SendMessageTimeoutW(HWND_BROADCAST, SMTO_ABORTIFHUNG)`: hung windows are skipped and
//!   every other window gets at most [`BROADCAST_TIMEOUT`]. Never `SendMessage`, which
//!   would wait forever for a stuck application. A broadcast that times out is logged and
//!   does not fail the operation: the registry is already updated and new processes
//!   started by Explorer read it.
//!
//! Blocking (registry, broadcast): call from `spawn_blocking`.

use std::time::Duration;

use ::windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS, GetLastError, LPARAM, WIN32_ERROR, WPARAM,
};
use ::windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_EXPAND_SZ, REG_SAM_FLAGS, REG_SZ,
    REG_VALUE_TYPE, RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use ::windows::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
};
use ::windows::core::{PCWSTR, w};

use crate::error::{ForgeError, Result};
use crate::platform::user_env::{PathValue, UserEnv, expand_with_process_env};

/// Key of the user environment under `HKEY_CURRENT_USER`.
const ENVIRONMENT_KEY: PCWSTR = w!("Environment");
/// The value holding the user `PATH`.
const PATH_VALUE: PCWSTR = w!("Path");

/// Time each top-level window gets to process `WM_SETTINGCHANGE`.
pub const BROADCAST_TIMEOUT: Duration = Duration::from_millis(1000);

/// [`UserEnv`] backed by `HKCU\Environment`.
#[derive(Debug, Clone, Copy)]
pub struct WindowsUserEnv {
    /// Per-window timeout of the `WM_SETTINGCHANGE` broadcast.
    pub broadcast_timeout: Duration,
}

impl Default for WindowsUserEnv {
    fn default() -> Self {
        Self {
            broadcast_timeout: BROADCAST_TIMEOUT,
        }
    }
}

/// An open registry key, closed on drop.
struct Key(HKEY);

impl Key {
    /// Opens `HKCU\Environment` (always present in a user profile).
    fn open(access: REG_SAM_FLAGS) -> Result<Self> {
        let mut key = HKEY::default();
        // SAFETY: valid NUL-terminated subkey; `key` receives the handle.
        let status =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, ENVIRONMENT_KEY, None, access, &mut key) };
        check(status, r"opening HKCU\Environment")?;
        Ok(Self(key))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from RegOpenKeyExW and is closed once.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn check(status: WIN32_ERROR, what: &str) -> Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(ForgeError::io(
            what,
            std::io::Error::from_raw_os_error(status.0 as i32),
        ))
    }
}

/// The stored user `PATH`: text and registry type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawPath {
    pub text: String,
    pub kind: REG_VALUE_TYPE,
    /// The value existed (an absent value reads as an empty `REG_EXPAND_SZ`).
    pub exists: bool,
}

fn read(key: &Key) -> Result<RawPath> {
    let mut bytes = 0_u32;
    loop {
        let mut buffer = vec![0_u16; (bytes as usize).div_ceil(2).max(1)];
        let mut size = (buffer.len() * 2) as u32;
        let mut kind = REG_VALUE_TYPE::default();
        // SAFETY: `buffer` holds `size` bytes; `kind` and `size` are writable.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PATH_VALUE,
                None,
                Some(&mut kind),
                Some(buffer.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(RawPath {
                text: String::new(),
                kind: REG_EXPAND_SZ,
                exists: false,
            });
        }
        if status == ERROR_MORE_DATA {
            bytes = size.max(bytes + 2);
            continue;
        }
        check(status, r"reading HKCU\Environment\Path")?;
        if kind != REG_SZ && kind != REG_EXPAND_SZ {
            return Err(ForgeError::Conflict(format!(
                r"HKCU\Environment\Path has registry type {} instead of a string; not touching it",
                kind.0
            )));
        }
        buffer.truncate(size as usize / 2);
        while buffer.last() == Some(&0) {
            buffer.pop();
        }
        return Ok(RawPath {
            text: String::from_utf16_lossy(&buffer),
            kind,
            exists: true,
        });
    }
}

fn write(key: &Key, text: &str, kind: REG_VALUE_TYPE) -> Result<()> {
    let bytes: Vec<u8> = text
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    // SAFETY: `bytes` is a NUL-terminated UTF-16 string of the given type.
    let status = unsafe { RegSetValueExW(key.0, PATH_VALUE, None, kind, Some(&bytes)) };
    check(status, r"writing HKCU\Environment\Path")
}

/// Reads the stored value (tests snapshot and restore it).
pub(crate) fn read_raw() -> Result<RawPath> {
    read(&Key::open(KEY_QUERY_VALUE)?)
}

/// Writes `raw` back as is (tests restore the snapshot with it).
#[cfg(test)]
pub(crate) fn write_raw(raw: &RawPath) -> Result<()> {
    write(&Key::open(KEY_SET_VALUE)?, &raw.text, raw.kind)
}

impl WindowsUserEnv {
    /// Read-modify-write of the value; `change` returns whether it modified `PathValue`.
    fn modify<T>(&self, change: impl FnOnce(&mut PathValue) -> (bool, T)) -> Result<T> {
        let key = Key::open(KEY_QUERY_VALUE | KEY_SET_VALUE)?;
        let raw = read(&key)?;
        let mut value = PathValue::parse(&raw.text);
        let (changed, out) = change(&mut value);
        if changed {
            write(&key, &value.to_raw(), raw.kind)?;
            drop(key);
            self.broadcast();
        }
        Ok(out)
    }

    /// `WM_SETTINGCHANGE("Environment")` to every top-level window, bounded per window.
    fn broadcast(&self) {
        let timeout = u32::try_from(self.broadcast_timeout.as_millis()).unwrap_or(u32::MAX);
        let mut result = 0_usize;
        let area = w!("Environment");
        // SAFETY: `area` is a static NUL-terminated string that outlives the call (the
        // system copies it for windows of other processes); `result` is writable.
        let sent = unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                WPARAM(0),
                LPARAM(area.as_ptr() as isize),
                SMTO_ABORTIFHUNG,
                timeout,
                Some(&mut result),
            )
        };
        if sent.0 == 0 {
            // SAFETY: plain thread-local error read right after the call.
            let error = unsafe { GetLastError() };
            tracing::warn!(
                code = error.0,
                "WM_SETTINGCHANGE broadcast did not complete; new terminals still read the \
                 updated PATH"
            );
        } else {
            tracing::debug!("WM_SETTINGCHANGE(Environment) broadcast");
        }
    }
}

impl UserEnv for WindowsUserEnv {
    fn path_entries(&self) -> Result<Vec<String>> {
        Ok(PathValue::parse(&read_raw()?.text).entries())
    }

    fn add_path_entry(&self, entry: &str) -> Result<bool> {
        let changed = self.modify(|value| {
            let changed = value.add(entry, &|name| std::env::var(name).ok());
            (changed, changed)
        })?;
        if changed {
            tracing::info!(entry, expanded = %expand_with_process_env(entry), "added to the user PATH");
        }
        Ok(changed)
    }

    fn remove_path_entries(&self, predicate: &dyn Fn(&str) -> bool) -> Result<Vec<String>> {
        let removed = self.modify(|value| {
            let removed = value.remove_where(predicate);
            (!removed.is_empty(), removed)
        })?;
        if !removed.is_empty() {
            tracing::info!(?removed, "removed from the user PATH");
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_user_path_without_changing_it() {
        let before = read_raw().unwrap();
        let entries = WindowsUserEnv::default().path_entries().unwrap();
        assert_eq!(entries, PathValue::parse(&before.text).entries());
        assert_eq!(read_raw().unwrap(), before);
    }

    /// Restores the snapshot even if an assertion fails.
    struct Restore(RawPath);

    impl Drop for Restore {
        fn drop(&mut self) {
            if read_raw().ok().as_ref() != Some(&self.0) {
                write_raw(&self.0).expect("restoring the user PATH");
            }
        }
    }

    /// Writes the real `HKCU\Environment\Path` and broadcasts `WM_SETTINGCHANGE`; run by
    /// hand: `cargo test -p forge-core --lib -- --ignored real_hkcu_path_add_remove`.
    #[test]
    #[ignore = "writes the real HKCU\\Environment\\Path (manual)"]
    fn real_hkcu_path_add_remove() {
        const ENTRY: &str = r"C:\Forgebox-test-path-entry";
        let before = read_raw().unwrap();
        if !before.exists {
            println!("no user Path value on this machine; nothing to test");
            return;
        }
        let _restore = Restore(before.clone());
        println!(
            "before: type {} ({} entries)",
            before.kind.0,
            PathValue::parse(&before.text).entries().len()
        );
        let env = WindowsUserEnv::default();
        let started = std::time::Instant::now();
        assert!(env.add_path_entry(ENTRY).unwrap(), "added");
        println!("add + broadcast took {:?}", started.elapsed());
        assert!(!env.add_path_entry(ENTRY).unwrap(), "idempotent");
        assert!(!env.add_path_entry(r"c:/forgebox-test-path-entry/").unwrap());
        let entries = env.path_entries().unwrap();
        let count = entries.iter().filter(|e| e.as_str() == ENTRY).count();
        assert_eq!(count, 1, "single entry: {entries:?}");
        assert_eq!(entries[0], ENTRY, "prepended");
        let during = read_raw().unwrap();
        assert_eq!(during.kind, before.kind, "registry type preserved");

        let removed = env.remove_path_entries(&|e| e == ENTRY).unwrap();
        assert_eq!(removed, [ENTRY]);
        let after = read_raw().unwrap();
        assert_eq!(after.text, before.text, "PATH restored byte for byte");
        assert_eq!(after.kind, before.kind);
        println!("after: identical to before (type {})", after.kind.0);
    }
}
