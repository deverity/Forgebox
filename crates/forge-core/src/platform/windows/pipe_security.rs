//! Security of the local API named pipe (docs/11 §4, ADR-0009).
//!
//! * The server creates every pipe instance with a security descriptor whose **owner**
//!   and only DACL entry are the current user (`O:<sid>D:P(A;;GA;;;<sid>)`). Other users
//!   (and services) cannot open it; `PIPE_REJECT_REMOTE_CLIENTS` (tokio's default) keeps
//!   SMB clients out.
//! * The client checks, right after opening, that the pipe is **owned by the current
//!   user**. A process of another user that squatted the name first cannot set our SID
//!   as owner, so the client never sends its token to an impostor.
//!
//! The token handshake on top of this (`system.hello`) is defence in depth.

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::os::windows::io::AsRawHandle;

use ::windows::Win32::Foundation::{ERROR_PIPE_BUSY, HANDLE};
use ::windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, SECURITY_ATTRIBUTES,
};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use super::security::{OwnedDescriptor, current_user_sid, handle_sddl};
use crate::error::{ForgeError, Result};

/// Security descriptor for the pipe instances of one server. Build it once and reuse it
/// for every instance (each `accept` creates a new one).
pub struct PipeSecurity {
    sid: String,
    sddl: String,
    descriptor: OwnedDescriptor,
}

impl fmt::Debug for PipeSecurity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PipeSecurity")
            .field("sddl", &self.sddl)
            .finish()
    }
}

impl PipeSecurity {
    /// Owner and only grantee = the user this process runs as.
    pub fn current_user() -> Result<Self> {
        let sid = current_user_sid()?;
        let sddl = format!("O:{sid}D:P(A;;GA;;;{sid})");
        let descriptor = OwnedDescriptor::from_sddl(&sddl)?;
        Ok(Self {
            sid,
            sddl,
            descriptor,
        })
    }

    /// SID of the user the descriptor grants access to.
    pub fn user_sid(&self) -> &str {
        &self.sid
    }

    /// The descriptor in SDDL form.
    pub fn sddl(&self) -> &str {
        &self.sddl
    }

    /// Creates a pipe instance `name` with `options` and this descriptor.
    pub fn create_server(
        &self,
        options: &ServerOptions,
        name: impl AsRef<OsStr>,
    ) -> io::Result<NamedPipeServer> {
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor.as_psd().0,
            bInheritHandle: false.into(),
        };
        // SAFETY: `attributes` is a valid SECURITY_ATTRIBUTES whose descriptor lives as
        // long as `self`; CreateNamedPipeW copies what it needs during the call.
        unsafe {
            options.create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
        }
    }
}

/// Fails with `Forbidden` unless the pipe behind `pipe` (a client or server end) is owned
/// by the current user.
pub fn verify_pipe_owner(pipe: &impl AsRawHandle) -> Result<()> {
    let sddl = handle_sddl(HANDLE(pipe.as_raw_handle()), OWNER_SECURITY_INFORMATION)?;
    let sid = current_user_sid()?;
    let owner = sddl.strip_prefix("O:").unwrap_or(&sddl);
    if owner == sid {
        Ok(())
    } else {
        Err(ForgeError::Forbidden(format!(
            "the API pipe is owned by {owner}, not by the current user {sid}; \
             another process may be impersonating the daemon"
        )))
    }
}

/// Owner and DACL of the pipe behind `pipe`, in SDDL form (diagnostics and tests).
pub fn pipe_sddl(pipe: &impl AsRawHandle) -> Result<String> {
    handle_sddl(
        HANDLE(pipe.as_raw_handle()),
        OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
    )
}

/// Whether `err` means "all pipe instances are busy, retry shortly".
pub fn is_pipe_busy(err: &io::Error) -> bool {
    err.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::windows::security::dacl_grants_only;

    #[tokio::test]
    async fn pipe_is_owned_by_and_restricted_to_the_current_user() {
        let name = format!(r"\\.\pipe\forgebox-test-{}", ulid::Ulid::generate());
        let security = PipeSecurity::current_user().unwrap();
        let server = security
            .create_server(ServerOptions::new().first_pipe_instance(true), &name)
            .unwrap();
        let sddl = pipe_sddl(&server).unwrap();
        assert!(
            sddl.contains(&format!("O:{}", security.user_sid())),
            "{sddl}"
        );
        assert!(dacl_grants_only(&sddl, security.user_sid()), "{sddl}");
        verify_pipe_owner(&server).unwrap();

        let client = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&name)
            .unwrap();
        verify_pipe_owner(&client).unwrap();
    }

    #[tokio::test]
    async fn second_first_instance_is_refused() {
        let name = format!(r"\\.\pipe\forgebox-test-{}", ulid::Ulid::generate());
        let security = PipeSecurity::current_user().unwrap();
        let _first = security
            .create_server(ServerOptions::new().first_pipe_instance(true), &name)
            .unwrap();
        let err = security
            .create_server(ServerOptions::new().first_pipe_instance(true), &name)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    }

    #[test]
    fn busy_detection() {
        assert!(is_pipe_busy(&io::Error::from_raw_os_error(231)));
        assert!(!is_pipe_busy(&io::Error::from_raw_os_error(2)));
    }
}
