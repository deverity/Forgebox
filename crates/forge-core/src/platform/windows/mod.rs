//! Windows implementations of the platform traits. The only place with Win32 calls.

mod authenticode;
mod case_sensitive_fs;
mod link_fs;

pub use authenticode::WindowsCodeSign;
pub use case_sensitive_fs::WindowsCaseSensitiveFs;
pub use link_fs::WindowsLinkFs;
