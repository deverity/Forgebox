//! The local CA (docs/06 §6.1, docs/11 §3, ADR-0023).
//!
//! * ECDSA P-256, `CN=Forgebox Local CA <hostname> <year>`, `O=Forgebox`,
//!   `OU=created <RFC 3339 instant>` (tells two CAs of the same year apart), 10 years,
//!   `basicConstraints` CA with `pathLenConstraint = 0` (it signs leaves only, never another
//!   CA), `keyUsage` `keyCertSign | cRLSign`, subject key identifier.
//! * **Name constraints** (critical): `permittedSubtrees` = one `dNSName` per configured TLD
//!   that is reserved for local use (`test`, plus `[dns] extra_tlds` such as `localhost`;
//!   a TLD that could be a real domain, like `.dev`, is left out), `localhost` always,
//!   `iPAddress` `127.0.0.0/8` and `::1/128`. Even with the key, nothing outside those
//!   names validates (`google.com` fails in every RFC 5280 verifier). DNS subtrees are
//!   written without a leading dot, the RFC form understood by CryptoAPI, NSS, OpenSSL,
//!   BoringSSL and webpki alike.
//! * Files: `certs/ca/forgebox-ca.crt` (PEM) and `certs/ca/forgebox-ca.key` (PKCS#8 PEM,
//!   DACL with the current user only, set before the key is written).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

use forge_core::Timestamp;
use forge_core::dns_name;
use forge_core::error::{ForgeError, Result};
use forge_core::platform::{CertThumbprint, FileAcl};
use jiff::SignedDuration;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, GeneralSubtree, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, NameConstraints, PKCS_ECDSA_P256_SHA256,
};
use serde::Serialize;

use crate::cert::{
    self, CertSummary, PermittedSubtree, pem_to_der, read_text, recover_pair, summarize, write_pair,
};

/// `certs/ca/forgebox-ca.crt`.
pub const CA_CERT_FILE: &str = "forgebox-ca.crt";
/// `certs/ca/forgebox-ca.key`.
pub const CA_KEY_FILE: &str = "forgebox-ca.key";
/// Validity of a new CA: 10 years.
pub const CA_VALIDITY: SignedDuration = SignedDuration::from_hours(3650 * 24);
/// How far before `not_before` "now" is placed, against clock skew between the machine,
/// VMs and WSL.
pub const BACKDATE: SignedDuration = SignedDuration::from_hours(1);

/// How to create a CA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaOptions {
    /// `Forgebox Local CA <hostname> <year>`.
    pub common_name: String,
    /// Configured TLDs with the leading dot (`.test`, `.localhost`).
    pub tlds: Vec<String>,
    pub validity: SignedDuration,
    pub now: Timestamp,
}

impl CaOptions {
    /// The CA of this machine: standard name and validity.
    pub fn for_this_machine(tlds: Vec<String>, now: Timestamp) -> Self {
        Self {
            common_name: default_common_name(&machine_name(), now),
            tlds,
            validity: CA_VALIDITY,
            now,
        }
    }
}

/// `Forgebox Local CA <hostname> <year>` (docs/06 §6.1).
pub fn default_common_name(hostname: &str, now: Timestamp) -> String {
    let year = now.to_zoned(jiff::tz::TimeZone::UTC).year();
    format!("Forgebox Local CA {hostname} {year}")
}

/// Name of this computer (`COMPUTERNAME` on Windows, `HOSTNAME` elsewhere).
pub fn machine_name() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .map(|name| name.trim().to_owned())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "localhost".to_owned())
}

/// The permitted subtrees for `tlds`, and the TLDs left out because they are not reserved
/// for local use (`dns_name::LOCAL_USE_SUFFIXES`) or are malformed.
pub fn permitted_subtrees(tlds: &[String]) -> (Vec<PermittedSubtree>, Vec<String>) {
    let mut out: Vec<PermittedSubtree> = Vec::new();
    let mut skipped = Vec::new();
    let mut push = |subtree: PermittedSubtree| {
        if !out.contains(&subtree) {
            out.push(subtree);
        }
    };
    for tld in tlds {
        match dns_name::parse_suffix(tld) {
            Ok(name) if dns_name::is_local_use(name) => push(PermittedSubtree::dns(name)),
            _ => skipped.push(tld.clone()),
        }
    }
    push(PermittedSubtree::dns("localhost"));
    push(PermittedSubtree::ip(
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 0)),
        8,
    ));
    push(PermittedSubtree::ip(IpAddr::V6(Ipv6Addr::LOCALHOST), 128));
    (out, skipped)
}

fn rcgen_subtree(subtree: &PermittedSubtree) -> GeneralSubtree {
    match subtree {
        PermittedSubtree::Dns { name } => GeneralSubtree::DnsName(name.clone()),
        PermittedSubtree::Ip { network, prefix } => {
            GeneralSubtree::IpAddress(rcgen::CidrSubnet::from_addr_prefix(*network, *prefix))
        }
    }
}

fn rcgen_err(what: &str, err: rcgen::Error) -> ForgeError {
    ForgeError::Internal(format!("{what}: {err}"))
}

/// Public facts about the CA (no key material).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaInfo {
    pub subject: String,
    pub common_name: String,
    /// Windows thumbprint (`certmgr`, `Cert:\`, helper operations).
    pub thumbprint_sha1: CertThumbprint,
    pub thumbprint_sha256: String,
    pub serial: String,
    pub not_before: Timestamp,
    pub not_after: Timestamp,
    pub permitted: Vec<PermittedSubtree>,
}

impl CaInfo {
    fn from_summary(summary: CertSummary) -> Result<Self> {
        if !summary.is_ca {
            return Err(ForgeError::ConfigInvalid(
                "the local CA certificate is not a CA (basicConstraints)".to_owned(),
            ));
        }
        Ok(Self {
            common_name: summary.common_name.unwrap_or_default(),
            subject: summary.subject,
            thumbprint_sha1: summary.thumbprint_sha1,
            thumbprint_sha256: summary.thumbprint_sha256,
            serial: summary.serial,
            not_before: summary.not_before,
            not_after: summary.not_after,
            permitted: summary.permitted,
        })
    }

    /// The name constraints as shown to people (`.test`, `.localhost`, `127.0.0.0/8`,
    /// `::1/128`).
    pub fn name_constraints(&self) -> Vec<String> {
        self.permitted.iter().map(ToString::to_string).collect()
    }

    /// Whether the DNS name `host` (lowercase, no wildcard) is inside the constraints.
    /// A CA without the extension permits nothing: Forgebox never issues under such a CA.
    pub fn permits_dns(&self, host: &str) -> bool {
        self.permitted.iter().any(|s| s.permits_dns(host))
    }

    /// Configured TLDs the CA cannot sign for (added after it was created): rotating the
    /// CA includes them.
    pub fn missing_tlds(&self, tlds: &[String]) -> Vec<String> {
        let (wanted, _) = permitted_subtrees(tlds);
        wanted
            .into_iter()
            .filter(|s| matches!(s, PermittedSubtree::Dns { .. }) && !self.permitted.contains(s))
            .map(|s| s.to_string())
            .collect()
    }
}

/// The CA with its private key (lives only in the daemon).
pub struct LocalCa {
    cert_der: Vec<u8>,
    cert_pem: String,
    key: KeyPair,
    info: CaInfo,
}

impl std::fmt::Debug for LocalCa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalCa")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// `certs/ca/forgebox-ca.crt` and `.key` under `dir`.
pub fn ca_paths(dir: &Path) -> (PathBuf, PathBuf) {
    (dir.join(CA_CERT_FILE), dir.join(CA_KEY_FILE))
}

impl LocalCa {
    /// Creates a new CA (in memory; [`LocalCa::save`] writes it).
    pub fn generate(opts: &CaOptions) -> Result<Self> {
        let (permitted, skipped) = permitted_subtrees(&opts.tlds);
        if !skipped.is_empty() {
            tracing::warn!(
                ?skipped,
                "tls: TLDs not reserved for local use are left out of the CA name constraints"
            );
        }
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
            .map_err(|e| rcgen_err("generating the CA key", e))?;
        let not_before = opts.now - BACKDATE;
        let not_after = not_before + opts.validity;
        let mut params = CertificateParams::default();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, opts.common_name.clone());
        params
            .distinguished_name
            .push(DnType::OrganizationName, "Forgebox");
        // Tells CAs of the same machine and year apart (after a rotation both may be listed
        // for a while): people see it in certmgr, verifiers that match issuers by name only
        // pick the right one.
        params.distinguished_name.push(
            DnType::OrganizationalUnitName,
            format!("created {}", opts.now.strftime("%Y-%m-%dT%H:%M:%SZ")),
        );
        params.not_before = cert::to_offset(not_before)?;
        params.not_after = cert::to_offset(not_after)?;
        params.serial_number = Some(cert::random_serial()?);
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.name_constraints = Some(NameConstraints {
            permitted_subtrees: permitted.iter().map(rcgen_subtree).collect(),
            excluded_subtrees: Vec::new(),
        });
        let cert = params
            .self_signed(&key)
            .map_err(|e| rcgen_err("signing the CA certificate", e))?;
        Self::from_parts(cert.der().to_vec(), key)
    }

    fn from_parts(cert_der: Vec<u8>, key: KeyPair) -> Result<Self> {
        let summary = summarize(&cert_der)?;
        if summary.public_key != key.public_key_raw() {
            return Err(ForgeError::ConfigInvalid(
                "the CA private key does not match the CA certificate".to_owned(),
            ));
        }
        let info = CaInfo::from_summary(summary)?;
        Ok(Self {
            cert_pem: cert::der_to_pem(&cert_der),
            cert_der,
            key,
            info,
        })
    }

    /// A CA from its PEM certificate and PKCS#8 PEM key. `config_invalid` when the key does
    /// not belong to the certificate or the certificate is not a CA.
    pub fn from_pem(cert_pem: &str, key_pem: &str) -> Result<Self> {
        let der = pem_to_der(cert_pem)?;
        let key = KeyPair::from_pem(key_pem)
            .map_err(|e| ForgeError::ConfigInvalid(format!("the CA private key: {e}")))?;
        Self::from_parts(der, key)
    }

    /// Reads `certs/ca/` (`dir`). `Ok(None)` when there is no CA yet. An interrupted write
    /// is finished or discarded first. Blocking.
    pub fn load(dir: &Path) -> Result<Option<Self>> {
        let (cert_path, key_path) = ca_paths(dir);
        if key_path.exists() {
            recover_pair(&cert_path, &key_path)?;
        }
        match (cert_path.exists(), key_path.exists()) {
            (false, false) => Ok(None),
            (true, true) => {
                Self::from_pem(&read_text(&cert_path)?, &read_text(&key_path)?).map(Some)
            }
            (true, false) => Err(ForgeError::ConfigInvalid(format!(
                "{} exists without its private key {}",
                cert_path.display(),
                key_path.display()
            ))),
            (false, true) => Err(ForgeError::ConfigInvalid(format!(
                "{} exists without its certificate {}",
                key_path.display(),
                cert_path.display()
            ))),
        }
    }

    /// Writes the certificate and the key (owner-only DACL) to `dir`, replacing a previous
    /// CA. Blocking.
    pub fn save(&self, dir: &Path, acl: &dyn FileAcl) -> Result<()> {
        let (cert_path, key_path) = ca_paths(dir);
        write_pair(
            &cert_path,
            &self.cert_pem,
            &key_path,
            &cert::lf(&self.key.serialize_pem()),
            acl,
        )
    }

    pub fn info(&self) -> &CaInfo {
        &self.info
    }

    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// The certificate in PEM (LF), as `tls.ca_export` returns it.
    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// The signing side for leaves. Uses the subject and key identifier of the stored
    /// certificate, so the leaf's issuer and authority key identifier match it byte for
    /// byte.
    pub(crate) fn issuer(&self) -> Result<Issuer<'static, &KeyPair>> {
        Issuer::from_ca_cert_der(&self.cert_der.as_slice().into(), &self.key)
            .map_err(|e| rcgen_err("reading the CA certificate", e))
    }

    /// The private key in PKCS#8 PEM. Only for tests and the CA files: the API never
    /// returns it (docs/11 §3).
    pub fn key_pem(&self) -> String {
        cert::lf(&self.key.serialize_pem())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Timestamp {
        "2026-10-27T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn subtrees_keep_local_tlds_only() {
        let tlds = vec![
            ".test".to_owned(),
            ".dev".to_owned(),
            ".localhost".to_owned(),
        ];
        let (permitted, skipped) = permitted_subtrees(&tlds);
        let shown: Vec<String> = permitted.iter().map(ToString::to_string).collect();
        assert_eq!(shown, [".test", ".localhost", "127.0.0.0/8", "::1/128"]);
        assert_eq!(skipped, [".dev"]);
    }

    #[test]
    fn common_name_has_host_and_year() {
        assert_eq!(
            default_common_name("DEVBOX", now()),
            "Forgebox Local CA DEVBOX 2026"
        );
        assert!(!machine_name().is_empty());
    }

    #[test]
    fn generated_ca_round_trips_through_pem() {
        let opts = CaOptions {
            common_name: "Forgebox Local CA test 2026".to_owned(),
            tlds: vec![".test".to_owned()],
            validity: CA_VALIDITY,
            now: now(),
        };
        let ca = LocalCa::generate(&opts).unwrap();
        let info = ca.info();
        assert_eq!(info.common_name, "Forgebox Local CA test 2026");
        assert_eq!(info.not_before, now() - BACKDATE);
        assert_eq!(info.not_after, info.not_before + CA_VALIDITY);
        assert!(info.permits_dns("loja.test"));
        assert!(info.permits_dns("app.localhost"));
        assert!(!info.permits_dns("google.com"));
        assert_eq!(
            info.missing_tlds(&[".test".to_owned()]),
            Vec::<String>::new()
        );
        assert_eq!(
            info.missing_tlds(&[".test".to_owned(), ".internal".to_owned()]),
            [".internal"]
        );
        let back = LocalCa::from_pem(ca.cert_pem(), &ca.key_pem()).unwrap();
        assert_eq!(back.info(), info);
        assert!(!ca.key_pem().contains('\r'));

        let other = LocalCa::generate(&opts).unwrap();
        let err = LocalCa::from_pem(ca.cert_pem(), &other.key_pem()).unwrap_err();
        assert_eq!(err.code(), "config_invalid");
        assert_ne!(other.info().serial, info.serial, "random serials");
    }
}
