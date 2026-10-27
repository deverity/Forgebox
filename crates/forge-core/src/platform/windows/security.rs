//! Security descriptor helpers shared by [`super::file_acl`] and [`super::pipe_security`].
//!
//! Everything works through SDDL strings: the descriptor we want is written as SDDL,
//! converted with `ConvertStringSecurityDescriptorToSecurityDescriptorW` and freed with
//! `LocalFree`; the descriptor we read back is converted to SDDL for inspection. All
//! functions are blocking but fast (token and descriptor queries, no I/O).

use std::ffi::c_void;
use std::path::Path;

use ::windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, HLOCAL, LocalFree};
use ::windows::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT, SE_KERNEL_OBJECT,
};
use ::windows::Win32::Security::{
    GetTokenInformation, OBJECT_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_ELEVATION,
    TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenUser,
};
use ::windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use ::windows::core::{HSTRING, PWSTR};

use crate::error::{ForgeError, Result};

/// Converts a Win32 error into [`ForgeError::Io`] with `context`.
pub(crate) fn win_err(context: impl Into<String>, err: ::windows::core::Error) -> ForgeError {
    ForgeError::io(context, std::io::Error::from(err))
}

/// Memory returned by an API that must be released with `LocalFree`.
pub(crate) struct LocalBox(*mut c_void);

impl LocalBox {
    /// Takes ownership of memory allocated with `LocalAlloc` by a Win32 API (null is
    /// accepted and never freed).
    pub(crate) fn new(ptr: *mut c_void) -> Self {
        Self(ptr)
    }

    pub(crate) fn as_ptr(&self) -> *mut c_void {
        self.0
    }
}

impl Drop for LocalBox {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer was allocated by a Win32 API documented to use
            // `LocalAlloc` and is freed exactly once.
            unsafe {
                LocalFree(Some(HLOCAL(self.0)));
            }
        }
    }
}

/// Takes ownership of a `PWSTR` allocated with `LocalAlloc` and returns it as `String`.
fn take_local_wide(text: PWSTR) -> Result<String> {
    let owner = LocalBox(text.0.cast());
    // SAFETY: `text` is a valid NUL-terminated wide string returned by the API; it is
    // read before `owner` frees it.
    let value = unsafe { text.to_string() }
        .map_err(|e| ForgeError::Internal(format!("invalid UTF-16 from Win32: {e}")));
    drop(owner);
    value
}

/// String form (`S-1-5-21-...`) of a SID.
pub(crate) fn sid_to_string(sid: PSID) -> Result<String> {
    let mut text = PWSTR::null();
    // SAFETY: `sid` points to a valid SID owned by the caller for the duration of the
    // call; `text` receives a LocalAlloc'd string released by `take_local_wide`.
    unsafe { ConvertSidToStringSidW(sid, &mut text) }
        .map_err(|e| win_err("converting SID to string", e))?;
    take_local_wide(text)
}

/// SID of the user this process runs as (`S-1-5-21-...`).
pub fn current_user_sid() -> Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: `GetCurrentProcess` returns a pseudo handle that needs no closing; `token`
    // receives a real handle closed below.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(|e| win_err("opening the process token", e))?;
    let result = token_user_sid(token);
    // SAFETY: `token` is a valid handle opened above and closed exactly once.
    let _ = unsafe { CloseHandle(token) };
    result
}

/// Whether this process runs with an elevated token (`TokenElevation`): an administrator
/// after consent, or with UAC disabled. The elevated helper refuses to act otherwise.
pub fn process_is_elevated() -> Result<bool> {
    let mut token = HANDLE::default();
    // SAFETY: pseudo handle of the current process; `token` is closed below.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(|e| win_err("opening the process token", e))?;
    let mut elevation = TOKEN_ELEVATION::default();
    let mut len = 0_u32;
    // SAFETY: `elevation` is writable and its size is passed.
    let result = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    }
    .map_err(|e| win_err("querying the token elevation", e));
    // SAFETY: `token` is a valid handle opened above and closed exactly once.
    let _ = unsafe { CloseHandle(token) };
    result.map(|()| elevation.TokenIsElevated != 0)
}

fn token_user_sid(token: HANDLE) -> Result<String> {
    let mut len = 0u32;
    // SAFETY: size query: no buffer, `len` receives the required size. The call fails
    // with ERROR_INSUFFICIENT_BUFFER by design.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
    if len == 0 {
        return Err(ForgeError::Internal(
            "TokenUser size query returned 0".into(),
        ));
    }
    // u64 storage keeps TOKEN_USER (pointer-aligned) properly aligned.
    let mut buffer = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buffer` is writable, aligned and at least `len` bytes long.
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            len,
            &mut len,
        )
    }
    .map_err(|e| win_err("querying the token user", e))?;
    // SAFETY: on success the buffer starts with a TOKEN_USER whose SID points inside
    // the same buffer, alive until the end of this function.
    let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
    sid_to_string(user.User.Sid)
}

/// Self-relative security descriptor built from SDDL, freed on drop.
pub(crate) struct OwnedDescriptor(LocalBox);

// SAFETY: the descriptor is an immutable, self-relative block of memory owned by this
// value; Win32 only reads it.
unsafe impl Send for OwnedDescriptor {}
// SAFETY: see `Send`; shared references never mutate it.
unsafe impl Sync for OwnedDescriptor {}

impl OwnedDescriptor {
    pub(crate) fn from_sddl(sddl: &str) -> Result<Self> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is a valid wide string for the call; `sd` receives a LocalAlloc'd
        // descriptor owned by the returned value.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(sddl),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
        }
        .map_err(|e| win_err(format!("building security descriptor {sddl}"), e))?;
        Ok(Self(LocalBox(sd.0)))
    }

    pub(crate) fn as_psd(&self) -> PSECURITY_DESCRIPTOR {
        PSECURITY_DESCRIPTOR(self.0.as_ptr())
    }
}

/// SDDL of `sd` restricted to `info` (owner, DACL...).
fn descriptor_to_sddl(
    sd: PSECURITY_DESCRIPTOR,
    info: OBJECT_SECURITY_INFORMATION,
) -> Result<String> {
    let mut text = PWSTR::null();
    // SAFETY: `sd` is a valid descriptor for the duration of the call; `text` receives a
    // LocalAlloc'd string released by `take_local_wide`.
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            info,
            &mut text,
            None,
        )
    }
    .map_err(|e| win_err("converting security descriptor to SDDL", e))?;
    take_local_wide(text)
}

/// SDDL (`info` parts) of the kernel object behind `handle` (a pipe, a file...).
pub(crate) fn handle_sddl(handle: HANDLE, info: OBJECT_SECURITY_INFORMATION) -> Result<String> {
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `handle` is valid for the call (owned by the caller); `sd` receives a
    // LocalAlloc'd descriptor freed by `owner`.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            info,
            None,
            None,
            None,
            None,
            Some(&mut sd),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(ForgeError::io(
            "reading the security descriptor of a handle",
            std::io::Error::from_raw_os_error(status.0 as i32),
        ));
    }
    let owner = LocalBox(sd.0);
    let sddl = descriptor_to_sddl(sd, info);
    drop(owner);
    sddl
}

/// SDDL (`info` parts) of the file or directory at `path`.
pub(crate) fn path_sddl(path: &Path, info: OBJECT_SECURITY_INFORMATION) -> Result<String> {
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: the path string lives for the call; `sd` receives a LocalAlloc'd
    // descriptor freed by `owner`.
    let status = unsafe {
        GetNamedSecurityInfoW(
            &HSTRING::from(path),
            SE_FILE_OBJECT,
            info,
            None,
            None,
            None,
            None,
            &mut sd,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(ForgeError::io_at(
            "reading the security descriptor of",
            path,
            std::io::Error::from_raw_os_error(status.0 as i32),
        ));
    }
    let owner = LocalBox(sd.0);
    let sddl = descriptor_to_sddl(sd, info);
    drop(owner);
    sddl
}

/// Whether a DACL in SDDL form (`D:P(A;;FA;;;S-1-...)...`) is protected (no inherited
/// ACEs) and only grants access to `sid`.
pub fn dacl_grants_only(sddl: &str, sid: &str) -> bool {
    let Some(dacl) = sddl.find("D:").map(|i| &sddl[i + 2..]) else {
        return false;
    };
    let flags_end = dacl.find('(').unwrap_or(dacl.len());
    if !dacl[..flags_end].contains('P') {
        return false;
    }
    let aces: Vec<&str> = dacl[flags_end..]
        .split(['(', ')'])
        .filter(|part| !part.is_empty())
        .take_while(|part| !part.starts_with("S:"))
        .collect();
    !aces.is_empty()
        && aces.iter().all(|ace| {
            let fields: Vec<&str> = ace.split(';').collect();
            fields.len() == 6 && fields[0] == "A" && fields[5] == sid
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_user_sid_looks_like_a_sid() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
    }

    #[test]
    fn dacl_parser() {
        let sid = "S-1-5-21-1-2-3-1001";
        assert!(dacl_grants_only(&format!("D:P(A;;FA;;;{sid})"), sid));
        assert!(dacl_grants_only(
            &format!("O:{sid}D:PAI(A;;FA;;;{sid})"),
            sid
        ));
        assert!(
            !dacl_grants_only(&format!("D:(A;;FA;;;{sid})"), sid),
            "not protected"
        );
        assert!(!dacl_grants_only(
            &format!("D:P(A;;FA;;;{sid})(A;;FR;;;WD)"),
            sid
        ));
        assert!(!dacl_grants_only("D:P", sid), "empty DACL denies everyone");
        assert!(!dacl_grants_only("O:BA", sid));
    }

    #[test]
    fn descriptor_round_trips_through_sddl() {
        let sid = current_user_sid().unwrap();
        let sd = OwnedDescriptor::from_sddl(&format!("D:P(A;;FA;;;{sid})")).unwrap();
        let back = descriptor_to_sddl(
            sd.as_psd(),
            ::windows::Win32::Security::DACL_SECURITY_INFORMATION,
        )
        .unwrap();
        assert!(dacl_grants_only(&back, &sid), "{back}");
    }
}
