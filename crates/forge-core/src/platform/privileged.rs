//! Machine-wide changes that need administrator rights (docs/11 §1, ADR-0022, ADR-0024).
//!
//! Used **only** by `forgebox-helper`, after the request was parsed and validated against
//! the allowlist ([`crate::helper::HelperOp::validate`]). The daemon never calls these: it
//! asks the helper through [`crate::helper::HelperClient`]. Reading methods work without
//! elevation (the helper's `--dry-run` uses them to build its plan); changing methods fail
//! with `needs_elevation` when the process is not elevated.
//!
//! All methods are **blocking** (COM, WMI, registry, file and process I/O).

use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

#[cfg(not(windows))]
use crate::error::ForgeError;
use crate::error::Result;
use crate::platform::{CertThumbprint, NrptRule};

/// An inbound TCP allow rule (`firewall.add`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FirewallRuleSpec {
    /// `Forgebox-*` (validated by the allowlist).
    pub name: String,
    pub description: String,
    /// Group shown by the Windows Firewall console (`Forgebox`).
    pub grouping: String,
    /// Canonical path of the program inside `<root>/bin` (no `\\?\` prefix).
    pub exe: PathBuf,
    pub port: u16,
}

/// An IP range of a name constraint (`127.0.0.0/8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct IpPrefix {
    pub addr: IpAddr,
    /// Prefix length; `None` when the mask is not contiguous.
    pub len: Option<u8>,
}

impl fmt::Display for IpPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.len {
            Some(len) => write!(f, "{}/{len}", self.addr),
            None => write!(f, "{}/?", self.addr),
        }
    }
}

/// The Name Constraints extension of a CA certificate (RFC 5280 §4.2.1.10).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct NameConstraints {
    pub critical: bool,
    /// Permitted `dNSName` subtrees, as written (`test`, `.test`).
    pub permitted_dns: Vec<String>,
    /// Permitted `iPAddress` subtrees.
    pub permitted_ip: Vec<IpPrefix>,
    /// Permitted subtrees of any other form (e-mail, URI, directory name...).
    pub permitted_other: usize,
    /// Number of excluded subtrees (they only restrict further).
    pub excluded: usize,
}

/// What the helper needs to know about a certificate before trusting or removing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CertificateInfo {
    /// DER encoding.
    #[serde(skip)]
    pub der: Vec<u8>,
    /// SHA-1 of the DER (the Windows "thumbprint").
    pub thumbprint: CertThumbprint,
    /// Simple display name of the subject (its CN).
    pub subject: String,
    /// Subject = issuer and the signature verifies with its own key.
    pub self_signed: bool,
    /// Basic Constraints `cA = true`.
    pub is_ca: bool,
    pub name_constraints: Option<NameConstraints>,
}

/// A file copied by the helper into a directory only `SYSTEM` and `Administrators` can
/// write (`%SystemRoot%\Temp\forgebox-helper-<tag>`), after its sha256 matched. Running
/// the copy (never the original, which the user can still replace) closes the window
/// between verification and execution. The directory is deleted on drop.
#[derive(Debug)]
pub struct StagedFile {
    dir: PathBuf,
    path: PathBuf,
}

impl StagedFile {
    /// Wraps an already populated staging directory (implementations only).
    pub fn new(dir: PathBuf, path: PathBuf) -> Self {
        Self { dir, path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_dir_all(&self.dir) {
            tracing::warn!(dir = %self.dir.display(), %err, "could not delete a staging directory");
        }
    }
}

/// The privileged operations behind the helper's allowlist.
pub trait PrivilegedOps: Send + Sync + fmt::Debug {
    /// Whether this process runs with an elevated (administrator) token.
    fn is_elevated(&self) -> Result<bool>;

    /// Every NRPT rule, local and Group Policy (no elevation).
    fn nrpt_rules(&self) -> Result<Vec<NrptRule>>;

    /// Adds a local NRPT rule sending `namespace` to `nameserver`, tagged with `comment`.
    fn nrpt_add(&self, namespace: &str, nameserver: IpAddr, comment: &str) -> Result<()>;

    /// Removes the local NRPT rule `rule_id` (`{GUID}`).
    fn nrpt_remove(&self, rule_id: &str) -> Result<()>;

    /// The system `hosts` file.
    fn hosts_path(&self) -> PathBuf;

    /// Atomically replaces the content of the existing system file `path`, keeping its
    /// ACL and attributes (Windows: `ReplaceFileW` from a sibling temporary file).
    fn replace_system_file(&self, path: &Path, contents: &[u8]) -> Result<()>;

    /// Empties the OS resolver cache.
    fn flush_dns_cache(&self) -> Result<()>;

    /// Parses a certificate (PEM or DER).
    fn certificate_info(&self, bytes: &[u8]) -> Result<CertificateInfo>;

    /// The certificate with SHA-1 `thumbprint` in `LocalMachine\Root` (its registry part,
    /// where Forgebox adds roots), if any (no elevation).
    fn machine_root_find(&self, thumbprint: &CertThumbprint) -> Result<Option<CertificateInfo>>;

    /// Adds the DER certificate to `LocalMachine\Root` (kept when already there).
    fn machine_root_add(&self, der: &[u8]) -> Result<()>;

    /// Deletes the certificate with SHA-1 `thumbprint` from `LocalMachine\Root`; whether
    /// it was there.
    fn machine_root_remove(&self, thumbprint: &CertThumbprint) -> Result<bool>;

    /// Number of firewall rules named `name` (no elevation).
    fn firewall_rule_count(&self, name: &str) -> Result<usize>;

    /// Replaces the rules named `rule.name` by `rule`; how many were replaced.
    fn firewall_add(&self, rule: &FirewallRuleSpec) -> Result<usize>;

    /// Removes every rule named `name`; how many there were.
    fn firewall_remove(&self, name: &str) -> Result<usize>;

    /// Adds `path` to the Microsoft Defender exclusions.
    fn defender_add_exclusion(&self, path: &Path) -> Result<()>;

    /// Removes `path` from the Microsoft Defender exclusions.
    fn defender_remove_exclusion(&self, path: &Path) -> Result<()>;

    /// Copies `source` into a fresh admin-only directory named after `tag`, checking that
    /// the copy's sha256 is `sha256` (hex, any case). `verification_failed` on mismatch.
    fn stage_verified_copy(&self, source: &Path, sha256: &str, tag: &str) -> Result<StagedFile>;

    /// Runs `exe args...` without a window and waits at most `timeout`; its exit code.
    /// `timeout` when it runs longer (the process is killed).
    fn run_installer(&self, exe: &Path, args: &[&str], timeout: Duration) -> Result<i32>;
}

/// Privileged operations of the running OS.
#[cfg(windows)]
pub fn privileged_ops() -> Arc<dyn PrivilegedOps> {
    Arc::new(super::windows::WindowsPrivilegedOps)
}

/// Privileged operations of the running OS (stub until the Linux/macOS ports).
#[cfg(not(windows))]
pub fn privileged_ops() -> Arc<dyn PrivilegedOps> {
    Arc::new(Unsupported)
}

#[cfg(not(windows))]
#[derive(Debug)]
struct Unsupported;

#[cfg(not(windows))]
fn unsupported<T>() -> Result<T> {
    Err(ForgeError::UnsupportedPlatform(format!(
        "privileged operations are not implemented on {}",
        std::env::consts::OS
    )))
}

#[cfg(not(windows))]
impl PrivilegedOps for Unsupported {
    fn is_elevated(&self) -> Result<bool> {
        unsupported()
    }
    fn nrpt_rules(&self) -> Result<Vec<NrptRule>> {
        Ok(Vec::new())
    }
    fn nrpt_add(&self, _: &str, _: IpAddr, _: &str) -> Result<()> {
        unsupported()
    }
    fn nrpt_remove(&self, _: &str) -> Result<()> {
        unsupported()
    }
    fn hosts_path(&self) -> PathBuf {
        PathBuf::from("/etc/hosts")
    }
    fn replace_system_file(&self, _: &Path, _: &[u8]) -> Result<()> {
        unsupported()
    }
    fn flush_dns_cache(&self) -> Result<()> {
        unsupported()
    }
    fn certificate_info(&self, _: &[u8]) -> Result<CertificateInfo> {
        unsupported()
    }
    fn machine_root_find(&self, _: &CertThumbprint) -> Result<Option<CertificateInfo>> {
        unsupported()
    }
    fn machine_root_add(&self, _: &[u8]) -> Result<()> {
        unsupported()
    }
    fn machine_root_remove(&self, _: &CertThumbprint) -> Result<bool> {
        unsupported()
    }
    fn firewall_rule_count(&self, _: &str) -> Result<usize> {
        unsupported()
    }
    fn firewall_add(&self, _: &FirewallRuleSpec) -> Result<usize> {
        unsupported()
    }
    fn firewall_remove(&self, _: &str) -> Result<usize> {
        unsupported()
    }
    fn defender_add_exclusion(&self, _: &Path) -> Result<()> {
        unsupported()
    }
    fn defender_remove_exclusion(&self, _: &Path) -> Result<()> {
        unsupported()
    }
    fn stage_verified_copy(&self, _: &Path, _: &str, _: &str) -> Result<StagedFile> {
        unsupported()
    }
    fn run_installer(&self, _: &Path, _: &[&str], _: Duration) -> Result<i32> {
        unsupported()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_prefixes_display() {
        let p = IpPrefix {
            addr: "127.0.0.0".parse().unwrap(),
            len: Some(8),
        };
        assert_eq!(p.to_string(), "127.0.0.0/8");
        let odd = IpPrefix {
            addr: "10.0.0.0".parse().unwrap(),
            len: None,
        };
        assert_eq!(odd.to_string(), "10.0.0.0/?");
    }

    #[test]
    fn staged_files_are_deleted_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("stage");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("x.exe");
        std::fs::write(&path, b"x").unwrap();
        let staged = StagedFile::new(dir.clone(), path.clone());
        assert_eq!(staged.path(), path);
        assert_eq!(staged.dir(), dir);
        drop(staged);
        assert!(!dir.exists());
    }
}
