//! OS abstraction. Every Win32 call lives in [`windows`] (CLAUDE.md); the rest of the
//! workspace only sees the traits below, obtained through [`current`].
//!
//! All trait methods are **blocking** (file system and Win32 calls that may stall on
//! slow volumes, antivirus or network shares): async code calls them from
//! `tokio::task::spawn_blocking` (ADR-0011).
//!
//! Status: [`LinkFs`] and [`CaseSensitiveFs`] (M0-E1-T4), [`CodeSign`] (Authenticode,
//! M1-E2-T2), [`FileAcl`] (owner-only secret files) and [`windows::pipe_security`]
//! (DACL and owner check of the local API pipe; M1-E5-T1) and the process primitives of
//! [`process`] ([`ProcessGroup`], [`ProcessControl`]: Job Objects, spawn, `CTRL_BREAK`,
//! `WM_CLOSE`; M1-E3-T1), the per-process counters of [`process_stats`]
//! ([`ProcessStats`]: CPU time, working set, handles; M1-E3-T2), the TCP table of
//! [`net_table`] ([`PortTable`]: listeners with owning PID/exe, established connections
//! per port) and the system runtimes of [`runtime_deps`] ([`RuntimeDeps`]: VC++
//! Redistributable; both M1-E3-T3) are implemented for Windows. [`TrustStore`]
//! (M1-E4-T4) and
//! [`DnsPolicy`] (M1-E4-T3) are declared only; their signatures may still grow in those
//! tasks.
//! On other OSes every operation returns [`ForgeError::UnsupportedPlatform`] until the
//! Linux/macOS ports (docs/13 §4).

use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[cfg(not(windows))]
use crate::error::ForgeError;
use crate::error::Result;

pub mod net_table;
pub mod probe;
pub mod process;
pub mod process_stats;
pub mod runtime_deps;
#[cfg(windows)]
pub mod windows;

pub use net_table::{PortOwner, PortProto, PortTable, TcpConnection, TcpState, port_table};
pub use probe::{PROBE_MAX_OUTPUT, PROBE_TIMEOUT, ProbeCommand, ProbeOutput, run_probe};
pub use process::{
    ASSIGN_RETRY_LIMIT, CTRL_BREAK_HELPER_NAME, ConsoleMode, ExitSource, GroupOptions, PipeReader,
    ProcessCommand, ProcessControl, ProcessControlOptions, ProcessExit, ProcessGroup,
    SpawnedProcess, StdioTarget, TERMINATE_TIMEOUT, ctrl_break_helper_main, process_control,
};
pub use process_stats::{ProcStats, ProcessStats, process_stats};
pub use runtime_deps::{
    RuntimeDep, RuntimeDeps, VCREDIST_X64_URL, VcRedistFlavor, VcRuntimeVersion, runtime_deps,
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

/// Restricts files holding secrets (`state/ipc.token`, the CA key) to the current OS user
/// (docs/11 §3-§4).
pub trait FileAcl: Send + Sync {
    /// Replaces the access rules of the existing file `path` so that only the current
    /// user can open it (Windows: protected DACL with one ACE; Unix: mode `0600`).
    /// `NotFound` if `path` does not exist.
    fn restrict_to_current_user(&self, path: &Path) -> Result<()>;

    /// Whether `path` is accessible to the current user only.
    fn is_restricted_to_current_user(&self, path: &Path) -> Result<bool>;
}

/// Where the Authenticode signature of a file was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeSignKind {
    /// PKCS#7 signature embedded in the file (security directory of a PE).
    Embedded,
    /// The file hash is listed in a signed system catalog (`System32\CatRoot`), as for
    /// most binaries shipped with Windows.
    Catalog,
}

/// Result of [`CodeSign::verify`] (docs/05 §3 item 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeSignInfo {
    /// The file carries a signature (embedded or through a catalog), valid or not.
    pub signed: bool,
    /// The signature is intact and chains to a trusted root.
    pub trusted: bool,
    /// Simple display name of the signer certificate (`Microsoft Windows`,
    /// `Cloudflare, Inc.`), when a signature could be read.
    pub publisher: Option<String>,
    pub kind: Option<CodeSignKind>,
    /// Why the file is not trusted (`CERT_E_UNTRUSTEDROOT (0x800B0109)`...); `None` when
    /// trusted.
    pub status: Option<String>,
}

/// Code signing verification of executables (Authenticode on Windows).
pub trait CodeSign: Send + Sync {
    /// Checks the signature of `path`: embedded first, then the system catalogs.
    /// Revocation is not checked (works offline and never stalls on CRL/OCSP servers);
    /// the downloaded file is already pinned by its sha256 (docs/11 §2).
    /// Unsigned or untrusted files are `Ok` with `signed`/`trusted` false; `Err` only when
    /// the file cannot be opened or the OS has no such mechanism.
    fn verify(&self, path: &Path) -> Result<CodeSignInfo>;
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
    process_stats: Arc<dyn ProcessStats>,
    code_sign: Arc<dyn CodeSign>,
    file_acl: Arc<dyn FileAcl>,
    port_table: Arc<dyn PortTable>,
    runtime_deps: Arc<dyn RuntimeDeps>,
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
    /// [`process_control`] with default options; process counters, code signing, file
    /// ACLs, the TCP table and runtime detection to the implementations of the running
    /// OS. See [`Platform::with_processes`], [`Platform::with_process_stats`],
    /// [`Platform::with_code_sign`], [`Platform::with_file_acl`],
    /// [`Platform::with_port_table`] and [`Platform::with_runtime_deps`].
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
            process_stats: process_stats(),
            code_sign: default_code_sign(),
            file_acl: default_file_acl(),
            port_table: port_table(),
            runtime_deps: runtime_deps(),
        }
    }

    /// Replaces the process operations (custom `forgebox-ctrl` location, fakes).
    pub fn with_processes(mut self, processes: Arc<dyn ProcessControl>) -> Self {
        self.processes = processes;
        self
    }

    /// Replaces the per-process counters (fakes in tests).
    pub fn with_process_stats(mut self, process_stats: Arc<dyn ProcessStats>) -> Self {
        self.process_stats = process_stats;
        self
    }

    /// Replaces the code signing verifier (fakes in tests).
    pub fn with_code_sign(mut self, code_sign: Arc<dyn CodeSign>) -> Self {
        self.code_sign = code_sign;
        self
    }

    /// Replaces the secret file ACL implementation (fakes in tests).
    pub fn with_file_acl(mut self, file_acl: Arc<dyn FileAcl>) -> Self {
        self.file_acl = file_acl;
        self
    }

    /// Replaces the TCP table (fakes in tests).
    pub fn with_port_table(mut self, port_table: Arc<dyn PortTable>) -> Self {
        self.port_table = port_table;
        self
    }

    /// Replaces the runtime detection (fakes in tests).
    pub fn with_runtime_deps(mut self, runtime_deps: Arc<dyn RuntimeDeps>) -> Self {
        self.runtime_deps = runtime_deps;
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

    /// CPU time, memory and handles of single processes (shared, so it can move into
    /// `spawn_blocking`).
    pub fn process_stats(&self) -> &Arc<dyn ProcessStats> {
        &self.process_stats
    }

    /// Authenticode verification (shared, so it can move into `spawn_blocking`).
    pub fn code_sign(&self) -> &Arc<dyn CodeSign> {
        &self.code_sign
    }

    /// Owner-only ACLs for secret files (`state/ipc.token`).
    pub fn file_acl(&self) -> &dyn FileAcl {
        self.file_acl.as_ref()
    }

    /// TCP listeners and connections with their owning processes (shared, so it can
    /// move into `spawn_blocking`).
    pub fn port_table(&self) -> &Arc<dyn PortTable> {
        &self.port_table
    }

    /// VC++ Redistributable detection (shared, so it can move into `spawn_blocking`).
    pub fn runtime_deps(&self) -> &Arc<dyn RuntimeDeps> {
        &self.runtime_deps
    }
}

#[cfg(windows)]
fn default_code_sign() -> Arc<dyn CodeSign> {
    Arc::new(windows::WindowsCodeSign)
}

#[cfg(not(windows))]
fn default_code_sign() -> Arc<dyn CodeSign> {
    Arc::new(unsupported::Unsupported)
}

#[cfg(windows)]
fn default_file_acl() -> Arc<dyn FileAcl> {
    Arc::new(windows::WindowsFileAcl)
}

#[cfg(not(windows))]
fn default_file_acl() -> Arc<dyn FileAcl> {
    Arc::new(unsupported::Unsupported)
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

    impl CodeSign for Unsupported {
        fn verify(&self, _: &Path) -> Result<CodeSignInfo> {
            err("code signing verification")
        }
    }

    /// Unix permission bits; other OSes fail with `UnsupportedPlatform`.
    impl FileAcl for Unsupported {
        #[cfg(unix)]
        fn restrict_to_current_user(&self, path: &Path) -> Result<()> {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| ForgeError::io_at("restricting permissions of", path, e))
        }
        #[cfg(not(unix))]
        fn restrict_to_current_user(&self, _: &Path) -> Result<()> {
            err("file ACLs")
        }
        #[cfg(unix)]
        fn is_restricted_to_current_user(&self, path: &Path) -> Result<bool> {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path)
                .map_err(|e| ForgeError::io_at("reading permissions of", path, e))?
                .permissions()
                .mode();
            Ok(mode & 0o077 == 0)
        }
        #[cfg(not(unix))]
        fn is_restricted_to_current_user(&self, _: &Path) -> Result<bool> {
            err("file ACLs")
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
        let err = platform.code_sign().verify(&dir).unwrap_err();
        assert_eq!(err.code(), "unsupported_platform");
    }

    #[test]
    fn file_acl_can_be_replaced() {
        struct AlwaysRestricted;
        impl FileAcl for AlwaysRestricted {
            fn restrict_to_current_user(&self, _: &Path) -> Result<()> {
                Ok(())
            }
            fn is_restricted_to_current_user(&self, _: &Path) -> Result<bool> {
                Ok(true)
            }
        }
        let platform = current().with_file_acl(Arc::new(AlwaysRestricted));
        let missing = std::env::temp_dir().join("forgebox-no-such-file");
        assert!(
            platform
                .file_acl()
                .is_restricted_to_current_user(&missing)
                .unwrap()
        );
    }

    #[test]
    fn code_sign_info_wire_format() {
        let info = CodeSignInfo {
            signed: true,
            trusted: false,
            publisher: Some("Example, Inc.".into()),
            kind: Some(CodeSignKind::Catalog),
            status: Some("CERT_E_UNTRUSTEDROOT (0x800B0109)".into()),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["kind"], "catalog");
        assert_eq!(json["publisher"], "Example, Inc.");
        let back: CodeSignInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back, info);
    }
}
