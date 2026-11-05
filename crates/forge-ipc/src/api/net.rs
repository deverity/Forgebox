//! `net.*`: DNS for `*.test` and LAN exposure (docs/09 §net, docs/06 §5).

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::SiteRef;
use super::services::{PortHolderDto, PortProtocol};
use forge_core::ids::SiteId;

/// How the OS routes the Forgebox TLDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DnsPolicyState {
    /// NRPT rule (Windows) / resolver routing domain (Linux) in place.
    Applied,
    /// Entries in the `hosts` managed block (fallback).
    HostsFallback,
    NotApplied,
}

/// Which path really takes the OS resolver to the sites of a TLD (docs/06 §5.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DnsMode {
    /// An NRPT rule Windows follows (verified by asking the OS resolver).
    Nrpt,
    /// The `hosts` managed block.
    Hosts,
    /// Nothing routes the TLD.
    #[default]
    None,
}

/// Why a TLD is not (or not only) routed by NRPT. Stable codes; the UI translates them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DnsPolicyReason {
    /// The Group Policy NRPT table exists, even empty: Windows ignores local rules.
    GroupPolicyTablePresent,
    /// The rule is in the registry, but the OS resolver did not answer a probe name of
    /// the TLD with loopback (VPN or security agent intercepting DNS, rule not yet
    /// followed).
    ResolverProbeFailed,
    /// The Forgebox resolver is not on port 53 (NRPT cannot name a port).
    ResolverNotOnPort53,
    /// The OS has no NRPT.
    NrptUnsupported,
    /// An effective rule sends the TLD to another server.
    NrptPointsElsewhere,
}

/// Verification of a TLD through the OS resolver: a random name under it
/// (`forgebox-probe-<ulid>.test`), which only the Forgebox resolver answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DnsProbeDto {
    pub host: String,
    /// Resolved, and only to loopback.
    pub ok: bool,
    pub resolved: Vec<String>,
    /// Last error of the OS resolver (`NXDOMAIN`, timeout...).
    pub error: Option<String>,
    pub attempts: u32,
    pub elapsed_ms: u32,
}

/// Routing of one TLD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DnsTldPolicyDto {
    /// `.test`.
    pub tld: String,
    pub mode: DnsMode,
    pub policy: DnsPolicyState,
    /// Where the Forgebox NRPT rule points, when there is one (effective or not).
    pub nameserver: Option<String>,
    pub reason: Option<DnsPolicyReason>,
    /// The Group Policy NRPT table exists on the machine.
    pub group_policy_table: bool,
    /// Last verification through the OS resolver, when there was an NRPT rule to verify.
    pub probe: Option<DnsProbeDto>,
}

/// Result of `net.dns_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DnsStatusDto {
    pub enabled: bool,
    pub server_running: bool,
    /// `127.0.0.1:53`.
    pub listen: Option<String>,
    /// `[".test"]`.
    pub tlds: Vec<String>,
    pub policy: DnsPolicyState,
    /// Effective mode over every TLD: `nrpt` only when all follow NRPT, `hosts` when any
    /// uses the `hosts` block.
    #[serde(default)]
    pub mode: DnsMode,
    /// First reason a TLD is not routed by NRPT.
    #[serde(default)]
    pub reason: Option<DnsPolicyReason>,
    #[serde(default)]
    pub tld_policies: Vec<DnsTldPolicyDto>,
}

/// Params of `net.dns_test`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DnsTestParams {
    pub host: String,
}

/// Result of `net.dns_test`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DnsTestDto {
    pub host: String,
    pub resolved: Vec<String>,
    pub expected: Vec<String>,
    pub ok: bool,
    /// Which path answered (`forgebox-dns`, `hosts`, `system`).
    pub via: String,
    /// Which path really routes the host (`nrpt`, `hosts`, `none`).
    #[serde(default)]
    pub mode: DnsMode,
    /// Why the host is not answered through NRPT, when known.
    #[serde(default)]
    pub reason: Option<DnsPolicyReason>,
    /// A probe name of the TLD through the OS resolver (one try).
    #[serde(default)]
    pub probe: Option<DnsProbeDto>,
}

/// Params of `net.lan_expose`, `net.lan_unexpose`, `net.lan_qr`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LanSiteParams {
    pub site: SiteRef,
}

/// Result of `net.lan_expose` (job) and `net.lan_qr`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LanAccessDto {
    pub site_id: SiteId,
    /// URLs reachable from the LAN (`http://192.168.0.10:8080`).
    pub urls: Vec<String>,
    /// QR code of the first URL as SVG (only `net.lan_qr`).
    pub qr_svg: Option<String>,
}

/// Params of `net.port_check` (API 1.1): is this port usable right now?
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PortCheckParams {
    pub port: u16,
    #[serde(default = "tcp")]
    pub protocol: PortProtocol,
}

fn tcp() -> PortProtocol {
    PortProtocol::Tcp
}

/// Result of `net.port_check` (API 1.1): the port is `free` when no process listens on it
/// and no Forgebox owner has it reserved. The GUI uses it to validate a port field while
/// the user types ("Trocar porta").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PortCheckDto {
    pub free: bool,
    /// Forgebox owner that has the port reserved: `<tool>/<instance>` (`mysql/default`)
    /// or `<kind>/<id>`. Set even when the owner itself is the one listening.
    #[serde(default)]
    pub reserved_by: Option<String>,
    /// The process listening on the port, when there is one.
    #[serde(default)]
    pub holder: Option<PortHolderDto>,
}
