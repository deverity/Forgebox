//! Windows named pipe transport (docs/11 §4).
//!
//! Name: `\\.\pipe\forgebox` for the default root (`C:\Forgebox`), otherwise
//! `\\.\pipe\forgebox-<16 hex>` (FNV-1a of the lowercased root path), so several roots
//! (portable copies, tests) never collide. Every instance is created with the security
//! descriptor of `forge_core::platform::windows::pipe_security` (owner and only grantee:
//! the current user; remote clients rejected), the first one with
//! `FILE_FLAG_FIRST_PIPE_INSTANCE` so a second daemon (or a squatter) cannot share the
//! name. The client verifies the pipe owner before sending anything.

use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};

use forge_core::paths::{ForgeRoot, root_instance_key};
use forge_core::platform::windows::pipe_security::{PipeSecurity, is_pipe_busy, verify_pipe_owner};

use super::framing::{DEFAULT_MAX_FRAME_BYTES, framed};
use super::{Connection, ConnectionInfo, Listener, Transport, TransportError, TransportKind};

/// Pipe name of the default installation.
pub const DEFAULT_PIPE_NAME: &str = r"\\.\pipe\forgebox";

/// How long `connect` retries while every pipe instance is busy.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Pipe name for the installation at `root` (`forge_core::paths::root_instance_key`, the
/// same key as the daemon's single-instance mutex).
pub fn pipe_name_for_root(root: &ForgeRoot) -> String {
    match root_instance_key(root) {
        None => DEFAULT_PIPE_NAME.to_owned(),
        Some(key) => format!("{DEFAULT_PIPE_NAME}-{key}"),
    }
}

/// Named pipe transport.
#[derive(Debug, Clone)]
pub struct NamedPipeTransport {
    name: String,
    max_frame: usize,
}

impl NamedPipeTransport {
    /// Transport for an explicit pipe name (`\\.\pipe\...`).
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            max_frame: DEFAULT_MAX_FRAME_BYTES,
        }
    }

    /// Transport of the installation at `root`.
    pub fn for_root(root: &ForgeRoot) -> Self {
        Self::new(pipe_name_for_root(root))
    }

    /// Frame size limit (both directions).
    pub fn with_max_frame(mut self, max_frame: usize) -> Self {
        self.max_frame = max_frame;
        self
    }

    /// The pipe name.
    pub fn name(&self) -> &str {
        &self.name
    }

    fn info(&self) -> ConnectionInfo {
        ConnectionInfo {
            transport: TransportKind::NamedPipe,
            peer: format!("pipe:{}", self.name),
            local: true,
        }
    }
}

impl Transport for NamedPipeTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::NamedPipe
    }

    fn endpoint(&self) -> String {
        self.name.clone()
    }

    fn connect(&self) -> BoxFuture<'_, Result<Connection, TransportError>> {
        async move {
            let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
            let client = loop {
                match ClientOptions::new().open(&self.name) {
                    Ok(client) => break client,
                    Err(err) if is_pipe_busy(&err) => {
                        if tokio::time::Instant::now() >= deadline {
                            return Err(TransportError::Unavailable {
                                endpoint: self.name.clone(),
                                reason: "every pipe instance stayed busy".into(),
                            });
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        return Err(TransportError::Unavailable {
                            endpoint: self.name.clone(),
                            reason: "the daemon is not running".into(),
                        });
                    }
                    Err(err) => return Err(TransportError::io("opening the API pipe", &err)),
                }
            };
            verify_pipe_owner(&client).map_err(|e| TransportError::Untrusted(e.to_string()))?;
            Ok(framed(client, self.info(), self.max_frame))
        }
        .boxed()
    }

    fn bind(&self) -> BoxFuture<'_, Result<Box<dyn Listener>, TransportError>> {
        async move {
            let security = PipeSecurity::current_user().map_err(|e| {
                TransportError::io(
                    "building the pipe security descriptor",
                    &std::io::Error::other(e.to_string()),
                )
            })?;
            let first = security
                .create_server(ServerOptions::new().first_pipe_instance(true), &self.name)
                .map_err(|err| {
                    if err.kind() == std::io::ErrorKind::PermissionDenied {
                        TransportError::AddrInUse(self.name.clone())
                    } else {
                        TransportError::io("creating the API pipe", &err)
                    }
                })?;
            Ok(Box::new(NamedPipeListener {
                transport: self.clone(),
                security: Arc::new(security),
                next: Some(first),
            }) as Box<dyn Listener>)
        }
        .boxed()
    }
}

/// Listener of [`NamedPipeTransport`]: always keeps one instance waiting for a client.
pub struct NamedPipeListener {
    transport: NamedPipeTransport,
    security: Arc<PipeSecurity>,
    next: Option<NamedPipeServer>,
}

impl NamedPipeListener {
    fn new_instance(&self) -> Result<NamedPipeServer, TransportError> {
        self.security
            .create_server(&ServerOptions::new(), &self.transport.name)
            .map_err(|err| TransportError::io("creating an API pipe instance", &err))
    }
}

impl Listener for NamedPipeListener {
    fn accept(&mut self) -> BoxFuture<'_, Result<Connection, TransportError>> {
        async move {
            let server = match self.next.take() {
                Some(server) => server,
                None => self.new_instance()?,
            };
            if let Err(err) = server.connect().await {
                // The instance is unusable; a fresh one is created on the next call.
                return Err(TransportError::io("accepting an API client", &err));
            }
            // Keep an instance waiting before handing this one over, so clients rarely
            // see ERROR_PIPE_BUSY.
            self.next = self.new_instance().ok();
            Ok(framed(
                server,
                self.transport.info(),
                self.transport.max_frame,
            ))
        }
        .boxed()
    }

    fn endpoint(&self) -> String {
        self.transport.name.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::paths::default_root;

    #[test]
    fn default_root_uses_the_short_name() {
        let root = ForgeRoot::new(default_root()).unwrap();
        assert_eq!(pipe_name_for_root(&root), DEFAULT_PIPE_NAME);
        let upper = ForgeRoot::new(r"C:\FORGEBOX\").unwrap();
        assert_eq!(pipe_name_for_root(&upper), DEFAULT_PIPE_NAME);
    }

    #[test]
    fn other_roots_get_a_stable_hashed_name() {
        let a = ForgeRoot::new(r"D:\Portable\Forgebox").unwrap();
        let b = ForgeRoot::new(r"d:/portable/forgebox").unwrap();
        let c = ForgeRoot::new(r"D:\Other").unwrap();
        assert_eq!(pipe_name_for_root(&a), pipe_name_for_root(&b));
        assert_ne!(pipe_name_for_root(&a), pipe_name_for_root(&c));
        assert!(pipe_name_for_root(&a).starts_with(r"\\.\pipe\forgebox-"));
        assert_eq!(pipe_name_for_root(&a).len(), DEFAULT_PIPE_NAME.len() + 17);
    }
}
