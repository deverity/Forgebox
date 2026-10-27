//! How Windows reaches the Forgebox resolver (docs/06 §5.1, ADR-0004): the async façade
//! over [`forge_core::platform::DnsPolicy`].
//!
//! Order of preference:
//! 1. **NRPT** (`nrpt.add` through the helper, one UAC prompt per TLD): `.test` goes to
//!    the resolver wherever it listens on port 53 (`127.0.0.1` or `127.0.0.53`).
//! 2. **`hosts` fallback** (`hosts.set_block`): when the resolver could not get port 53,
//!    when the OS has no NRPT, or when Group Policy rules make local NRPT rules
//!    ineffective. One line per host and alias, `127.0.0.1` and `::1`, no wildcard.
//! 3. Nothing: the diagnosis says why.
//!
//! Every change is followed by a resolver cache flush and published as
//! `dns.policy_applied {v, tld, policy}`; every helper call as `helper.op {v, name, ok,
//! error}` (same shapes as `forge_ipc::api::events::{DnsPolicyPayload, HelperOpPayload}`).
//!
//! Blocking calls run in `spawn_blocking` with timeouts: reads within
//! [`PolicyTimeouts::read`], helper calls within [`PolicyTimeouts::helper`] (the UAC prompt
//! waits for a human). Long operations are meant to run inside a job of the daemon
//! (`net.dns_enable` is a job, docs/09).

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use forge_core::dns_name;
use forge_core::error::{ForgeError, Result};
use forge_core::event::{EventBus, EventKind};
use forge_core::helper::HostsEntry;
use forge_core::helper::hosts::read_hosts_block;
use forge_core::platform::{DnsPolicy, DnsPolicyStatus, NrptSource};
use forge_core::runtime::spawn_blocking_timeout;
use serde::{Deserialize, Serialize};

/// Version of the `dns.policy_applied` and `helper.op` payloads.
pub const POLICY_EVENT_VERSION: u32 = 1;

/// How a TLD reaches the resolver (wire values of `forge_ipc` `DnsPolicyState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyState {
    /// An effective NRPT rule sends the TLD to the resolver.
    Applied,
    /// The sites are in the `hosts` managed block.
    HostsFallback,
    NotApplied,
}

/// State of one TLD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TldPolicy {
    /// `.test`.
    pub tld: String,
    pub state: PolicyState,
    /// Where the NRPT rule points, when there is one.
    pub nameserver: Option<IpAddr>,
}

/// What [`DnsPolicyManager::enable`] should set up.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnableRequest {
    /// `[".test"]`.
    pub tlds: Vec<String>,
    /// Address the resolver listens on **when it is port 53** (`DnsStats::nrpt_capable`);
    /// `None` forces the `hosts` fallback.
    pub nameserver: Option<IpAddr>,
    /// Sites and aliases for the `hosts` fallback (`loja.test`, `api.loja.test`).
    pub hosts: Vec<String>,
}

/// Timeouts of the blocking calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyTimeouts {
    /// Registry and `hosts` reads, cache flush.
    pub read: Duration,
    /// A helper run, UAC prompt included.
    pub helper: Duration,
}

impl Default for PolicyTimeouts {
    fn default() -> Self {
        Self {
            read: Duration::from_secs(5),
            helper: Duration::from_secs(15 * 60),
        }
    }
}

#[derive(Serialize)]
struct DnsPolicyApplied<'a> {
    v: u32,
    tld: &'a str,
    policy: PolicyState,
}

#[derive(Serialize)]
struct HelperOpEvent<'a> {
    v: u32,
    name: &'a str,
    ok: bool,
    error: Option<String>,
}

/// Async façade over the OS resolver policy. Cheap to clone.
#[derive(Debug, Clone)]
pub struct DnsPolicyManager {
    policy: Arc<dyn DnsPolicy>,
    events: Option<EventBus>,
    timeouts: PolicyTimeouts,
}

/// The `hosts` entries for `hosts` under `tlds`: `127.0.0.1` and `::1` for each valid
/// name below a TLD; the rest is skipped with a warning (the helper would refuse the
/// whole block).
pub fn hosts_entries(hosts: &[String], tlds: &[String]) -> Vec<HostsEntry> {
    let suffixes: Vec<&str> = tlds
        .iter()
        .filter_map(|t| dns_name::parse_suffix(t).ok())
        .collect();
    let mut names = BTreeSet::new();
    for host in hosts {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        let valid = dns_name::check_name(&host).is_ok()
            && suffixes
                .iter()
                .any(|s| host != *s && dns_name::is_within(&host, s));
        if valid {
            names.insert(host);
        } else {
            tracing::warn!(%host, "dns: host left out of the hosts block (not below a TLD)");
        }
    }
    names
        .into_iter()
        .flat_map(|host| {
            [
                HostsEntry::new(IpAddr::V4(Ipv4Addr::LOCALHOST), host.clone()),
                HostsEntry::new(IpAddr::V6(Ipv6Addr::LOCALHOST), host),
            ]
        })
        .collect()
}

impl DnsPolicyManager {
    pub fn new(policy: Arc<dyn DnsPolicy>) -> Self {
        Self {
            policy,
            events: None,
            timeouts: PolicyTimeouts::default(),
        }
    }

    /// Publishes `dns.policy_applied` and `helper.op` on `bus`.
    pub fn with_events(mut self, bus: EventBus) -> Self {
        self.events = Some(bus);
        self
    }

    pub fn with_timeouts(mut self, timeouts: PolicyTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    async fn read<T: Send + 'static>(
        &self,
        what: &str,
        f: impl FnOnce(&dyn DnsPolicy) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let policy = Arc::clone(&self.policy);
        spawn_blocking_timeout(what, self.timeouts.read, move || f(policy.as_ref())).await
    }

    /// Runs one helper operation and publishes `helper.op`.
    async fn helper(
        &self,
        op: &'static str,
        f: impl FnOnce(&dyn DnsPolicy) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        let policy = Arc::clone(&self.policy);
        let result =
            spawn_blocking_timeout(op, self.timeouts.helper, move || f(policy.as_ref())).await;
        if let Some(bus) = &self.events {
            let payload = HelperOpEvent {
                v: POLICY_EVENT_VERSION,
                name: op,
                ok: result.is_ok(),
                error: result.as_ref().err().map(ToString::to_string),
            };
            bus.publish(
                EventKind::HelperOp,
                serde_json::to_value(payload).unwrap_or_default(),
            );
        }
        result
    }

    fn publish_state(&self, tld: &str, policy: PolicyState) {
        if let Some(bus) = &self.events {
            let payload = DnsPolicyApplied {
                v: POLICY_EVENT_VERSION,
                tld,
                policy,
            };
            bus.publish(
                EventKind::DnsPolicyApplied,
                serde_json::to_value(payload).unwrap_or_default(),
            );
        }
    }

    async fn current_block(&self) -> Result<Option<Vec<HostsEntry>>> {
        let text = self.read("reading hosts", |p| p.read_hosts()).await?;
        Ok(read_hosts_block(&text)?.map(|b| b.entries))
    }

    /// State of each TLD. `nameserver` is where the resolver listens now: a rule
    /// pointing elsewhere does not count as applied.
    pub async fn status(
        &self,
        tlds: &[String],
        nameserver: Option<IpAddr>,
    ) -> Result<Vec<TldPolicy>> {
        let block = self.current_block().await.unwrap_or_else(|err| {
            tracing::warn!(%err, "dns: cannot read hosts");
            None
        });
        let mut out = Vec::new();
        for tld in tlds {
            let t = tld.clone();
            let status = self
                .read("reading NRPT rules", move |p| p.status(&t))
                .await?;
            let rule_ns = match status {
                DnsPolicyStatus::Applied { nameserver } => Some(nameserver),
                DnsPolicyStatus::NotApplied => None,
            };
            let applied = rule_ns.is_some_and(|ns| nameserver.is_none_or(|want| want == ns));
            let suffix = tld.trim_start_matches('.');
            let in_hosts = block.as_ref().is_some_and(|entries| {
                entries.iter().any(|e| dns_name::is_within(&e.host, suffix))
            });
            let state = if applied {
                PolicyState::Applied
            } else if in_hosts {
                PolicyState::HostsFallback
            } else {
                PolicyState::NotApplied
            };
            out.push(TldPolicy {
                tld: tld.clone(),
                state,
                nameserver: rule_ns,
            });
        }
        Ok(out)
    }

    /// Routes the TLDs to the resolver (`forge dns enable`): NRPT when possible, `hosts`
    /// otherwise; then flushes the cache. Already correct TLDs cost no prompt.
    ///
    /// Errors: `needs_elevation` when the user declines a prompt (nothing else is tried:
    /// the fallback needs the same consent), the helper's refusals, timeouts.
    pub async fn enable(&self, req: EnableRequest) -> Result<Vec<TldPolicy>> {
        let mut states: Vec<TldPolicy> = Vec::new();
        let mut needs_hosts = Vec::new();
        for tld in &req.tlds {
            let Some(nameserver) = req.nameserver else {
                needs_hosts.push(tld.clone());
                continue;
            };
            let t = tld.clone();
            let current = self
                .read("reading NRPT rules", move |p| p.status(&t))
                .await?;
            if current == (DnsPolicyStatus::Applied { nameserver }) {
                states.push(TldPolicy {
                    tld: tld.clone(),
                    state: PolicyState::Applied,
                    nameserver: Some(nameserver),
                });
                continue;
            }
            let t = tld.clone();
            match self
                .helper("nrpt.add", move |p| p.apply(&t, nameserver))
                .await
            {
                Ok(()) => {}
                Err(err) if err.code() == "unsupported_platform" => {
                    needs_hosts.push(tld.clone());
                    continue;
                }
                Err(err) => return Err(err),
            }
            let t = tld.clone();
            let after = self
                .read("reading NRPT rules", move |p| p.status(&t))
                .await?;
            if after == (DnsPolicyStatus::Applied { nameserver }) {
                states.push(TldPolicy {
                    tld: tld.clone(),
                    state: PolicyState::Applied,
                    nameserver: Some(nameserver),
                });
            } else {
                // Group Policy rules hide the local one: only `hosts` works here.
                tracing::warn!(%tld, ?after, "dns: NRPT rule added but not effective; using hosts");
                needs_hosts.push(tld.clone());
            }
        }
        if !needs_hosts.is_empty() {
            let entries = hosts_entries(&req.hosts, &needs_hosts);
            self.write_block(entries).await?;
            for tld in needs_hosts {
                states.push(TldPolicy {
                    tld,
                    state: PolicyState::HostsFallback,
                    nameserver: None,
                });
            }
        }
        self.flush().await;
        for state in &states {
            self.publish_state(&state.tld, state.state);
        }
        Ok(states)
    }

    /// Removes the routing (`forge dns disable`): the Forgebox NRPT rules of the TLDs and
    /// the `hosts` block, then flushes the cache.
    pub async fn disable(&self, tlds: &[String]) -> Result<()> {
        for tld in tlds {
            let t = tld.clone();
            let rules = self.read("reading NRPT rules", |p| p.nrpt_rules()).await?;
            let present = rules.iter().any(|r| {
                r.source == NrptSource::Local && r.namespaces.contains(&t.to_ascii_lowercase())
            });
            if present {
                let t = tld.clone();
                self.helper("nrpt.remove", move |p| p.remove(&t)).await?;
            }
        }
        if self.current_block().await?.is_some() {
            self.helper("hosts.set_block", |p| p.set_hosts_block(&[]))
                .await?;
        }
        self.flush().await;
        for tld in tlds {
            self.publish_state(tld, PolicyState::NotApplied);
        }
        Ok(())
    }

    /// Keeps the `hosts` block in step with the sites while the fallback is in use.
    /// Writes (one prompt) only when the entries change; returns whether it wrote.
    pub async fn sync_hosts(&self, hosts: &[String], tlds: &[String]) -> Result<bool> {
        let wanted = hosts_entries(hosts, tlds);
        let current = self.current_block().await?.unwrap_or_default();
        let as_set = |v: &[HostsEntry]| v.iter().cloned().collect::<BTreeSet<_>>();
        if as_set(&wanted) == as_set(&current) {
            return Ok(false);
        }
        self.write_block(wanted).await?;
        self.flush().await;
        Ok(true)
    }

    async fn write_block(&self, entries: Vec<HostsEntry>) -> Result<()> {
        if entries.is_empty() {
            return Err(ForgeError::InvalidInput(
                "no site below the TLDs to write to hosts".to_owned(),
            ));
        }
        self.helper("hosts.set_block", move |p| p.set_hosts_block(&entries))
            .await
    }

    /// Flushes the OS resolver cache; a failure is only logged.
    pub async fn flush(&self) {
        if let Err(err) = self
            .read("flushing the DNS cache", |p| p.flush_cache())
            .await
        {
            tracing::warn!(%err, "dns: could not flush the resolver cache");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_entries_cover_both_families_and_skip_strangers() {
        let entries = hosts_entries(
            &[
                "Loja.test".into(),
                "api.loja.test.".into(),
                "loja.test".into(),
                "test".into(),
                "google.com".into(),
                "bad_name.test".into(),
            ],
            &[".test".into()],
        );
        let lines: Vec<String> = entries
            .iter()
            .map(|e| format!("{} {}", e.ip, e.host))
            .collect();
        assert_eq!(
            lines,
            vec![
                "127.0.0.1 api.loja.test",
                "::1 api.loja.test",
                "127.0.0.1 loja.test",
                "::1 loja.test",
            ]
        );
    }
}
