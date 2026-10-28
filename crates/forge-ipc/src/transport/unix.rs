//! Unix domain socket transport (Linux/macOS, docs/13 §4). Minimal implementation for
//! the headless Linux daemon (v1.x): `<root>/state/forgebox.sock`, mode `0600` set by
//! the server right after binding. Not exercised by the Windows CI yet.

use std::path::{Path, PathBuf};

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::net::{UnixListener, UnixStream};

use forge_core::paths::ForgeRoot;

use super::framing::{DEFAULT_MAX_FRAME_BYTES, framed};
use super::{Connection, ConnectionInfo, Listener, Transport, TransportError, TransportKind};

/// Socket path of the installation at `root`.
pub fn socket_path_for_root(root: &ForgeRoot) -> PathBuf {
    root.state().join("forgebox.sock")
}

/// Unix socket transport.
#[derive(Debug, Clone)]
pub struct UnixSocketTransport {
    path: PathBuf,
    max_frame: usize,
}

impl UnixSocketTransport {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            max_frame: DEFAULT_MAX_FRAME_BYTES,
        }
    }

    pub fn for_root(root: &ForgeRoot) -> Self {
        Self::new(socket_path_for_root(root))
    }

    fn info(&self) -> ConnectionInfo {
        ConnectionInfo {
            transport: TransportKind::UnixSocket,
            peer: format!("unix:{}", self.path.display()),
            local: true,
        }
    }
}

impl Transport for UnixSocketTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::UnixSocket
    }

    fn endpoint(&self) -> String {
        self.path.display().to_string()
    }

    fn connect(&self) -> BoxFuture<'_, Result<Connection, TransportError>> {
        async move {
            let stream = UnixStream::connect(&self.path).await.map_err(|err| {
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) {
                    TransportError::Unavailable {
                        endpoint: self.endpoint(),
                        reason: "the daemon is not running".into(),
                    }
                } else {
                    TransportError::io("connecting to the API socket", &err)
                }
            })?;
            Ok(framed(stream, self.info(), self.max_frame))
        }
        .boxed()
    }

    fn bind(&self) -> BoxFuture<'_, Result<Box<dyn Listener>, TransportError>> {
        async move {
            if UnixStream::connect(&self.path).await.is_ok() {
                return Err(TransportError::AddrInUse(self.endpoint()));
            }
            // A stale socket file from a crashed daemon.
            let _ = std::fs::remove_file(&self.path);
            let listener = UnixListener::bind(&self.path)
                .map_err(|err| TransportError::io("binding the API socket", &err))?;
            forge_core::platform::current()
                .file_acl()
                .restrict_to_current_user(&self.path)
                .map_err(|e| TransportError::Io {
                    context: "restricting the API socket".into(),
                    kind: std::io::ErrorKind::PermissionDenied,
                    message: e.to_string(),
                })?;
            Ok(Box::new(UnixSocketListener {
                transport: self.clone(),
                listener,
            }) as Box<dyn Listener>)
        }
        .boxed()
    }
}

/// Listener of [`UnixSocketTransport`].
pub struct UnixSocketListener {
    transport: UnixSocketTransport,
    listener: UnixListener,
}

impl Listener for UnixSocketListener {
    fn accept(&mut self) -> BoxFuture<'_, Result<Connection, TransportError>> {
        async move {
            let (stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|err| TransportError::io("accepting an API client", &err))?;
            Ok(framed(
                stream,
                self.transport.info(),
                self.transport.max_frame,
            ))
        }
        .boxed()
    }

    fn endpoint(&self) -> String {
        self.transport.endpoint()
    }
}
