//! Forgebox local TLS: local CA, leaf certificates, user trust store and renewal
//! (docs/06 §6, docs/11 §3, ADR-0023).
//!
//! * [`ca`]: the local CA (ECDSA P-256, 10 years, `pathLen 0`, critical name constraints
//!   limited to the local TLDs, `localhost`, `127.0.0.0/8` and `::1/128`), stored in
//!   `certs/ca/` with an owner-only key.
//! * [`leaf`]: site certificates (host + `*.host` + aliases, 825 days, `serverAuth`) in
//!   `certs/sites/<host>.crt|key`.
//! * [`trust`]: the ledger of roots Forgebox added to the OS stores and the instructions
//!   for programs that do not read them (Node.js, Firefox, Python, Java). The OS store
//!   itself is `forge_core::platform::TrustStore`.
//! * [`manager`]: the `tls` actor ([`TlsManager`]): issue, renew, rotate, status, trust,
//!   plus the daily renewal check.
//!
//! Crate of the Forgebox workspace, see docs/02-project-structure.md.

pub mod ca;
mod cert;
pub mod crl;
pub mod leaf;
pub mod manager;
pub mod messages;
pub mod trust;

pub use ca::{CaInfo, CaOptions, LocalCa};
pub use cert::{PermittedSubtree, sha256_hex};
pub use leaf::{LeafInfo, LeafRequest, SiteCert, StoredLeaf, issue_leaf, load_leaf, scan_leaves};
pub use manager::{
    CRL_PORT_RANGE, CrlStatus, InstallReport, LeafExport, LeafStatus, RenewScope, RotationReport,
    TLS_EVENT_VERSION, TlsConfig, TlsEventPayload, TlsManager, TlsStatus,
};
pub use trust::{TrustEntry, TrustInstruction, TrustLedger};

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-tls");
    }
}
