//! OS abstraction. Every Win32 call lives in [`windows`] (CLAUDE.md); the rest of the
//! workspace only sees the traits below, obtained through [`current`].
//!
//! All trait methods are **blocking** (file system and Win32 calls that may stall on
//! slow volumes, antivirus or network shares): async code calls them from
//! `tokio::task::spawn_blocking` (ADR-0011).
//!
//! Status: [`LinkFs`] and [`CaseSensitiveFs`] (M0-E1-T4) and the process primitives of
//! [`process`] ([`ProcessGroup`], [`ProcessControl`]: Job Objects, spawn, `CTRL_BREAK`,
//! `WM_CLOSE`; M1-E3-T1) are implemented for Windows. [`TrustStore`] (M1-E4-T4) and
//! [`DnsPolicy`] (M1-E4-T3) are declared only; their signatures may still grow in those
//! tasks.
//! On other OSes every operation returns [`ForgeError::UnsupportedPlatform`] until the
//! Linux/macOS ports (docs/13 §4).

use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(not(windows))]
use crate::error::ForgeError;
use crate::error::Result;

pub mod process;
#[cfg(windows)]
pub mod windows;

pub use process::{
    CTRL_BREAK_HELPER_NAME, ConsoleMode, ExitSource, GroupOptions, PipeReader, ProcessCommand,
    ProcessControl, ProcessControlOptions, ProcessExit, ProcessGroup, SpawnedProcess, StdioTarget,
    ctrl_break_helper_main, process_control,
};

/// Directory links used for `bin/<tool>/current` (ADR-0005). On Windows these are
/// junctions: no privilege nor Developer Mode needed.
pub trait LinkFs: Send + Sync {
    /// Creates `link` pointing at the existing directory `target` (made absolute).
    /// Fails with `Conflict` if `link` exists and `NotFound` if `target` does not.
    fn create_junction(&self, link: &Path, target: &Path) -> Result<()>;

    /// Target of the junction `link`. `InvalidInput` if `link` is not a junction.
    fn read_junction(&self, link: &Path) -> Result<PathBuf>;

    /// Whether `path` exists and is a junction.
    fn is_junction(&self, path: &Path) -> Result<bool>;

    /// Removes the junction `link` only; the target and its content are untouched.
    /// Refuses (`InvalidInput`) to remove anything that is not a junction.
    fn remove_junction(&self, link: &Path) -> Result<()>;

    /// Points `link` at `target` atomically: builds `<link>.tmp` and renames it over
    /// `link`, so readers see either the old or the new target, never nothing.
    /// Creates `link` if it does not exist. Refuses to replace a real directory.
    fn replace_junction(&self, link: &Path, target: &Path) -> Result<()>;
}

/// Answer of [`CaseSensitiveFs::support`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaseSensitivitySupport {
    Supported,
    /// Human readable reason (volume is not NTFS, Windows too old, policy...).
    Unsupported(String),
}

/// Per-directory case sensitivity (docs/15 §3, ADR-0010).
pub trait CaseSensitiveFs: Send + Sync {
    /// Whether the volume/OS holding the existing directory `dir` supports the flag.
    fn support(&self, dir: &Path) -> CaseSensitivitySupport;

    /// Shorthand for `support(dir) == Supported`.
    fn is_supported(&self, dir: &Path) -> bool {
        matches!(self.support(dir), CaseSensitivitySupport::Supported)
    }

    /// Whether `dir` is case-sensitive.
    fn get(&self, dir: &Path) -> Result<bool>;

    /// Turns the flag on or off for `dir` (not recursive: existing subdirectories keep
    /// their own flag; new ones inherit it). `UnsupportedPlatform` when the volume or
    /// OS cannot do it, `NeedsElevation` when access is denied (route through
    /// `forgebox-helper`), `Conflict` when disabling a directory that holds names
    /// differing only by case.
    fn set(&self, dir: &Path, enabled: bool) -> Result<()>;
}

/// Where a root certificate is trusted (docs/06 §6, docs/11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustScope {
    /// `CurrentUser\Root`, no elevation.
    CurrentUser,
    /// `LocalMachine\Root`, through `forgebox-helper`.
    LocalMachine,
}

/// OS certificate trust store. Implemented in M1-E4-T4.
pub trait TrustStore: Send + Sync {
    /// Whether the DER certificate is trusted in `scope`.
    fn is_trusted(&self, cert_der: &[u8], scope: TrustScope) -> Result<bool>;
    /// Adds the DER root certificate to `scope`.
    fn add_root(&self, cert_der: &[u8], scope: TrustScope) -> Result<()>;
    /// Removes the DER root certificate from `scope`.
    fn remove_root(&self, cert_der: &[u8], scope: TrustScope) -> Result<()>;
}

/// State of the OS resolver policy for a TLD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsPolicyStatus {
    /// Queries for the TLD go to `nameserver`.
    Applied {
        nameserver: IpAddr,
    },
    NotApplied,
}

/// OS resolver policy (NRPT on Windows, docs/06 §5). Implemented in M1-E4-T3.
pub trait DnsPolicy: Send + Sync {
    /// Current policy for `tld` (`.test`).
    fn status(&self, tld: &str) -> Result<DnsPolicyStatus>;
    /// Routes `tld` to `nameserver` (needs elevation through the helper).
    fn apply(&self, tld: &str, nameserver: IpAddr) -> Result<()>;
    /// Removes the policy for `tld`.
    fn remove(&self, tld: &str) -> Result<()>;
}

/// Implementations for the running OS. Cheap to clone.
#[derive(Clone)]
pub struct Platform {
    name: &'static str,
    link_fs: Arc<dyn LinkFs>,
    case_sensitive_fs: Arc<dyn CaseSensitiveFs>,
    processes: Arc<dyn ProcessControl>,
}

impl fmt::Debug for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Platform")
            .field("name", &self.name)
            .finish()
    }
}

impl Platform {
    /// Assembles a platform from parts (tests use fakes). Process operations default to
    /// [`process_control`] with default options; see [`Platform::with_processes`].
    pub fn new(
        name: &'static str,
        link_fs: Arc<dyn LinkFs>,
        case_sensitive_fs: Arc<dyn CaseSensitiveFs>,
    ) -> Self {
        Self {
            name,
            link_fs,
            case_sensitive_fs,
            processes: process_control(ProcessControlOptions::default()),
        }
    }

    /// Replaces the process operations (custom `forgebox-ctrl` location, fakes).
    pub fn with_processes(mut self, processes: Arc<dyn ProcessControl>) -> Self {
        self.processes = processes;
        self
    }

    /// `"windows"`, `"linux"`, `"macos"`...
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn link_fs(&self) -> &dyn LinkFs {
        self.link_fs.as_ref()
    }

    pub fn case_sensitive_fs(&self) -> &dyn CaseSensitiveFs {
        self.case_sensitive_fs.as_ref()
    }

    /// Job Objects, spawn, console control events, window close requests.
    pub fn processes(&self) -> &Arc<dyn ProcessControl> {
        &self.processes
    }
}

/// Implementations for the OS this binary runs on.
#[cfg(windows)]
pub fn current() -> Platform {
    Platform::new(
        "windows",
        Arc::new(windows::WindowsLinkFs),
        Arc::new(windows::WindowsCaseSensitiveFs),
    )
}

/// Implementations for the OS this binary runs on (stubs until the Linux/macOS ports).
#[cfg(not(windows))]
pub fn current() -> Platform {
    Platform::new(
        std::env::consts::OS,
        Arc::new(unsupported::Unsupported),
        Arc::new(unsupported::Unsupported),
    )
}

#[cfg(not(windows))]
mod unsupported {
    use super::*;

    /// Every operation fails with `UnsupportedPlatform`; never panics.
    pub struct Unsupported;

    fn err<T>(what: &str) -> Result<T> {
        Err(ForgeError::UnsupportedPlatform(format!(
            "{what} is not implemented on {}",
            std::env::consts::OS
        )))
    }

    impl LinkFs for Unsupported {
        fn create_junction(&self, _: &Path, _: &Path) -> Result<()> {
            err("junctions")
        }
        fn read_junction(&self, _: &Path) -> Result<PathBuf> {
            err("junctions")
        }
        fn is_junction(&self, _: &Path) -> Result<bool> {
            err("junctions")
        }
        fn remove_junction(&self, _: &Path) -> Result<()> {
            err("junctions")
        }
        fn replace_junction(&self, _: &Path, _: &Path) -> Result<()> {
            err("junctions")
        }
    }

    impl CaseSensitiveFs for Unsupported {
        fn support(&self, _: &Path) -> CaseSensitivitySupport {
            CaseSensitivitySupport::Unsupported(format!(
                "per-directory case sensitivity is not implemented on {}",
                std::env::consts::OS
            ))
        }
        fn get(&self, _: &Path) -> Result<bool> {
            err("case sensitivity query")
        }
        fn set(&self, _: &Path, _: bool) -> Result<()> {
            err("case sensitivity")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_platform_is_named() {
        let platform = current();
        assert_eq!(platform.name(), std::env::consts::OS);
        assert!(format!("{platform:?}").contains(platform.name()));
    }

    #[cfg(not(windows))]
    #[test]
    fn stubs_fail_without_panicking() {
        let platform = current();
        let dir = std::env::temp_dir();
        assert!(!platform.case_sensitive_fs().is_supported(&dir));
        let err = platform.link_fs().is_junction(&dir).unwrap_err();
        assert_eq!(err.code(), "unsupported_platform");
    }
}
