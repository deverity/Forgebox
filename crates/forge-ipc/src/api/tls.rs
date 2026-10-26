//! `tls.*`: local CA and site certificates (docs/09 §tls, docs/06 §6, docs/11 §3).
//! The CA private key never leaves the daemon; only certificates are exported.

use serde::{Deserialize, Serialize};
use specta::Type;

use forge_core::Timestamp;
use forge_core::ids::SiteId;

/// Where a root certificate is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TrustScopeDto {
    /// `CurrentUser\Root`, no elevation.
    CurrentUser,
    /// `LocalMachine\Root`, through `forgebox-helper` (UAC).
    LocalMachine,
}

/// Result of `tls.ca_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaStatusDto {
    pub exists: bool,
    pub subject: Option<String>,
    /// SHA-256 thumbprint (hex).
    pub thumbprint_sha256: Option<String>,
    pub not_after: Option<Timestamp>,
    pub trusted_current_user: bool,
    pub trusted_local_machine: bool,
    /// Permitted subtrees (`.test`, `.localhost`, `127.0.0.0/8`).
    pub name_constraints: Vec<String>,
}

/// Params of `tls.ca_install`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaInstallParams {
    pub scope: TrustScopeDto,
}

/// Params of `tls.ca_rotate`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CaRotateParams {
    /// Destructive (re-issues every certificate): must be `true`.
    #[serde(default)]
    pub confirm: bool,
}

/// A PEM document (`tls.ca_export`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PemDto {
    /// Suggested file name (`forgebox-ca.crt`).
    pub filename: String,
    pub pem: String,
}

/// Params of `tls.issue`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TlsHostParams {
    pub host: String,
}

/// A leaf certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CertificateDto {
    pub host: String,
    pub sans: Vec<String>,
    pub site_id: Option<SiteId>,
    pub not_before: Timestamp,
    pub not_after: Timestamp,
    pub thumbprint_sha256: String,
}

/// Params of `tls.renew`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TlsRenewParams {
    /// One host, or all certificates close to expiry when `None`.
    #[serde(default)]
    pub host: Option<String>,
    /// Renew every certificate, not only those close to expiry.
    #[serde(default)]
    pub all: bool,
}

/// Result of the `tls.renew` job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CertificateListDto {
    pub certificates: Vec<CertificateDto>,
}

/// Export format of a leaf certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CertFormat {
    Pem,
    Pfx,
}

/// Params of `tls.export`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TlsExportParams {
    pub host: String,
    pub format: CertFormat,
    /// PFX password.
    #[serde(default)]
    pub password: Option<String>,
}

/// Result of `tls.export` (certificate and private key of the site: admin only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CertExportDto {
    pub format: CertFormat,
    pub filename: String,
    /// File content, base64.
    pub data_base64: String,
}
