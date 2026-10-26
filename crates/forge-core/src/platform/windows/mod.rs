//! Windows implementations of the platform traits. The only place with Win32 calls.

mod case_sensitive_fs;
mod file_acl;
mod link_fs;
pub mod pipe_security;
mod security;

pub use case_sensitive_fs::WindowsCaseSensitiveFs;
pub use file_acl::WindowsFileAcl;
pub use link_fs::WindowsLinkFs;
pub use security::{current_user_sid, dacl_grants_only};
