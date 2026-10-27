//! Windows implementations of the platform traits. The only place with Win32 calls.

mod authenticode;
mod case_sensitive_fs;
pub(crate) mod console_ctrl;
mod file_acl;
mod link_fs;
mod net_table;
pub mod pipe_security;
mod process;
mod process_stats;
mod security;
mod vcredist;
mod window_close;

pub use authenticode::WindowsCodeSign;
pub use case_sensitive_fs::WindowsCaseSensitiveFs;
pub use file_acl::WindowsFileAcl;
pub use link_fs::WindowsLinkFs;
pub use net_table::WindowsPortTable;
pub use process::WindowsProcessControl;
pub use process_stats::WindowsProcessStats;
pub use security::{current_user_sid, dacl_grants_only};
pub use vcredist::WindowsRuntimeDeps;
