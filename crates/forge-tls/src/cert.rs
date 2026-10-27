//! Shared certificate plumbing: PEM, parsing back what rcgen wrote, thumbprints, time
//! conversions and the owner-only key files.

use std::fmt;
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use forge_core::Timestamp;
use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::platform::{CertThumbprint, FileAcl};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x509_parser::certificate::X509Certificate;
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::FromDer;

/// A permitted subtree of the CA name constraints (RFC 5280 §4.2.1.10).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PermittedSubtree {
    /// `dNSName`: the name itself and every name below it (`test` covers `loja.test`).
    Dns { name: String },
    /// `iPAddress`: a network (`127.0.0.0/8`, `::1/128`).
    Ip { network: IpAddr, prefix: u8 },
}

impl PermittedSubtree {
    pub fn dns(name: impl Into<String>) -> Self {
        Self::Dns { name: name.into() }
    }

    pub fn ip(network: IpAddr, prefix: u8) -> Self {
        Self::Ip { network, prefix }
    }

    /// Whether the DNS name `host` (lowercase, no wildcard) falls in this subtree.
    pub fn permits_dns(&self, host: &str) -> bool {
        match self {
            Self::Dns { name } => forge_core::dns_name::is_within(host, name),
            Self::Ip { .. } => false,
        }
    }

    /// Whether `addr` falls in this subtree.
    pub fn permits_ip(&self, addr: IpAddr) -> bool {
        let Self::Ip { network, prefix } = self else {
            return false;
        };
        match (network, addr) {
            (IpAddr::V4(net), IpAddr::V4(a)) => {
                let mask = u32::MAX.checked_shl(32 - u32::from(*prefix)).unwrap_or(0);
                u32::from(*net) & mask == u32::from(a) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(a)) => {
                let mask = u128::MAX.checked_shl(128 - u32::from(*prefix)).unwrap_or(0);
                u128::from(*net) & mask == u128::from(a) & mask
            }
            _ => false,
        }
    }
}

impl fmt::Display for PermittedSubtree {
    /// `.test`, `.localhost`, `127.0.0.0/8`, `::1/128` (docs/11 §3 notation).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dns { name } => write!(f, ".{name}"),
            Self::Ip { network, prefix } => write!(f, "{network}/{prefix}"),
        }
    }
}

/// What Forgebox needs to know about a certificate, read back from its DER.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CertSummary {
    pub subject: String,
    pub common_name: Option<String>,
    pub serial: String,
    pub not_before: Timestamp,
    pub not_after: Timestamp,
    pub is_ca: bool,
    pub dns_names: Vec<String>,
    pub ip_addresses: Vec<IpAddr>,
    /// Permitted subtrees of the name constraints (empty without the extension).
    pub permitted: Vec<PermittedSubtree>,
    pub name_constraints_critical: bool,
    pub thumbprint_sha1: CertThumbprint,
    pub thumbprint_sha256: String,
    /// Raw subject public key (uncompressed EC point for P-256).
    pub public_key: Vec<u8>,
}

fn bad_cert(what: &str, err: impl fmt::Display) -> ForgeError {
    ForgeError::InvalidInput(format!("{what}: {err}"))
}

/// Lowercase hex SHA-256 of `bytes` (the API's `thumbprint_sha256`).
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn timestamp(seconds: i64) -> Result<Timestamp> {
    Timestamp::from_second(seconds)
        .map_err(|e| ForgeError::InvalidInput(format!("certificate date out of range: {e}")))
}

/// Parses one DER certificate.
pub(crate) fn summarize(der: &[u8]) -> Result<CertSummary> {
    let (_, cert) = X509Certificate::from_der(der).map_err(|e| bad_cert("certificate", e))?;
    let mut dns_names = Vec::new();
    let mut ip_addresses = Vec::new();
    if let Some(san) = cert
        .subject_alternative_name()
        .map_err(|e| bad_cert("subjectAltName", e))?
    {
        for name in &san.value.general_names {
            match name {
                GeneralName::DNSName(dns) => dns_names.push((*dns).to_owned()),
                GeneralName::IPAddress(bytes) => {
                    if let Some(ip) = ip_from_bytes(bytes) {
                        ip_addresses.push(ip);
                    }
                }
                _ => {}
            }
        }
    }
    let mut permitted = Vec::new();
    let mut name_constraints_critical = false;
    if let Some(nc) = cert
        .name_constraints()
        .map_err(|e| bad_cert("nameConstraints", e))?
    {
        name_constraints_critical = nc.critical;
        for subtree in nc.value.permitted_subtrees.iter().flatten() {
            match &subtree.base {
                GeneralName::DNSName(name) => permitted.push(PermittedSubtree::dns(*name)),
                GeneralName::IPAddress(bytes) => {
                    if let Some(net) = network_from_bytes(bytes) {
                        permitted.push(net);
                    }
                }
                _ => {}
            }
        }
    }
    let common_name = cert
        .subject()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map(str::to_owned);
    Ok(CertSummary {
        subject: cert.subject().to_string(),
        common_name,
        serial: cert.raw_serial_as_string(),
        not_before: timestamp(cert.validity().not_before.timestamp())?,
        not_after: timestamp(cert.validity().not_after.timestamp())?,
        is_ca: cert.is_ca(),
        dns_names,
        ip_addresses,
        permitted,
        name_constraints_critical,
        thumbprint_sha1: CertThumbprint::of_der(der),
        thumbprint_sha256: sha256_hex(der),
        public_key: cert.public_key().subject_public_key.data.to_vec(),
    })
}

/// Whether `leaf_der` carries a valid signature by the key of `issuer_der`.
pub(crate) fn signed_by(leaf_der: &[u8], issuer_der: &[u8]) -> bool {
    let (Ok((_, leaf)), Ok((_, issuer))) = (
        X509Certificate::from_der(leaf_der),
        X509Certificate::from_der(issuer_der),
    ) else {
        return false;
    };
    leaf.verify_signature(Some(issuer.public_key())).is_ok()
}

fn ip_from_bytes(bytes: &[u8]) -> Option<IpAddr> {
    match bytes.len() {
        4 => Some(IpAddr::from(<[u8; 4]>::try_from(bytes).ok()?)),
        16 => Some(IpAddr::from(<[u8; 16]>::try_from(bytes).ok()?)),
        _ => None,
    }
}

/// An `iPAddress` name constraint: address followed by mask (8 or 32 bytes).
fn network_from_bytes(bytes: &[u8]) -> Option<PermittedSubtree> {
    let half = bytes.len() / 2;
    if bytes.len() != 8 && bytes.len() != 32 {
        return None;
    }
    let network = ip_from_bytes(&bytes[..half])?;
    let prefix = bytes[half..].iter().map(|b| b.count_ones()).sum::<u32>();
    Some(PermittedSubtree::ip(network, prefix as u8))
}

/// DER of the first `CERTIFICATE` block of `pem`.
pub(crate) fn pem_to_der(pem: &str) -> Result<Vec<u8>> {
    let (_, block) = x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .map_err(|e| bad_cert("PEM certificate", e))?;
    if block.label != "CERTIFICATE" {
        return Err(ForgeError::InvalidInput(format!(
            "expected a CERTIFICATE PEM block, found {}",
            block.label
        )));
    }
    Ok(block.contents)
}

/// PEM of a DER certificate, LF line endings.
pub(crate) fn der_to_pem(der: &[u8]) -> String {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
    for line in encoded.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        pem.push('\n');
    }
    pem.push_str("-----END CERTIFICATE-----\n");
    pem
}

/// `text` with LF line endings (generated files are LF, docs/15 §7).
pub(crate) fn lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// `jiff` instant as rcgen's `time` type.
pub(crate) fn to_offset(ts: Timestamp) -> Result<time::OffsetDateTime> {
    time::OffsetDateTime::from_unix_timestamp(ts.as_second())
        .map_err(|e| ForgeError::InvalidInput(format!("date out of range: {e}")))
}

/// 16 random bytes as a positive serial number.
pub(crate) fn random_serial() -> Result<rcgen::SerialNumber> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| ForgeError::Internal(format!("OS random generator: {e}")))?;
    // Positive and without a leading zero byte, so the DER INTEGER keeps 16 bytes.
    bytes[0] = (bytes[0] & 0x7F) | 0x40;
    Ok(rcgen::SerialNumber::from_slice(&bytes))
}

/// `<path>.tmp`.
pub(crate) fn tmp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

/// Writes `bytes` to `<path>.tmp` and flushes it. With `acl`, the empty file is restricted
/// to the current user **before** the secret is written (it is never readable by anyone
/// else, not even for an instant).
pub(crate) fn write_tmp(path: &Path, bytes: &[u8], acl: Option<&dyn FileAcl>) -> Result<PathBuf> {
    let tmp = tmp_path(path);
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(ForgeError::io_at("removing", &tmp, e)),
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .at("creating", &tmp)?;
    if let Some(acl) = acl {
        if let Err(err) = acl.restrict_to_current_user(&tmp) {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            return Err(err);
        }
    }
    file.write_all(bytes).at("writing", &tmp)?;
    file.sync_all().at("flushing", &tmp)?;
    Ok(tmp)
}

/// Renames `tmp` over `path` (the DACL of `tmp` travels with it).
pub(crate) fn commit_tmp(tmp: &Path, path: &Path) -> Result<()> {
    std::fs::rename(tmp, path).at("replacing", path)
}

/// Writes a certificate and its key so that a reader never sees a key without its
/// certificate being complete on disk: both go to `.tmp` files (the key one restricted to
/// the user before anything is written), then the key and finally the certificate are
/// renamed into place. A crash between the two renames leaves `<cert>.tmp` matching the
/// new key; [`recover_pair`] finishes the job.
pub(crate) fn write_pair(
    cert_path: &Path,
    cert_pem: &str,
    key_path: &Path,
    key_pem: &str,
    acl: &dyn FileAcl,
) -> Result<()> {
    if let Some(dir) = cert_path.parent() {
        std::fs::create_dir_all(dir).at("creating", dir)?;
    }
    let key_tmp = write_tmp(key_path, key_pem.as_bytes(), Some(acl))?;
    let cert_tmp = match write_tmp(cert_path, cert_pem.as_bytes(), None) {
        Ok(tmp) => tmp,
        Err(err) => {
            let _ = std::fs::remove_file(&key_tmp);
            return Err(err);
        }
    };
    commit_tmp(&key_tmp, key_path)?;
    commit_tmp(&cert_tmp, cert_path)
}

/// Finishes or discards an interrupted [`write_pair`]: a leftover `<key>.tmp` means the
/// old pair is intact (removed); a leftover `<cert>.tmp` whose public key is the one of
/// `key_pem_on_disk` completes the rename, otherwise it is removed.
pub(crate) fn recover_pair(cert_path: &Path, key_path: &Path) -> Result<()> {
    let key_tmp = tmp_path(key_path);
    if key_tmp.exists() {
        tracing::warn!(path = %key_tmp.display(), "tls: removing an unfinished key file");
        std::fs::remove_file(&key_tmp).at("removing", &key_tmp)?;
    }
    let cert_tmp = tmp_path(cert_path);
    if !cert_tmp.exists() {
        return Ok(());
    }
    let matches = (|| -> Result<bool> {
        let key = rcgen::KeyPair::from_pem(&read_text(key_path)?)
            .map_err(|e| bad_cert("private key", e))?;
        let cert = summarize(&pem_to_der(&read_text(&cert_tmp)?)?)?;
        Ok(cert.public_key == key.public_key_raw())
    })()
    .unwrap_or(false);
    if matches {
        tracing::warn!(path = %cert_path.display(), "tls: completing an interrupted certificate write");
        commit_tmp(&cert_tmp, cert_path)
    } else {
        tracing::warn!(path = %cert_tmp.display(), "tls: removing an unfinished certificate file");
        std::fs::remove_file(&cert_tmp).at("removing", &cert_tmp)
    }
}

/// Reads a UTF-8 file.
pub(crate) fn read_text(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).at("reading", path)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    #[test]
    fn subtrees_match_like_rfc5280() {
        let test = PermittedSubtree::dns("test");
        assert!(test.permits_dns("loja.test"));
        assert!(test.permits_dns("a.b.test"));
        assert!(test.permits_dns("test"));
        assert!(!test.permits_dns("latest"));
        assert!(!test.permits_dns("test.com"));
        assert_eq!(test.to_string(), ".test");

        let lo = PermittedSubtree::ip(Ipv4Addr::new(127, 0, 0, 0).into(), 8);
        assert!(lo.permits_ip(Ipv4Addr::new(127, 1, 2, 3).into()));
        assert!(!lo.permits_ip(Ipv4Addr::new(10, 0, 0, 1).into()));
        assert!(!lo.permits_ip(Ipv6Addr::LOCALHOST.into()));
        assert_eq!(lo.to_string(), "127.0.0.0/8");
        let v6 = PermittedSubtree::ip(Ipv6Addr::LOCALHOST.into(), 128);
        assert!(v6.permits_ip(Ipv6Addr::LOCALHOST.into()));
        assert!(!v6.permits_ip("::2".parse().unwrap()));
        assert_eq!(v6.to_string(), "::1/128");
        assert!(!lo.permits_dns("loja.test"));
    }

    #[test]
    fn network_bytes_decode() {
        let v4 = network_from_bytes(&[127, 0, 0, 0, 255, 0, 0, 0]).unwrap();
        assert_eq!(
            v4,
            PermittedSubtree::ip(Ipv4Addr::new(127, 0, 0, 0).into(), 8)
        );
        let mut v6 = [0_u8; 32];
        v6[15] = 1;
        v6[16..].fill(0xFF);
        assert_eq!(
            network_from_bytes(&v6).unwrap(),
            PermittedSubtree::ip(Ipv6Addr::LOCALHOST.into(), 128)
        );
        assert!(network_from_bytes(&[1, 2, 3]).is_none());
    }

    #[test]
    fn pem_round_trips() {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["a.test".to_owned()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let ours = der_to_pem(cert.der());
        assert_eq!(ours, lf(&cert.pem()), "same text as the pem crate, LF");
        assert_eq!(pem_to_der(&ours).unwrap(), cert.der().to_vec());
        assert!(!ours.contains('\r'));
        let summary = summarize(cert.der()).unwrap();
        assert_eq!(summary.dns_names, ["a.test"]);
        assert_eq!(summary.public_key, key.public_key_raw());
        assert_eq!(summary.thumbprint_sha256.len(), 64);
        assert!(signed_by(cert.der(), cert.der()));
    }

    #[test]
    fn sha256_is_lowercase_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
