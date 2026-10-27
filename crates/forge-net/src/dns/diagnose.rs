//! Why does (or does not) `loja.test` resolve? (docs/06 §5.2)
//!
//! [`DnsDiagnostics::diagnose`] gathers, concurrently and each with a timeout:
//! * what the OS resolver answers ([`SystemResolver`], `getaddrinfo`: hosts file, NRPT,
//!   cache, VPN rules — what browsers see);
//! * what the Forgebox resolver answers when asked directly ([`super::client`]);
//! * the NRPT rules (local and Group Policy), the `hosts` file ([`DnsPolicy`]);
//! * who holds port 53 (UDP and TCP, [`PortTable`]) and which adapters are up
//!   ([`NetAdapters`]);
//!
//! then turns them into [`DnsHint`]s (VPN, Docker, Acrylic, Group Policy rules, `hosts`
//! overrides...). Reading never needs elevation; a source that cannot be read is listed
//! in [`DnsDiagnosis::read_errors`] and the rest of the diagnosis still comes out.

use std::collections::BTreeSet;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use forge_core::dns_name;
use forge_core::error::{ForgeError, Result};
use forge_core::helper::hosts::{lookup_hosts, read_hosts_block};
use forge_core::platform::{
    DnsPolicy, NetAdapter, NetAdapters, NrptRule, NrptSource, PortOwner, PortTable,
    effective_nrpt_rules,
};
use forge_core::runtime::spawn_blocking_timeout;
use serde::Serialize;

use super::client;
use super::zones::{ZoneKind, ZoneSet};
use crate::messages;

/// Default budget of each probe of a diagnosis.
pub const DIAGNOSE_TIMEOUT: Duration = Duration::from_secs(3);

/// The OS resolver, as applications use it.
#[async_trait::async_trait]
pub trait SystemResolver: Send + Sync + fmt::Debug {
    /// Addresses of `host` through the OS resolver.
    async fn lookup(&self, host: &str) -> Result<Vec<IpAddr>>;
}

/// `getaddrinfo` through tokio (hosts file, NRPT and cache included).
#[derive(Debug, Default, Clone, Copy)]
pub struct OsResolver;

#[async_trait::async_trait]
impl SystemResolver for OsResolver {
    async fn lookup(&self, host: &str) -> Result<Vec<IpAddr>> {
        let addrs = tokio::net::lookup_host((host, 0))
            .await
            .map_err(|e| ForgeError::io(format!("resolving {host}"), e))?;
        let mut out: Vec<IpAddr> = Vec::new();
        for addr in addrs {
            if !out.contains(&addr.ip()) {
                out.push(addr.ip());
            }
        }
        Ok(out)
    }
}

/// Stable code of a hint (key of the UI text, [`messages::hint`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DnsHintCode {
    NotInZone,
    ServerNotRunning,
    ServerNotOnPort53,
    ServerNotAnswering,
    NrptMissing,
    NrptPointsElsewhere,
    GroupPolicyNrpt,
    HostsOverride,
    Port53Acrylic,
    Port53Docker,
    Port53DnsServer,
    Port53Vpn,
    Port53SystemService,
    Port53Other,
    VpnAdapter,
    SystemMismatch,
    SystemUnresolved,
}

impl DnsHintCode {
    pub const ALL: &'static [Self] = &[
        Self::NotInZone,
        Self::ServerNotRunning,
        Self::ServerNotOnPort53,
        Self::ServerNotAnswering,
        Self::NrptMissing,
        Self::NrptPointsElsewhere,
        Self::GroupPolicyNrpt,
        Self::HostsOverride,
        Self::Port53Acrylic,
        Self::Port53Docker,
        Self::Port53DnsServer,
        Self::Port53Vpn,
        Self::Port53SystemService,
        Self::Port53Other,
        Self::VpnAdapter,
        Self::SystemMismatch,
        Self::SystemUnresolved,
    ];
}

/// How much a hint matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// One finding of a diagnosis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DnsHint {
    pub code: DnsHintCode,
    pub severity: Severity,
    /// What it is about: process, adapter, rule, addresses.
    pub detail: String,
    /// Explanation for the user (pt-BR until i18n).
    pub message: String,
}

impl DnsHint {
    fn new(code: DnsHintCode, severity: Severity, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        Self {
            code,
            severity,
            message: messages::hint(code, &detail),
            detail,
        }
    }
}

/// Everything known about the resolution of one host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DnsDiagnosis {
    pub host: String,
    /// What the host should resolve to (empty outside the zones).
    pub expected: Vec<IpAddr>,
    /// What the OS resolver returns.
    pub system_resolves_to: Vec<IpAddr>,
    pub system_error: Option<String>,
    /// Where the Forgebox resolver listens, if running.
    pub server: Option<SocketAddr>,
    /// What the Forgebox resolver returns when asked directly.
    pub our_server_resolves_to: Vec<IpAddr>,
    pub our_server_error: Option<String>,
    /// An effective NRPT rule covers the host.
    pub nrpt_present: bool,
    /// Every NRPT rule covering the host (effective or not).
    pub nrpt_rules: Vec<NrptRule>,
    /// Group Policy NRPT rules on the machine (they disable every local rule).
    pub group_policy_rules: usize,
    /// The Forgebox block exists in `hosts`.
    pub hosts_block_present: bool,
    /// What `hosts` maps the host to, anywhere in the file.
    pub hosts_entries: Vec<IpAddr>,
    /// Sockets on port 53 (UDP and TCP) of loopback or wildcard addresses.
    pub port53_owner: Vec<PortOwner>,
    /// Adapters up that look like VPNs.
    pub vpn_adapters: Vec<String>,
    pub hints: Vec<DnsHint>,
    /// The OS resolves the host and only to the expected addresses.
    pub ok: bool,
    /// Sources that could not be read (`nrpt: ...`, `hosts: ...`).
    pub read_errors: Vec<String>,
}

/// Lowercase substrings of adapter names/descriptions of VPN clients.
const VPN_ADAPTER_PATTERNS: &[&str] = &[
    "vpn",
    "tap-windows",
    "wireguard",
    "wintun",
    "anyconnect",
    "fortinet",
    "fortissl",
    "pangp",
    "globalprotect",
    "nordlynx",
    "openvpn",
    "juniper",
    "pulse secure",
    "zscaler",
    "cloudflare warp",
];

/// Lowercase executable name patterns of processes seen on port 53.
const PORT53_PATTERNS: &[(&str, DnsHintCode)] = &[
    ("acrylic", DnsHintCode::Port53Acrylic),
    ("com.docker", DnsHintCode::Port53Docker),
    ("docker", DnsHintCode::Port53Docker),
    ("vpnkit", DnsHintCode::Port53Docker),
    ("dnscrypt-proxy", DnsHintCode::Port53DnsServer),
    ("dnsmasq", DnsHintCode::Port53DnsServer),
    ("unbound", DnsHintCode::Port53DnsServer),
    ("named", DnsHintCode::Port53DnsServer),
    ("coredns", DnsHintCode::Port53DnsServer),
    ("technitium", DnsHintCode::Port53DnsServer),
    ("adguardhome", DnsHintCode::Port53DnsServer),
    ("dnsproxy", DnsHintCode::Port53DnsServer),
    ("openvpn", DnsHintCode::Port53Vpn),
    ("nordvpn", DnsHintCode::Port53Vpn),
    ("expressvpn", DnsHintCode::Port53Vpn),
    ("protonvpn", DnsHintCode::Port53Vpn),
    ("vpnagent", DnsHintCode::Port53Vpn),
    ("pangps", DnsHintCode::Port53Vpn),
    ("forti", DnsHintCode::Port53Vpn),
    ("wireguard", DnsHintCode::Port53Vpn),
    ("warp-svc", DnsHintCode::Port53Vpn),
    ("zsatunnel", DnsHintCode::Port53Vpn),
    ("mullvad", DnsHintCode::Port53Vpn),
    ("svchost", DnsHintCode::Port53SystemService),
];

/// Classifies a process holding port 53 by its executable name. A process the user
/// cannot even open (no executable path: the kernel, protected services, `svchost.exe`
/// running as SYSTEM or LocalService) is a system service.
pub fn classify_port53(owner: &PortOwner) -> DnsHintCode {
    let Some(name) = owner.exe_name() else {
        return DnsHintCode::Port53SystemService;
    };
    let name = name.to_ascii_lowercase();
    PORT53_PATTERNS
        .iter()
        .find(|(pattern, _)| name.contains(pattern))
        .map_or(DnsHintCode::Port53Other, |(_, code)| *code)
}

/// Whether an adapter that is up looks like a VPN.
pub fn is_vpn_adapter(adapter: &NetAdapter) -> bool {
    let text = format!("{} {}", adapter.name, adapter.description).to_ascii_lowercase();
    adapter.up && VPN_ADAPTER_PATTERNS.iter().any(|p| text.contains(p))
}

fn owner_label(owner: &PortOwner) -> String {
    let exe = owner
        .exe_name()
        .unwrap_or_else(|| "processo do sistema".to_owned());
    format!("{exe} (PID {}) em {}", owner.pid, owner.addr)
}

fn join(ips: &[IpAddr]) -> String {
    ips.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The local zone (TLD) holding `host`, as an NRPT namespace (`.test`).
fn tld_of(zones: &ZoneSet, host: &str) -> Option<String> {
    zones
        .zones()
        .iter()
        .filter(|z| z.kind == ZoneKind::Local)
        .find(|z| dns_name::is_within(host, &z.name))
        .map(|z| format!(".{}", z.name))
}

/// Turns the facts of a diagnosis into hints (pure).
pub fn hints(d: &DnsDiagnosis, tld: Option<&str>) -> Vec<DnsHint> {
    use DnsHintCode as C;
    let mut out = Vec::new();
    if d.expected.is_empty() {
        out.push(DnsHint::new(C::NotInZone, Severity::Error, &d.host));
        return out;
    }
    match d.server {
        None => out.push(DnsHint::new(C::ServerNotRunning, Severity::Error, "")),
        Some(addr) => {
            if addr.port() != 53 {
                out.push(DnsHint::new(
                    C::ServerNotOnPort53,
                    Severity::Warning,
                    addr.to_string(),
                ));
            }
            let expected: BTreeSet<_> = d.expected.iter().collect();
            let ours: BTreeSet<_> = d.our_server_resolves_to.iter().collect();
            if let Some(err) = &d.our_server_error {
                out.push(DnsHint::new(C::ServerNotAnswering, Severity::Error, err));
            } else if ours != expected {
                out.push(DnsHint::new(
                    C::ServerNotAnswering,
                    Severity::Error,
                    format!("respondeu [{}]", join(&d.our_server_resolves_to)),
                ));
            }
        }
    }
    let tld = tld.unwrap_or_default();
    let effective: Vec<&NrptRule> = d
        .nrpt_rules
        .iter()
        .filter(|r| d.group_policy_rules == 0 || r.source == NrptSource::GroupPolicy)
        .collect();
    if let (Some(rule), Some(server)) = (effective.first(), d.server) {
        if !rule.nameservers.contains(&server.ip()) {
            out.push(DnsHint::new(
                C::NrptPointsElsewhere,
                Severity::Error,
                format!("{tld} aponta para [{}]", join(&rule.nameservers)),
            ));
        }
    }
    if d.group_policy_rules > 0 && effective.is_empty() {
        out.push(DnsHint::new(
            C::GroupPolicyNrpt,
            if d.ok {
                Severity::Info
            } else {
                Severity::Warning
            },
            format!("{} regra(s)", d.group_policy_rules),
        ));
    }
    let overrides: Vec<IpAddr> = d
        .hosts_entries
        .iter()
        .filter(|ip| !d.expected.contains(ip))
        .copied()
        .collect();
    if !overrides.is_empty() {
        out.push(DnsHint::new(
            C::HostsOverride,
            Severity::Error,
            join(&overrides),
        ));
    }
    if !d.ok && !d.nrpt_present && d.hosts_entries.is_empty() {
        out.push(DnsHint::new(C::NrptMissing, Severity::Error, tld));
    }
    let own_pid = std::process::id();
    let mut seen = BTreeSet::new();
    for owner in d.port53_owner.iter().filter(|o| o.pid != own_pid) {
        let code = classify_port53(owner);
        if !seen.insert((code, owner.pid)) {
            continue;
        }
        let severity = match code {
            C::Port53SystemService | C::Port53Other => Severity::Info,
            _ => Severity::Warning,
        };
        out.push(DnsHint::new(code, severity, owner_label(owner)));
    }
    for adapter in &d.vpn_adapters {
        let severity = if d.ok {
            Severity::Info
        } else {
            Severity::Warning
        };
        out.push(DnsHint::new(C::VpnAdapter, severity, adapter));
    }
    if !d.ok {
        if d.system_resolves_to.is_empty() {
            let detail = d
                .system_error
                .clone()
                .unwrap_or_else(|| "sem endereços".to_owned());
            out.push(DnsHint::new(C::SystemUnresolved, Severity::Error, detail));
        } else {
            out.push(DnsHint::new(
                C::SystemMismatch,
                Severity::Error,
                join(&d.system_resolves_to),
            ));
        }
    }
    out
}

/// Runs diagnoses. Cheap to clone.
#[derive(Debug, Clone)]
pub struct DnsDiagnostics {
    policy: Arc<dyn DnsPolicy>,
    ports: Arc<dyn PortTable>,
    adapters: Arc<dyn NetAdapters>,
    resolver: Arc<dyn SystemResolver>,
    timeout: Duration,
}

impl DnsDiagnostics {
    /// Diagnostics over the OS policy, port table and adapters, asking the OS resolver
    /// through [`OsResolver`].
    pub fn new(
        policy: Arc<dyn DnsPolicy>,
        ports: Arc<dyn PortTable>,
        adapters: Arc<dyn NetAdapters>,
    ) -> Self {
        Self {
            policy,
            ports,
            adapters,
            resolver: Arc::new(OsResolver),
            timeout: DIAGNOSE_TIMEOUT,
        }
    }

    /// Replaces the OS resolver (fakes in tests).
    pub fn with_resolver(mut self, resolver: Arc<dyn SystemResolver>) -> Self {
        self.resolver = resolver;
        self
    }

    /// Budget of each probe.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Diagnoses `host` against `zones`, with the Forgebox resolver at `server` (from
    /// `DnsServer::stats().listen`; `None` when not running). Never fails: what cannot be
    /// read is reported in the result.
    pub async fn diagnose(
        &self,
        host: &str,
        zones: &ZoneSet,
        server: Option<SocketAddr>,
    ) -> DnsDiagnosis {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        let expected = zones.expected(&host);
        let timeout = self.timeout;

        let system = async {
            match tokio::time::timeout(timeout, self.resolver.lookup(&host)).await {
                Ok(result) => result,
                Err(_) => Err(ForgeError::Timeout {
                    operation: format!("resolving {host}"),
                    after: timeout,
                }),
            }
        };
        let ours = async {
            match server {
                Some(addr) => Some(client::resolve(addr, &host, timeout).await),
                None => None,
            }
        };
        let policy = Arc::clone(&self.policy);
        let rules =
            spawn_blocking_timeout("reading NRPT rules", timeout, move || policy.nrpt_rules());
        let policy = Arc::clone(&self.policy);
        let hosts = spawn_blocking_timeout("reading hosts", timeout, move || policy.read_hosts());
        let ports = Arc::clone(&self.ports);
        let port53 = spawn_blocking_timeout("reading the port tables", timeout, move || {
            let mut owners = ports.udp_endpoints()?;
            owners.extend(ports.listeners()?);
            Ok(owners)
        });
        let adapters = Arc::clone(&self.adapters);
        let adapters = spawn_blocking_timeout("listing network adapters", timeout, move || {
            adapters.adapters()
        });
        let (system, ours, rules, hosts, port53, adapters) =
            tokio::join!(system, ours, rules, hosts, port53, adapters);

        let mut read_errors = Vec::new();
        let (system_resolves_to, system_error) = match system {
            Ok(ips) => (ips, None),
            Err(err) => (Vec::new(), Some(err.to_string())),
        };
        let (our_server_resolves_to, our_server_error) = match ours {
            Some(Ok(ips)) => (ips, None),
            Some(Err(err)) => (Vec::new(), Some(err.to_string())),
            None => (Vec::new(), None),
        };
        let rules = rules.unwrap_or_else(|err| {
            read_errors.push(format!("nrpt: {err}"));
            Vec::new()
        });
        let group_policy_rules = rules
            .iter()
            .filter(|r| r.source == NrptSource::GroupPolicy)
            .count();
        let nrpt_present = effective_nrpt_rules(&rules)
            .iter()
            .any(|r| r.covers(&host) && !r.nameservers.is_empty());
        let nrpt_rules: Vec<NrptRule> = rules.into_iter().filter(|r| r.covers(&host)).collect();
        let (hosts_block_present, hosts_entries) = match hosts {
            Ok(text) => {
                let block = match read_hosts_block(&text) {
                    Ok(block) => block.is_some(),
                    Err(err) => {
                        read_errors.push(format!("hosts: {err}"));
                        false
                    }
                };
                (block, lookup_hosts(&text, &host))
            }
            Err(err) => {
                read_errors.push(format!("hosts: {err}"));
                (false, Vec::new())
            }
        };
        let port53_owner: Vec<PortOwner> = match port53 {
            Ok(owners) => owners
                .into_iter()
                .filter(|o| o.port == 53 && (o.addr.is_loopback() || o.addr.is_unspecified()))
                .collect(),
            Err(err) => {
                read_errors.push(format!("ports: {err}"));
                Vec::new()
            }
        };
        let vpn_adapters = match adapters {
            Ok(list) => list
                .iter()
                .filter(|a| is_vpn_adapter(a))
                .map(|a| format!("{} ({})", a.name, a.description))
                .collect(),
            Err(err) => {
                read_errors.push(format!("adapters: {err}"));
                Vec::new()
            }
        };
        let ok = !expected.is_empty()
            && !system_resolves_to.is_empty()
            && system_resolves_to.iter().all(|ip| expected.contains(ip));
        let mut diagnosis = DnsDiagnosis {
            host: host.clone(),
            expected,
            system_resolves_to,
            system_error,
            server,
            our_server_resolves_to,
            our_server_error,
            nrpt_present,
            nrpt_rules,
            group_policy_rules,
            hosts_block_present,
            hosts_entries,
            port53_owner,
            vpn_adapters,
            hints: Vec::new(),
            ok,
            read_errors,
        };
        diagnosis.hints = hints(&diagnosis, tld_of(zones, &host).as_deref());
        diagnosis
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use forge_core::platform::PortProto;

    use super::*;

    fn owner(pid: u32, exe: Option<&str>, addr: &str) -> PortOwner {
        PortOwner {
            port: 53,
            addr: addr.parse().unwrap(),
            pid,
            exe_path: exe.map(PathBuf::from),
            proto: PortProto::Udp,
        }
    }

    #[test]
    fn port53_owners_are_classified() {
        for (exe, code) in [
            (
                r"C:\Program Files\Acrylic DNS Proxy\AcrylicService.exe",
                DnsHintCode::Port53Acrylic,
            ),
            (
                r"C:\Program Files\Docker\Docker\resources\com.docker.backend.exe",
                DnsHintCode::Port53Docker,
            ),
            (r"C:\tools\dnscrypt-proxy.exe", DnsHintCode::Port53DnsServer),
            (
                r"C:\Program Files\OpenVPN\bin\openvpn.exe",
                DnsHintCode::Port53Vpn,
            ),
            (
                r"C:\Program Files\Fortinet\FortiClient\FortiTray.exe",
                DnsHintCode::Port53Vpn,
            ),
            (
                r"C:\Windows\System32\svchost.exe",
                DnsHintCode::Port53SystemService,
            ),
            (r"C:\x\mystery.exe", DnsHintCode::Port53Other),
        ] {
            assert_eq!(
                classify_port53(&owner(1000, Some(exe), "127.0.0.1")),
                code,
                "{exe}"
            );
        }
        assert_eq!(
            classify_port53(&owner(4, None, "0.0.0.0")),
            DnsHintCode::Port53SystemService
        );
    }

    #[test]
    fn vpn_adapters_are_recognized_when_up() {
        let adapter = |name: &str, description: &str, up: bool| NetAdapter {
            name: name.into(),
            description: description.into(),
            up,
            dns_servers: Vec::new(),
        };
        assert!(is_vpn_adapter(&adapter(
            "Ethernet 3",
            "TAP-Windows Adapter V9",
            true
        )));
        assert!(is_vpn_adapter(&adapter("corp", "WireGuard Tunnel", true)));
        assert!(is_vpn_adapter(&adapter(
            "NordLynx",
            "NordLynx Tunnel",
            true
        )));
        assert!(!is_vpn_adapter(&adapter(
            "Ethernet 3",
            "TAP-Windows Adapter V9",
            false
        )));
        assert!(!is_vpn_adapter(&adapter(
            "Wi-Fi",
            "Intel(R) Wi-Fi 6 AX201",
            true
        )));
        assert!(!is_vpn_adapter(&adapter(
            "vEthernet (WSL)",
            "Hyper-V Virtual Ethernet Adapter",
            true
        )));
    }
}
