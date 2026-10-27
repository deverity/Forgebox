//! Helpers shared by the integration tests: an in-memory server with a token and a
//! client connected to it.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use forge_core::event::{EventBus, EventBusOptions};
use forge_ipc::auth::{IpcToken, TokenAuthenticator};
use forge_ipc::server::{Router, Server, ServerBuilder, ServerConfig, ServerHandle};
use forge_ipc::transport::{InMemoryTransport, Transport};
use forge_ipc::{Client, ClientOptions, Credentials};

pub struct TestServer {
    pub transport: Arc<InMemoryTransport>,
    pub handle: ServerHandle,
    pub token: IpcToken,
}

impl TestServer {
    pub fn client_with(&self, options: ClientOptions) -> Client {
        Client::new(
            self.transport.clone() as Arc<dyn Transport>,
            Credentials::new(self.token.clone(), "forge-test", "0.0.0"),
            options,
        )
    }

    pub fn client(&self) -> Client {
        self.client_with(fast_options())
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

/// Client options with short timeouts for tests.
pub fn fast_options() -> ClientOptions {
    ClientOptions {
        call_timeout: Duration::from_secs(5),
        connect_timeout: Duration::from_secs(5),
        reconnect_min: Duration::from_millis(20),
        reconnect_max: Duration::from_millis(200),
        ..ClientOptions::default()
    }
}

/// A bus with a small history, to exercise the store fallback.
pub fn bus(history: usize) -> EventBus {
    EventBus::with_options(EventBusOptions {
        capacity: 8192,
        history,
        next_seq: 1,
    })
}

/// Builds and spawns a server on `transport`.
pub async fn spawn_on(
    transport: InMemoryTransport,
    configure: impl FnOnce(ServerBuilder) -> ServerBuilder,
) -> TestServer {
    let token = IpcToken::generate().unwrap();
    let builder = Server::builder(Arc::new(TokenAuthenticator::new(token.clone())));
    let server = configure(builder).build();
    let transport = Arc::new(transport);
    let (handle, _task) = server.bind_and_spawn(transport.as_ref()).await.unwrap();
    TestServer {
        transport,
        handle,
        token,
    }
}

/// Local in-memory server with `router` and default config.
pub async fn spawn(router: Router) -> TestServer {
    spawn_on(InMemoryTransport::new(), |b| b.router(router)).await
}

/// Server config with a short request timeout.
pub fn quick_config() -> ServerConfig {
    ServerConfig {
        request_timeout: Duration::from_millis(300),
        ..ServerConfig::default()
    }
}

/// Polls `check` until it is true or `timeout` passes.
pub async fn eventually(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    check()
}
