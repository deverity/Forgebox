//! Shared helpers of the forge-tls integration tests.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

use forge_core::error::{ForgeError, Result};
use forge_core::paths::ForgeRoot;
use forge_core::platform::{CertThumbprint, FileAcl, TrustScope, TrustStore};
use forge_tls::LocalCa;
use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, SanType};

/// A temporary Forgebox root with `certs/ca` and `certs/sites`.
pub struct TempRoot {
    pub dir: tempfile::TempDir,
    pub root: ForgeRoot,
}

pub fn temp_root() -> TempRoot {
    let dir = tempfile::tempdir().unwrap();
    let root = ForgeRoot::new(dir.path()).unwrap();
    std::fs::create_dir_all(root.certs_ca()).unwrap();
    std::fs::create_dir_all(root.certs_sites()).unwrap();
    TempRoot { dir, root }
}

/// Signs a server certificate for `dns`/`ips` with the CA key read from `certs/ca/`, as an
/// attacker holding a leaked key would, bypassing every check of `forge-tls`. Returns
/// `(certificate DER, PKCS#8 key DER)`.
pub fn forge_leaf(ca_dir: &Path, dns: &[&str], ips: &[IpAddr]) -> (Vec<u8>, Vec<u8>) {
    let ca = LocalCa::load(ca_dir).unwrap().expect("a CA on disk");
    let ca_key = KeyPair::from_pem(&ca.key_pem()).unwrap();
    let issuer = rcgen::Issuer::from_ca_cert_der(&ca.cert_der().into(), &ca_key).unwrap();
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let mut params = CertificateParams::default();
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, dns.first().copied().unwrap_or("forged"));
    params.subject_alt_names = dns
        .iter()
        .map(|d| SanType::DnsName((*d).try_into().unwrap()))
        .chain(ips.iter().map(|ip| SanType::IpAddress(*ip)))
        .collect();
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
    params.not_before = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(30);
    let cert = params.signed_by(&key, &issuer).unwrap();
    (cert.der().to_vec(), key.serialize_der())
}

/// DER of a PKCS#8 PEM key.
pub fn key_der(pem: &str) -> Vec<u8> {
    KeyPair::from_pem(pem).unwrap().serialize_der()
}

/// [`FileAcl`] that only records which files were restricted.
#[derive(Debug, Default)]
pub struct RecordingAcl {
    pub restricted: Mutex<Vec<String>>,
}

impl FileAcl for RecordingAcl {
    fn restrict_to_current_user(&self, path: &Path) -> Result<()> {
        self.restricted
            .lock()
            .unwrap()
            .push(path.file_name().unwrap().to_string_lossy().into_owned());
        Ok(())
    }
    fn is_restricted_to_current_user(&self, _: &Path) -> Result<bool> {
        Ok(true)
    }
}

/// In-memory trust store: records every call; `decline` makes `add_root` fail like a
/// declined Windows dialog.
#[derive(Debug, Default)]
pub struct FakeTrust {
    pub trusted: Mutex<BTreeSet<(CertThumbprint, TrustScope)>>,
    pub calls: Mutex<Vec<String>>,
    pub decline: Mutex<bool>,
}

impl FakeTrust {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn contains(&self, thumbprint: CertThumbprint, scope: TrustScope) -> bool {
        self.trusted.lock().unwrap().contains(&(thumbprint, scope))
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl TrustStore for FakeTrust {
    fn is_trusted(&self, thumbprint: &CertThumbprint, scope: TrustScope) -> Result<bool> {
        Ok(self.contains(*thumbprint, scope))
    }

    fn add_root(&self, cert_der: &[u8], scope: TrustScope) -> Result<()> {
        let thumbprint = CertThumbprint::of_der(cert_der);
        self.calls
            .lock()
            .unwrap()
            .push(format!("add {scope} {thumbprint}"));
        if *self.decline.lock().unwrap() {
            return Err(ForgeError::Cancelled("declined by the test".into()));
        }
        self.trusted.lock().unwrap().insert((thumbprint, scope));
        Ok(())
    }

    fn remove_root(&self, thumbprint: &CertThumbprint, scope: TrustScope) -> Result<bool> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("remove {scope} {thumbprint}"));
        Ok(self.trusted.lock().unwrap().remove(&(*thumbprint, scope)))
    }

    fn verify_server(&self, _: &[Vec<u8>], _: &[u8], _: &str, _: bool) -> Result<()> {
        Ok(())
    }
}
