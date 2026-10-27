//! Fakes shared by the DNS tests: the OS resolver policy in memory (writes through the
//! real `HelperClient` + `FakeElevation`), port table, adapters and OS resolver.
#![allow(dead_code)]

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use forge_core::error::{ForgeError, Result};
use forge_core::helper::hosts::{apply_hosts_block, read_hosts_block};
use forge_core::helper::{FakeElevation, HelperClient, HelperOp, HelperPolicy, HostsEntry};
use forge_core::platform::{
    DnsPolicy, NetAdapter, NetAdapters, NrptRule, NrptSource, PortOwner, PortTable,
};
use forge_net::dns::SystemResolver;

pub const STOCK_HOSTS: &str = "# Copyright (c) 1993-2009 Microsoft Corp.\r\n#\r\n";

/// The OS side in memory. Writes go through `helper` like the Windows implementation.
#[derive(Debug)]
pub struct MemoryPolicy {
    helper: HelperClient,
    pub rules: Mutex<Vec<NrptRule>>,
    pub hosts: Mutex<String>,
    pub flushes: AtomicUsize,
    /// Reads fail (diagnosis must survive it).
    pub broken: bool,
}

impl MemoryPolicy {
    pub fn new(elevation: Arc<FakeElevation>) -> Self {
        let policy = HelperPolicy::new(r"C:\Forgebox", [".test", ".localhost"]).unwrap();
        Self {
            helper: HelperClient::new(elevation, r"C:\Forgebox\forgebox-helper.exe", policy),
            rules: Mutex::new(Vec::new()),
            hosts: Mutex::new(STOCK_HOSTS.to_owned()),
            flushes: AtomicUsize::new(0),
            broken: false,
        }
    }

    pub fn accepting() -> Self {
        Self::new(Arc::new(FakeElevation::accepting()))
    }

    pub fn with_rule(self, source: NrptSource, namespace: &str, server: &str) -> Self {
        self.rules.lock().unwrap().push(NrptRule {
            id: format!("{{{namespace}}}"),
            namespaces: vec![namespace.into()],
            nameservers: vec![server.parse().unwrap()],
            source,
            comment: None,
        });
        self
    }

    pub fn with_hosts_line(self, line: &str) -> Self {
        self.hosts.lock().unwrap().push_str(line);
        self
    }

    pub fn hosts_entries(&self) -> Option<Vec<HostsEntry>> {
        read_hosts_block(&self.hosts.lock().unwrap())
            .unwrap()
            .map(|b| b.entries)
    }

    fn check(&self) -> Result<()> {
        if self.broken {
            Err(ForgeError::io(
                "reading",
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ))
        } else {
            Ok(())
        }
    }
}

impl DnsPolicy for MemoryPolicy {
    fn nrpt_rules(&self) -> Result<Vec<NrptRule>> {
        self.check()?;
        Ok(self.rules.lock().unwrap().clone())
    }

    fn apply(&self, tld: &str, nameserver: IpAddr) -> Result<()> {
        self.helper.run(HelperOp::NrptAdd {
            namespace: tld.into(),
            nameserver,
        })?;
        let mut rules = self.rules.lock().unwrap();
        rules.retain(|r| !(r.source == NrptSource::Local && r.namespaces == [tld.to_owned()]));
        rules.push(NrptRule {
            id: format!("{{forgebox-{tld}}}"),
            namespaces: vec![tld.to_owned()],
            nameservers: vec![nameserver],
            source: NrptSource::Local,
            comment: Some("Forgebox".into()),
        });
        Ok(())
    }

    fn remove(&self, tld: &str) -> Result<()> {
        self.helper.run(HelperOp::NrptRemove {
            namespace: tld.into(),
        })?;
        self.rules
            .lock()
            .unwrap()
            .retain(|r| !(r.source == NrptSource::Local && r.namespaces == [tld.to_owned()]));
        Ok(())
    }

    fn hosts_path(&self) -> PathBuf {
        PathBuf::from(r"C:\Windows\System32\drivers\etc\hosts")
    }

    fn read_hosts(&self) -> Result<String> {
        self.check()?;
        Ok(self.hosts.lock().unwrap().clone())
    }

    fn set_hosts_block(&self, entries: &[HostsEntry]) -> Result<()> {
        self.helper.run(HelperOp::HostsSetBlock {
            entries: entries.to_vec(),
        })?;
        let mut hosts = self.hosts.lock().unwrap();
        *hosts = apply_hosts_block(&hosts, entries)?;
        Ok(())
    }

    fn flush_cache(&self) -> Result<()> {
        self.flushes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// Port table with fixed sockets.
#[derive(Debug, Default)]
pub struct FakePorts(pub Vec<PortOwner>);

impl PortTable for FakePorts {
    fn listeners(&self) -> Result<Vec<PortOwner>> {
        Ok(Vec::new())
    }
    fn established_count(&self, _: u16) -> Result<u32> {
        Ok(0)
    }
    fn udp_endpoints(&self) -> Result<Vec<PortOwner>> {
        Ok(self.0.clone())
    }
}

/// Fixed adapters.
#[derive(Debug, Default)]
pub struct FakeAdapters(pub Vec<NetAdapter>);

impl NetAdapters for FakeAdapters {
    fn adapters(&self) -> Result<Vec<NetAdapter>> {
        Ok(self.0.clone())
    }
}

/// OS resolver answering a fixed list (or failing).
#[derive(Debug)]
pub struct FakeResolver(pub Result<Vec<IpAddr>, String>);

#[async_trait::async_trait]
impl SystemResolver for FakeResolver {
    async fn lookup(&self, host: &str) -> Result<Vec<IpAddr>> {
        self.0
            .clone()
            .map_err(|e| ForgeError::NotFound(format!("{host}: {e}")))
    }
}
