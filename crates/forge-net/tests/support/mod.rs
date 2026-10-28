//! Fakes shared by the DNS tests: the OS resolver policy in memory (writes through the
//! real `HelperClient` + `FakeElevation`), port table, adapters and OS resolver.
//!
//! The in-memory OS resolver answers like Windows with the Forgebox resolver running:
//! a name covered by an **effective** NRPT rule resolves to `127.0.0.1`; anything else is
//! NXDOMAIN (the corporate DNS). `intercept` simulates a VPN/security agent that answers
//! every query itself, whatever the NRPT says.
#![allow(dead_code)]

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use forge_core::error::{ForgeError, Result};
use forge_core::helper::hosts::{apply_hosts_block, read_hosts_block};
use forge_core::helper::{FakeElevation, HelperClient, HelperOp, HelperPolicy, HostsEntry};
use forge_core::platform::{
    DnsPolicy, NetAdapter, NetAdapters, NrptApplied, NrptRule, NrptSource, NrptTable, PortOwner,
    PortTable,
};
use forge_net::dns::SystemResolver;

pub const STOCK_HOSTS: &str = "# Copyright (c) 1993-2009 Microsoft Corp.\r\n#\r\n";

/// The OS side in memory. Writes go through `helper` like the Windows implementation.
#[derive(Debug)]
pub struct MemoryPolicy {
    helper: HelperClient,
    pub rules: Mutex<Vec<NrptRule>>,
    /// The Group Policy NRPT key exists (even empty).
    pub group_policy_table: AtomicBool,
    /// Every OS resolver answer is replaced by this one (VPN/agent intercepting DNS).
    pub intercept: Mutex<Option<Result<Vec<IpAddr>, String>>>,
    /// Names asked to the OS resolver.
    pub queries: Mutex<Vec<String>>,
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
            group_policy_table: AtomicBool::new(false),
            intercept: Mutex::new(None),
            queries: Mutex::new(Vec::new()),
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

    /// The corporate case of the M1 gate: the Group Policy key exists with no rule.
    pub fn with_group_policy_table(self) -> Self {
        self.group_policy_table.store(true, Ordering::Relaxed);
        self
    }

    /// Every OS resolver query gets `answer` (a VPN or security agent answering DNS).
    pub fn intercepting(self, answer: Result<Vec<IpAddr>, String>) -> Self {
        *self.intercept.lock().unwrap() = Some(answer);
        self
    }

    fn table(&self) -> NrptTable {
        let rules = self.rules.lock().unwrap().clone();
        let group_policy_table = self.group_policy_table.load(Ordering::Relaxed)
            || rules.iter().any(|r| r.source == NrptSource::GroupPolicy);
        NrptTable {
            rules,
            group_policy_table,
        }
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
    fn nrpt_table(&self) -> Result<NrptTable> {
        self.check()?;
        Ok(self.table())
    }

    fn query_system(&self, host: &str) -> Result<Vec<IpAddr>> {
        self.queries.lock().unwrap().push(host.to_owned());
        if let Some(answer) = self.intercept.lock().unwrap().clone() {
            return answer.map_err(|e| ForgeError::NotFound(format!("{host}: {e}")));
        }
        let table = self.table();
        if table
            .effective_rules()
            .iter()
            .any(|r| r.covers(host) && !r.nameservers.is_empty())
        {
            Ok(vec![IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)])
        } else {
            Err(ForgeError::NotFound(format!("{host}: NXDOMAIN")))
        }
    }

    fn apply(&self, tld: &str, nameserver: IpAddr) -> Result<NrptApplied> {
        let response = self.helper.run(HelperOp::NrptAdd {
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
        drop(rules);
        // What the real helper reports (forge-helper `nrpt_add`); the fake elevation
        // answers without details, so they are derived from the table here.
        let mut applied = NrptApplied::from_details(&response.details);
        if applied.effective.is_none() {
            let ignored = self.table().local_rules_ignored();
            applied.effective = Some(ignored.is_none());
            applied.reason = ignored;
        }
        Ok(applied)
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
