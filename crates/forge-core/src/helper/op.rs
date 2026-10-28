//! The allowlist of `forgebox-helper` and the validation of its parameters (docs/11 §1).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Component, Path, PathBuf, Prefix};

use serde::{Deserialize, Serialize};

use super::hosts::HostsEntry;
use crate::config::ForgeConfig;
use crate::dns_name;
use crate::error::{ForgeError, Result};
use crate::paths::ForgeRoot;

/// Addresses an NRPT rule may point at: where the Forgebox resolver listens on port 53
/// (`127.0.0.53` when `127.0.0.1:53` is taken, ADR-0004). NRPT takes no port.
pub const NRPT_NAMESERVERS: [IpAddr; 2] = [
    IpAddr::V4(Ipv4Addr::LOCALHOST),
    IpAddr::V4(Ipv4Addr::new(127, 0, 0, 53)),
];

/// The only addresses the `hosts` managed block may map names to.
pub const HOSTS_ALLOWED_IPS: [IpAddr; 2] = [
    IpAddr::V4(Ipv4Addr::LOCALHOST),
    IpAddr::V6(Ipv6Addr::LOCALHOST),
];

/// Prefix of every firewall rule the helper creates or removes.
pub const FIREWALL_RULE_PREFIX: &str = "Forgebox-";

/// Most entries a `hosts.set_block` may carry (one line per host and address).
pub const MAX_HOSTS_ENTRIES: usize = 4096;

/// Longest firewall rule name after the prefix.
const MAX_FIREWALL_SUFFIX: usize = 64;

/// One privileged operation. The set is closed: an unknown `name` or an unknown field in
/// `params` fails to deserialize, so it can never reach the code that acts.
///
/// Wire form: `{"name": "nrpt.add", "params": {"namespace": ".test", "nameserver":
/// "127.0.0.1"}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "name", content = "params", deny_unknown_fields)]
pub enum HelperOp {
    /// Routes `namespace` (`.test`) to `nameserver` through the Name Resolution Policy
    /// Table. Replaces a Forgebox rule that already exists for the namespace.
    #[serde(rename = "nrpt.add")]
    NrptAdd {
        namespace: String,
        nameserver: IpAddr,
    },
    /// Removes the Forgebox NRPT rule of `namespace` (rules created by others are kept).
    #[serde(rename = "nrpt.remove")]
    NrptRemove { namespace: String },
    /// Rewrites the managed block of the system `hosts` file with `entries`; an empty list
    /// removes the block. Nothing outside the block is touched.
    #[serde(rename = "hosts.set_block")]
    HostsSetBlock { entries: Vec<HostsEntry> },
    /// Trusts the Forgebox CA for every user (`LocalMachine\Root`). The certificate is
    /// `certs/ca/forgebox-ca.crt` and must have this SHA-1 thumbprint.
    #[serde(rename = "trust.add_machine_root")]
    TrustAddMachineRoot { thumbprint: String },
    /// Removes the certificate with this thumbprint from `LocalMachine\Root`.
    #[serde(rename = "trust.remove_machine_root")]
    TrustRemoveMachineRoot { thumbprint: String },
    /// Inbound TCP rule `name` for `exe` on `port` (LAN exposure, docs/06 §5.3).
    #[serde(rename = "firewall.add")]
    FirewallAdd {
        name: String,
        port: u16,
        exe: PathBuf,
    },
    /// Removes the firewall rule `name`.
    #[serde(rename = "firewall.remove")]
    FirewallRemove { name: String },
    /// Excludes the Forgebox root from Microsoft Defender real-time scanning.
    #[serde(rename = "defender.exclude")]
    DefenderExclude { path: PathBuf },
    /// Removes the Defender exclusion of the root.
    #[serde(rename = "defender.remove")]
    DefenderRemove { path: PathBuf },
    /// Runs the VC++ Redistributable installer downloaded by the daemon (docs/03 §1.5)
    /// after checking its sha256 and its Microsoft Authenticode signature.
    #[serde(rename = "vcredist.install")]
    VcredistInstall { path: PathBuf, sha256: String },
    /// Turns the NTFS case-sensitive flag on for a directory when the user lacks the
    /// right to do it (docs/15 §3).
    #[serde(rename = "fs.set_case_sensitive")]
    FsSetCaseSensitive { path: PathBuf },
}

impl HelperOp {
    /// Every operation name, in the order of docs/11 §1.
    pub const NAMES: &'static [&'static str] = &[
        "nrpt.add",
        "nrpt.remove",
        "hosts.set_block",
        "trust.add_machine_root",
        "trust.remove_machine_root",
        "firewall.add",
        "firewall.remove",
        "defender.exclude",
        "defender.remove",
        "vcredist.install",
        "fs.set_case_sensitive",
    ];

    /// Wire name (`nrpt.add`), also the `name` of the `helper.op` event.
    pub fn name(&self) -> &'static str {
        match self {
            Self::NrptAdd { .. } => "nrpt.add",
            Self::NrptRemove { .. } => "nrpt.remove",
            Self::HostsSetBlock { .. } => "hosts.set_block",
            Self::TrustAddMachineRoot { .. } => "trust.add_machine_root",
            Self::TrustRemoveMachineRoot { .. } => "trust.remove_machine_root",
            Self::FirewallAdd { .. } => "firewall.add",
            Self::FirewallRemove { .. } => "firewall.remove",
            Self::DefenderExclude { .. } => "defender.exclude",
            Self::DefenderRemove { .. } => "defender.remove",
            Self::VcredistInstall { .. } => "vcredist.install",
            Self::FsSetCaseSensitive { .. } => "fs.set_case_sensitive",
        }
    }

    /// Checks every parameter against `policy`. Pure and lexical: the helper repeats the
    /// file checks that need I/O (certificate thumbprint, installer hash and signature,
    /// canonical paths) right before acting.
    ///
    /// Errors: `invalid_input` for malformed values (bad thumbprint, port 0, name syntax),
    /// `forbidden` for well-formed values outside the policy (TLD not configured or not
    /// reserved for local use, address not loopback, path outside the root).
    pub fn validate(&self, policy: &HelperPolicy) -> Result<()> {
        let name = self.name();
        let fail = |err: ForgeError| prefix_error(name, err);
        match self {
            Self::NrptAdd {
                namespace,
                nameserver,
            } => {
                policy.check_namespace(namespace).map_err(fail)?;
                if !NRPT_NAMESERVERS.contains(nameserver) {
                    return Err(fail(ForgeError::Forbidden(format!(
                        "nameserver {nameserver} is not one of {NRPT_NAMESERVERS:?}"
                    ))));
                }
                Ok(())
            }
            Self::NrptRemove { namespace } => policy.check_namespace(namespace).map_err(fail),
            Self::HostsSetBlock { entries } => {
                if entries.len() > MAX_HOSTS_ENTRIES {
                    return Err(fail(ForgeError::InvalidInput(format!(
                        "{} entries; at most {MAX_HOSTS_ENTRIES} are accepted",
                        entries.len()
                    ))));
                }
                for (i, entry) in entries.iter().enumerate() {
                    if !HOSTS_ALLOWED_IPS.contains(&entry.ip) {
                        return Err(fail(ForgeError::Forbidden(format!(
                            "{}: only 127.0.0.1 and ::1 may be mapped, not {}",
                            entry.host, entry.ip
                        ))));
                    }
                    policy.check_host(&entry.host).map_err(fail)?;
                    if entries[..i].contains(entry) {
                        return Err(fail(ForgeError::InvalidInput(format!(
                            "duplicate entry {} {}",
                            entry.ip, entry.host
                        ))));
                    }
                }
                Ok(())
            }
            Self::TrustAddMachineRoot { thumbprint }
            | Self::TrustRemoveMachineRoot { thumbprint } => {
                check_hex(thumbprint, 40, "thumbprint (SHA-1)").map_err(fail)
            }
            Self::FirewallAdd { name, port, exe } => {
                check_firewall_name(name).map_err(fail)?;
                if *port == 0 {
                    return Err(fail(ForgeError::InvalidInput("port 0".to_owned())));
                }
                let bin = policy.root.join("bin");
                policy
                    .check_file_within(exe, &bin, "exe", "exe")
                    .map_err(fail)
            }
            Self::FirewallRemove { name } => check_firewall_name(name).map_err(fail),
            Self::DefenderExclude { path } | Self::DefenderRemove { path } => {
                if same_path(path, &policy.root).map_err(fail)? {
                    Ok(())
                } else {
                    Err(fail(ForgeError::Forbidden(format!(
                        "{} is not the Forgebox root {}",
                        path.display(),
                        policy.root.display()
                    ))))
                }
            }
            Self::VcredistInstall { path, sha256 } => {
                check_hex(sha256, 64, "sha256").map_err(fail)?;
                let downloads = policy.root.join("tmp").join("downloads");
                policy
                    .check_file_within(path, &downloads, "installer", "exe")
                    .map_err(fail)
            }
            Self::FsSetCaseSensitive { path } => {
                policy.check_case_sensitive_dir(path).map_err(fail)
            }
        }
    }
}

fn prefix_error(op: &str, err: ForgeError) -> ForgeError {
    match err {
        ForgeError::Forbidden(msg) => ForgeError::Forbidden(format!("{op}: {msg}")),
        ForgeError::InvalidInput(msg) => ForgeError::InvalidInput(format!("{op}: {msg}")),
        other => other,
    }
}

fn check_hex(value: &str, len: usize, what: &str) -> Result<()> {
    if value.len() == len && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ForgeError::InvalidInput(format!(
            "{what} must be {len} hexadecimal digits, got {value:?}"
        )))
    }
}

fn check_firewall_name(name: &str) -> Result<()> {
    let suffix = name.strip_prefix(FIREWALL_RULE_PREFIX).ok_or_else(|| {
        ForgeError::Forbidden(format!(
            "rule name {name:?} must start with {FIREWALL_RULE_PREFIX:?}"
        ))
    })?;
    let ok = !suffix.is_empty()
        && suffix.len() <= MAX_FIREWALL_SUFFIX
        && suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(ForgeError::InvalidInput(format!(
            "rule name {name:?} may only use letters, digits, '.', '_' and '-' \
             (1 to {MAX_FIREWALL_SUFFIX} after the prefix)"
        )))
    }
}

/// Case folding of path components: Windows file names are compared case-insensitively
/// (`bin/` is never case-sensitive, docs/15 §3.1).
fn fold(name: &str) -> String {
    if cfg!(windows) {
        name.to_lowercase()
    } else {
        name.to_owned()
    }
}

/// Lexical key of an absolute path: folded components, `\\?\C:\x` and `c:/X` give the
/// same key. Refuses relative paths, `.`/`..` (also inside verbatim paths), alternate data
/// streams (`:`) and components ending with `.` or a space (Windows strips them, so they
/// alias another name).
fn path_key(path: &Path) -> Result<Vec<String>> {
    let bad = |why: &str| {
        Err(ForgeError::InvalidInput(format!(
            "path {} {why}",
            path.display()
        )))
    };
    let mut key = Vec::new();
    let mut rooted = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                    key.push(format!("{}:", char::from(drive).to_ascii_lowercase()));
                }
                Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                    let (Some(server), Some(share)) = (server.to_str(), share.to_str()) else {
                        return bad("is not valid Unicode");
                    };
                    key.push(format!(r"\\{}\{}", fold(server), fold(share)));
                }
                _ => return bad("uses an unsupported prefix"),
            },
            Component::RootDir => rooted = true,
            Component::CurDir | Component::ParentDir => return bad("must not contain '.' or '..'"),
            Component::Normal(name) => {
                let Some(name) = name.to_str() else {
                    return bad("is not valid Unicode");
                };
                if name == "." || name == ".." {
                    return bad("must not contain '.' or '..'");
                }
                if name.contains(':') || name.ends_with('.') || name.ends_with(' ') {
                    return bad("has a component Windows would alias (':' or trailing '.'/' ')");
                }
                key.push(fold(name));
            }
        }
    }
    if !rooted || !path.is_absolute() {
        return bad("must be absolute");
    }
    Ok(key)
}

fn same_path(a: &Path, b: &Path) -> Result<bool> {
    Ok(path_key(a)? == path_key(b)?)
}

/// Whether `path` is strictly inside `base` (lexically).
fn strictly_within(path: &Path, base: &Path) -> Result<bool> {
    let path = path_key(path)?;
    let base = path_key(base)?;
    Ok(path.len() > base.len() && path.starts_with(&base))
}

fn within_or_equal(path: &Path, base: &Path) -> Result<bool> {
    let path = path_key(path)?;
    let base = path_key(base)?;
    Ok(path.starts_with(&base))
}

/// What the helper may act on: the configured TLDs, the root and the parked directories.
/// The helper builds it from its own root's `etc/forgebox.toml` (never from the request);
/// the daemon builds the same one to refuse invalid operations before any UAC prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperPolicy {
    root: PathBuf,
    tlds: Vec<String>,
    parked_dirs: Vec<PathBuf>,
}

impl HelperPolicy {
    /// Policy for the absolute `root` and the suffixes `tlds` (`.test`). No parked
    /// directory besides those inside the root.
    pub fn new(
        root: impl Into<PathBuf>,
        tlds: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self> {
        let root = root.into();
        path_key(&root)?;
        let mut checked = Vec::new();
        for tld in tlds {
            let tld = tld.into();
            dns_name::parse_suffix(&tld).map_err(ForgeError::InvalidInput)?;
            if !checked.contains(&tld) {
                checked.push(tld);
            }
        }
        Ok(Self {
            root,
            tlds: checked,
            parked_dirs: Vec::new(),
        })
    }

    /// Adds parked directories (absolute) where `fs.set_case_sensitive` is allowed.
    pub fn with_parked_dirs(mut self, dirs: impl IntoIterator<Item = PathBuf>) -> Result<Self> {
        for dir in dirs {
            path_key(&dir)?;
            self.parked_dirs.push(dir);
        }
        Ok(self)
    }

    /// Policy of an installation: `config.tlds()` and `parked_dirs` (relative entries are
    /// resolved against the root).
    pub fn from_config(root: &ForgeRoot, config: &ForgeConfig) -> Result<Self> {
        let parked = config.parked_dirs.iter().map(|dir| {
            let dir = Path::new(dir);
            if dir.is_absolute() {
                dir.to_path_buf()
            } else {
                root.path().join(dir)
            }
        });
        Self::new(root.path(), config.tlds())?.with_parked_dirs(parked)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Configured suffixes, with the leading dot.
    pub fn tlds(&self) -> &[String] {
        &self.tlds
    }

    pub fn parked_dirs(&self) -> &[PathBuf] {
        &self.parked_dirs
    }

    fn check_namespace(&self, namespace: &str) -> Result<()> {
        let name = dns_name::parse_suffix(namespace).map_err(ForgeError::InvalidInput)?;
        if !self.tlds.iter().any(|tld| tld == namespace) {
            return Err(ForgeError::Forbidden(format!(
                "namespace {namespace:?} is not one of the configured TLDs {:?}",
                self.tlds
            )));
        }
        check_local_use(name)
    }

    fn check_host(&self, host: &str) -> Result<()> {
        dns_name::check_name(host).map_err(ForgeError::InvalidInput)?;
        let Some(tld) = self
            .tlds
            .iter()
            .map(|tld| &tld[1..])
            .find(|tld| host != *tld && dns_name::is_within(host, tld))
        else {
            return Err(ForgeError::Forbidden(format!(
                "host {host:?} is not below one of the configured TLDs {:?}",
                self.tlds
            )));
        };
        check_local_use(tld)
    }

    /// `path` is a file strictly inside `base` with the extension `ext`.
    fn check_file_within(&self, path: &Path, base: &Path, what: &str, ext: &str) -> Result<()> {
        if !strictly_within(path, base)? {
            return Err(ForgeError::Forbidden(format!(
                "{what} {} is not inside {}",
                path.display(),
                base.display()
            )));
        }
        let has_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case(ext));
        if has_ext {
            Ok(())
        } else {
            Err(ForgeError::Forbidden(format!(
                "{what} {} is not a .{ext} file",
                path.display()
            )))
        }
    }

    /// Inside the root (not the root itself, not `bin/` nor `state/`), or a parked
    /// directory or something inside one.
    fn check_case_sensitive_dir(&self, path: &Path) -> Result<()> {
        let in_root = strictly_within(path, &self.root)?
            && !within_or_equal(path, &self.root.join("bin"))?
            && !within_or_equal(path, &self.root.join("state"))?;
        let mut allowed = in_root;
        for parked in &self.parked_dirs {
            if allowed {
                break;
            }
            allowed = within_or_equal(path, parked)?;
        }
        if allowed {
            Ok(())
        } else {
            Err(ForgeError::Forbidden(format!(
                "{} is neither inside the Forgebox root (outside bin/ and state/) nor in a \
                 parked directory",
                path.display()
            )))
        }
    }
}

fn check_local_use(name: &str) -> Result<()> {
    if dns_name::is_local_use(name) {
        Ok(())
    } else {
        Err(ForgeError::Forbidden(format!(
            ".{name} is not reserved for local use; the helper only routes names under {}",
            dns_name::LOCAL_USE_SUFFIXES
                .iter()
                .map(|s| format!(".{s}"))
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    const ROOT: &str = r"C:\Forgebox";
    #[cfg(not(windows))]
    const ROOT: &str = "/opt/forgebox";

    fn root() -> PathBuf {
        PathBuf::from(ROOT)
    }

    fn policy() -> HelperPolicy {
        HelperPolicy::new(root(), [".test", ".localhost", ".dev"])
            .unwrap()
            .with_parked_dirs([root().join("www"), outside("clientes")])
            .unwrap()
    }

    /// An absolute path outside the root.
    fn outside(name: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"D:\").join(name)
        } else {
            PathBuf::from("/srv").join(name)
        }
    }

    fn entry(ip: &str, host: &str) -> HostsEntry {
        HostsEntry {
            ip: ip.parse().unwrap(),
            host: host.to_owned(),
        }
    }

    fn assert_code(op: &HelperOp, code: &str) {
        match op.validate(&policy()) {
            Ok(()) => panic!("{op:?} must be refused with {code}"),
            Err(err) => {
                assert_eq!(err.code(), code, "{op:?}: {err}");
                assert!(err.to_string().contains(op.name()), "{err}");
            }
        }
    }

    fn valid_ops() -> Vec<HelperOp> {
        vec![
            HelperOp::NrptAdd {
                namespace: ".test".into(),
                nameserver: "127.0.0.1".parse().unwrap(),
            },
            HelperOp::NrptAdd {
                namespace: ".localhost".into(),
                nameserver: "127.0.0.53".parse().unwrap(),
            },
            HelperOp::NrptRemove {
                namespace: ".test".into(),
            },
            HelperOp::HostsSetBlock { entries: vec![] },
            HelperOp::HostsSetBlock {
                entries: vec![
                    entry("127.0.0.1", "loja.test"),
                    entry("::1", "loja.test"),
                    entry("127.0.0.1", "api.loja.test"),
                    entry("127.0.0.1", "xn--caf-dma.test"),
                    entry("127.0.0.1", "app.localhost"),
                ],
            },
            HelperOp::TrustAddMachineRoot {
                thumbprint: "0123456789abcdef0123456789ABCDEF01234567".into(),
            },
            HelperOp::TrustRemoveMachineRoot {
                thumbprint: "a".repeat(40),
            },
            HelperOp::FirewallAdd {
                name: "Forgebox-nginx-1.27_lan".into(),
                port: 443,
                exe: root().join("bin/nginx/1.27.4/nginx.exe"),
            },
            HelperOp::FirewallAdd {
                name: "Forgebox-x".into(),
                port: 65535,
                exe: root().join("BIN").join("Caddy").join("caddy.EXE"),
            },
            HelperOp::FirewallRemove {
                name: "Forgebox-nginx".into(),
            },
            HelperOp::DefenderExclude { path: root() },
            HelperOp::DefenderRemove { path: root() },
            HelperOp::VcredistInstall {
                path: root().join("tmp/downloads/vc_redist.x64.exe"),
                sha256: "f".repeat(64),
            },
            HelperOp::FsSetCaseSensitive {
                path: root().join("www"),
            },
            HelperOp::FsSetCaseSensitive {
                path: root().join("data").join("mysql"),
            },
            HelperOp::FsSetCaseSensitive {
                path: outside("clientes"),
            },
            HelperOp::FsSetCaseSensitive {
                path: outside("clientes").join("loja"),
            },
        ]
    }

    #[test]
    fn valid_operations_pass() {
        for op in valid_ops() {
            op.validate(&policy())
                .unwrap_or_else(|e| panic!("{op:?}: {e}"));
        }
    }

    #[test]
    fn every_operation_is_named_and_covered() {
        let mut seen: Vec<&str> = valid_ops().iter().map(HelperOp::name).collect();
        seen.sort_unstable();
        seen.dedup();
        let mut names = HelperOp::NAMES.to_vec();
        names.sort_unstable();
        assert_eq!(seen, names, "every op has a positive test");
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_compare_without_case_and_prefix() {
        for path in [
            r"c:\forgebox",
            r"C:\FORGEBOX",
            r"C:/Forgebox",
            r"\\?\C:\Forgebox",
            r"C:\Forgebox\",
        ] {
            HelperOp::DefenderExclude { path: path.into() }
                .validate(&policy())
                .unwrap_or_else(|e| panic!("{path}: {e}"));
        }
        HelperOp::FirewallAdd {
            name: "Forgebox-x".into(),
            port: 80,
            exe: r"\\?\c:\forgebox\bin\x\x.exe".into(),
        }
        .validate(&policy())
        .unwrap();
    }

    #[test]
    fn nrpt_refusals() {
        let ns = |namespace: &str, nameserver: &str| HelperOp::NrptAdd {
            namespace: namespace.into(),
            nameserver: nameserver.parse().unwrap(),
        };
        // Malformed namespaces.
        for bad in [
            "test", ".TEST", "..test", ".test.", ".", "", ".te st", "*.test",
        ] {
            assert_code(&ns(bad, "127.0.0.1"), "invalid_input");
            assert_code(
                &HelperOp::NrptRemove {
                    namespace: bad.into(),
                },
                "invalid_input",
            );
        }
        // Well formed, but not configured.
        for not_configured in [".com", ".local", ".hml.test", ".example"] {
            assert_code(&ns(not_configured, "127.0.0.1"), "forbidden");
        }
        // Configured but a public TLD: never routed (forgebox.toml is user-writable).
        assert_code(&ns(".dev", "127.0.0.1"), "forbidden");
        assert_code(
            &HelperOp::NrptRemove {
                namespace: ".dev".into(),
            },
            "forbidden",
        );
        // Only the resolver's loopback addresses.
        for server in ["127.0.0.2", "10.0.0.5", "8.8.8.8", "::1", "0.0.0.0"] {
            assert_code(&ns(".test", server), "forbidden");
        }
    }

    #[test]
    fn hosts_refusals() {
        let block = |entries: Vec<HostsEntry>| HelperOp::HostsSetBlock { entries };
        for (ip, host, code) in [
            ("10.0.0.5", "loja.test", "forbidden"),
            ("0.0.0.0", "loja.test", "forbidden"),
            ("127.0.0.2", "loja.test", "forbidden"),
            ("::2", "loja.test", "forbidden"),
            ("127.0.0.1", "google.com", "forbidden"),
            ("127.0.0.1", "loja.dev", "forbidden"),
            ("127.0.0.1", "test", "forbidden"),
            ("127.0.0.1", "latest", "forbidden"),
            ("127.0.0.1", "Loja.test", "invalid_input"),
            ("127.0.0.1", "*.loja.test", "invalid_input"),
            ("127.0.0.1", "lo ja.test", "invalid_input"),
            ("127.0.0.1", "loja.test\nevil.com", "invalid_input"),
            ("127.0.0.1", "loja.test #x", "invalid_input"),
            ("127.0.0.1", "-loja.test", "invalid_input"),
            ("127.0.0.1", "loja..test", "invalid_input"),
            ("127.0.0.1", "", "invalid_input"),
        ] {
            assert_code(&block(vec![entry(ip, host)]), code);
        }
        assert_code(
            &block(vec![
                entry("127.0.0.1", "loja.test"),
                entry("127.0.0.1", "loja.test"),
            ]),
            "invalid_input",
        );
        let too_many = (0..=MAX_HOSTS_ENTRIES)
            .map(|i| entry("127.0.0.1", &format!("s{i}.test")))
            .collect();
        assert_code(&block(too_many), "invalid_input");
        // The limit itself is fine.
        block(
            (0..MAX_HOSTS_ENTRIES)
                .map(|i| entry("127.0.0.1", &format!("s{i}.test")))
                .collect(),
        )
        .validate(&policy())
        .unwrap();
    }

    #[test]
    fn trust_refusals() {
        for bad in [
            "",
            "0123",
            &"g".repeat(40),
            &"a".repeat(39),
            &"a".repeat(41),
            &"a".repeat(64),
            "01 23456789abcdef0123456789abcdef0123456",
        ] {
            assert_code(
                &HelperOp::TrustAddMachineRoot {
                    thumbprint: bad.into(),
                },
                "invalid_input",
            );
            assert_code(
                &HelperOp::TrustRemoveMachineRoot {
                    thumbprint: bad.into(),
                },
                "invalid_input",
            );
        }
    }

    #[test]
    fn firewall_refusals() {
        let add = |name: &str, port: u16, exe: PathBuf| HelperOp::FirewallAdd {
            name: name.into(),
            port,
            exe,
        };
        let good_exe = root().join("bin/nginx/1.27.4/nginx.exe");
        for (name, code) in [
            ("nginx", "forbidden"),
            ("forgebox-nginx", "forbidden"),
            ("Forgebox-", "invalid_input"),
            ("Forgebox-a b", "invalid_input"),
            ("Forgebox-a\"b", "invalid_input"),
            ("Forgebox-a/b", "invalid_input"),
            ("Forgebox-ação", "invalid_input"),
        ] {
            assert_code(&add(name, 80, good_exe.clone()), code);
            assert_code(&HelperOp::FirewallRemove { name: name.into() }, code);
        }
        let long = format!("Forgebox-{}", "a".repeat(MAX_FIREWALL_SUFFIX + 1));
        assert_code(&HelperOp::FirewallRemove { name: long }, "invalid_input");
        assert_code(&add("Forgebox-x", 0, good_exe), "invalid_input");
        for (exe, code) in [
            (root().join("forgeboxd.exe"), "forbidden"),
            (root().join("bin"), "forbidden"),
            (root().join("bin/nginx/nginx.bat"), "forbidden"),
            (root().join("bin/nginx/nginx"), "forbidden"),
            (root().join("data/nginx.exe"), "forbidden"),
            (outside("bin/nginx.exe"), "forbidden"),
            (root().join("bin/../tools/x.exe"), "invalid_input"),
            (PathBuf::from("bin/x.exe"), "invalid_input"),
            (root().join("bin/x.exe:stream"), "invalid_input"),
            (root().join("bin/x.exe."), "invalid_input"),
            (root().join("bin/x.exe "), "invalid_input"),
        ] {
            assert_code(&add("Forgebox-x", 80, exe), code);
        }
        // `.` components are dropped by the Windows path normalization too: same file.
        add("Forgebox-x", 80, root().join("bin/./x.exe"))
            .validate(&policy())
            .unwrap();
        #[cfg(windows)]
        for exe in [
            r"\\?\C:\Forgebox\bin\..\x.exe",
            r"C:\Forgebox\bin\..\..\Windows\System32\cmd.exe",
            r"C:Forgebox\bin\x.exe",
            r"\Forgebox\bin\x.exe",
        ] {
            assert_code(&add("Forgebox-x", 80, exe.into()), "invalid_input");
        }
        #[cfg(windows)]
        assert_code(
            &add(
                "Forgebox-x",
                80,
                r"\\server\share\Forgebox\bin\x.exe".into(),
            ),
            "forbidden",
        );
    }

    #[test]
    fn defender_refusals() {
        for (path, code) in [
            (root().join("www"), "forbidden"),
            (root().join("bin"), "forbidden"),
            (outside("Forgebox"), "forbidden"),
            (PathBuf::from("Forgebox"), "invalid_input"),
            (root().join(".."), "invalid_input"),
        ] {
            assert_code(&HelperOp::DefenderExclude { path: path.clone() }, code);
            assert_code(&HelperOp::DefenderRemove { path }, code);
        }
        #[cfg(windows)]
        assert_code(
            &HelperOp::DefenderExclude {
                path: r"C:\".into(),
            },
            "forbidden",
        );
    }

    #[test]
    fn vcredist_refusals() {
        let install = |path: PathBuf, sha256: &str| HelperOp::VcredistInstall {
            path,
            sha256: sha256.into(),
        };
        let good = root().join("tmp/downloads/vc_redist.x64.exe");
        for sha in ["", "abc", &"g".repeat(64), &"a".repeat(63), &"a".repeat(65)] {
            assert_code(&install(good.clone(), sha), "invalid_input");
        }
        let sha = "a".repeat(64);
        for (path, code) in [
            (root().join("tmp/vc_redist.x64.exe"), "forbidden"),
            (root().join("tmp/downloads"), "forbidden"),
            (root().join("tmp/downloads/vc_redist.x64.msi"), "forbidden"),
            (root().join("bin/vc_redist.x64.exe"), "forbidden"),
            (outside("vc_redist.x64.exe"), "forbidden"),
            (root().join("tmp/downloads/../x.exe"), "invalid_input"),
        ] {
            assert_code(&install(path, &sha), code);
        }
    }

    #[test]
    fn case_sensitive_refusals() {
        for (path, code) in [
            (root(), "forbidden"),
            (root().join("bin"), "forbidden"),
            (root().join("bin/php"), "forbidden"),
            (root().join("state"), "forbidden"),
            (root().join("state/registry"), "forbidden"),
            (outside("other"), "forbidden"),
            (outside(""), "forbidden"),
            (PathBuf::from("www"), "invalid_input"),
            (root().join("www/../bin"), "invalid_input"),
        ] {
            assert_code(&HelperOp::FsSetCaseSensitive { path }, code);
        }
    }

    #[test]
    fn policy_construction() {
        assert_eq!(
            HelperPolicy::new("relative", [".test"]).unwrap_err().code(),
            "invalid_input"
        );
        assert_eq!(
            HelperPolicy::new(root(), ["test"]).unwrap_err().code(),
            "invalid_input"
        );
        let p = HelperPolicy::new(root(), [".test", ".test"]).unwrap();
        assert_eq!(p.tlds(), [".test".to_owned()]);
        assert!(p.parked_dirs().is_empty());
        assert_eq!(p.root(), root());

        let forge_root = ForgeRoot::new(root()).unwrap();
        let mut config = ForgeConfig::default();
        config
            .parked_dirs
            .push(outside("clientes").display().to_string());
        config.dns.extra_tlds.push(".localhost".into());
        let p = HelperPolicy::from_config(&forge_root, &config).unwrap();
        assert_eq!(p.tlds(), [".test".to_owned(), ".localhost".to_owned()]);
        assert_eq!(p.parked_dirs(), [root().join("www"), outside("clientes")]);
    }

    #[test]
    fn wire_format_is_closed() {
        let op = HelperOp::NrptAdd {
            namespace: ".test".into(),
            nameserver: "127.0.0.1".parse().unwrap(),
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "name": "nrpt.add",
                "params": {"namespace": ".test", "nameserver": "127.0.0.1"}
            })
        );
        assert_eq!(serde_json::from_value::<HelperOp>(json).unwrap(), op);

        let entries = serde_json::json!({
            "name": "hosts.set_block",
            "params": {"entries": [{"ip": "::1", "host": "loja.test"}]}
        });
        let op: HelperOp = serde_json::from_value(entries).unwrap();
        assert_eq!(op.name(), "hosts.set_block");

        for bad in [
            serde_json::json!({"name": "shell.run", "params": {"command": "cmd"}}),
            serde_json::json!({"name": "NRPT.ADD", "params": {"namespace": ".test", "nameserver": "127.0.0.1"}}),
            serde_json::json!({"name": "nrpt.add", "params": {"namespace": ".test"}}),
            serde_json::json!({"name": "nrpt.add", "params": {"namespace": ".test", "nameserver": "127.0.0.1", "extra": 1}}),
            serde_json::json!({"name": "nrpt.add", "params": {"namespace": ".test", "nameserver": "localhost"}}),
            serde_json::json!({"name": "nrpt.remove", "params": {"namespace": ".test"}, "more": true}),
            serde_json::json!({"name": "firewall.add", "params": {"name": "Forgebox-x", "port": 70000, "exe": "x"}}),
            serde_json::json!({"name": "hosts.set_block", "params": {"entries": [{"ip": "127.0.0.1", "host": "a.test", "comment": "x"}]}}),
            serde_json::json!({"name": "nrpt.remove"}),
            serde_json::json!({"params": {"namespace": ".test"}}),
        ] {
            assert!(
                serde_json::from_value::<HelperOp>(bad.clone()).is_err(),
                "{bad} must not parse"
            );
        }
    }
}
