//! Running `forgebox-helper` with administrator rights (docs/11 §1, ADR-0021).
//!
//! The only elevation path of Forgebox: one helper process per request, started by the
//! OS consent mechanism (UAC on Windows). The request and the response are the JSON
//! documents of [`crate::helper`]; how they cross the privilege boundary is the
//! implementation's business (Windows: two files in `state/locks/`, see
//! [`crate::helper`]).

use std::fmt;
use std::path::Path;
use std::sync::Arc;

#[cfg(not(windows))]
use crate::error::ForgeError;
use crate::error::Result;
use crate::helper::{HelperRequest, HelperResponse};

/// Launches the elevated helper.
pub trait Elevation: Send + Sync + fmt::Debug {
    /// Runs `exe` (the `forgebox-helper` binary) elevated with `request` and returns its
    /// response document as written (a refusal is `Ok` with `ok: false`; see
    /// [`HelperResponse::into_result`]).
    ///
    /// **Blocking** while the consent prompt is open and the helper runs: call it from
    /// `spawn_blocking`. Errors: `needs_elevation` when the user declines the prompt,
    /// `not_found` when `exe` does not exist, `timeout` when the helper does not finish in
    /// time, `internal` when it exits without a valid answer.
    fn run_helper(&self, exe: &Path, request: &HelperRequest) -> Result<HelperResponse>;
}

/// Elevation of the running OS (Windows: `ShellExecuteExW` verb `runas`).
#[cfg(windows)]
pub fn elevation() -> Arc<dyn Elevation> {
    Arc::new(super::windows::WindowsElevation::new())
}

/// Elevation of the running OS (stub until the Linux/macOS ports: `pkexec`, docs/13 §4).
#[cfg(not(windows))]
pub fn elevation() -> Arc<dyn Elevation> {
    Arc::new(Unsupported)
}

#[cfg(not(windows))]
#[derive(Debug)]
struct Unsupported;

#[cfg(not(windows))]
impl Elevation for Unsupported {
    fn run_helper(&self, _: &Path, _: &HelperRequest) -> Result<HelperResponse> {
        Err(ForgeError::UnsupportedPlatform(format!(
            "the elevated helper is not implemented on {}",
            std::env::consts::OS
        )))
    }
}
