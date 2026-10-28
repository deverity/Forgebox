//! Certificate revocation list of the local CA and its loopback responder (ADR-0023).
//!
//! Why: Windows `curl.exe` (Schannel) checks revocation of the server certificate by
//! default and refuses a certificate that has no way to be checked
//! (`CRYPT_E_NO_REVOCATION_CHECK` / `CERT_TRUST_REVOCATION_STATUS_UNKNOWN`), which is what
//! happens with mkcert-style local CAs. So every site certificate carries a CRL
//! distribution point `http://127.0.0.1:<port>/forgebox/<CA SHA-1>.crl`, and the `tls`
//! actor serves there an empty CRL signed by the CA (CryptoAPI fetches it once and caches
//! it until `nextUpdate`; `file://` distribution points are not fetched by Schannel,
//! measured). The CA SHA-1 in the path keeps a rotated CA from meeting the cached CRL of
//! the previous one. Nothing is ever revoked: a compromised site key is replaced by
//! re-issuing; the list exists only so the check can succeed.
//!
//! The responder is a minimal HTTP/1.x server on loopback: `GET`/`HEAD` of the one path,
//! `404` for anything else, one request per connection, 5 s to send the request line.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use forge_core::Timestamp;
use forge_core::error::{ForgeError, Result};
use forge_core::platform::CertThumbprint;
use jiff::SignedDuration;
use rcgen::{CertificateRevocationListParams, KeyIdMethod, SerialNumber};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use crate::ca::{BACKDATE, LocalCa};
use crate::cert;

/// `certs/ca/forgebox-ca.crl` (DER), a copy of what the responder serves.
pub const CA_CRL_FILE: &str = "forgebox-ca.crl";
/// `nextUpdate` of a new CRL; it is re-signed when less than half is left.
pub const CRL_VALIDITY: SignedDuration = SignedDuration::from_hours(7 * 24);
/// Longest request head the responder reads.
const MAX_REQUEST: usize = 8 * 1024;
/// Time a client has to send its request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Path of the CRL of the CA `thumbprint` on the responder.
pub fn crl_path(thumbprint: &CertThumbprint) -> String {
    format!("/forgebox/{thumbprint}.crl")
}

/// Distribution point written in the leaves of the CA `thumbprint` when the responder
/// listens on `listen`.
pub fn crl_url(listen: SocketAddr, thumbprint: &CertThumbprint) -> String {
    format!("http://{listen}{}", crl_path(thumbprint))
}

/// A signed CRL ready to be served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrlDocument {
    /// Path it is served at (`/forgebox/<SHA1>.crl`).
    pub path: String,
    pub der: Arc<Vec<u8>>,
    pub this_update: Timestamp,
    pub next_update: Timestamp,
}

impl CrlDocument {
    /// Whether less than half of its validity is left at `now`.
    pub fn needs_refresh(&self, now: Timestamp) -> bool {
        let total = self.next_update.duration_since(self.this_update);
        self.next_update.duration_since(now) < total / 2
    }
}

/// An empty CRL of `ca`, valid from `now - 1 h` for `validity`. The CRL number is the
/// signing time in seconds (monotonic across restarts).
pub fn build_crl(ca: &LocalCa, now: Timestamp, validity: SignedDuration) -> Result<CrlDocument> {
    let this_update = now - BACKDATE;
    let next_update = now + validity;
    let params = CertificateRevocationListParams {
        this_update: cert::to_offset(this_update)?,
        next_update: cert::to_offset(next_update)?,
        crl_number: SerialNumber::from(u64::try_from(now.as_second()).unwrap_or(1)),
        issuing_distribution_point: None,
        revoked_certs: Vec::new(),
        key_identifier_method: KeyIdMethod::Sha256,
    };
    let issuer = ca.issuer()?;
    let crl = params
        .signed_by(&issuer)
        .map_err(|e| ForgeError::Internal(format!("signing the CRL: {e}")))?;
    // rcgen derives the CRL's authority key identifier with `key_identifier_method` from
    // the CA public key: SHA-256, the method that produced the CA's subject key identifier
    // (checked by a test).
    Ok(CrlDocument {
        path: crl_path(&ca.info().thumbprint_sha1),
        der: Arc::new(crl.der().to_vec()),
        this_update,
        next_update,
    })
}

/// Serves the current document of `current` on `listener` until the task is aborted.
pub(crate) async fn serve(listener: TcpListener, current: watch::Receiver<Option<CrlDocument>>) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(err) => {
                tracing::debug!(%err, "tls: CRL responder accept failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let document = current.borrow().clone();
        tokio::spawn(async move {
            if let Err(err) = answer(stream, document).await {
                tracing::debug!(%err, "tls: CRL request failed");
            }
        });
    }
}

/// Reads one request and writes the response.
async fn answer(mut stream: TcpStream, document: Option<CrlDocument>) -> std::io::Result<()> {
    let mut head = Vec::with_capacity(512);
    let read = tokio::time::timeout(REQUEST_TIMEOUT, async {
        let mut buf = [0_u8; 1024];
        while !end_of_head(&head) && head.len() < MAX_REQUEST {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            head.extend_from_slice(&buf[..n]);
        }
        Ok::<_, std::io::Error>(())
    })
    .await;
    if !matches!(read, Ok(Ok(()))) {
        return Ok(());
    }
    let response = respond(&head, document.as_ref());
    stream.write_all(&response).await?;
    stream.shutdown().await
}

/// Whether `head` holds a complete request head (`CRLF CRLF`, or bare `LF LF`).
fn end_of_head(head: &[u8]) -> bool {
    head.windows(4).any(|w| w == b"\r\n\r\n") || head.windows(2).any(|w| w == b"\n\n")
}

/// The bytes of the answer to the request `head`.
fn respond(head: &[u8], document: Option<&CrlDocument>) -> Vec<u8> {
    let line = head.split(|b| *b == b'\n').next().unwrap_or_default();
    let line = String::from_utf8_lossy(line);
    let mut parts = line.trim_end().split(' ');
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let path = target.split('?').next().unwrap_or("");
    let found = document.filter(|d| d.path == path);
    match (method, found) {
        ("GET" | "HEAD", Some(doc)) => {
            let mut out = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/pkix-crl\r\nContent-Length: {}\r\nCache-Control: max-age=3600\r\nConnection: close\r\n\r\n",
                doc.der.len()
            )
            .into_bytes();
            if method == "GET" {
                out.extend_from_slice(&doc.der);
            }
            out
        }
        ("GET" | "HEAD", None) => {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        }
        _ => b"HTTP/1.1 405 Method Not Allowed\r\nAllow: GET, HEAD\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use x509_parser::extensions::ParsedExtension;
    use x509_parser::prelude::FromDer;
    use x509_parser::revocation_list::CertificateRevocationList;

    use super::*;
    use crate::ca::{CA_VALIDITY, CaOptions};

    fn ca() -> LocalCa {
        LocalCa::generate(&CaOptions {
            common_name: "Forgebox Local CA crl 2026".to_owned(),
            tlds: vec![".test".to_owned()],
            validity: CA_VALIDITY,
            now: Timestamp::now(),
        })
        .unwrap()
    }

    #[test]
    fn crl_is_signed_by_the_ca_and_empty() {
        let ca = ca();
        let now: Timestamp = "2026-10-27T12:00:00Z".parse().unwrap();
        let doc = build_crl(&ca, now, CRL_VALIDITY).unwrap();
        assert_eq!(
            doc.path,
            format!("/forgebox/{}.crl", ca.info().thumbprint_sha1)
        );
        let (_, crl) = CertificateRevocationList::from_der(&doc.der).unwrap();
        let (_, ca_cert) =
            x509_parser::certificate::X509Certificate::from_der(ca.cert_der()).unwrap();
        assert_eq!(crl.issuer(), ca_cert.subject());
        crl.verify_signature(ca_cert.public_key()).unwrap();
        assert_eq!(crl.iter_revoked_certificates().count(), 0);
        let aki = crl
            .extensions()
            .iter()
            .find_map(|e| match e.parsed_extension() {
                ParsedExtension::AuthorityKeyIdentifier(aki) => aki.key_identifier.clone(),
                _ => None,
            });
        let ski = ca_cert
            .iter_extensions()
            .find_map(|e| match e.parsed_extension() {
                ParsedExtension::SubjectKeyIdentifier(ski) => Some(ski.clone()),
                _ => None,
            });
        assert_eq!(aki.unwrap().0, ski.unwrap().0, "CRL AKI = CA SKI");
        assert_eq!(crl.last_update().timestamp(), (now - BACKDATE).as_second());
        assert_eq!(
            crl.next_update().unwrap().timestamp(),
            (now + CRL_VALIDITY).as_second()
        );
        assert!(!doc.needs_refresh(now));
        assert!(doc.needs_refresh(now + SignedDuration::from_hours(4 * 24)));
    }

    #[test]
    fn responder_answers_only_its_path() {
        let ca = ca();
        let doc = build_crl(&ca, Timestamp::now(), CRL_VALIDITY).unwrap();
        let get = format!("GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n", doc.path);
        let ok = respond(get.as_bytes(), Some(&doc));
        let text = String::from_utf8_lossy(&ok);
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
        assert!(text.contains("Content-Type: application/pkix-crl"));
        assert!(ok.ends_with(&doc.der));
        let head = respond(
            format!("HEAD {} HTTP/1.1\r\n\r\n", doc.path).as_bytes(),
            Some(&doc),
        );
        assert!(!head.ends_with(&doc.der));
        for request in [
            "GET /forgebox/other.crl HTTP/1.1\r\n\r\n".to_owned(),
            "GET / HTTP/1.1\r\n\r\n".to_owned(),
        ] {
            let answer = respond(request.as_bytes(), Some(&doc));
            assert!(answer.starts_with(b"HTTP/1.1 404"), "{request}");
        }
        assert!(respond(get.as_bytes(), None).starts_with(b"HTTP/1.1 404"));
        assert!(end_of_head(b"GET / HTTP/1.0\n\n") && !end_of_head(b"GET / HTTP/1.1\r\n"));
        let post = respond(
            format!("POST {} HTTP/1.1\r\n\r\n", doc.path).as_bytes(),
            Some(&doc),
        );
        assert!(post.starts_with(b"HTTP/1.1 405"));
    }

    #[tokio::test]
    async fn responder_serves_over_tcp() {
        let ca = ca();
        let doc = build_crl(&ca, Timestamp::now(), CRL_VALIDITY).unwrap();
        let (tx, rx) = watch::channel(Some(doc.clone()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(serve(listener, rx));
        let fetch = |path: String| async move {
            let mut conn = TcpStream::connect(addr).await.unwrap();
            conn.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut out = Vec::new();
            conn.read_to_end(&mut out).await.unwrap();
            out
        };
        assert!(fetch(doc.path.clone()).await.ends_with(&doc.der));
        // A new document is served from the next request on.
        let newer = build_crl(
            &ca,
            Timestamp::now() + SignedDuration::from_secs(5),
            CRL_VALIDITY,
        )
        .unwrap();
        tx.send_replace(Some(newer.clone()));
        assert!(fetch(doc.path.clone()).await.ends_with(&newer.der));
        // A silent client does not block the others.
        let _idle = TcpStream::connect(addr).await.unwrap();
        assert!(fetch(doc.path.clone()).await.ends_with(&newer.der));
        task.abort();
    }
}
