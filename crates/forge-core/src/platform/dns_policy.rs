//! How the OS resolver reaches the Forgebox resolver (docs/06 §5, ADR-0004).
//!
//! * Reading needs no elevation: the NRPT table (Windows: the registry keys behind
//!   `Get-DnsClientNrptRule`, local and Group Policy, and whether the Group Policy table
//!   exists at all), the `hosts` file, asking the OS resolver ([`DnsPolicy::query_system`])
//!   and flushing the resolver cache.
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
    /// Group Policy (corporate policy, DirectAccess, some VPN clients). While the Group
    /// Policy table exists (even empty, see [`NrptTable::group_policy_table`]), Windows
    /// ignores every local rule.
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

/// The whole Name Resolution Policy Table as Windows sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NrptTable {
    /// Every rule, local and Group Policy.
    pub rules: Vec<NrptRule>,
    /// The Group Policy table exists (Windows:
    /// `HKLM\SOFTWARE\Policies\Microsoft\Windows NT\DNSClient\DnsPolicyConfig`), **even
    /// with no rule in it**: a corporate GPO that once touched the NRPT leaves the key
    /// behind, and while it exists the DNS client ignores the whole local table
    /// (`Get-DnsClientNrptPolicy` comes back empty). Found on the M1 gate machine
    /// (ADR-0033).
    pub group_policy_table: bool,
}

impl NrptTable {
    /// Local rules only, no Group Policy table.
    pub fn local(rules: Vec<NrptRule>) -> Self {
        Self {
            rules,
            group_policy_table: false,
        }
    }

    /// Number of Group Policy rules.
    pub fn group_policy_rules(&self) -> usize {
        self.rules
            .iter()
            .filter(|r| r.source == NrptSource::GroupPolicy)
            .count()
    }

    /// Why Windows ignores the local rules, if it does.
    pub fn local_rules_ignored(&self) -> Option<NrptIneffective> {
        (self.group_policy_table || self.group_policy_rules() > 0)
            .then_some(NrptIneffective::GroupPolicyTablePresent)
    }

    /// The rules Windows actually uses: only the Group Policy ones while the Group Policy
    /// table exists.
    pub fn effective_rules(&self) -> Vec<&NrptRule> {
        let gpo = self.local_rules_ignored().is_some();
        self.rules
            .iter()
            .filter(|r| !gpo || r.source == NrptSource::GroupPolicy)
            .collect()
    }
}

/// Why a local NRPT rule present in the registry routes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NrptIneffective {
    /// The Group Policy NRPT table exists (with or without rules): Windows ignores every
    /// local rule.
    GroupPolicyTablePresent,
}

impl NrptIneffective {
    /// Wire value (`group_policy_table_present`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GroupPolicyTablePresent => "group_policy_table_present",
        }
    }
}

impl fmt::Display for NrptIneffective {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// State of the OS resolver policy for a TLD, as written in the registry. Whether the
/// resolver really follows it is checked by asking it ([`DnsPolicy::query_system`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsPolicyStatus {
    /// An effective rule sends queries for the TLD to `nameserver`.
    Applied {
        nameserver: IpAddr,
    },
    /// A local rule sends the TLD to `nameserver`, but Windows ignores it.
    Ignored {
        nameserver: IpAddr,
        reason: NrptIneffective,
    },
    NotApplied,
}

/// What `nrpt.add` reported (helper details `effective` / `reason`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NrptApplied {
    /// Whether Windows follows the local rule; `None` when the helper did not say.
    pub effective: Option<bool>,
    pub reason: Option<NrptIneffective>,
}

impl NrptApplied {
    /// From the helper's `details` (`{"effective": false, "reason": "..."}`).
    pub fn from_details(details: &serde_json::Value) -> Self {
        Self {
            effective: details
                .get("effective")
                .and_then(serde_json::Value::as_bool),
            reason: details
                .get("reason")
                .and_then(|r| serde_json::from_value(r.clone()).ok()),
        }
    }
}

/// The rules Windows actually uses ([`NrptTable::effective_rules`]).
pub fn effective_nrpt_rules(table: &NrptTable) -> Vec<&NrptRule> {
    table.effective_rules()
}

/// [`DnsPolicy::status`] over an already read table.
pub fn policy_status(table: &NrptTable, tld: &str) -> DnsPolicyStatus {
    let tld = tld.to_ascii_lowercase();
    let routes = |r: &&NrptRule| r.namespaces.contains(&tld) && !r.nameservers.is_empty();
    if let Some(rule) = table.effective_rules().into_iter().find(routes) {
        return DnsPolicyStatus::Applied {
            nameserver: rule.nameservers[0],
        };
    }
    let local = table
        .rules
        .iter()
        .filter(|r| r.source == NrptSource::Local)
        .find(routes);
    match (table.local_rules_ignored(), local) {
        (Some(reason), Some(rule)) => DnsPolicyStatus::Ignored {
            nameserver: rule.nameservers[0],
            reason,
        },
        _ => DnsPolicyStatus::NotApplied,
    }
}

/// OS resolver policy (NRPT on Windows, docs/06 §5).
pub trait DnsPolicy: Send + Sync + fmt::Debug {
    /// The NRPT table: every rule (local and Group Policy) and whether the Group Policy
    /// table exists. No elevation.
    fn nrpt_table(&self) -> Result<NrptTable>;

    /// Current policy for `tld` (`.test`) from the registry ([`policy_status`]).
    fn status(&self, tld: &str) -> Result<DnsPolicyStatus> {
        Ok(policy_status(&self.nrpt_table()?, tld))
    }

    /// IPv4 addresses of `host` through the OS resolver, the way applications ask it
    /// (Windows: `DnsQuery_W` through the DNS Client service, NRPT applied). Verifies that
    /// a rule is really followed (ADR-0033), asked with a random name under the TLD that
    /// no cache or `hosts` line can answer. No elevation; the OS has its own timeouts,
    /// callers add theirs.
    fn query_system(&self, host: &str) -> Result<Vec<IpAddr>>;

    /// Routes `tld` to `nameserver` (helper `nrpt.add`, one UAC prompt).
    fn apply(&self, tld: &str, nameserver: IpAddr) -> Result<NrptApplied>;

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
    fn nrpt_table(&self) -> Result<NrptTable> {
        Ok(NrptTable::default())
    }
    fn query_system(&self, _: &str) -> Result<Vec<IpAddr>> {
        Err(unsupported())
    }
    fn apply(&self, _: &str, _: IpAddr) -> Result<NrptApplied> {
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
    struct Rules(NrptTable);

    impl Rules {
        fn local(rules: Vec<NrptRule>) -> Self {
            Self(NrptTable::local(rules))
        }
    }

    impl DnsPolicy for Rules {
        fn nrpt_table(&self) -> Result<NrptTable> {
            Ok(self.0.clone())
        }
        fn query_system(&self, _: &str) -> Result<Vec<IpAddr>> {
            Ok(Vec::new())
        }
        fn apply(&self, _: &str, _: IpAddr) -> Result<NrptApplied> {
            Ok(NrptApplied::default())
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
        let policy = Rules::local(vec![local.clone()]);
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
        let policy = Rules::local(vec![local.clone(), corp]);
        assert_eq!(
            policy.status(".test").unwrap(),
            DnsPolicyStatus::Ignored {
                nameserver: "127.0.0.1".parse().unwrap(),
                reason: NrptIneffective::GroupPolicyTablePresent
            }
        );
        assert_eq!(
            effective_nrpt_rules(&policy.0).len(),
            1,
            "only the GPO rule is effective"
        );

        // Rules without servers do not route.
        let policy = Rules::local(vec![rule(NrptSource::Local, &[".test"], &[])]);
        assert_eq!(policy.status(".test").unwrap(), DnsPolicyStatus::NotApplied);
    }

    #[test]
    fn an_empty_group_policy_table_hides_the_local_rules() {
        // The M1 gate machine: a corporate GPO left the policy key behind with no rule
        // in it; Windows ignored the Forgebox rule (ADR-0033).
        let local = rule(NrptSource::Local, &[".test"], &["127.0.0.1"]);
        let table = NrptTable {
            rules: vec![local],
            group_policy_table: true,
        };
        assert_eq!(table.group_policy_rules(), 0);
        assert_eq!(
            table.local_rules_ignored(),
            Some(NrptIneffective::GroupPolicyTablePresent)
        );
        assert!(table.effective_rules().is_empty());
        let status = Rules(table).status(".test").unwrap();
        assert_eq!(
            status,
            DnsPolicyStatus::Ignored {
                nameserver: "127.0.0.1".parse().unwrap(),
                reason: NrptIneffective::GroupPolicyTablePresent
            }
        );
        assert_eq!(
            NrptIneffective::GroupPolicyTablePresent.to_string(),
            "group_policy_table_present"
        );
        assert_eq!(
            serde_json::to_value(NrptIneffective::GroupPolicyTablePresent).unwrap(),
            "group_policy_table_present"
        );
        assert_eq!(
            NrptApplied::from_details(&serde_json::json!({
                "effective": false,
                "reason": "group_policy_table_present"
            })),
            NrptApplied {
                effective: Some(false),
                reason: Some(NrptIneffective::GroupPolicyTablePresent)
            }
        );
        assert_eq!(
            NrptApplied::from_details(&serde_json::Value::Null),
            NrptApplied::default()
        );

        // Without a local rule there is nothing to ignore.
        let empty = NrptTable {
            rules: Vec::new(),
            group_policy_table: true,
        };
        assert_eq!(policy_status(&empty, ".test"), DnsPolicyStatus::NotApplied);

        // A Group Policy rule for the TLD itself routes it (wherever it points).
        let corp = NrptTable {
            rules: vec![rule(NrptSource::GroupPolicy, &[".test"], &["10.0.0.1"])],
            group_policy_table: true,
        };
        assert_eq!(
            policy_status(&corp, ".test"),
            DnsPolicyStatus::Applied {
                nameserver: "10.0.0.1".parse().unwrap()
            }
        );
    }

    #[test]
    fn hosts_is_read_lossily() {
        #[derive(Debug)]
        struct File(PathBuf);
        impl DnsPolicy for File {
            fn nrpt_table(&self) -> Result<NrptTable> {
                Ok(NrptTable::default())
            }
            fn query_system(&self, _: &str) -> Result<Vec<IpAddr>> {
                Ok(Vec::new())
            }
            fn apply(&self, _: &str, _: IpAddr) -> Result<NrptApplied> {
                Ok(NrptApplied::default())
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
