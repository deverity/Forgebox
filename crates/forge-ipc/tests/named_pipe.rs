//! Real named pipe: owner-only security, token handshake, simultaneous clients, one
//! server per pipe name, and the local root discovery path (`connect_local`).
#![cfg(windows)]

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde_json::json;

use forge_core::event::{EventBus, EventKind};
use forge_core::paths::ForgeRoot;
use forge_core::platform::windows::pipe_security::pipe_sddl;
use forge_ipc::api::{EventFilter, NoParams, Role, TransportKind};
use forge_ipc::auth::{IpcToken, TokenAuthenticator};
use forge_ipc::server::{EventSource, Server};
use forge_ipc::transport::{NamedPipeTransport, Transport, TransportError, pipe_name_for_root};
use forge_ipc::{Client, ClientOptions, Credentials};

const WAIT: Duration = Duration::from_secs(5);

fn unique_pipe() -> NamedPipeTransport {
    NamedPipeTransport::new(format!(
        r"\\.\pipe\forgebox-test-{}",
        ulid::Ulid::generate()
    ))
}

async fn start(
    transport: &NamedPipeTransport,
    token: &IpcToken,
    bus: &EventBus,
) -> forge_ipc::ServerHandle {
    let server = Server::builder(Arc::new(TokenAuthenticator::new(token.clone())))
        .events(EventSource::new(bus.clone()))
        .build();
    let (handle, _task) = server.bind_and_spawn(transport).await.unwrap();
    handle
}

fn client(transport: &NamedPipeTransport, token: IpcToken, name: &str) -> Client {
    Client::new(
        Arc::new(transport.clone()) as Arc<dyn Transport>,
        Credentials::new(token, name, "0.0.0"),
        ClientOptions::default(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hello_with_valid_token_and_two_simultaneous_clients() {
    let transport = unique_pipe();
    let token = IpcToken::generate().unwrap();
    let bus = EventBus::new();
    let handle = start(&transport, &token, &bus).await;

    let gui = client(&transport, token.clone(), "Forgebox.exe");
    let cli = client(&transport, token.clone(), "forge");
    let (hello_gui, hello_cli) = tokio::join!(gui.hello(), cli.hello());
    let (hello_gui, hello_cli) = (hello_gui.unwrap(), hello_cli.unwrap());
    for hello in [&hello_gui, &hello_cli] {
        assert_eq!(hello.transport, TransportKind::NamedPipe);
        assert_eq!(hello.role, Role::Admin);
        assert!(hello.local);
    }
    assert_ne!(hello_gui.session_id, hello_cli.session_id);
    assert_eq!(handle.connection_count(), 2);

    let mut gui_events = gui
        .subscribe_events(EventFilter::default(), None)
        .await
        .unwrap();
    let mut cli_events = cli
        .subscribe_events(EventFilter::kinds(["site.*"]), None)
        .await
        .unwrap();
    // Interleaved calls on both connections while events flow.
    let calls = async {
        for _ in 0..20 {
            let (a, b) = tokio::join!(gui.system_ping(NoParams {}), cli.daemon_status(NoParams {}));
            a.unwrap();
            assert_eq!(b.unwrap().connections, 2);
        }
    };
    let publish = async {
        for n in 0..50 {
            bus.publish(EventKind::SiteUpdated, json!({ "n": n }));
            tokio::task::yield_now().await;
        }
    };
    tokio::join!(calls, publish);
    for n in 0..50 {
        let a = tokio::time::timeout(WAIT, gui_events.next())
            .await
            .unwrap()
            .unwrap();
        let b = tokio::time::timeout(WAIT, cli_events.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(a.event.payload["n"], n);
        assert_eq!(b.event.payload["n"], n);
    }
    handle.shutdown();
}

#[tokio::test]
async fn invalid_token_is_refused() {
    let transport = unique_pipe();
    let token = IpcToken::generate().unwrap();
    let handle = start(&transport, &token, &EventBus::new()).await;
    let intruder = client(&transport, IpcToken::generate().unwrap(), "intruder");
    let err = intruder.system_ping(NoParams {}).await.unwrap_err();
    assert_eq!(err.code(), "unauthorized");
    assert_eq!(err.exit_code(), 1);
    // The legitimate client is not affected.
    client(&transport, token, "forge")
        .system_ping(NoParams {})
        .await
        .unwrap();
    handle.shutdown();
}

#[tokio::test]
async fn one_server_per_pipe_and_owner_only_security() {
    let transport = unique_pipe();
    let token = IpcToken::generate().unwrap();
    let handle = start(&transport, &token, &EventBus::new()).await;
    match transport.bind().await {
        Err(TransportError::AddrInUse(name)) => assert_eq!(name, transport.name()),
        Err(other) => panic!("expected AddrInUse, got {other}"),
        Ok(_) => panic!("a second server must not bind the same pipe"),
    }
    // The pipe is owned by, and only accessible to, the current user.
    let raw = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(transport.name())
        .unwrap();
    let sddl = pipe_sddl(&raw).unwrap();
    let sid = forge_core::platform::windows::current_user_sid().unwrap();
    assert!(sddl.starts_with(&format!("O:{sid}")), "{sddl}");
    assert!(
        forge_core::platform::windows::dacl_grants_only(&sddl, &sid),
        "{sddl}"
    );
    drop(raw);
    handle.shutdown();
}

#[tokio::test]
async fn daemon_not_running_is_reported_as_unavailable() {
    let transport = unique_pipe();
    let err = client(&transport, IpcToken::generate().unwrap(), "forge")
        .system_ping(NoParams {})
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            forge_ipc::ClientError::Transport(TransportError::Unavailable { .. })
        ),
        "{err:?}"
    );
    assert_eq!(err.code(), "io");
}

#[tokio::test]
async fn connect_local_uses_the_root_pipe_name_and_token_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = ForgeRoot::new(dir.path()).unwrap();
    let token = IpcToken::load_or_create(
        &root.ipc_token(),
        forge_core::platform::current().file_acl(),
    )
    .unwrap();
    let transport = NamedPipeTransport::for_root(&root);
    assert_eq!(transport.name(), pipe_name_for_root(&root));
    assert_ne!(
        transport.name(),
        r"\\.\pipe\forgebox",
        "temp roots get their own pipe"
    );
    let handle = start(&transport, &token, &EventBus::new()).await;
    let client = Client::connect_local(&root, "forge", "0.0.0")
        .await
        .unwrap();
    let info = client.connection_info().await.unwrap();
    assert_eq!(info.transport, TransportKind::NamedPipe);
    client.system_ping(NoParams {}).await.unwrap();
    handle.shutdown();
}
