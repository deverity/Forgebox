//! Manual test of the Forgebox resolver (docs/06 §5.4), until the daemon and `forge dns`
//! exist: serves the zones on `127.0.0.1:53` (or `127.0.0.53:53` when taken), prints a
//! diagnosis, and stops after `--for` seconds or on Ctrl+C. Needs no elevation.
//!
//! ```text
//! cargo run -p forge-net --example dns_serve -- [--listen 127.0.0.1:53]... [--tld .test]...
//!     [--remote hml.test=10.0.0.5]... [--diagnose qualquer.test] [--for 60]
//! Resolve-DnsName qualquer.test -Server 127.0.0.1 -DnsOnly
//! ```

use std::net::SocketAddr;
use std::time::Duration;

use forge_core::actor::ActorSupervisor;
use forge_core::event::EventBus;
use forge_core::helper::{HelperClient, HelperPolicy};
use forge_core::platform;
use forge_net::dns::{DnsDiagnostics, DnsServer, DnsServerConfig, STANDARD_LISTEN, ZoneSet};

struct Args {
    listen: Vec<SocketAddr>,
    tlds: Vec<String>,
    remotes: Vec<(String, std::net::IpAddr)>,
    diagnose: Vec<String>,
    seconds: Option<u64>,
}

fn parse() -> Result<Args, String> {
    let mut args = Args {
        listen: Vec::new(),
        tlds: Vec::new(),
        remotes: Vec::new(),
        diagnose: Vec::new(),
        seconds: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--listen" => args
                .listen
                .push(value()?.parse().map_err(|e| format!("--listen: {e}"))?),
            "--tld" => args.tlds.push(value()?),
            "--remote" => {
                let v = value()?;
                let (zone, ip) = v.split_once('=').ok_or("--remote zone=ip")?;
                args.remotes.push((
                    zone.to_owned(),
                    ip.parse().map_err(|e| format!("--remote: {e}"))?,
                ));
            }
            "--diagnose" => args.diagnose.push(value()?),
            "--for" => args.seconds = Some(value()?.parse().map_err(|e| format!("--for: {e}"))?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if args.listen.is_empty() {
        args.listen = STANDARD_LISTEN.to_vec();
    }
    if args.tlds.is_empty() {
        args.tlds.push(".test".to_owned());
    }
    Ok(args)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse()?;
    let mut zones = ZoneSet::local(&args.tlds)?;
    for (zone, ip) in &args.remotes {
        zones.add_remote(zone, *ip)?;
    }
    let supervisor = ActorSupervisor::new(EventBus::new());
    let server = DnsServer::spawn(&supervisor, DnsServerConfig::on(args.listen.clone()), zones);
    let addr = server.start().await?;
    let stats = server.stats().await?;
    println!("listening on {addr} (nrpt_capable: {})", stats.nrpt_capable);
    println!("zones: {:?}", stats.zones);

    // Reading needs no helper; the client below is never asked to change anything.
    let root = std::env::current_dir()?;
    let helper = HelperClient::new(
        platform::elevation(),
        root.join("forgebox-helper.exe"),
        HelperPolicy::new(&root, args.tlds.clone())?,
    );
    let diagnostics = DnsDiagnostics::new(
        platform::dns_policy(helper),
        platform::port_table(),
        platform::net_adapters(),
    );
    for host in &args.diagnose {
        let report = diagnostics
            .diagnose(host, &server.zones(), Some(addr))
            .await;
        println!("{}", serde_json::to_string_pretty(&report)?);
    }

    let wait = async {
        match args.seconds {
            Some(s) => tokio::time::sleep(Duration::from_secs(s)).await,
            None => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    };
    println!(
        "serving; stop with Ctrl+C{}",
        args.seconds
            .map(|s| format!(" or after {s} s"))
            .unwrap_or_default()
    );
    wait.await;
    let stats = server.stats().await?;
    println!(
        "queries: {} answered: {} nodata: {} nxdomain: {} errors: {}",
        stats.queries, stats.answered, stats.nodata, stats.nxdomain, stats.errors
    );
    server.shutdown().await;
    Ok(())
}
