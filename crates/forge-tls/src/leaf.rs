//! Site certificates (docs/06 §6.2).
//!
//! * One per site: SAN = host + `*.host` + aliases, `CN` = host, ECDSA P-256, `keyUsage`
//!   `digitalSignature`, `extendedKeyUsage` `serverAuth`, `basicConstraints` CA:FALSE,
//!   authority key identifier of the CA, random serial, 825 days (Apple/Chrome limit for
//!   TLS server certificates, counted from the backdated `notBefore`).
//! * Every name must be inside the CA name constraints; the host must be **below** a TLD
//!   (`loja.test`, never `test`, so no certificate covers `*.test`).
//! * Files: `certs/sites/<host>.crt` (the leaf only, PEM) and `certs/sites/<host>.key`
//!   (PKCS#8 PEM, owner-only DACL), the paths `forge_sites::vhost` puts in the
//!   server configs.
//! * Renewal: when less than [`RENEW_BEFORE`] (30 days) is left, when the CA changed or
//!   when the key file does not match ([`crate::manager`]).

use std::path::{Path, PathBuf};

use forge_core::Timestamp;
use forge_core::dns_name;
use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::ids::SiteId;
use forge_core::platform::FileAcl;
use jiff::SignedDuration;
use rcgen::{
    CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
    PKCS_ECDSA_P256_SHA256, SanType,
};
use serde::Serialize;

use crate::ca::{BACKDATE, LocalCa};
use crate::cert::{self, pem_to_der, read_text, recover_pair, summarize, write_pair};

/// Validity of a site certificate: 825 days.
pub const LEAF_VALIDITY: SignedDuration = SignedDuration::from_hours(825 * 24);
/// Site certificates are renewed when less than this is left: 30 days.
pub const RENEW_BEFORE: SignedDuration = SignedDuration::from_hours(30 * 24);

/// What to issue a certificate for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafRequest {
    /// Main host of the site (`loja.test`).
    pub host: String,
    /// More names (`api.loja.test`, `loja.localhost`, `*.cdn.test`).
    pub aliases: Vec<String>,
    /// Site the certificate belongs to, carried in the events.
    pub site_id: Option<SiteId>,
}

impl LeafRequest {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            aliases: Vec::new(),
            site_id: None,
        }
    }

    pub fn with_aliases<I, S>(mut self, aliases: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.aliases = aliases.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_site(mut self, site_id: SiteId) -> Self {
        self.site_id = Some(site_id);
        self
    }

    /// The host (normalized) and the SAN list: host, `*.host`, then the aliases, without
    /// duplicates. Names are lowercased; a trailing dot is dropped.
    pub fn sans(&self) -> Result<(String, Vec<String>)> {
        let host = normalize(&self.host, false)?;
        let mut sans = vec![host.clone(), format!("*.{host}")];
        for alias in &self.aliases {
            let alias = normalize(alias, true)?;
            if !sans.contains(&alias) {
                sans.push(alias);
            }
        }
        Ok((host, sans))
    }
}

/// Lowercase, no trailing dot, valid DNS syntax; a leading `*.` only when `wildcard`.
fn normalize(name: &str, wildcard: bool) -> Result<String> {
    let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
    let bare = match name.strip_prefix("*.") {
        Some(rest) if wildcard => rest,
        Some(_) => {
            return Err(ForgeError::InvalidInput(format!(
                "{name:?}: the site host cannot be a wildcard"
            )));
        }
        None => &name,
    };
    dns_name::check_name(bare).map_err(ForgeError::InvalidInput)?;
    Ok(name)
}

/// Checks that every SAN is inside the CA constraints and that `host` is below a TLD.
pub(crate) fn check_against_ca(ca: &LocalCa, host: &str, sans: &[String]) -> Result<()> {
    let info = ca.info();
    let below_tld = info.permitted.iter().any(|s| {
        matches!(s, crate::cert::PermittedSubtree::Dns { name } if host != name && s.permits_dns(host))
    });
    if !below_tld {
        return Err(ForgeError::InvalidInput(format!(
            "{host} is not below a TLD of the local CA ({}); a new TLD needs `forge tls ca rotate`",
            info.name_constraints().join(", ")
        )));
    }
    for san in sans {
        let bare = san.strip_prefix("*.").unwrap_or(san);
        if !info.permits_dns(bare) {
            return Err(ForgeError::InvalidInput(format!(
                "{san} is outside the name constraints of the local CA ({})",
                info.name_constraints().join(", ")
            )));
        }
    }
    Ok(())
}

/// Public facts about a site certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeafInfo {
    pub host: String,
    pub sans: Vec<String>,
    pub serial: String,
    pub not_before: Timestamp,
    pub not_after: Timestamp,
    pub thumbprint_sha256: String,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

impl LeafInfo {
    /// Whether less than `window` is left before `not_after` at `now`.
    pub fn expires_within(&self, now: Timestamp, window: SignedDuration) -> bool {
        self.not_after.duration_since(now) < window
    }
}

/// `certs/sites/<host>.crt` and `.key` under `dir`.
pub fn leaf_paths(dir: &Path, host: &str) -> (PathBuf, PathBuf) {
    (
        dir.join(format!("{host}.crt")),
        dir.join(format!("{host}.key")),
    )
}

/// A freshly issued certificate with its key, not yet on disk.
pub struct SiteCert {
    info: LeafInfo,
    cert_der: Vec<u8>,
    cert_pem: String,
    key_pem: String,
}

impl std::fmt::Debug for SiteCert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteCert")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl SiteCert {
    pub fn info(&self) -> &LeafInfo {
        &self.info
    }

    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    pub fn key_pem(&self) -> &str {
        &self.key_pem
    }

    /// Writes `<host>.crt` and `<host>.key` (owner-only DACL). Blocking.
    pub fn save(&self, acl: &dyn FileAcl) -> Result<()> {
        write_pair(
            &self.info.cert_path,
            &self.cert_pem,
            &self.info.key_path,
            &self.key_pem,
            acl,
        )
    }
}

/// Issues a certificate for `host` with `sans` (from [`LeafRequest::sans`]) signed by `ca`,
/// valid for `validity` from `now - 1 h`, to be stored in `sites_dir`.
pub fn issue_leaf(
    ca: &LocalCa,
    sites_dir: &Path,
    host: &str,
    sans: &[String],
    validity: SignedDuration,
    now: Timestamp,
) -> Result<SiteCert> {
    check_against_ca(ca, host, sans)?;
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .map_err(|e| ForgeError::Internal(format!("generating a site key: {e}")))?;
    let not_before = now - BACKDATE;
    let mut params = CertificateParams::default();
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.distinguished_name.push(DnType::CommonName, host);
    params
        .distinguished_name
        .push(DnType::OrganizationName, "Forgebox");
    params.subject_alt_names = sans
        .iter()
        .map(|san| {
            san.as_str()
                .try_into()
                .map(SanType::DnsName)
                .map_err(|e| ForgeError::InvalidInput(format!("{san}: {e}")))
        })
        .collect::<Result<_>>()?;
    params.not_before = cert::to_offset(not_before)?;
    params.not_after = cert::to_offset(not_before + validity)?;
    params.serial_number = Some(cert::random_serial()?);
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
    let issuer = ca.issuer()?;
    let leaf = params
        .signed_by(&key, &issuer)
        .map_err(|e| ForgeError::Internal(format!("signing the certificate of {host}: {e}")))?;
    let cert_der = leaf.der().to_vec();
    let summary = summarize(&cert_der)?;
    let (cert_path, key_path) = leaf_paths(sites_dir, host);
    Ok(SiteCert {
        info: LeafInfo {
            host: host.to_owned(),
            sans: summary.dns_names,
            serial: summary.serial,
            not_before: summary.not_before,
            not_after: summary.not_after,
            thumbprint_sha256: summary.thumbprint_sha256,
            cert_path,
            key_path,
        },
        cert_pem: cert::der_to_pem(&cert_der),
        cert_der,
        key_pem: cert::lf(&key.serialize_pem()),
    })
}

/// A site certificate read from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredLeaf {
    pub info: LeafInfo,
    pub cert_der: Vec<u8>,
    /// The key file exists and belongs to the certificate.
    pub key_ok: bool,
}

impl StoredLeaf {
    /// Whether the certificate carries a valid signature of the CA `ca_der`.
    pub fn signed_by(&self, ca_der: &[u8]) -> bool {
        cert::signed_by(&self.cert_der, ca_der)
    }
}

/// Reads `<host>.crt`/`.key` from `sites_dir` (finishing an interrupted write first).
/// `Ok(None)` without a certificate. Blocking.
pub fn load_leaf(sites_dir: &Path, host: &str) -> Result<Option<StoredLeaf>> {
    let (cert_path, key_path) = leaf_paths(sites_dir, host);
    if key_path.exists() {
        recover_pair(&cert_path, &key_path)?;
    }
    if !cert_path.exists() {
        return Ok(None);
    }
    let cert_der = pem_to_der(&read_text(&cert_path)?)?;
    let summary = summarize(&cert_der)?;
    let key_ok = read_text(&key_path)
        .ok()
        .and_then(|pem| KeyPair::from_pem(&pem).ok())
        .is_some_and(|key| key.public_key_raw() == summary.public_key.as_slice());
    Ok(Some(StoredLeaf {
        info: LeafInfo {
            host: host.to_owned(),
            sans: summary.dns_names,
            serial: summary.serial,
            not_before: summary.not_before,
            not_after: summary.not_after,
            thumbprint_sha256: summary.thumbprint_sha256,
            cert_path,
            key_path,
        },
        cert_der,
        key_ok,
    }))
}

/// Every site certificate in `sites_dir` (`<host>.crt` with a valid host name), sorted by
/// host, plus the files that could not be read (`file: reason`). Blocking.
pub fn scan_leaves(sites_dir: &Path) -> Result<(Vec<StoredLeaf>, Vec<String>)> {
    let entries = match std::fs::read_dir(sites_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), Vec::new())),
        Err(e) => return Err(ForgeError::io_at("listing", sites_dir, e)),
    };
    let mut hosts = Vec::new();
    for entry in entries {
        let entry = entry.at("listing", sites_dir)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(host) = name.strip_suffix(".crt") else {
            continue;
        };
        if dns_name::check_name(host).is_ok() {
            hosts.push(host.to_owned());
        }
    }
    hosts.sort();
    let mut leaves = Vec::new();
    let mut problems = Vec::new();
    for host in hosts {
        match load_leaf(sites_dir, &host) {
            Ok(Some(leaf)) => leaves.push(leaf),
            Ok(None) => {}
            Err(err) => problems.push(format!("{host}.crt: {err}")),
        }
    }
    Ok((leaves, problems))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ca::{CA_VALIDITY, CaOptions};

    fn now() -> Timestamp {
        "2026-10-27T12:00:00Z".parse().unwrap()
    }

    fn ca() -> LocalCa {
        LocalCa::generate(&CaOptions {
            common_name: "Forgebox Local CA unit 2026".to_owned(),
            tlds: vec![".test".to_owned()],
            validity: CA_VALIDITY,
            now: now(),
        })
        .unwrap()
    }

    #[test]
    fn sans_are_normalized() {
        let request = LeafRequest::new("Loja.Test.").with_aliases([
            "api.loja.test",
            "*.cdn.test",
            "loja.test",
        ]);
        let (host, sans) = request.sans().unwrap();
        assert_eq!(host, "loja.test");
        assert_eq!(
            sans,
            ["loja.test", "*.loja.test", "api.loja.test", "*.cdn.test"]
        );
        for bad in ["*.loja.test", "lo ja.test", "", "loja..test"] {
            assert!(LeafRequest::new(bad).sans().is_err(), "{bad:?}");
        }
        assert!(
            LeafRequest::new("loja.test")
                .with_aliases(["a_b.test"])
                .sans()
                .is_err()
        );
    }

    #[test]
    fn leaves_stay_inside_the_constraints() {
        let ca = ca();
        let dir = Path::new("unused");
        let leaf = issue_leaf(
            &ca,
            dir,
            "loja.test",
            &["loja.test".into(), "*.loja.test".into()],
            LEAF_VALIDITY,
            now(),
        )
        .unwrap();
        assert_eq!(leaf.info().sans, ["loja.test", "*.loja.test"]);
        assert_eq!(
            leaf.info().not_after.duration_since(leaf.info().not_before),
            LEAF_VALIDITY
        );
        assert!(cert::signed_by(leaf.cert_der(), ca.cert_der()));
        assert_eq!(leaf.info().cert_path, dir.join("loja.test.crt"));

        for (host, sans) in [
            ("google.com", vec!["google.com".to_owned()]),
            ("test", vec!["test".to_owned()]),
            (
                "loja.test",
                vec!["loja.test".to_owned(), "evil.example.com".to_owned()],
            ),
        ] {
            let err = issue_leaf(&ca, dir, host, &sans, LEAF_VALIDITY, now()).unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{host} {sans:?}: {err}");
        }
    }

    #[test]
    fn expiry_window() {
        let ca = ca();
        let leaf = issue_leaf(
            &ca,
            Path::new("x"),
            "a.test",
            &["a.test".into()],
            SignedDuration::from_hours(24 * 10),
            now(),
        )
        .unwrap();
        assert!(leaf.info().expires_within(now(), RENEW_BEFORE));
        let leaf = issue_leaf(
            &ca,
            Path::new("x"),
            "a.test",
            &["a.test".into()],
            LEAF_VALIDITY,
            now(),
        )
        .unwrap();
        assert!(!leaf.info().expires_within(now(), RENEW_BEFORE));
    }
}
