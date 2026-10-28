//! The name constraints hold in a real TLS handshake: a local tokio-rustls server presents
//! the certificate, a rustls client (webpki) trusts only the Forgebox CA. A leaf for
//! `loja.test` connects; leaves for `google.com` or `evil.example.com` signed with the
//! (leaked) CA key are refused for violating the constraints, not just for a name mismatch.

mod support;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use forge_core::actor::ActorSupervisor;
use forge_core::event::EventBus;
use forge_tls::{LeafRequest, TlsConfig, TlsManager};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use support::{FakeTrust, RecordingAcl, forge_leaf, key_der, temp_root};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const GREETING: &str = "hello from forgebox";

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

fn pem_der(pem: &str) -> Vec<u8> {
    x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .unwrap()
        .1
        .contents
}

/// A TLS server on `127.0.0.1:0` presenting `cert`; it greets every client and closes.
async fn serve(cert: Vec<u8>, key: Vec<u8>) -> SocketAddr {
    let config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
        )
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(tcp).await {
                    let _ = tls.write_all(GREETING.as_bytes()).await;
                    let _ = tls.shutdown().await;
                }
            });
        }
    });
    addr
}

/// Connects to `addr` asking for `name`, trusting only `ca_der`: the greeting, or the
/// rustls error of the handshake.
async fn connect(addr: SocketAddr, name: &str, ca_der: &[u8]) -> Result<String, rustls::Error> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(ca_der.to_vec())).unwrap();
    let config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let tcp = TcpStream::connect(addr).await.unwrap();
    let server_name = ServerName::try_from(name.to_owned()).unwrap();
    match connector.connect(server_name, tcp).await {
        Ok(mut tls) => {
            let mut text = String::new();
            tls.read_to_string(&mut text).await.unwrap();
            Ok(text)
        }
        Err(io) => Err(io
            .get_ref()
            .and_then(|e| e.downcast_ref::<rustls::Error>())
            .cloned()
            .unwrap_or_else(|| panic!("not a rustls error: {io}"))),
    }
}

fn is_name_constraint_violation(err: &rustls::Error) -> bool {
    matches!(
        err,
        rustls::Error::InvalidCertificate(rustls::CertificateError::Other(other))
            if format!("{other:?}").contains("NameConstraintViolation")
    )
}

/// webpki path validation of `leaf` with `ca` as the only anchor (no name check).
fn webpki_path(leaf: &[u8], ca: &[u8]) -> Result<(), webpki::Error> {
    let ca = CertificateDer::from(ca.to_vec());
    let anchor = webpki::anchor_from_trusted_cert(&ca).unwrap();
    let leaf = CertificateDer::from(leaf.to_vec());
    let ee = webpki::EndEntityCert::try_from(&leaf).unwrap();
    ee.verify_for_usage(
        webpki::ALL_VERIFICATION_ALGS,
        &[anchor],
        &[],
        UnixTime::now(),
        webpki::KeyUsage::server_auth(),
        None,
        None,
    )
    .map(|_| ())
}

struct Site {
    _root: support::TempRoot,
    tls: TlsManager,
    ca_der: Vec<u8>,
    ca_dir: std::path::PathBuf,
}

async fn site_with_leaf(host: &str) -> (Site, SocketAddr) {
    let root = temp_root();
    let tls = TlsManager::spawn(
        &ActorSupervisor::new(EventBus::new()),
        &root.root,
        TlsConfig::new(vec![".test".to_owned()]),
        Arc::new(RecordingAcl::default()),
        FakeTrust::new(),
    );
    tls.issue(LeafRequest::new(host)).await.unwrap();
    let export = tls.export_leaf(host).await.unwrap();
    let ca_der = pem_der(&tls.export_ca().await.unwrap());
    let addr = serve(pem_der(&export.cert_pem), key_der(&export.key_pem)).await;
    let ca_dir = root.root.certs_ca();
    (
        Site {
            _root: root,
            tls,
            ca_der,
            ca_dir,
        },
        addr,
    )
}

#[tokio::test]
async fn the_site_certificate_works_for_its_names_only() {
    let (site, addr) = site_with_leaf("loja.test").await;
    assert_eq!(
        connect(addr, "loja.test", &site.ca_der).await.unwrap(),
        GREETING
    );
    assert_eq!(
        connect(addr, "api.loja.test", &site.ca_der).await.unwrap(),
        GREETING,
        "*.loja.test"
    );
    let err = connect(addr, "google.com", &site.ca_der).await.unwrap_err();
    assert!(
        matches!(
            err,
            rustls::Error::InvalidCertificate(
                rustls::CertificateError::NotValidForNameContext { .. }
            )
        ),
        "{err:?}"
    );
    // Another Forgebox CA does not vouch for this one. Created in the same second on the
    // same machine it has the same subject, so webpki tries it and the signature fails;
    // otherwise the issuer is unknown. Both refuse.
    let (other, _) = site_with_leaf("blog.test").await;
    let err = connect(addr, "loja.test", &other.ca_der).await.unwrap_err();
    assert!(
        matches!(
            err,
            rustls::Error::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer | rustls::CertificateError::BadSignature
            )
        ),
        "{err:?}"
    );
    site.tls.shutdown().await;
    other.tls.shutdown().await;
}

#[tokio::test]
async fn a_leaked_ca_key_cannot_sign_for_real_domains() {
    let (site, _) = site_with_leaf("loja.test").await;

    // Control: a forged leaf inside the constraints is accepted, so the signature itself is
    // fine and only the constraints decide below.
    let (cert, key) = forge_leaf(&site.ca_dir, &["forged.test"], &[]);
    let addr = serve(cert.clone(), key).await;
    assert_eq!(
        connect(addr, "forged.test", &site.ca_der).await.unwrap(),
        GREETING
    );
    webpki_path(&cert, &site.ca_der).unwrap();

    for evil in ["google.com", "evil.example.com"] {
        let (cert, key) = forge_leaf(&site.ca_dir, &[evil], &[]);
        let addr = serve(cert.clone(), key).await;
        let err = connect(addr, evil, &site.ca_der).await.unwrap_err();
        assert!(is_name_constraint_violation(&err), "{evil}: {err:?}");
        assert_eq!(
            webpki_path(&cert, &site.ca_der),
            Err(webpki::Error::NameConstraintViolation),
            "{evil}"
        );
    }

    // A leaf mixing a permitted and a forbidden name is refused as a whole.
    let (cert, _) = forge_leaf(&site.ca_dir, &["loja.test", "www.google.com"], &[]);
    assert_eq!(
        webpki_path(&cert, &site.ca_der),
        Err(webpki::Error::NameConstraintViolation)
    );
    site.tls.shutdown().await;
}

#[tokio::test]
async fn ip_constraints_allow_loopback_only() {
    let (site, _) = site_with_leaf("loja.test").await;
    let loopback = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let (cert, key) = forge_leaf(&site.ca_dir, &[], &[loopback]);
    webpki_path(&cert, &site.ca_der).unwrap();
    let addr = serve(cert, key).await;
    assert_eq!(
        connect(addr, "127.0.0.1", &site.ca_der).await.unwrap(),
        GREETING
    );

    for ip in ["10.0.0.5", "8.8.8.8", "::2"] {
        let (cert, _) = forge_leaf(&site.ca_dir, &[], &[ip.parse().unwrap()]);
        assert_eq!(
            webpki_path(&cert, &site.ca_der),
            Err(webpki::Error::NameConstraintViolation),
            "{ip}"
        );
    }
    site.tls.shutdown().await;
}
