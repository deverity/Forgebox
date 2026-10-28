//! `DnsDiagnostics` with a real resolver on a high port and faked OS sources: what is
//! reported and which hints come out (VPN, Docker, Acrylic, Group Policy, hosts...).

mod support;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use forge_core::actor::ActorSupervisor;
use forge_core::event::EventBus;
use forge_core::platform::{NetAdapter, NrptSource, PortOwner, PortProto};
use forge_net::dns::{
    DnsDiagnosis, DnsDiagnostics, DnsHintCode, DnsServer, DnsServerConfig, Severity, ZoneSet,
};
use forge_net::policy::{PolicyMode, PolicyReason};
use support::{FakeAdapters, FakePorts, FakeResolver, MemoryPolicy};

const V4: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);

fn zones() -> ZoneSet {
    let mut zones = ZoneSet::local(&[".test".into()]).unwrap();
    zones
        .add_remote("hml.test", "10.0.0.5".parse().unwrap())
        .unwrap();
    zones
}

async fn server() -> (DnsServer, SocketAddr) {
    let config = DnsServerConfig::on(vec![(Ipv4Addr::LOCALHOST, 0).into()]);
    let server = DnsServer::spawn(&ActorSupervisor::new(EventBus::new()), config, zones());
    let addr = server.start().await.unwrap();
    (server, addr)
}

struct Sources {
    policy: MemoryPolicy,
    ports: Vec<PortOwner>,
    adapters: Vec<NetAdapter>,
    system: Result<Vec<IpAddr>, String>,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            policy: MemoryPolicy::accepting(),
            ports: Vec::new(),
            adapters: Vec::new(),
            system: Ok(vec![V4, V6]),
        }
    }
}

async fn diagnose(sources: Sources, host: &str, server: Option<SocketAddr>) -> DnsDiagnosis {
    DnsDiagnostics::new(
        Arc::new(sources.policy),
        Arc::new(FakePorts(sources.ports)),
        Arc::new(FakeAdapters(sources.adapters)),
    )
    .with_resolver(Arc::new(FakeResolver(sources.system)))
    .diagnose(host, &zones(), server)
    .await
}

fn codes(d: &DnsDiagnosis) -> Vec<DnsHintCode> {
    d.hints.iter().map(|h| h.code).collect()
}

fn port53(pid: u32, exe: &str, addr: &str) -> PortOwner {
    PortOwner {
        port: 53,
        addr: addr.parse().unwrap(),
        pid,
        exe_path: Some(PathBuf::from(exe)),
        proto: PortProto::Udp,
    }
}

#[tokio::test]
async fn healthy_resolution() {
    let (server, addr) = server().await;
    let sources = Sources {
        policy: MemoryPolicy::accepting().with_rule(NrptSource::Local, ".test", "127.0.0.1"),
        ..Sources::default()
    };
    let d = diagnose(sources, "Loja.Test.", Some(addr)).await;
    assert_eq!(d.host, "loja.test");
    assert!(d.ok, "{d:#?}");
    assert_eq!(d.expected, vec![V4, V6]);
    assert_eq!(d.system_resolves_to, vec![V4, V6]);
    assert_eq!(d.our_server_resolves_to, vec![V4, V6]);
    assert!(d.nrpt_present);
    assert_eq!(d.nrpt_rules.len(), 1);
    assert!(!d.hosts_block_present);
    assert!(d.read_errors.is_empty());
    assert!(!d.group_policy_table);
    assert!(d.probe.as_ref().unwrap().ok, "{:?}", d.probe);
    assert_eq!(d.mode, PolicyMode::Nrpt);
    assert_eq!(d.mode_reason, None);
    // Only the high test port is worth mentioning.
    assert_eq!(codes(&d), vec![DnsHintCode::ServerNotOnPort53]);
    assert!(d.hints[0].message.contains(&addr.to_string()));
    let json = serde_json::to_value(&d).unwrap();
    assert_eq!(json["hints"][0]["code"], "server_not_on_port53");
    server.shutdown().await;
}

#[tokio::test]
async fn remote_zone_expectations() {
    let (server, addr) = server().await;
    let sources = Sources {
        system: Ok(vec!["10.0.0.5".parse().unwrap()]),
        ..Sources::default()
    };
    let d = diagnose(sources, "loja.hml.test", Some(addr)).await;
    assert_eq!(d.expected, vec!["10.0.0.5".parse::<IpAddr>().unwrap()]);
    assert_eq!(d.our_server_resolves_to, d.expected);
    assert!(d.ok);
    server.shutdown().await;
}

#[tokio::test]
async fn nothing_routes_the_tld() {
    let (server, addr) = server().await;
    let sources = Sources {
        system: Err("no such host".into()),
        ..Sources::default()
    };
    let d = diagnose(sources, "loja.test", Some(addr)).await;
    assert!(!d.ok);
    assert!(d.system_error.as_deref().unwrap().contains("no such host"));
    let codes = codes(&d);
    assert!(codes.contains(&DnsHintCode::NrptMissing), "{codes:?}");
    assert!(codes.contains(&DnsHintCode::SystemUnresolved), "{codes:?}");
    let missing = d
        .hints
        .iter()
        .find(|h| h.code == DnsHintCode::NrptMissing)
        .unwrap();
    assert_eq!(missing.detail, ".test");
    assert_eq!(missing.severity, Severity::Error);
    server.shutdown().await;
}

#[tokio::test]
async fn server_down_and_rule_pointing_elsewhere() {
    let sources = Sources {
        policy: MemoryPolicy::accepting().with_rule(NrptSource::Local, ".test", "127.0.0.1"),
        system: Err("timeout".into()),
        ..Sources::default()
    };
    let d = diagnose(sources, "loja.test", None).await;
    assert!(codes(&d).contains(&DnsHintCode::ServerNotRunning));
    assert!(d.our_server_resolves_to.is_empty() && d.our_server_error.is_none());

    let (server, addr) = server().await;
    let sources = Sources {
        policy: MemoryPolicy::accepting().with_rule(NrptSource::Local, ".test", "127.0.0.53"),
        system: Err("timeout".into()),
        ..Sources::default()
    };
    let d = diagnose(sources, "loja.test", Some(addr)).await;
    let elsewhere = d
        .hints
        .iter()
        .find(|h| h.code == DnsHintCode::NrptPointsElsewhere)
        .expect("rule points elsewhere");
    assert!(
        elsewhere.detail.contains("127.0.0.53"),
        "{}",
        elsewhere.detail
    );
    server.shutdown().await;
}

#[tokio::test]
async fn group_policy_rules_and_hosts_overrides() {
    let (server, addr) = server().await;
    let sources = Sources {
        policy: MemoryPolicy::accepting()
            .with_rule(NrptSource::Local, ".test", "127.0.0.1")
            .with_rule(NrptSource::GroupPolicy, ".corp.example", "10.0.0.1")
            .with_hosts_line("10.9.9.9 loja.test\r\n"),
        system: Ok(vec!["10.9.9.9".parse().unwrap()]),
        ..Sources::default()
    };
    let d = diagnose(sources, "loja.test", Some(addr)).await;
    assert!(!d.ok);
    assert!(
        !d.nrpt_present,
        "the local rule is ignored under Group Policy"
    );
    assert_eq!(d.group_policy_rules, 1);
    assert_eq!(d.hosts_entries, vec!["10.9.9.9".parse::<IpAddr>().unwrap()]);
    let codes = codes(&d);
    for expected in [
        DnsHintCode::GroupPolicyNrpt,
        DnsHintCode::HostsOverride,
        DnsHintCode::SystemMismatch,
    ] {
        assert!(codes.contains(&expected), "{expected:?} in {codes:?}");
    }
    assert!(
        !codes.contains(&DnsHintCode::NrptMissing),
        "hosts has an entry"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn an_empty_group_policy_table_hides_the_local_rule() {
    // The M1 gate machine: the Forgebox rule is in the local table, the corporate GPO key
    // exists with no rule, the OS resolver sends .test to the corporate DNS (ADR-0033).
    let (server, addr) = server().await;
    let sources = Sources {
        policy: MemoryPolicy::accepting()
            .with_rule(NrptSource::Local, ".test", "127.0.0.1")
            .with_group_policy_table(),
        system: Err("NXDOMAIN".into()),
        ..Sources::default()
    };
    let d = diagnose(sources, "loja.test", Some(addr)).await;
    assert!(!d.ok);
    assert!(!d.nrpt_present);
    assert!(d.group_policy_table);
    assert_eq!(d.group_policy_rules, 0);
    assert!(!d.probe.as_ref().unwrap().ok);
    assert_eq!(d.mode, PolicyMode::None);
    assert_eq!(d.mode_reason, Some(PolicyReason::GroupPolicyTablePresent));
    let hint = d
        .hints
        .iter()
        .find(|h| h.code == DnsHintCode::GroupPolicyNrpt)
        .expect("group policy hint");
    assert_eq!(hint.severity, Severity::Warning);
    assert!(hint.message.contains("mesmo vazia"), "{}", hint.message);
    assert!(
        !codes(&d).contains(&DnsHintCode::NrptNotFollowed),
        "the rule is not effective, nothing to follow"
    );
    let json = serde_json::to_value(&d).unwrap();
    assert_eq!(json["mode"], "none");
    assert_eq!(json["mode_reason"], "group_policy_table_present");

    // With the hosts fallback in place the host resolves through hosts.
    let sources = Sources {
        policy: MemoryPolicy::accepting()
            .with_rule(NrptSource::Local, ".test", "127.0.0.1")
            .with_group_policy_table()
            .with_hosts_line(
                "127.0.0.1 loja.test
::1 loja.test
",
            ),
        ..Sources::default()
    };
    let d = diagnose(sources, "loja.test", Some(addr)).await;
    assert!(d.ok);
    assert_eq!(d.mode, PolicyMode::Hosts);
    assert_eq!(d.mode_reason, Some(PolicyReason::GroupPolicyTablePresent));
    let hint = d
        .hints
        .iter()
        .find(|h| h.code == DnsHintCode::GroupPolicyNrpt)
        .unwrap();
    assert_eq!(hint.severity, Severity::Info);
    server.shutdown().await;
}

#[tokio::test]
async fn an_empty_group_policy_table_without_a_local_rule_gives_the_same_reason() {
    // `forge dns enable` skips `nrpt.add` under the Group Policy table, so the usual state
    // of the M1 gate machine has no local rule at all: diagnosis and `dns status` must
    // still agree on the reason (review of ADR-0033).
    let (server, addr) = server().await;
    let sources = Sources {
        policy: MemoryPolicy::accepting().with_group_policy_table(),
        system: Err("NXDOMAIN".into()),
        ..Sources::default()
    };
    let d = diagnose(sources, "loja.test", Some(addr)).await;
    assert!(!d.ok);
    assert!(d.nrpt_rules.is_empty());
    assert!(d.group_policy_table);
    assert_eq!(d.mode, PolicyMode::None);
    assert_eq!(d.mode_reason, Some(PolicyReason::GroupPolicyTablePresent));
    assert!(
        codes(&d).contains(&DnsHintCode::GroupPolicyNrpt),
        "{:?}",
        codes(&d)
    );

    // Same machine with the hosts block: mode hosts, same reason.
    let sources = Sources {
        policy: MemoryPolicy::accepting()
            .with_group_policy_table()
            .with_hosts_line("127.0.0.1 loja.test\r\n::1 loja.test\r\n"),
        ..Sources::default()
    };
    let d = diagnose(sources, "loja.test", Some(addr)).await;
    assert!(d.ok);
    assert_eq!(d.mode, PolicyMode::Hosts);
    assert_eq!(d.mode_reason, Some(PolicyReason::GroupPolicyTablePresent));
    server.shutdown().await;
}

#[tokio::test]
async fn a_rule_the_resolver_does_not_follow() {
    // The rule is effective in the registry but a VPN/agent answers DNS itself.
    let (server, addr) = server().await;
    let sources = Sources {
        policy: MemoryPolicy::accepting()
            .with_rule(NrptSource::Local, ".test", "127.0.0.1")
            .intercepting(Err("NXDOMAIN".into())),
        system: Err("NXDOMAIN".into()),
        ..Sources::default()
    };
    let d = diagnose(sources, "loja.test", Some(addr)).await;
    assert!(!d.ok);
    assert!(d.nrpt_present);
    assert_eq!(d.mode, PolicyMode::None);
    assert_eq!(d.mode_reason, Some(PolicyReason::ResolverProbeFailed));
    let hint = d
        .hints
        .iter()
        .find(|h| h.code == DnsHintCode::NrptNotFollowed)
        .expect("not followed");
    assert_eq!(hint.severity, Severity::Error);
    assert!(hint.detail.contains("NXDOMAIN"), "{}", hint.detail);
    server.shutdown().await;
}

#[tokio::test]
async fn port_53_owners_and_vpn_adapters() {
    let (server, addr) = server().await;
    let sources = Sources {
        ports: vec![
            port53(
                1200,
                r"C:\Program Files\Acrylic DNS Proxy\AcrylicService.exe",
                "127.0.0.1",
            ),
            port53(1460, r"C:\Windows\System32\svchost.exe", "0.0.0.0"),
            port53(
                3000,
                r"C:\Program Files\Docker\Docker\resources\com.docker.backend.exe",
                "0.0.0.0",
            ),
            // Not loopback nor wildcard: not a conflict for us.
            port53(4000, r"C:\x\other.exe", "192.168.0.10"),
            // Our own sockets are never a problem.
            port53(
                std::process::id(),
                r"C:\Forgebox\forgeboxd.exe",
                "127.0.0.1",
            ),
        ],
        adapters: vec![
            NetAdapter {
                name: "Ethernet 3".into(),
                description: "TAP-Windows Adapter V9".into(),
                up: true,
                dns_servers: vec!["10.8.0.1".parse().unwrap()],
            },
            NetAdapter {
                name: "Wi-Fi".into(),
                description: "Intel(R) Wi-Fi 6".into(),
                up: true,
                dns_servers: Vec::new(),
            },
        ],
        system: Ok(vec![V4]),
        ..Sources::default()
    };
    let d = diagnose(sources, "loja.test", Some(addr)).await;
    assert!(d.ok, "IPv4 only is still correct");
    assert_eq!(d.port53_owner.len(), 4, "192.168.0.10 filtered out");
    let codes = codes(&d);
    for expected in [
        DnsHintCode::Port53Acrylic,
        DnsHintCode::Port53SystemService,
        DnsHintCode::Port53Docker,
        DnsHintCode::VpnAdapter,
    ] {
        assert!(codes.contains(&expected), "{expected:?} in {codes:?}");
    }
    assert!(!codes.contains(&DnsHintCode::Port53Other));
    assert_eq!(
        d.vpn_adapters,
        vec!["Ethernet 3 (TAP-Windows Adapter V9)".to_owned()]
    );
    let acrylic = d
        .hints
        .iter()
        .find(|h| h.code == DnsHintCode::Port53Acrylic)
        .unwrap();
    assert_eq!(acrylic.severity, Severity::Warning);
    assert!(acrylic.detail.contains("AcrylicService.exe (PID 1200)"));
    server.shutdown().await;
}

#[tokio::test]
async fn hosts_outside_the_zones_and_unreadable_sources() {
    let d = diagnose(Sources::default(), "google.com", None).await;
    assert!(d.expected.is_empty());
    assert!(!d.ok);
    assert_eq!(codes(&d), vec![DnsHintCode::NotInZone]);

    let mut policy = MemoryPolicy::accepting();
    policy.broken = true;
    let sources = Sources {
        policy,
        ..Sources::default()
    };
    let d = diagnose(sources, "loja.test", None).await;
    assert_eq!(d.read_errors.len(), 2, "{:?}", d.read_errors);
    assert!(d.read_errors[0].starts_with("nrpt:"));
    assert!(d.read_errors[1].starts_with("hosts:"));
    assert!(d.ok, "the OS still resolves it");
}

/// The real Windows sources (registry NRPT tables, hosts, UDP/TCP tables, adapters,
/// getaddrinfo) read without elevation, with the resolver on a high port.
#[cfg(windows)]
#[tokio::test]
async fn real_windows_sources_are_readable() {
    use forge_core::helper::{FakeElevation, HelperClient, HelperPolicy};
    use forge_core::platform;

    let (server, addr) = server().await;
    let helper = HelperClient::new(
        Arc::new(FakeElevation::accepting()),
        r"C:\Forgebox\forgebox-helper.exe",
        HelperPolicy::new(r"C:\Forgebox", [".test"]).unwrap(),
    );
    let diagnostics = DnsDiagnostics::new(
        platform::dns_policy(helper),
        platform::port_table(),
        platform::net_adapters(),
    );
    let host = format!("forgebox-diag-{}.test", std::process::id());
    let d = diagnostics.diagnose(&host, &zones(), Some(addr)).await;
    assert!(d.read_errors.is_empty(), "{:?}", d.read_errors);
    assert_eq!(d.our_server_resolves_to, vec![V4, V6]);
    // Sockets of this very process are listed (ours) only when on port 53, which the test
    // server is not; whatever holds 53 on this machine is classified.
    for owner in &d.port53_owner {
        assert_eq!(owner.port, 53);
    }
    eprintln!("{}", serde_json::to_string_pretty(&d).unwrap());
    server.shutdown().await;
}
