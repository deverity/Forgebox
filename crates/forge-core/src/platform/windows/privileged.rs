//! [`PrivilegedOps`] for Windows (docs/11 §1, ADR-0024). No shell, no `netsh`, no
//! PowerShell: every change goes through the API that the corresponding cmdlet uses.
//!
//! | operation | API |
//! |---|---|
//! | NRPT rules | WMI `PS_DnsClientNrptRule.Add/Remove` (`root/Microsoft/Windows/DNS`), read from the registry |
//! | `hosts` | sibling temporary file + `ReplaceFileW` (keeps ACL and attributes) |
//! | DNS cache | `DnsFlushResolverCache` |
//! | `LocalMachine\Root` | `CertOpenStore` + `CertAddEncodedCertificateToStore` / `CertDeleteCertificateFromStore` |
//! | firewall | COM `INetFwPolicy2` |
//! | Defender | WMI `MSFT_MpPreference.Add/Remove` (`root/Microsoft/Windows/Defender`) |
//! | installer | copy into `%SystemRoot%\Temp\forgebox-helper-<tag>` (DACL `SYSTEM` + `Administrators`), sha256 of the copy, `CreateProcess` without a window |

use std::io::{Read as _, Write as _};
use std::net::IpAddr;
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ::windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
use ::windows::Win32::Security::SECURITY_ATTRIBUTES;
use ::windows::Win32::Storage::FileSystem::{
    CreateDirectoryW, REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW,
};
use ::windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
use ::windows::core::{HSTRING, PCWSTR};
use sha2::{Digest, Sha256};

use super::cert_store;
use super::com::ComArg;
use super::dns_policy::{flush_resolver_cache, hosts_file_path, read_nrpt_rules};
use super::firewall;
use super::process::{os_error, win32_code};
use super::security::{OwnedDescriptor, process_is_elevated};
use super::wmi::Wmi;
use crate::error::{ForgeError, IoResultExt, Result};
use crate::ids::RequestId;
use crate::paths::plain_path;
use crate::platform::NrptRule;
use crate::platform::privileged::{CertificateInfo, FirewallRuleSpec, PrivilegedOps, StagedFile};
use crate::platform::secure_file::{NoFollow, open_no_follow};

/// WMI namespace of the DNS client cmdlets.
pub(crate) const NRPT_NAMESPACE: &str = "root/Microsoft/Windows/DNS";
/// Provider class behind `Add-DnsClientNrptRule` / `Remove-DnsClientNrptRule`.
pub(crate) const NRPT_CLASS: &str = "PS_DnsClientNrptRule";
/// WMI namespace of the Microsoft Defender cmdlets.
pub(crate) const DEFENDER_NAMESPACE: &str = "root/Microsoft/Windows/Defender";
/// Class behind `Add-MpPreference` / `Remove-MpPreference`.
pub(crate) const DEFENDER_CLASS: &str = "MSFT_MpPreference";

/// DACL of a staging directory: `SYSTEM` and `Administrators` only, inherited by the
/// staged file. A standard user can neither replace nor modify what will be executed.
pub(crate) const STAGING_DACL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

/// Interval between checks of a running installer.
const INSTALLER_POLL: Duration = Duration::from_millis(100);
/// `CREATE_NO_WINDOW`.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// [`PrivilegedOps`] of Windows. Stateless.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsPrivilegedOps;

fn windows_directory() -> Result<PathBuf> {
    let mut buf = [0_u16; 512];
    // SAFETY: writable buffer whose length is passed by the slice.
    let len = unsafe { GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
    if len == 0 || len > buf.len() {
        return Err(ForgeError::io(
            "locating the Windows directory",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

/// Creates `dir` with the protected DACL `sddl` applied at creation (no window with an
/// inherited ACL). An existing directory is a `conflict`.
fn create_dir_with_dacl(dir: &Path, sddl: &str) -> Result<()> {
    let descriptor = OwnedDescriptor::from_sddl(sddl)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_psd().0,
        bInheritHandle: false.into(),
    };
    // SAFETY: valid path string; the descriptor outlives the call.
    unsafe { CreateDirectoryW(&HSTRING::from(dir), Some(&attributes)) }.map_err(|e| {
        if win32_code(&e) == Some(ERROR_ALREADY_EXISTS.0) {
            ForgeError::Conflict(format!("{} already exists", dir.display()))
        } else {
            ForgeError::io_at("creating", dir, os_error(&e))
        }
    })
}

fn check_tag(tag: &str) -> Result<()> {
    let ok = !tag.is_empty()
        && tag.len() <= 64
        && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(ForgeError::InvalidInput(format!(
            "staging tag {tag:?} must be letters, digits and '-'"
        )))
    }
}

/// Lowercase hex of a digest.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// [`PrivilegedOps::stage_verified_copy`] under `base` with the directory DACL `sddl`.
pub(crate) fn stage_copy_in(
    base: &Path,
    source: &Path,
    sha256: &str,
    tag: &str,
    sddl: &str,
) -> Result<StagedFile> {
    check_tag(tag)?;
    let name = source.file_name().ok_or_else(|| {
        ForgeError::InvalidInput(format!("{} has no file name", source.display()))
    })?;
    // Open the source first: nothing is created for a file that cannot be read. While
    // this handle is open nobody can modify, rename or delete the source.
    let mut input = open_no_follow(source, NoFollow::ReadExclusive)?.file;
    let dir = base.join(format!("forgebox-helper-{tag}"));
    create_dir_with_dacl(&dir, sddl)?;
    let staged = StagedFile::new(dir.clone(), dir.join(name));
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(staged.path())
        .at("creating", staged.path())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buf).at("reading", source)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        output
            .write_all(&buf[..read])
            .at("writing", staged.path())?;
    }
    output.sync_all().at("writing", staged.path())?;
    drop(output);
    let actual = hex(&hasher.finalize());
    if !actual.eq_ignore_ascii_case(sha256) {
        return Err(ForgeError::VerificationFailed(format!(
            "{}: sha256 is {actual}, expected {}",
            source.display(),
            sha256.to_ascii_lowercase()
        )));
    }
    Ok(staged)
}

impl PrivilegedOps for WindowsPrivilegedOps {
    fn is_elevated(&self) -> Result<bool> {
        process_is_elevated()
    }

    fn nrpt_rules(&self) -> Result<Vec<NrptRule>> {
        read_nrpt_rules()
    }

    fn nrpt_add(&self, namespace: &str, nameserver: IpAddr, comment: &str) -> Result<()> {
        Wmi::connect(NRPT_NAMESPACE)?.call_static(
            NRPT_CLASS,
            "Add",
            &[
                ("Namespace", ComArg::StrArray(vec![namespace.to_owned()])),
                (
                    "NameServers",
                    ComArg::StrArray(vec![nameserver.to_string()]),
                ),
                ("Comment", ComArg::Str(comment.to_owned())),
                ("DisplayName", ComArg::Str(format!("{comment} {namespace}"))),
            ],
        )
    }

    fn nrpt_remove(&self, rule_id: &str) -> Result<()> {
        Wmi::connect(NRPT_NAMESPACE)?.call_static(
            NRPT_CLASS,
            "Remove",
            &[
                ("Name", ComArg::Str(rule_id.to_owned())),
                ("Force", ComArg::Bool(true)),
            ],
        )
    }

    fn hosts_path(&self) -> PathBuf {
        hosts_file_path()
    }

    fn replace_system_file(&self, path: &Path, contents: &[u8]) -> Result<()> {
        let name = path.file_name().ok_or_else(|| {
            ForgeError::InvalidInput(format!("{} has no file name", path.display()))
        })?;
        let temp = path.with_file_name(format!(
            "{}.forgebox-{}.tmp",
            name.to_string_lossy(),
            RequestId::new()
        ));
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .and_then(|mut f| f.write_all(contents).and_then(|()| f.sync_all()))
            .at("writing", &temp);
        if let Err(err) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(err);
        }
        if !path.exists() {
            return std::fs::rename(&temp, path).at("creating", path);
        }
        // SAFETY: valid path strings; no backup file, no reserved arguments.
        let replaced = unsafe {
            ReplaceFileW(
                &HSTRING::from(path),
                &HSTRING::from(temp.as_path()),
                PCWSTR::null(),
                REPLACEFILE_IGNORE_MERGE_ERRORS,
                None,
                None,
            )
        };
        if let Err(e) = replaced {
            let _ = std::fs::remove_file(&temp);
            return Err(ForgeError::io_at("replacing", path, os_error(&e)));
        }
        Ok(())
    }

    fn flush_dns_cache(&self) -> Result<()> {
        flush_resolver_cache()
    }

    fn certificate_info(&self, bytes: &[u8]) -> Result<CertificateInfo> {
        cert_store::certificate_info(bytes)
    }

    fn machine_root_find(&self, thumbprint: &str) -> Result<Option<CertificateInfo>> {
        cert_store::machine_root_find(thumbprint)
    }

    fn machine_root_add(&self, der: &[u8]) -> Result<()> {
        cert_store::machine_root_add(der)
    }

    fn machine_root_remove(&self, thumbprint: &str) -> Result<bool> {
        cert_store::machine_root_remove(thumbprint)
    }

    fn firewall_rule_count(&self, name: &str) -> Result<usize> {
        firewall::rule_count(name)
    }

    fn firewall_add(&self, rule: &FirewallRuleSpec) -> Result<usize> {
        firewall::add_rule(rule)
    }

    fn firewall_remove(&self, name: &str) -> Result<usize> {
        firewall::remove_rules(name)
    }

    fn defender_add_exclusion(&self, path: &Path) -> Result<()> {
        defender("Add", path)
    }

    fn defender_remove_exclusion(&self, path: &Path) -> Result<()> {
        defender("Remove", path)
    }

    fn stage_verified_copy(&self, source: &Path, sha256: &str, tag: &str) -> Result<StagedFile> {
        let base = windows_directory()?.join("Temp");
        stage_copy_in(&base, source, sha256, tag, STAGING_DACL)
    }

    fn run_installer(&self, exe: &Path, args: &[&str], timeout: Duration) -> Result<i32> {
        let mut child = std::process::Command::new(exe)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .at("starting", exe)?;
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait().at("waiting for", exe)? {
                return Ok(status.code().unwrap_or(-1));
            }
            if started.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ForgeError::Timeout {
                    operation: format!("running {}", exe.display()),
                    after: timeout,
                });
            }
            std::thread::sleep(INSTALLER_POLL);
        }
    }
}

fn defender(method: &str, path: &Path) -> Result<()> {
    let path = plain_path(path);
    let path = path.to_str().ok_or_else(|| {
        ForgeError::InvalidInput(format!("{} is not valid Unicode", path.display()))
    })?;
    Wmi::connect(DEFENDER_NAMESPACE)?.call_static(
        DEFENDER_CLASS,
        method,
        &[("ExclusionPath", ComArg::StrArray(vec![path.to_owned()]))],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::windows::security::{current_user_sid, path_sddl};

    fn sha256_hex(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    #[test]
    fn staging_copies_verifies_and_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("vc_redist.x64.exe");
        std::fs::write(&source, b"installer bytes").unwrap();
        let base = tmp.path().join("base");
        std::fs::create_dir(&base).unwrap();
        // In tests the current user stands in for SYSTEM/Administrators.
        let sddl = format!("D:P(A;OICI;FA;;;{})", current_user_sid().unwrap());

        let sha = sha256_hex(b"installer bytes").to_ascii_uppercase();
        let staged = stage_copy_in(&base, &source, &sha, "01TEST", &sddl).unwrap();
        assert_eq!(std::fs::read(staged.path()).unwrap(), b"installer bytes");
        assert_eq!(staged.dir(), base.join("forgebox-helper-01TEST"));
        let dacl = path_sddl(
            staged.dir(),
            ::windows::Win32::Security::DACL_SECURITY_INFORMATION,
        )
        .unwrap();
        assert!(dacl.starts_with("D:P"), "protected: {dacl}");
        // The same tag twice is a conflict (no reuse of a directory someone prepared).
        assert_eq!(
            stage_copy_in(&base, &source, &sha, "01TEST", &sddl)
                .unwrap_err()
                .code(),
            "conflict"
        );
        let dir = staged.dir().to_path_buf();
        drop(staged);
        assert!(!dir.exists());

        let err = stage_copy_in(&base, &source, &"0".repeat(64), "01BAD", &sddl).unwrap_err();
        assert_eq!(err.code(), "verification_failed", "{err}");
        assert!(
            !base.join("forgebox-helper-01BAD").exists(),
            "removed on mismatch"
        );

        assert_eq!(
            stage_copy_in(&base, &source, &sha, "../x", &sddl)
                .unwrap_err()
                .code(),
            "invalid_input"
        );
        assert_eq!(
            stage_copy_in(
                &base,
                &tmp.path().join("nope.exe"),
                &sha,
                "01MISSING",
                &sddl
            )
            .unwrap_err()
            .code(),
            "not_found"
        );
        assert!(!base.join("forgebox-helper-01MISSING").exists());
    }

    #[test]
    fn system_files_are_replaced_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hosts");
        std::fs::write(&path, b"old").unwrap();
        WindowsPrivilegedOps
            .replace_system_file(&path, b"new\r\n")
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new\r\n");
        let left: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            left,
            vec![std::ffi::OsString::from("hosts")],
            "no temp left"
        );
        let fresh = tmp.path().join("fresh");
        WindowsPrivilegedOps
            .replace_system_file(&fresh, b"x")
            .unwrap();
        assert_eq!(std::fs::read(&fresh).unwrap(), b"x");
    }

    #[test]
    fn installers_run_without_window_and_time_out() {
        let cmd = super::super::process::system_directory().join("cmd.exe");
        let code = WindowsPrivilegedOps
            .run_installer(&cmd, &["/c", "exit", "3010"], Duration::from_secs(30))
            .unwrap();
        assert_eq!(code, 3010);
        let ping = super::super::process::system_directory().join("PING.EXE");
        let err = WindowsPrivilegedOps
            .run_installer(
                &ping,
                &["-n", "30", "127.0.0.1"],
                Duration::from_millis(300),
            )
            .unwrap_err();
        assert_eq!(err.code(), "timeout", "{err}");
    }

    #[test]
    fn read_only_queries_work_without_elevation() {
        let ops = WindowsPrivilegedOps;
        ops.is_elevated().unwrap();
        ops.nrpt_rules().unwrap();
        assert!(ops.hosts_path().is_file());
        assert_eq!(ops.firewall_rule_count("Forgebox-never-0123").unwrap(), 0);
        assert!(ops.machine_root_find(&"ab".repeat(20)).unwrap().is_none());
        assert!(windows_directory().unwrap().join("System32").is_dir());
    }

    /// Changes must fail cleanly (no UAC prompt, no partial change) when not elevated.
    /// Skipped when the test process is elevated: it would really change the machine.
    #[test]
    fn changes_need_elevation() {
        let ops = WindowsPrivilegedOps;
        if ops.is_elevated().unwrap() {
            eprintln!("elevated test process: skipping the needs_elevation checks");
            return;
        }
        let err = ops
            .nrpt_add(
                ".forgebox-test-never",
                "127.0.0.1".parse().unwrap(),
                "Forgebox test",
            )
            .unwrap_err();
        assert_eq!(err.code(), "needs_elevation", "{err}");
        let rule = FirewallRuleSpec {
            name: "Forgebox-test-never".into(),
            description: "test".into(),
            grouping: "Forgebox".into(),
            exe: super::super::process::system_directory().join("cmd.exe"),
            port: 65000,
        };
        let err = ops.firewall_add(&rule).unwrap_err();
        assert_eq!(err.code(), "needs_elevation", "{err}");
        assert_eq!(ops.firewall_rule_count("Forgebox-test-never").unwrap(), 0);
        let err = ops.machine_root_remove(&"ab".repeat(20));
        // Opening LocalMachine\Root for writing is refused, or nothing is there to delete.
        assert!(
            matches!(&err, Ok(false)) || err.as_ref().unwrap_err().code() == "needs_elevation",
            "{err:?}"
        );
    }
}
