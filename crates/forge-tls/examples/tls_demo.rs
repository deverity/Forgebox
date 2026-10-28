//! Manual checks of the local CA (docs/06 §6.4), without the daemon.
//!
//! ```text
//! cargo run -p forge-tls --example tls_demo -- <root> issue loja.test [more hosts]
//! cargo run -p forge-tls --example tls_demo -- <root> serve loja.test 8443
//! ```
//!
//! `issue` creates `<root>/certs/ca` (if missing) and the site certificates, owner-only
//! keys included, and prints paths and thumbprints for `openssl x509`/`openssl verify`.
//! `serve` answers HTTPS on `127.0.0.1:<port>` with the certificate of the host (and the
//! CRL responder on `127.0.0.1:47480`, `[ports.ranges] tls-crl`), so
//! `curl --cacert <root>/certs/ca/forgebox-ca.crt --resolve loja.test:<port>:127.0.0.1
//! https://loja.test:<port>/` can check the chain, revocation included, with the system
//! TLS stack (Schannel for `C:\Windows\System32\curl.exe`). Nothing is added to any trust
//! store.

use std::net::Ipv4Addr;
use std::sync::Arc;

use forge_core::actor::ActorSupervisor;
use forge_core::config::ForgeConfig;
use forge_core::event::EventBus;
use forge_core::paths::ForgeRoot;
use forge_tls::{LeafRequest, TlsConfig, TlsManager};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn pem_der(pem: &str) -> Vec<u8> {
    x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .expect("PEM")
        .1
        .contents
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: tls_demo <root> issue <host>... | tls_demo <root> serve <host> <port>";
    let (Some(root), Some(command)) = (args.first(), args.get(1)) else {
        eprintln!("{usage}");
        std::process::exit(2);
    };
    let root = ForgeRoot::new(root).expect("root");
    let platform = forge_core::platform::current();
    // Same settings as the daemon: TLDs and the CRL responder port from forgebox.toml.
    let config = ForgeConfig::load_or_default(&root.forgebox_toml()).expect("forgebox.toml");
    let tls = TlsManager::spawn(
        &ActorSupervisor::new(EventBus::new()),
        &root,
        TlsConfig::for_root(&root, &config),
        platform.shared_file_acl(),
        forge_core::platform::trust_store(None),
    );
    match command.as_str() {
        "issue" => {
            let ca = tls.ensure_ca().await.expect("CA");
            println!(
                "CA       {}",
                root.certs_ca().join("forgebox-ca.crt").display()
            );
            println!("subject  {}", ca.subject);
            println!("sha1     {}", ca.thumbprint_sha1);
            println!("sha256   {}", ca.thumbprint_sha256);
            println!("permits  {}", ca.name_constraints().join(", "));
            let status = tls.status().await.expect("status");
            println!(
                "crl      {:?} serving={}",
                status.crl.url, status.crl.serving
            );
            for host in &args[2..] {
                let leaf = tls
                    .issue(LeafRequest::new(host.as_str()))
                    .await
                    .expect("leaf");
                println!(
                    "leaf     {} {:?} until {}",
                    leaf.cert_path.display(),
                    leaf.sans,
                    leaf.not_after
                );
                match tls.verify_with_os(host).await {
                    Ok(()) => println!("os check {host}: ok"),
                    Err(err) => println!("os check {host}: {err}"),
                }
            }
        }
        "serve" => {
            let (Some(host), Some(port)) = (args.get(2), args.get(3)) else {
                eprintln!("{usage}");
                std::process::exit(2);
            };
            let port: u16 = port.parse().expect("port");
            tls.issue(LeafRequest::new(host.as_str()))
                .await
                .expect("leaf");
            let export = tls.export_leaf(host).await.expect("export");
            let key = rcgen::KeyPair::from_pem(&export.key_pem)
                .expect("key")
                .serialize_der();
            let config = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::aws_lc_rs::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .expect("versions")
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(pem_der(&export.cert_pem))],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )
            .expect("server config");
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
            let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))
                .await
                .expect("bind");
            println!("serving https://{host}:{port}/ on 127.0.0.1 (Ctrl+C to stop)");
            loop {
                let (tcp, _) = listener.accept().await.expect("accept");
                let acceptor = acceptor.clone();
                let host = host.clone();
                tokio::spawn(async move {
                    match acceptor.accept(tcp).await {
                        Ok(mut stream) => {
                            let mut buf = [0_u8; 4096];
                            let _ = stream.read(&mut buf).await;
                            let body = format!("forgebox tls demo: {host}\n");
                            let response = format!(
                                "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                            let _ = stream.shutdown().await;
                        }
                        Err(err) => eprintln!("handshake failed: {err}"),
                    }
                });
            }
        }
        _ => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    }
    tls.shutdown().await;
}
