//! WebSocket + TLS transport for remote mode (docs/13 §3.1): placeholder until M6.
//!
//! The type exists so profiles and the GUI context selector can already name it; every
//! operation fails with [`TransportError::Unsupported`]. The implementation will map one
//! WebSocket text message to one frame (no NDJSON) and authenticate with mTLS or a
//! bearer token; connections will be non-local (`local_only` methods refused, role from
//! the pairing).

use futures::FutureExt;
use futures::future::BoxFuture;

use super::{Connection, Listener, Transport, TransportError, TransportKind};

/// `wss://host:port` endpoint (not implemented yet).
#[derive(Debug, Clone)]
pub struct WebSocketTlsTransport {
    url: String,
}

impl WebSocketTlsTransport {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }

    fn unsupported(&self) -> TransportError {
        TransportError::Unsupported(format!(
            "WebSocket/TLS transport ({}) arrives with remote mode (M6)",
            self.url
        ))
    }
}

impl Transport for WebSocketTlsTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::WebSocketTls
    }

    fn endpoint(&self) -> String {
        self.url.clone()
    }

    fn connect(&self) -> BoxFuture<'_, Result<Connection, TransportError>> {
        let err = self.unsupported();
        async move { Err(err) }.boxed()
    }

    fn bind(&self) -> BoxFuture<'_, Result<Box<dyn Listener>, TransportError>> {
        let err = self.unsupported();
        async move { Err(err) }.boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn placeholder_fails_cleanly() {
        let t = WebSocketTlsTransport::new("wss://10.0.0.5:7788");
        assert_eq!(t.kind(), TransportKind::WebSocketTls);
        assert!(matches!(
            t.connect().await,
            Err(TransportError::Unsupported(_))
        ));
        assert!(matches!(
            t.bind().await,
            Err(TransportError::Unsupported(_))
        ));
    }
}
