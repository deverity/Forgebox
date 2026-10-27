//! `net.*`: the local resolver and the OS routing of the Forgebox TLDs (docs/06 §5).
//! `net.lan_*` are later work (see `not_implemented`).

use std::net::IpAddr;
use std::sync::Arc;

use forge_core::error::Result;
use forge_ipc::api::methods::{net_dns_disable, net_dns_enable, net_dns_status, net_dns_test};
use forge_ipc::api::{DnsPolicyState, DnsStatusDto, DnsTestDto, DnsTestParams, JobDto, NoParams};
use forge_ipc::server::{Ctx, Router};
use forge_net::dns::{DnsDiagnostics, ZoneSet};
use forge_net::policy::{EnableRequest, PolicyState};

use super::route;
use crate::daemon::Daemon;

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, net_dns_status, status);
    route!(router, daemon, net_dns_enable, enable);
    route!(router, daemon, net_dns_disable, disable);
    route!(router, daemon, net_dns_test, test);
}

fn policy_state(states: &[forge_net::policy::TldPolicy]) -> DnsPolicyState {
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

async fn snapshot(daemon: &Daemon) -> Result<DnsStatusDto> {
    let config = daemon.config();
    let (running, listen, ns) = nameserver(daemon).await;
    let tlds = config.tlds();
    let states = daemon
        .dns_policy
        .status(&tlds, ns)
        .await
        .unwrap_or_default();
    Ok(DnsStatusDto {
        enabled: config.dns.enabled,
        server_running: running,
        listen,
        tlds,
        policy: policy_state(&states),
    })
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
        let hosts: Vec<String> = d
            .sites
            .list()
            .await?
            .iter()
            .filter(|s| s.enabled)
            .flat_map(|s| s.names().map(str::to_owned).collect::<Vec<_>>())
            .collect();
        d.dns_policy
            .enable(EnableRequest {
                tlds: d.config().tlds(),
                nameserver: ns,
                hosts,
            })
            .await?;
        snapshot(&d).await
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
    let config = daemon.config();
    let zones = ZoneSet::from_config(&config)?;
    let server = daemon.dns.stats().await.ok().and_then(|s| s.listen);
    let diagnostics = DnsDiagnostics::new(
        Arc::clone(&daemon.dns_os_policy),
        Arc::clone(daemon.platform.port_table()),
        Arc::clone(daemon.platform.net_adapters()),
    );
    let diagnosis = diagnostics.diagnose(&params.host, &zones, server).await;
    let via = if diagnosis.nrpt_present {
        "forgebox-dns"
    } else if !diagnosis.hosts_entries.is_empty() {
        "hosts"
    } else {
        "system"
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
    })
}
