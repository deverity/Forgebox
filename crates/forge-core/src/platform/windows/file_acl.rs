//! Owner-only ACLs for secret files (`state/ipc.token`, the CA key; docs/11 §3-§4).
//!
//! The DACL is replaced by a **protected** one (`D:P`, no inherited ACEs) with a single
//! ACE granting full control to the current user, through `SetNamedSecurityInfoW`. The
//! owner is not changed (the creator already owns the file).

use std::path::Path;

use ::windows::Win32::Foundation::ERROR_SUCCESS;
use ::windows::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
use ::windows::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
};
use ::windows::core::{BOOL, HSTRING};

use super::security::{OwnedDescriptor, current_user_sid, dacl_grants_only, path_sddl, win_err};
use crate::error::{ForgeError, Result};
use crate::platform::FileAcl;

/// [`FileAcl`] backed by NTFS DACLs.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsFileAcl;

impl FileAcl for WindowsFileAcl {
    fn restrict_to_current_user(&self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Err(ForgeError::NotFound(format!(
                "{} (restricting ACL)",
                path.display()
            )));
        }
        let sid = current_user_sid()?;
        let sd = OwnedDescriptor::from_sddl(&format!("D:P(A;;FA;;;{sid})"))?;
        let mut present = BOOL::default();
        let mut defaulted = BOOL::default();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        // SAFETY: `sd` is a valid descriptor; `dacl` receives a pointer into it, used
        // only while `sd` is alive.
        unsafe { GetSecurityDescriptorDacl(sd.as_psd(), &mut present, &mut dacl, &mut defaulted) }
            .map_err(|e| win_err("reading the DACL of a new descriptor", e))?;
        // SAFETY: the path string and `dacl` are valid for the call; no owner, group or
        // SACL is changed.
        let status = unsafe {
            SetNamedSecurityInfoW(
                &HSTRING::from(path),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(dacl),
                None,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(ForgeError::io_at(
                "restricting the ACL of",
                path,
                std::io::Error::from_raw_os_error(status.0 as i32),
            ));
        }
        Ok(())
    }

    fn is_restricted_to_current_user(&self, path: &Path) -> Result<bool> {
        let sddl = path_sddl(path, DACL_SECURITY_INFORMATION)?;
        Ok(dacl_grants_only(&sddl, &current_user_sid()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricts_a_file_to_the_current_user() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret.token");
        std::fs::write(&file, b"x").unwrap();
        let acl = WindowsFileAcl;
        assert!(
            !acl.is_restricted_to_current_user(&file).unwrap(),
            "inherits by default"
        );
        acl.restrict_to_current_user(&file).unwrap();
        assert!(acl.is_restricted_to_current_user(&file).unwrap());
        // Still readable and writable by us.
        std::fs::write(&file, b"y").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"y");
    }

    #[test]
    fn missing_file_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = WindowsFileAcl
            .restrict_to_current_user(&dir.path().join("nope"))
            .unwrap_err();
        assert_eq!(err.code(), "not_found");
    }
}
