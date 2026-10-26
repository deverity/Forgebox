//! Windows implementations of the platform traits. The only place with Win32 calls.

mod case_sensitive_fs;
pub(crate) mod console_ctrl;
mod link_fs;
mod process;
mod window_close;

pub use case_sensitive_fs::WindowsCaseSensitiveFs;
pub use link_fs::WindowsLinkFs;
pub use process::WindowsProcessControl;
