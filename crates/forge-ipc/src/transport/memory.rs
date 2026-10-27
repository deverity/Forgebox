//! In-process transport over `tokio::io::duplex`, with the same NDJSON framing as the
//! named pipe. Used by tests and to embed a server in the same process. It can pretend
//! to be remote (`local: false`) to exercise `local_only` and role checks.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::io::DuplexStream;
use tokio::sync::mpsc;

use forge_core::sync::{LockLevel, OrderedMutex};

use super::framing::{DEFAULT_MAX_FRAME_BYTES, framed};
use super::{Connection, ConnectionInfo, Listener, Transport, TransportError, TransportKind};

/// Default duplex buffer per direction (64 KiB, like a pipe).
pub const DEFAULT_MEMORY_BUFFER: usize = 64 * 1024;

/// In-memory transport. Cheap to clone; clones share the listener.
#[derive(Clone)]
pub struct InMemoryTransport {
    inner: Arc<Inner>,
}

struct Inner {
    name: String,
    local: bool,
    buffer: usize,
    max_frame: usize,
    /// Frame limit of the connecting side (tests use a larger one to send oversized
    /// frames to the server).
    client_max_frame: usize,
    next_peer: AtomicU64,
    sender: mpsc::UnboundedSender<(u64, DuplexStream)>,
    /// Taken by `bind` (once). Lock order: `ipc.connections` (rank 10), held only to take
    /// the receiver.
    receiver: OrderedMutex<Option<mpsc::UnboundedReceiver<(u64, DuplexStream)>>>,
}

impl std::fmt::Debug for InMemoryTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemoryTransport")
            .field("name", &self.inner.name)
            .field("local", &self.inner.local)
            .finish()
    }
}

impl Default for InMemoryTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryTransport {
    /// Local transport (connections count as local: role admin, `local_only` allowed).
    pub fn new() -> Self {
        Self::build(true, DEFAULT_MEMORY_BUFFER, DEFAULT_MAX_FRAME_BYTES)
    }

    /// Transport whose connections are treated as remote.
    pub fn remote() -> Self {
        Self::build(false, DEFAULT_MEMORY_BUFFER, DEFAULT_MAX_FRAME_BYTES)
    }

    /// Same, with an explicit duplex buffer (bytes per direction) and frame limit.
    pub fn with_limits(local: bool, buffer: usize, max_frame: usize) -> Self {
        Self::build(local, buffer, max_frame)
    }

    /// Same as [`InMemoryTransport::with_limits`] with a different frame limit for the
    /// connecting side.
    pub fn with_client_limit(local: bool, buffer: usize, max_frame: usize, client: usize) -> Self {
        Self::build_with(local, buffer, max_frame, client)
    }

    fn build(local: bool, buffer: usize, max_frame: usize) -> Self {
        Self::build_with(local, buffer, max_frame, max_frame)
    }

    fn build_with(local: bool, buffer: usize, max_frame: usize, client_max_frame: usize) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            inner: Arc::new(Inner {
                name: format!("memory:{}", ulid::Ulid::generate()),
                local,
                buffer,
                max_frame,
                client_max_frame,
                next_peer: AtomicU64::new(1),
                sender,
                receiver: OrderedMutex::new(LockLevel::IPC_CONNECTIONS, Some(receiver)),
            }),
        }
    }

    fn info(&self, peer: u64) -> ConnectionInfo {
        ConnectionInfo {
            transport: TransportKind::InMemory,
            peer: format!("{}#{peer}", self.inner.name),
            local: self.inner.local,
        }
    }
}

/// Listener side of [`InMemoryTransport`].
pub struct InMemoryListener {
    transport: InMemoryTransport,
    receiver: mpsc::UnboundedReceiver<(u64, DuplexStream)>,
}

impl Listener for InMemoryListener {
    fn accept(&mut self) -> BoxFuture<'_, Result<Connection, TransportError>> {
        async move {
            let (peer, stream) = self.receiver.recv().await.ok_or(TransportError::Closed)?;
            Ok(framed(
                stream,
                self.transport.info(peer),
                self.transport.inner.max_frame,
            ))
        }
        .boxed()
    }

    fn endpoint(&self) -> String {
        self.transport.inner.name.clone()
    }
}

impl Transport for InMemoryTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::InMemory
    }

    fn endpoint(&self) -> String {
        self.inner.name.clone()
    }

    fn connect(&self) -> BoxFuture<'_, Result<Connection, TransportError>> {
        async move {
            let (client, server) = tokio::io::duplex(self.inner.buffer);
            let peer = self.inner.next_peer.fetch_add(1, Ordering::Relaxed);
            self.inner
                .sender
                .send((peer, server))
                .map_err(|_| TransportError::Unavailable {
                    endpoint: self.inner.name.clone(),
                    reason: "no listener".into(),
                })?;
            Ok(framed(client, self.info(peer), self.inner.client_max_frame))
        }
        .boxed()
    }

    fn bind(&self) -> BoxFuture<'_, Result<Box<dyn Listener>, TransportError>> {
        async move {
            let receiver = self
                .inner
                .receiver
                .lock()
                .take()
                .ok_or_else(|| TransportError::AddrInUse(self.inner.name.clone()))?;
            Ok(Box::new(InMemoryListener {
                transport: self.clone(),
                receiver,
            }) as Box<dyn Listener>)
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use futures::{SinkExt, StreamExt};

    use super::*;

    #[tokio::test]
    async fn connect_accept_and_exchange() {
        let transport = InMemoryTransport::new();
        let mut listener = transport.bind().await.unwrap();
        assert!(matches!(
            transport.bind().await,
            Err(TransportError::AddrInUse(_))
        ));
        let mut client = transport.connect().await.unwrap();
        let mut server = listener.accept().await.unwrap();
        assert!(server.info.local);
        client.sink.send("{}".into()).await.unwrap();
        assert_eq!(server.stream.next().await.unwrap().unwrap(), "{}");
        server.sink.send("[1]".into()).await.unwrap();
        assert_eq!(client.stream.next().await.unwrap().unwrap(), "[1]");
    }

    #[tokio::test]
    async fn remote_flag_and_missing_listener() {
        let transport = InMemoryTransport::remote();
        let listener = transport.bind().await.unwrap();
        drop(listener);
        assert!(matches!(
            transport.connect().await,
            Err(TransportError::Unavailable { .. })
        ));
        assert!(!transport.info(1).local);
    }
}
