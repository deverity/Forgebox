//! What the CA and the leaves contain, read back with x509-parser (independent of the
//! parsing in forge-tls).

use forge_core::Timestamp;
use forge_tls::ca::{CA_VALIDITY, CaOptions, LocalCa};
use forge_tls::leaf::{LEAF_VALIDITY, LeafRequest, issue_leaf};
use pretty_assertions::assert_eq;
use x509_parser::certificate::X509Certificate;
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::FromDer;

fn options(tlds: &[&str]) -> CaOptions {
    CaOptions {
        common_name: "Forgebox Local CA TESTBOX 2026".to_owned(),
        tlds: tlds.iter().map(|t| (*t).to_owned()).collect(),
        validity: CA_VALIDITY,
        now: Timestamp::now(),
    }
}

#[test]
fn ca_has_critical_name_constraints_for_local_names_only() {
    let ca = LocalCa::generate(&options(&[".test", ".localhost", ".dev"])).unwrap();
    let (_, cert) = X509Certificate::from_der(ca.cert_der()).unwrap();

    let nc = cert
        .name_constraints()
        .unwrap()
        .expect("nameConstraints present");
    assert!(nc.critical, "RFC 5280: conforming CAs mark it critical");
    assert!(nc.value.excluded_subtrees.is_none());
    let permitted: Vec<String> = nc
        .value
        .permitted_subtrees
        .as_ref()
        .expect("permittedSubtrees")
        .iter()
        .map(|s| match &s.base {
            GeneralName::DNSName(name) => format!("DNS:{name}"),
            GeneralName::IPAddress(bytes) => format!("IP:{bytes:?}"),
            other => format!("other:{other:?}"),
        })
        .collect();
    let mut v6 = vec![0_u8; 32];
    v6[15] = 1;
    v6[16..].fill(0xFF);
    assert_eq!(
        permitted,
        vec![
            "DNS:test".to_owned(),
            "DNS:localhost".to_owned(),
            format!("IP:{:?}", [127_u8, 0, 0, 0, 255, 0, 0, 0]),
            format!("IP:{v6:?}"),
        ],
        ".dev is a real TLD: never in the constraints"
    );

    let bc = cert.basic_constraints().unwrap().unwrap();
    assert!(bc.critical && bc.value.ca);
    assert_eq!(bc.value.path_len_constraint, Some(0), "signs leaves only");
    let ku = cert.key_usage().unwrap().unwrap();
    assert!(ku.value.key_cert_sign() && ku.value.crl_sign());
    assert!(!ku.value.digital_signature());
    let subject = cert.subject().to_string();
    assert!(
        subject.starts_with("CN=Forgebox Local CA TESTBOX 2026, O=Forgebox, OU=created 20"),
        "{subject}"
    );
    let validity = cert.validity();
    let days = (validity.not_after.timestamp() - validity.not_before.timestamp()) / 86_400;
    assert_eq!(days, 3650);
    assert_eq!(
        cert.public_key().algorithm.algorithm.to_id_string(),
        "1.2.840.10045.2.1",
        "EC public key"
    );
    cert.verify_signature(None).expect("self-signed");

    let names = ca.info().name_constraints();
    assert_eq!(names, [".test", ".localhost", "127.0.0.0/8", "::1/128"]);
}

#[test]
fn leaf_covers_host_wildcard_and_aliases_and_is_signed_by_the_ca() {
    let ca = LocalCa::generate(&options(&[".test"])).unwrap();
    let (host, sans) = LeafRequest::new("loja.test")
        .with_aliases(["api.loja.test", "loja.localhost"])
        .sans()
        .unwrap();
    let dir = std::path::Path::new("certs/sites");
    let leaf = issue_leaf(
        &ca,
        dir,
        &host,
        &sans,
        LEAF_VALIDITY,
        Timestamp::now(),
        None,
    )
    .unwrap();
    let (_, cert) = X509Certificate::from_der(leaf.cert_der()).unwrap();
    let (_, ca_cert) = X509Certificate::from_der(ca.cert_der()).unwrap();

    let san = cert.subject_alternative_name().unwrap().unwrap();
    let dns: Vec<&str> = san
        .value
        .general_names
        .iter()
        .filter_map(|n| match n {
            GeneralName::DNSName(d) => Some(*d),
            _ => None,
        })
        .collect();
    assert_eq!(
        dns,
        [
            "loja.test",
            "*.loja.test",
            "api.loja.test",
            "loja.localhost"
        ]
    );
    assert_eq!(cert.subject().to_string(), "CN=loja.test, O=Forgebox");
    assert_eq!(cert.issuer(), ca_cert.subject());
    cert.verify_signature(Some(ca_cert.public_key()))
        .expect("signed by the CA");
    assert!(!cert.is_ca());
    let eku = cert.extended_key_usage().unwrap().unwrap();
    assert!(eku.value.server_auth && !eku.value.client_auth);
    assert!(cert.key_usage().unwrap().unwrap().value.digital_signature());
    let validity = cert.validity();
    let days = (validity.not_after.timestamp() - validity.not_before.timestamp()) / 86_400;
    assert_eq!(days, 825);
    assert_eq!(leaf.info().cert_path, dir.join("loja.test.crt"));
    assert_eq!(leaf.info().key_path, dir.join("loja.test.key"));
    assert!(leaf.key_pem().starts_with("-----BEGIN PRIVATE KEY-----\n"));
    assert!(!leaf.cert_pem().contains('\r') && !leaf.key_pem().contains('\r'));

    assert_eq!(leaf.info().crl_url, None);
    let url = format!(
        "http://127.0.0.1:47480/forgebox/{}.crl",
        ca.info().thumbprint_sha1
    );
    let with_crl = issue_leaf(
        &ca,
        dir,
        &host,
        &sans,
        LEAF_VALIDITY,
        Timestamp::now(),
        Some(&url),
    )
    .unwrap();
    assert_eq!(with_crl.info().crl_url.as_deref(), Some(url.as_str()));
    let (_, parsed) = X509Certificate::from_der(with_crl.cert_der()).unwrap();
    let cdp = parsed
        .iter_extensions()
        .find(|e| e.oid.to_id_string() == "2.5.29.31")
        .expect("cRLDistributionPoints");
    assert!(!cdp.critical);

    // Two leaves never share a serial (Firefox refuses reused issuer+serial pairs).
    let again = issue_leaf(
        &ca,
        dir,
        &host,
        &sans,
        LEAF_VALIDITY,
        Timestamp::now(),
        None,
    )
    .unwrap();
    assert_ne!(again.info().serial, leaf.info().serial);
}

#[test]
fn leaves_outside_the_constraints_are_refused() {
    let ca = LocalCa::generate(&options(&[".test"])).unwrap();
    let dir = std::path::Path::new("x");
    for host in ["google.com", "evil.example.com", "loja.dev"] {
        let (host, sans) = LeafRequest::new(host).sans().unwrap();
        let err = issue_leaf(
            &ca,
            dir,
            &host,
            &sans,
            LEAF_VALIDITY,
            Timestamp::now(),
            None,
        )
        .unwrap_err();
        assert_eq!(err.code(), "invalid_input", "{host}: {err}");
    }
    let (host, sans) = LeafRequest::new("loja.test")
        .with_aliases(["www.google.com"])
        .sans()
        .unwrap();
    let err = issue_leaf(
        &ca,
        dir,
        &host,
        &sans,
        LEAF_VALIDITY,
        Timestamp::now(),
        None,
    )
    .unwrap_err();
    assert!(err.to_string().contains("www.google.com"), "{err}");
}
