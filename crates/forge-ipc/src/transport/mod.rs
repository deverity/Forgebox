//! Transport abstraction (ADR-0009, rule 1; docs/13 §3.1).
//!
//! A transport moves **frames**: one JSON-RPC 2.0 message (request, response or
//! notification) per frame, as UTF-8 JSON text. Byte-stream transports (named pipe,
//! unix socket, in-memory duplex) delimit frames with `\n` (NDJSON; compact JSON never
//! contains a raw newline) and enforce a maximum frame size; message transports
//! (WebSocket, M6) map one message to one frame. Server and client only see
//! [`Connection`]s, so adding a transport never touches the API.
//!
//! | kind | status | local |
//! |---|---|---|
//! | [`NamedPipeTransport`] (Windows) | implemented | yes |
//! | [`UnixSocketTransport`] (unix) | minimal implementation, untested on CI | yes |
//! | [`InMemoryTransport`] | implemented (tests, embedding) | configurable |
//! | [`WebSocketTlsTransport`] | placeholder (M6) | no |
//! | SSH tunnel | enum value only (v1.x) | no |

mod framing;
mod memory;
#[cfg(windows)]
mod named_pipe;
#[cfg(unix)]
mod unix;
mod websocket;

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::{Sink, Stream};
use serde::{Deserialize, Serialize};
use specta::Type;

pub use framing::{DEFAULT_MAX_FRAME_BYTES, framed};
pub use memory::{InMemoryListener, InMemoryTransport};
#[cfg(windows)]
pub use named_pipe::{NamedPipeListener, NamedPipeTransport, pipe_name_for_root};
#[cfg(unix)]
pub use unix::{UnixSocketListener, UnixSocketTransport, socket_path_for_root};
pub use websocket::WebSocketTlsTransport;

/// Kind of transport behind a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// `\\.\pipe\forgebox[-<hash>]`, DACL = current user (Windows).
    NamedPipe,
    /// `<root>/state/forgebox.sock`, mode 0600 (Linux, macOS).
    UnixSocket,
    /// `wss://host:7788`, mTLS or token (remote mode, M6).
    WebSocketTls,
    /// SSH tunnel to a remote unix socket (remote mode, v1.x).
    SshTunnel,
    /// In-process duplex (tests, embedding).
    InMemory,
}

impl TransportKind {
    /// Wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            TransportKind::NamedPipe => "named_pipe",
            TransportKind::UnixSocket => "unix_socket",
            TransportKind::WebSocketTls => "web_socket_tls",
            TransportKind::SshTunnel => "ssh_tunnel",
            TransportKind::InMemory => "in_memory",
        }
    }
}

impl fmt::Display for TransportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Transport failure. Cloneable so one failure can be reported to many waiters.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// Nobody is listening (daemon not running, wrong root).
    #[error("{endpoint} is not available: {reason}")]
    Unavailable { endpoint: String, reason: String },
    /// Another server already owns the endpoint.
    #[error("{0} is already in use by another server")]
    AddrInUse(String),
    /// The peer is not who it should be (pipe owned by another user...).
    #[error("untrusted endpoint: {0}")]
    Untrusted(String),
    /// A frame exceeded the configured limit.
    #[error("frame of {size} bytes exceeds the limit of {limit} bytes")]
    FrameTooLarge { size: usize, limit: usize },
    /// The frame is not valid UTF-8 text.
    #[error("invalid frame: {0}")]
    InvalidFrame(String),
    /// The connection is closed.
    #[error("connection closed")]
    Closed,
    /// The transport is not available in this build or not implemented yet.
    #[error("unsupported transport: {0}")]
    Unsupported(String),
    /// Other I/O failure.
    #[error("{context}: {message}")]
    Io {
        context: String,
        kind: std::io::ErrorKind,
        message: String,
    },
}

impl TransportError {
    pub(crate) fn io(context: impl Into<String>, err: &std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            kind: err.kind(),
            message: err.to_string(),
        }
    }

    /// Stable error code (`forge_core::error::ERROR_CODES`) for clients and the CLI.
    pub fn code(&self) -> &'static str {
        match self {
            TransportError::Unavailable { .. } | TransportError::Closed => "io",
            TransportError::AddrInUse(_) => "conflict",
            TransportError::Untrusted(_) => "forbidden",
            TransportError::FrameTooLarge { .. } | TransportError::InvalidFrame(_) => {
                "invalid_input"
            }
            TransportError::Unsupported(_) => "unsupported_platform",
            TransportError::Io { .. } => "io",
        }
    }
}

/// Facts about a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub transport: TransportKind,
    /// Human description of the peer (`pipe:\\.\pipe\forgebox`, `memory#3`).
    pub peer: String,
    /// The peer runs on this machine as this user: `local_only` methods allowed and the
    /// role is `admin` (ADR-0009, rules 6-7).
    pub local: bool,
}

/// Outgoing half: frames written in order.
pub type FrameSink = Pin<Box<dyn Sink<String, Error = TransportError> + Send>>;
/// Incoming half: frames read in order; `None` at end of stream.
pub type FrameStream = Pin<Box<dyn Stream<Item = Result<String, TransportError>> + Send>>;

/// An open connection, split in two halves so reading and writing run independently.
pub struct Connection {
    pub info: ConnectionInfo,
    pub sink: FrameSink,
    pub stream: FrameStream,
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// Server side of a transport: yields accepted connections.
pub trait Listener: Send {
    /// Waits for the next connection. Errors are per-attempt; the listener stays usable
    /// unless it returns [`TransportError::Closed`].
    fn accept(&mut self) -> BoxFuture<'_, Result<Connection, TransportError>>;

    /// Where it listens (for logs).
    fn endpoint(&self) -> String;
}

/// A way to reach a daemon (client side) and to listen for clients (server side).
pub trait Transport: Send + Sync + 'static {
    fn kind(&self) -> TransportKind;

    /// Human description of the endpoint.
    fn endpoint(&self) -> String;

    /// Opens a connection to the server.
    fn connect(&self) -> BoxFuture<'_, Result<Connection, TransportError>>;

    /// Starts listening.
    fn bind(&self) -> BoxFuture<'_, Result<Box<dyn Listener>, TransportError>>;
}

impl<T: Transport + ?Sized> Transport for Arc<T> {
    fn kind(&self) -> TransportKind {
        (**self).kind()
    }
    fn endpoint(&self) -> String {
        (**self).endpoint()
    }
    fn connect(&self) -> BoxFuture<'_, Result<Connection, TransportError>> {
        (**self).connect()
    }
    fn bind(&self) -> BoxFuture<'_, Result<Box<dyn Listener>, TransportError>> {
        (**self).bind()
    }
}

/// The local transport of this OS for the installation at `root`: named pipe on
/// Windows, unix socket elsewhere.
pub fn local_transport(root: &forge_core::paths::ForgeRoot) -> Arc<dyn Transport> {
    #[cfg(windows)]
    {
        Arc::new(NamedPipeTransport::for_root(root))
    }
    #[cfg(unix)]
    {
        Arc::new(UnixSocketTransport::for_root(root))
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = root;
        Arc::new(WebSocketTlsTransport::new("unsupported"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_serialize_snake_case() {
        for kind in [
            TransportKind::NamedPipe,
            TransportKind::UnixSocket,
            TransportKind::WebSocketTls,
            TransportKind::SshTunnel,
            TransportKind::InMemory,
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        }
    }

    #[test]
    fn errors_have_stable_codes() {
        assert_eq!(TransportError::Closed.code(), "io");
        assert_eq!(TransportError::AddrInUse("x".into()).code(), "conflict");
        assert_eq!(TransportError::Untrusted("x".into()).code(), "forbidden");
        for code in [
            TransportError::FrameTooLarge { size: 2, limit: 1 }.code(),
            TransportError::Unsupported("x".into()).code(),
        ] {
            assert!(forge_core::error::ERROR_CODES.contains(&code));
        }
    }
}
