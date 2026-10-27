//! Windows implementations of the platform traits. The only place with Win32 calls.

mod authenticode;
mod case_sensitive_fs;
pub(crate) mod console_ctrl;
mod file_acl;
mod link_fs;
pub mod pipe_security;
mod process;
mod security;
mod window_close;

pub use authenticode::WindowsCodeSign;
pub use case_sensitive_fs::WindowsCaseSensitiveFs;
pub use file_acl::WindowsFileAcl;
pub use link_fs::WindowsLinkFs;
pub use process::WindowsProcessControl;
pub use security::{current_user_sid, dacl_grants_only};

/// Starts the child without a console window (`CREATE_NO_WINDOW`), for one-shot probes
/// ([`crate::platform::probe`]).
pub(crate) fn hide_console(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NO_WINDOW` from `winbase.h`.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}
