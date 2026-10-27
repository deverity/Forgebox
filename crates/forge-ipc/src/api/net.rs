//! `net.*`: DNS for `*.test` and LAN exposure (docs/09 §net, docs/06 §5).

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::SiteRef;
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
