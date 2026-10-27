//! [`Elevation`] for Windows (docs/11 §1, ADR-0022): `ShellExecuteExW` with the verb
//! `runas` starts `forgebox-helper.exe --request <file> --response <file>` elevated (one
//! UAC prompt).
//!
//! Standard handles do not cross the elevation boundary (the elevated process is started
//! by the AppInfo service, not by us), so the documents travel through two files in
//! `<root>/state/locks/`, created here **before** the helper starts:
//!
//! 1. `helper-<request_id>.request.json` and `helper-<request_id>.response.json` are
//!    created empty with `CREATE_NEW` (an existing file means a request id in use:
//!    `conflict`), then given a protected DACL granting full control to the current user
//!    and to `BUILTIN\Administrators` only (the elevated helper may run under another
//!    administrator account: over-the-shoulder elevation), then the request is written.
//! 2. The helper reads the request, writes the response into the existing file and exits.
//! 3. We wait for the process (handle from `SEE_MASK_NOCLOSEPROCESS`; the elevated
//!    handle still allows waiting and reading the exit code), read and parse the response
//!    and delete both files, whatever happened.
//!
//! `ShellExecuteExW` itself blocks while the consent prompt is open: that wait is not
//! bounded here (the user may take minutes); the helper's run after consent is bounded
//! by [`WindowsElevation::with_timeout`]. A declined prompt is `ERROR_CANCELLED` →
//! `needs_elevation`. Everything is blocking: callers use `spawn_blocking`.

use std::ffi::OsStr;
use std::io::Write as _;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ::windows::Win32::Foundation::{
    ERROR_CANCELLED, HANDLE, RPC_E_CHANGED_MODE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use ::windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use ::windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use ::windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    ShellExecuteExW,
};
use ::windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use ::windows::core::{PCWSTR, w};

use super::file_acl::set_protected_dacl;
use super::process::{os_error, wide, win32_code};
use super::security::current_user_sid;
use crate::error::{ForgeError, IoResultExt, Result};
use crate::helper::{HelperRequest, HelperResponse, request_path, response_path};
use crate::platform::Elevation;

/// Default bound of the helper's run once the user consented.
pub const HELPER_RUN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// [`Elevation`] through `ShellExecuteExW`.
#[derive(Debug, Clone, Copy)]
pub struct WindowsElevation {
    elevate: bool,
    timeout: Duration,
}

impl Default for WindowsElevation {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsElevation {
    /// Verb `runas` (UAC prompt), helper run bounded by [`HELPER_RUN_TIMEOUT`].
    pub fn new() -> Self {
        Self {
            elevate: true,
            timeout: HELPER_RUN_TIMEOUT,
        }
    }

    /// Same exchange with the verb `open`: the helper runs with the caller's token. For
    /// tests (no prompt) and for a daemon that already runs elevated.
    pub fn without_elevation() -> Self {
        Self {
            elevate: false,
            ..Self::new()
        }
    }

    /// Bounds the helper's run after consent.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// The two exchange files; deleted on drop.
struct Exchange {
    request: PathBuf,
    response: PathBuf,
}

impl Exchange {
    fn create(request: &HelperRequest) -> Result<Self> {
        let req_path = request_path(&request.root, request.request_id);
        let resp_path = response_path(&request.root, request.request_id);
        if let Some(dir) = req_path.parent() {
            std::fs::create_dir_all(dir).at("creating", dir)?;
        }
        let dacl = format!("D:P(A;;FA;;;{})(A;;FA;;;BA)", current_user_sid()?);
        let mut exchange = Self {
            request: PathBuf::new(),
            response: PathBuf::new(),
        };
        let mut file = create_new(&req_path)?;
        exchange.request = req_path.clone();
        set_protected_dacl(&req_path, &dacl)?;
        drop(create_new(&resp_path)?);
        exchange.response = resp_path.clone();
        set_protected_dacl(&resp_path, &dacl)?;
        file.write_all(request.to_json()?.as_bytes())
            .and_then(|()| file.sync_all())
            .at("writing", &req_path)?;
        Ok(exchange)
    }
}

impl Drop for Exchange {
    fn drop(&mut self) {
        for path in [&self.request, &self.response] {
            if !path.as_os_str().is_empty() {
                if let Err(err) = std::fs::remove_file(path) {
                    tracing::warn!(path = %path.display(), %err, "could not delete a helper exchange file");
                }
            }
        }
    }
}

fn create_new(path: &Path) -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                ForgeError::Conflict(format!(
                    "{} already exists: a helper run with this request id is in progress",
                    path.display()
                ))
            } else {
                ForgeError::io_at("creating", path, e)
            }
        })
}

/// `"path"` for a command line (paths never contain `"`; a trailing backslash would
/// escape the closing quote, so it is doubled).
fn quote(path: &Path) -> Result<String> {
    let text = path.to_str().ok_or_else(|| {
        ForgeError::InvalidInput(format!("{} is not valid Unicode", path.display()))
    })?;
    if text.contains('"') {
        return Err(ForgeError::InvalidInput(format!("{text} contains a quote")));
    }
    let trailing = text.len() - text.trim_end_matches('\\').len();
    Ok(format!("\"{text}{}\"", "\\".repeat(trailing)))
}

/// COM initialized for `ShellExecuteExW` on this thread (apartment threaded, as the shell
/// expects); uninitialized on drop when we did it.
struct Com(bool);

impl Com {
    fn init() -> Self {
        // SAFETY: plain initialization of the calling thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        // S_FALSE (already initialized) also needs a matching uninitialize;
        // RPC_E_CHANGED_MODE (multithreaded already) does not, and still works.
        Self(hr.is_ok() && hr != RPC_E_CHANGED_MODE)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx of `init` on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

impl Elevation for WindowsElevation {
    fn run_helper(&self, exe: &Path, request: &HelperRequest) -> Result<HelperResponse> {
        if !exe.is_file() {
            return Err(ForgeError::NotFound(format!(
                "{} (forgebox-helper)",
                exe.display()
            )));
        }
        let exchange = Exchange::create(request)?;
        let params = format!(
            "--request {} --response {}",
            quote(&exchange.request)?,
            quote(&exchange.response)?
        );
        let file = wide(exe.as_os_str());
        let params = wide(OsStr::new(&params));
        let _com = Com::init();
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
            lpVerb: if self.elevate {
                w!("runas")
            } else {
                w!("open")
            },
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        // SAFETY: `info` is fully initialized; the strings outlive the call.
        if let Err(err) = unsafe { ShellExecuteExW(&mut info) } {
            if win32_code(&err) == Some(ERROR_CANCELLED.0) {
                return Err(ForgeError::NeedsElevation(format!(
                    "the administrator prompt for forgebox-helper {} was declined",
                    request.op.name()
                )));
            }
            return Err(ForgeError::io(
                format!("starting {}", exe.display()),
                os_error(&err),
            ));
        }
        if info.hProcess.is_invalid() {
            return Err(ForgeError::Internal(
                "ShellExecuteExW returned no process handle for forgebox-helper".to_owned(),
            ));
        }
        // SAFETY: SEE_MASK_NOCLOSEPROCESS hands the process handle to us.
        let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess.0) };
        let handle = HANDLE(process.as_raw_handle());
        let millis = u32::try_from(self.timeout.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: valid process handle with SYNCHRONIZE access.
        let waited = unsafe { WaitForSingleObject(handle, millis) };
        if waited == WAIT_TIMEOUT {
            return Err(ForgeError::Timeout {
                operation: format!("forgebox-helper {}", request.op.name()),
                after: self.timeout,
            });
        }
        if waited != WAIT_OBJECT_0 {
            return Err(ForgeError::io(
                "waiting for forgebox-helper",
                std::io::Error::last_os_error(),
            ));
        }
        let mut code = 0_u32;
        // SAFETY: valid process handle; `code` is writable.
        unsafe { GetExitCodeProcess(handle, &mut code) }.map_err(|e| {
            ForgeError::io("reading the exit code of forgebox-helper", os_error(&e))
        })?;
        let text = std::fs::read_to_string(&exchange.response).at("reading", &exchange.response)?;
        if text.trim().is_empty() {
            return Err(ForgeError::Internal(format!(
                "forgebox-helper exited with code {code} without writing a response"
            )));
        }
        HelperResponse::from_json(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(
            quote(Path::new(r"C:\a b\x.json")).unwrap(),
            r#""C:\a b\x.json""#
        );
        assert_eq!(quote(Path::new(r"C:\dir\")).unwrap(), r#""C:\dir\\""#);
    }

    #[test]
    fn missing_helper_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let request = HelperRequest::new(
            dir.path(),
            crate::helper::HelperOp::FirewallRemove {
                name: "Forgebox-x".into(),
            },
        );
        let err = WindowsElevation::without_elevation()
            .run_helper(&dir.path().join("nope.exe"), &request)
            .unwrap_err();
        assert_eq!(err.code(), "not_found");
        assert!(
            !request_path(dir.path(), request.request_id).exists(),
            "nothing written before the helper is known"
        );
    }

    #[test]
    fn exchange_files_are_restricted_and_cleaned_up() {
        let dir = tempfile::tempdir().unwrap();
        let request = HelperRequest::new(
            dir.path(),
            crate::helper::HelperOp::FirewallRemove {
                name: "Forgebox-x".into(),
            },
        );
        let req_path = request_path(dir.path(), request.request_id);
        let resp_path = response_path(dir.path(), request.request_id);
        {
            let exchange = Exchange::create(&request).unwrap();
            assert_eq!(exchange.request, req_path);
            let written = std::fs::read_to_string(&req_path).unwrap();
            assert_eq!(HelperRequest::from_json(&written).unwrap(), request);
            assert_eq!(std::fs::read(&resp_path).unwrap(), b"");
            let sid = current_user_sid().unwrap();
            for path in [&req_path, &resp_path] {
                let sddl = super::super::security::path_sddl(
                    path,
                    ::windows::Win32::Security::DACL_SECURITY_INFORMATION,
                )
                .unwrap();
                assert!(sddl.contains("D:P"), "protected: {sddl}");
                assert!(sddl.contains(&format!(";;;{sid})")), "{sddl}");
                assert!(sddl.contains(";;;BA)"), "{sddl}");
                assert_eq!(sddl.matches("(A;").count(), 2, "{sddl}");
            }
            // A second run with the same id is refused while the first is alive.
            assert_eq!(Exchange::create(&request).err().unwrap().code(), "conflict");
        }
        assert!(!req_path.exists() && !resp_path.exists(), "deleted on drop");
    }
}
