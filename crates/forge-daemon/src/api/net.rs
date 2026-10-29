//! `net.*`: the local resolver and the OS routing of the Forgebox TLDs (docs/06 §5).
//! `net.lan_*` are later work (see `not_implemented`).

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use forge_core::error::ForgeError;
use forge_core::error::Result;
use forge_core::model::port::PortProtocol as StoredProtocol;
use forge_ipc::api::methods::{
    net_dns_disable, net_dns_enable, net_dns_status, net_dns_test, net_port_check,
};
use forge_ipc::api::{
    DnsMode, DnsPolicyReason, DnsPolicyState, DnsProbeDto, DnsStatusDto, DnsTestDto, DnsTestParams,
    DnsTldPolicyDto, JobDto, NoParams, PortCheckDto, PortCheckParams, PortHolderDto, PortProtocol,
};
use forge_ipc::server::{Ctx, Router};
use forge_net::dns::{DnsDiagnostics, ZoneSet};
use forge_net::policy::{
    EnableRequest, PolicyMode, PolicyReason, PolicyState, ProbeResult, TldPolicy,
};

use super::route;
use crate::daemon::Daemon;

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, net_dns_status, status);
    route!(router, daemon, net_dns_enable, enable);
    route!(router, daemon, net_dns_disable, disable);
    route!(router, daemon, net_dns_test, test);
    route!(router, daemon, net_port_check, port_check);
    // Below the 1 s default so a stalled OS table answers with a timeout, not silence.
    router.timeout::<net_port_check>(PORT_CHECK_BUDGET);
}

/// Budget of the OS table read of `net.port_check` (the acceptance is an answer in 50 ms;
/// the figure is measured in `tests/shell_api.rs`).
const PORT_CHECK_BUDGET: Duration = Duration::from_millis(900);

/// `net.port_check`: is `port` usable right now? Crosses the OS table (who listens) with
/// the Forgebox reservations (who has it reserved), both read at once; the table read is
/// blocking Win32 and runs in `spawn_blocking` (ADR-0011).
async fn port_check(daemon: Daemon, params: PortCheckParams, _ctx: Ctx) -> Result<PortCheckDto> {
    if params.port == 0 {
        return Err(ForgeError::InvalidInput("port 0 is not a port".into()));
    }
    let (port, protocol) = (params.port, params.protocol);
    let table = Arc::clone(daemon.platform.port_table());
    let stored = match protocol {
        PortProtocol::Tcp => StoredProtocol::Tcp,
        PortProtocol::Udp => StoredProtocol::Udp,
    };
    let listening = forge_core::runtime::spawn_blocking_timeout(
        "reading the port table",
        PORT_CHECK_BUDGET,
        move || {
            Ok(match protocol {
                PortProtocol::Tcp => table.listeners()?,
                PortProtocol::Udp => table.udp_endpoints()?,
            }
            .into_iter()
            .find(|l| l.port == port))
        },
    );
    let (listening, reservation) =
        tokio::join!(listening, daemon.ports.reservation_of(stored, port));
    let holder = listening?.map(|l| PortHolderDto {
        pid: l.pid,
        exe: l
            .exe_path
            .as_deref()
            .map(forge_core::paths::posix_string)
            .unwrap_or_else(|| "?".into()),
    });
    let reserved_by = reservation?
        .map(|r| super::services::owner_name(&super::services::instance_names(&daemon), &r.owner));
    Ok(PortCheckDto {
        free: holder.is_none() && reserved_by.is_none(),
        reserved_by,
        holder,
    })
}

fn state_dto(state: PolicyState) -> DnsPolicyState {
    match state {
        PolicyState::Applied => DnsPolicyState::Applied,
        PolicyState::HostsFallback => DnsPolicyState::HostsFallback,
        PolicyState::NotApplied => DnsPolicyState::NotApplied,
    }
}

pub(crate) fn mode_dto(mode: PolicyMode) -> DnsMode {
    match mode {
        PolicyMode::Nrpt => DnsMode::Nrpt,
        PolicyMode::Hosts => DnsMode::Hosts,
        PolicyMode::None => DnsMode::None,
    }
}

pub(crate) fn reason_dto(reason: PolicyReason) -> DnsPolicyReason {
    match reason {
        PolicyReason::GroupPolicyTablePresent => DnsPolicyReason::GroupPolicyTablePresent,
        PolicyReason::ResolverProbeFailed => DnsPolicyReason::ResolverProbeFailed,
        PolicyReason::ResolverNotOnPort53 => DnsPolicyReason::ResolverNotOnPort53,
        PolicyReason::NrptUnsupported => DnsPolicyReason::NrptUnsupported,
        PolicyReason::NrptPointsElsewhere => DnsPolicyReason::NrptPointsElsewhere,
    }
}

fn probe_dto(probe: &ProbeResult) -> DnsProbeDto {
    DnsProbeDto {
        host: probe.host.clone(),
        ok: probe.ok,
        resolved: probe.resolved.iter().map(IpAddr::to_string).collect(),
        error: probe.error.clone(),
        attempts: probe.attempts,
        elapsed_ms: u32::try_from(probe.elapsed.as_millis()).unwrap_or(u32::MAX),
    }
}

fn tld_dto(policy: &TldPolicy) -> DnsTldPolicyDto {
    DnsTldPolicyDto {
        tld: policy.tld.clone(),
        mode: mode_dto(policy.state.mode()),
        policy: state_dto(policy.state),
        nameserver: policy.nameserver.map(|ip| ip.to_string()),
        reason: policy.reason.map(reason_dto),
        group_policy_table: policy.group_policy_table,
        probe: policy.probe.as_ref().map(probe_dto),
    }
}

/// Effective mode over every TLD: `nrpt` only when all follow NRPT.
fn overall_mode(states: &[TldPolicy]) -> DnsMode {
    if !states.is_empty() && states.iter().all(|s| s.state == PolicyState::Applied) {
        DnsMode::Nrpt
    } else if states.iter().any(|s| s.state == PolicyState::HostsFallback) {
        DnsMode::Hosts
    } else {
        DnsMode::None
    }
}

fn policy_state(states: &[TldPolicy]) -> DnsPolicyState {
    if !states.is_empty() && states.iter().all(|s| s.state == PolicyState::Applied) {
        DnsPolicyState::Applied
    } else if states
        .iter()
        .any(|s| matches!(s.state, PolicyState::Applied | PolicyState::HostsFallback))
    {
        DnsPolicyState::HostsFallback
    } else {
        DnsPolicyState::NotApplied
    }
}

async fn nameserver(daemon: &Daemon) -> (bool, Option<String>, Option<IpAddr>) {
    match daemon.dns.stats().await {
        Ok(stats) => (
            stats.running,
            stats.listen.map(|a| a.to_string()),
            stats.listen.filter(|a| a.port() == 53).map(|a| a.ip()),
        ),
        Err(_) => (false, None, None),
    }
}

/// The status DTO for `states` (from `status` or, right after `enable`, its own result:
/// it carries the verification of the whole probe window).
fn status_dto(
    daemon: &Daemon,
    running: bool,
    listen: Option<String>,
    states: &[TldPolicy],
) -> DnsStatusDto {
    let config = daemon.config();
    DnsStatusDto {
        enabled: config.dns.enabled,
        server_running: running,
        listen,
        tlds: config.tlds(),
        policy: policy_state(states),
        mode: overall_mode(states),
        reason: states.iter().find_map(|s| s.reason).map(reason_dto),
        tld_policies: states.iter().map(tld_dto).collect(),
    }
}

async fn snapshot(daemon: &Daemon) -> Result<DnsStatusDto> {
    let (running, listen, ns) = nameserver(daemon).await;
    let tlds = daemon.config().tlds();
    let states = daemon
        .dns_policy
        .status(&tlds, ns)
        .await
        .unwrap_or_default();
    Ok(status_dto(daemon, running, listen, &states))
}

/// Every name (host and aliases) of the enabled sites: what the `hosts` block must hold.
pub(crate) async fn site_hosts(daemon: &Daemon) -> Result<Vec<String>> {
    Ok(daemon
        .sites
        .list()
        .await?
        .iter()
        .filter(|s| s.enabled)
        .flat_map(|s| s.names().map(str::to_owned).collect::<Vec<_>>())
        .collect())
}

async fn status(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<DnsStatusDto> {
    snapshot(&daemon).await
}

async fn set_enabled(daemon: &Daemon, enabled: bool) -> Result<()> {
    let mut config = (*daemon.config()).clone();
    if config.dns.enabled == enabled {
        return Ok(());
    }
    config.dns.enabled = enabled;
    let path = daemon.root.forgebox_toml();
    let saved = config.clone();
    forge_core::runtime::spawn_blocking_timeout(
        "saving forgebox.toml",
        std::time::Duration::from_secs(5),
        move || saved.save(&path),
    )
    .await?;
    daemon.set_config(config);
    Ok(())
}

async fn enable(daemon: Daemon, _params: NoParams, ctx: Ctx) -> Result<JobDto> {
    let d = daemon.clone();
    daemon.job(&ctx, "net.dns_enable", move |_job| async move {
        set_enabled(&d, true).await?;
        let addr = d.dns.start().await?;
        let ns = (addr.port() == 53).then_some(addr.ip());
        let hosts = site_hosts(&d).await?;
        let states = d
            .dns_policy
            .enable(EnableRequest {
                tlds: d.config().tlds(),
                nameserver: ns,
                hosts,
            })
            .await?;
        for state in &states {
            tracing::info!(tld = %state.tld, mode = ?state.state.mode(), reason = ?state.reason, probe_ok = ?state.probe.as_ref().map(|p| p.ok), "dns: enabled");
        }
        let (running, listen, _) = nameserver(&d).await;
        Ok(status_dto(&d, running, listen, &states))
    })
}

async fn disable(daemon: Daemon, _params: NoParams, ctx: Ctx) -> Result<JobDto> {
    let d = daemon.clone();
    daemon.job(&ctx, "net.dns_disable", move |_job| async move {
        d.dns_policy.disable(&d.config().tlds()).await?;
        d.dns.stop().await?;
        set_enabled(&d, false).await?;
        snapshot(&d).await
    })
}

async fn test(daemon: Daemon, params: DnsTestParams, _ctx: Ctx) -> Result<DnsTestDto> {
    dns_test_for(&daemon, &params.host).await
}

/// `net.dns_test` for `host`; also the `site.dns` check of `sites.info`.
pub(super) async fn dns_test_for(daemon: &Daemon, host: &str) -> Result<DnsTestDto> {
    let config = daemon.config();
    let zones = ZoneSet::from_config(&config)?;
    let server = daemon.dns.stats().await.ok().and_then(|s| s.listen);
    let diagnostics = DnsDiagnostics::new(
        Arc::clone(&daemon.dns_os_policy),
        Arc::clone(daemon.platform.port_table()),
        Arc::clone(daemon.platform.net_adapters()),
    );
    let diagnosis = diagnostics.diagnose(host, &zones, server).await;
    let via = match diagnosis.mode {
        PolicyMode::Hosts => "hosts",
        PolicyMode::Nrpt => "forgebox-dns",
        PolicyMode::None => "system",
    };
    Ok(DnsTestDto {
        host: diagnosis.host.clone(),
        resolved: diagnosis
            .system_resolves_to
            .iter()
            .map(IpAddr::to_string)
            .collect(),
        expected: diagnosis.expected.iter().map(IpAddr::to_string).collect(),
        ok: diagnosis.ok,
        via: via.to_owned(),
        mode: mode_dto(diagnosis.mode),
        reason: diagnosis.mode_reason.map(reason_dto),
        probe: diagnosis.probe.as_ref().map(probe_dto),
    })
}
