//! How the OS resolver reaches the Forgebox resolver (docs/06 §5, ADR-0004).
//!
//! * Reading needs no elevation: the NRPT rules (Windows: the registry keys behind
//!   `Get-DnsClientNrptRule`, local and Group Policy), the `hosts` file, and flushing the
//!   resolver cache.
//! * Changing goes through `forgebox-helper` ([`crate::helper::HelperClient`]):
//!   `nrpt.add`/`nrpt.remove` and `hosts.set_block`.
//!
//! All methods are **blocking** (registry, file and UAC): async code uses
//! `spawn_blocking` (ADR-0011). `forge_net::policy::DnsPolicyManager` is the async façade.

use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[cfg(not(windows))]
use crate::error::ForgeError;
use crate::error::{IoResultExt, Result};
use crate::helper::{HelperClient, HostsEntry};

/// Comment of the NRPT rules created by `forgebox-helper` (`nrpt.add`). Only local rules
/// whose comment starts with it are replaced or removed by the helper; anything else
/// (corporate, VPN, hand-made) is left alone.
pub const FORGEBOX_NRPT_COMMENT: &str = "Forgebox";

/// Where an NRPT rule is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NrptSource {
    /// Local table (`Add-DnsClientNrptRule` without `-GpoName`; what the helper writes).
    Local,
    /// Group Policy (corporate policy, DirectAccess, some VPN clients). While any rule of
    /// this kind exists, Windows ignores every local rule.
    GroupPolicy,
}

/// One rule of the Name Resolution Policy Table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NrptRule {
    /// Registry key of the rule (`{GUID}`).
    pub id: String,
    /// Namespaces, lowercase: `.test` (suffix) or `loja.test` (exact name).
    pub namespaces: Vec<String>,
    /// Name servers the namespaces are sent to (empty for rules that only carry other
    /// settings, such as DNSSEC or proxy options).
    pub nameservers: Vec<IpAddr>,
    pub source: NrptSource,
    /// `Comment` / `DisplayName` of the rule, when set.
    pub comment: Option<String>,
}

impl NrptRule {
    /// A local rule created by Forgebox (comment starting with [`FORGEBOX_NRPT_COMMENT`]).
    pub fn is_forgebox(&self) -> bool {
        self.source == NrptSource::Local
            && self
                .comment
                .as_deref()
                .is_some_and(|c| c.starts_with(FORGEBOX_NRPT_COMMENT))
    }

    /// Whether the rule applies to `name` (`loja.test`, no trailing dot, any case).
    pub fn covers(&self, name: &str) -> bool {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        self.namespaces.iter().any(|ns| match ns.strip_prefix('.') {
            Some(suffix) => crate::dns_name::is_within(&name, suffix),
            None => *ns == name,
        })
    }
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

/// The rules Windows actually uses: only the Group Policy ones when there is any.
pub fn effective_nrpt_rules(rules: &[NrptRule]) -> Vec<&NrptRule> {
    let gpo = rules.iter().any(|r| r.source == NrptSource::GroupPolicy);
    rules
        .iter()
        .filter(|r| !gpo || r.source == NrptSource::GroupPolicy)
        .collect()
}

/// OS resolver policy (NRPT on Windows, docs/06 §5).
pub trait DnsPolicy: Send + Sync + fmt::Debug {
    /// Every NRPT rule (local and Group Policy). No elevation.
    fn nrpt_rules(&self) -> Result<Vec<NrptRule>>;

    /// Current policy for `tld` (`.test`): the effective rule whose namespaces contain
    /// exactly `tld` and that names a server.
    fn status(&self, tld: &str) -> Result<DnsPolicyStatus> {
        let tld = tld.to_ascii_lowercase();
        let rules = self.nrpt_rules()?;
        Ok(effective_nrpt_rules(&rules)
            .into_iter()
            .find(|r| r.namespaces.contains(&tld) && !r.nameservers.is_empty())
            .map_or(DnsPolicyStatus::NotApplied, |r| DnsPolicyStatus::Applied {
                nameserver: r.nameservers[0],
            }))
    }

    /// Routes `tld` to `nameserver` (helper `nrpt.add`, one UAC prompt).
    fn apply(&self, tld: &str, nameserver: IpAddr) -> Result<()>;

    /// Removes the Forgebox rule of `tld` (helper `nrpt.remove`, one UAC prompt).
    fn remove(&self, tld: &str) -> Result<()>;

    /// The system `hosts` file.
    fn hosts_path(&self) -> PathBuf;

    /// Content of the `hosts` file (bytes that are not UTF-8 are replaced). No elevation.
    fn read_hosts(&self) -> Result<String> {
        let path = self.hosts_path();
        let bytes = std::fs::read(&path).at("reading", &path)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Rewrites the managed block of `hosts` (helper `hosts.set_block`, one UAC prompt);
    /// no entries removes the block.
    fn set_hosts_block(&self, entries: &[HostsEntry]) -> Result<()>;

    /// Empties the OS resolver cache (`ipconfig /flushdns`). No elevation.
    fn flush_cache(&self) -> Result<()>;
}

/// The policy of the running OS, changing it through `helper`.
#[cfg(windows)]
pub fn dns_policy(helper: HelperClient) -> Arc<dyn DnsPolicy> {
    Arc::new(super::windows::WindowsDnsPolicy::new(helper))
}

/// The policy of the running OS (stub until the Linux/macOS ports: systemd-resolved
/// routing domains, docs/13 §4).
#[cfg(not(windows))]
pub fn dns_policy(_helper: HelperClient) -> Arc<dyn DnsPolicy> {
    Arc::new(Unsupported)
}

#[cfg(not(windows))]
#[derive(Debug)]
struct Unsupported;

#[cfg(not(windows))]
impl DnsPolicy for Unsupported {
    fn nrpt_rules(&self) -> Result<Vec<NrptRule>> {
        Ok(Vec::new())
    }
    fn apply(&self, _: &str, _: IpAddr) -> Result<()> {
        Err(unsupported())
    }
    fn remove(&self, _: &str) -> Result<()> {
        Err(unsupported())
    }
    fn hosts_path(&self) -> PathBuf {
        PathBuf::from("/etc/hosts")
    }
    fn set_hosts_block(&self, _: &[HostsEntry]) -> Result<()> {
        Err(unsupported())
    }
    fn flush_cache(&self) -> Result<()> {
        Err(unsupported())
    }
}

#[cfg(not(windows))]
fn unsupported() -> ForgeError {
    ForgeError::UnsupportedPlatform(format!(
        "the resolver policy is not implemented on {}",
        std::env::consts::OS
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(source: NrptSource, ns: &[&str], servers: &[&str]) -> NrptRule {
        NrptRule {
            id: "{x}".into(),
            namespaces: ns.iter().map(|s| (*s).to_owned()).collect(),
            nameservers: servers.iter().map(|s| s.parse().unwrap()).collect(),
            source,
            comment: None,
        }
    }

    #[derive(Debug)]
    struct Rules(Vec<NrptRule>);

    impl DnsPolicy for Rules {
        fn nrpt_rules(&self) -> Result<Vec<NrptRule>> {
            Ok(self.0.clone())
        }
        fn apply(&self, _: &str, _: IpAddr) -> Result<()> {
            Ok(())
        }
        fn remove(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn hosts_path(&self) -> PathBuf {
            PathBuf::from("hosts")
        }
        fn set_hosts_block(&self, _: &[HostsEntry]) -> Result<()> {
            Ok(())
        }
        fn flush_cache(&self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn forgebox_rules_are_local_and_tagged() {
        let mut r = rule(NrptSource::Local, &[".test"], &["127.0.0.1"]);
        assert!(!r.is_forgebox(), "no comment");
        r.comment = Some("Forgebox .test".into());
        assert!(r.is_forgebox());
        r.comment = Some("forgebox".into());
        assert!(!r.is_forgebox(), "case-sensitive tag");
        r.comment = Some("Forgebox".into());
        r.source = NrptSource::GroupPolicy;
        assert!(!r.is_forgebox(), "Group Policy rules are never ours");
    }

    #[test]
    fn rules_cover_suffixes_and_exact_names() {
        let r = rule(NrptSource::Local, &[".test", "exact.lan"], &["127.0.0.1"]);
        assert!(r.covers("loja.test"));
        assert!(r.covers("LOJA.Test."));
        assert!(r.covers("test"));
        assert!(!r.covers("latest"));
        assert!(r.covers("exact.lan"));
        assert!(!r.covers("sub.exact.lan"));
    }

    #[test]
    fn status_uses_effective_rules() {
        let local = rule(NrptSource::Local, &[".test"], &["127.0.0.1"]);
        let policy = Rules(vec![local.clone()]);
        assert_eq!(
            policy.status(".test").unwrap(),
            DnsPolicyStatus::Applied {
                nameserver: "127.0.0.1".parse().unwrap()
            }
        );
        assert_eq!(
            policy.status(".TEST").unwrap(),
            policy.status(".test").unwrap()
        );
        assert_eq!(policy.status(".lan").unwrap(), DnsPolicyStatus::NotApplied);

        // A Group Policy rule (any namespace) hides the local ones.
        let corp = rule(NrptSource::GroupPolicy, &[".corp.example"], &["10.0.0.1"]);
        let policy = Rules(vec![local.clone(), corp]);
        assert_eq!(policy.status(".test").unwrap(), DnsPolicyStatus::NotApplied);
        assert_eq!(
            effective_nrpt_rules(&policy.0).len(),
            1,
            "only the GPO rule is effective"
        );

        // Rules without servers do not route.
        let policy = Rules(vec![rule(NrptSource::Local, &[".test"], &[])]);
        assert_eq!(policy.status(".test").unwrap(), DnsPolicyStatus::NotApplied);
    }

    #[test]
    fn hosts_is_read_lossily() {
        #[derive(Debug)]
        struct File(PathBuf);
        impl DnsPolicy for File {
            fn nrpt_rules(&self) -> Result<Vec<NrptRule>> {
                Ok(Vec::new())
            }
            fn apply(&self, _: &str, _: IpAddr) -> Result<()> {
                Ok(())
            }
            fn remove(&self, _: &str) -> Result<()> {
                Ok(())
            }
            fn hosts_path(&self) -> PathBuf {
                self.0.clone()
            }
            fn set_hosts_block(&self, _: &[HostsEntry]) -> Result<()> {
                Ok(())
            }
            fn flush_cache(&self) -> Result<()> {
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        std::fs::write(&path, b"127.0.0.1 a.test # caf\xe9\r\n").unwrap();
        let text = File(path).read_hosts().unwrap();
        assert!(text.starts_with("127.0.0.1 a.test # caf"));
        let missing = File(dir.path().join("nope")).read_hosts().unwrap_err();
        assert_eq!(missing.code(), "not_found");
    }
}
