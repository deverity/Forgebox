//! The app updater against a real local HTTP server and the real `tauri-plugin-updater`
//! (mock Tauri runtime): a `latest.json` per case, minisign key pairs generated per test, a
//! recording daemon and a recording installer. Nothing touches the network, the registry or
//! a real installer (ADR-0038).

use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::json;
use tauri::test::{MockRuntime, mock_builder, mock_context, noop_assets};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use forgebox_desktop::i18n::Language;
use forgebox_desktop::updater::{
    Channel, DaemonStop, Downloaded, Phase, TauriSource, UpdateInfo, UpdateService, UpdateSource,
    UpdaterError,
};

const INSTALLER: &[u8] = b"pretend this is an NSIS installer";

/// A minisign key pair in Tauri's encodings.
struct Keys {
    pair: minisign::KeyPair,
}

impl Keys {
    fn generate() -> Self {
        Self {
            pair: minisign::KeyPair::generate_unencrypted_keypair().unwrap(),
        }
    }

    /// `pubkey` of the plugin config: base64 of the `.pub` file.
    fn pubkey(&self) -> String {
        B64.encode(self.pair.pk.to_box().unwrap().to_string())
    }

    /// `signature` of `latest.json`: base64 of the `.sig` file.
    fn sign(&self, data: &[u8]) -> String {
        let sig = minisign::sign(
            Some(&self.pair.pk),
            &self.pair.sk,
            Cursor::new(data),
            Some("timestamp:1 file:Forgebox_setup.exe"),
            Some("signed by a test"),
        )
        .unwrap();
        B64.encode(sig.to_string())
    }
}

fn platform_key() -> String {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86" => "i686",
        _ => "x86_64",
    };
    format!("windows-{arch}")
}

type Routes = Arc<Mutex<Vec<(String, Vec<u8>, &'static str)>>>;

/// A local HTTP server whose routes can be set after its address is known.
struct Server {
    base: String,
    routes: Routes,
    hits: Arc<Mutex<Vec<String>>>,
}

impl Server {
    async fn start() -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: Routes = Arc::default();
        let hits: Arc<Mutex<Vec<String>>> = Arc::default();
        let (shared_routes, seen) = (routes.clone(), hits.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let (routes, seen) = (shared_routes.clone(), seen.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let path = head
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .to_owned();
                    seen.lock().unwrap().push(path.clone());
                    let found = routes
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|(p, _, _)| *p == path)
                        .cloned();
                    let reply = match found {
                        Some((_, body, kind)) => {
                            let mut out = format!(
                                "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                                body.len()
                            )
                            .into_bytes();
                            out.extend_from_slice(&body);
                            out
                        }
                        None => b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            .to_vec(),
                    };
                    let _ = socket.write_all(&reply).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Server { base, routes, hits }
    }

    fn route(&self, path: &str, body: Vec<u8>, kind: &'static str) {
        self.routes
            .lock()
            .unwrap()
            .push((path.to_owned(), body, kind));
    }

    /// Publishes `file` (a manifest name) announcing `version`, with the installer signed by
    /// `signer` while the served bytes are `served`.
    fn publish(&self, file: &str, version: &str, signer: &Keys, served: &[u8], notes: &str) {
        let signature = signer.sign(INSTALLER);
        self.route(
            &format!("/{file}"),
            manifest(&self.base, version, &signature, notes),
            "application/json",
        );
        self.route("/pkg.exe", served.to_vec(), "application/octet-stream");
    }

    fn template(&self) -> String {
        format!("{}/{{channel_file}}", self.base)
    }
}

fn manifest(base: &str, version: &str, signature: &str, notes: &str) -> Vec<u8> {
    json!({
        "version": version,
        "notes": notes,
        "pub_date": "2026-11-01T12:00:00Z",
        "platforms": {
            platform_key(): { "signature": signature, "url": format!("{base}/pkg.exe") }
        }
    })
    .to_string()
    .into_bytes()
}

/// A mock app (version 0.1.0) with the real updater plugin trusting `pubkey`.
fn mock_app(pubkey: &str) -> tauri::App<MockRuntime> {
    let mut context = mock_context(noop_assets());
    context.config_mut().plugins.0.insert(
        "updater".to_owned(),
        json!({ "pubkey": pubkey, "endpoints": [], "windows": { "installMode": "passive" } }),
    );
    mock_builder()
        .plugin(
            tauri_plugin_updater::Builder::new()
                .pubkey(pubkey.to_owned())
                .build(),
        )
        .build(context)
        .unwrap()
}

type Log = Arc<Mutex<Vec<String>>>;

/// The real source with the installer replaced by a recorder.
struct Recording {
    inner: TauriSource<MockRuntime>,
    log: Log,
}

impl UpdateSource for Recording {
    type Package = Downloaded;
    async fn check(&self) -> Result<Option<UpdateInfo>, UpdaterError> {
        self.inner.check().await
    }
    async fn download(&self) -> Result<Downloaded, UpdaterError> {
        self.log.lock().unwrap().push("download".to_owned());
        self.inner.download().await
    }
    fn install(&self, _package: Downloaded) -> Result<(), UpdaterError> {
        self.log.lock().unwrap().push("install".to_owned());
        Ok(())
    }
}

struct Daemon {
    log: Log,
    fail: bool,
}

impl DaemonStop for Daemon {
    async fn stop(&self) -> Result<(), String> {
        self.log.lock().unwrap().push("daemon.stop".to_owned());
        if self.fail {
            Err("busy".to_owned())
        } else {
            Ok(())
        }
    }
}

struct Rig {
    // Keeps the app (and its plugin state) alive for the service.
    _app: tauri::App<MockRuntime>,
    service: UpdateService<Recording, Daemon>,
    log: Log,
}

fn rig(keys: &Keys, endpoint: String, channel: Channel, fail_stop: bool) -> Rig {
    let app = mock_app(&keys.pubkey());
    let log: Log = Arc::default();
    let source = Arc::new(Recording {
        inner: TauriSource::new(app.handle().clone(), endpoint, move || channel),
        log: log.clone(),
    });
    let service = UpdateService::new(
        source,
        Daemon {
            log: log.clone(),
            fail: fail_stop,
        },
        true,
        "0.1.0",
        Box::new(|_| {}),
    )
    .with_stop_timeout(Duration::from_secs(2));
    Rig {
        _app: app,
        service,
        log,
    }
}

#[tokio::test]
async fn a_newer_version_is_detected_and_applied_with_the_daemon_stopped_first() {
    let keys = Keys::generate();
    let server = Server::start().await;
    server.publish(
        "latest.json",
        "0.2.0",
        &keys,
        INSTALLER,
        "- faster\n- fixes",
    );
    let rig = rig(&keys, server.template(), Channel::Stable, false);

    let found = rig
        .service
        .check()
        .await
        .unwrap()
        .expect("0.2.0 is newer than 0.1.0");
    assert_eq!(found.version, "0.2.0");
    assert_eq!(found.notes.as_deref(), Some("- faster\n- fixes"));
    let snapshot = rig.service.snapshot();
    assert_eq!(snapshot.phase, Phase::Available);
    assert_eq!(snapshot.current_version, "0.1.0");

    rig.service.apply().await.unwrap();
    assert_eq!(
        *rig.log.lock().unwrap(),
        ["download", "daemon.stop", "install"],
        "signature checked, daemon stopped, then the installer"
    );
    assert!(server.hits.lock().unwrap().contains(&"/pkg.exe".to_owned()));
}

#[tokio::test]
async fn an_equal_or_older_version_is_ignored() {
    let keys = Keys::generate();
    let server = Server::start().await;
    for version in ["0.1.0", "0.0.9"] {
        server.routes.lock().unwrap().clear();
        server.publish("latest.json", version, &keys, INSTALLER, "");
        let rig = rig(&keys, server.template(), Channel::Stable, false);
        assert_eq!(rig.service.check().await.unwrap(), None, "{version}");
        assert_eq!(rig.service.snapshot().phase, Phase::UpToDate);
        assert_eq!(rig.service.apply().await, Err(UpdaterError::NoUpdate));
    }
}

#[tokio::test]
async fn the_beta_channel_reads_its_own_manifest() {
    let keys = Keys::generate();
    let server = Server::start().await;
    server.publish("latest.json", "0.2.0", &keys, INSTALLER, "stable");
    server.publish("latest-beta.json", "0.3.0-beta.1", &keys, INSTALLER, "beta");
    let stable = rig(&keys, server.template(), Channel::Stable, false);
    assert_eq!(
        stable.service.check().await.unwrap().unwrap().version,
        "0.2.0"
    );
    let beta = rig(&keys, server.template(), Channel::Beta, false);
    let found = beta.service.check().await.unwrap().unwrap();
    assert_eq!(found.version, "0.3.0-beta.1");
    assert_eq!(found.notes.as_deref(), Some("beta"));
}

#[tokio::test]
async fn a_signature_from_another_key_is_rejected_with_a_localized_cause() {
    let trusted = Keys::generate();
    let attacker = Keys::generate();
    let server = Server::start().await;
    server.publish("latest.json", "0.2.0", &attacker, INSTALLER, "");
    let rig = rig(&trusted, server.template(), Channel::Stable, false);
    rig.service.check().await.unwrap().unwrap();

    let err = rig.service.apply().await.unwrap_err();
    assert_eq!(err, UpdaterError::BadSignature);
    assert!(err.message(Language::PtBr).contains("assinatura"));
    assert!(err.message(Language::EnUs).contains("signature"));
    assert_eq!(
        *rig.log.lock().unwrap(),
        ["download"],
        "neither the daemon nor the installer were touched"
    );
    assert_eq!(rig.service.snapshot().phase, Phase::Error);
}

#[tokio::test]
async fn tampered_bytes_fail_the_signature_check() {
    let keys = Keys::generate();
    let server = Server::start().await;
    server.publish("latest.json", "0.2.0", &keys, b"a different installer", "");
    let rig = rig(&keys, server.template(), Channel::Stable, false);
    rig.service.check().await.unwrap().unwrap();
    assert_eq!(rig.service.apply().await, Err(UpdaterError::BadSignature));
    assert!(!rig.log.lock().unwrap().iter().any(|e| e == "install"));
}

#[tokio::test]
async fn a_daemon_that_fails_to_stop_blocks_the_installer() {
    let keys = Keys::generate();
    let server = Server::start().await;
    server.publish("latest.json", "0.2.0", &keys, INSTALLER, "");
    let rig = rig(&keys, server.template(), Channel::Stable, true);
    rig.service.check().await.unwrap().unwrap();
    let err = rig.service.apply().await.unwrap_err();
    assert_eq!(err, UpdaterError::DaemonStop("busy".to_owned()));
    assert_eq!(*rig.log.lock().unwrap(), ["download", "daemon.stop"]);
}

#[tokio::test]
async fn an_unreachable_server_is_a_network_error_not_a_signature_error() {
    let keys = Keys::generate();
    // The listener is dropped, so nothing answers on this port.
    let dead = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}/{{channel_file}}", listener.local_addr().unwrap())
    };
    let rig = rig(&keys, dead, Channel::Stable, false);
    let err = rig.service.check().await.unwrap_err();
    assert!(matches!(err, UpdaterError::Network(_)), "{err:?}");
    assert_eq!(rig.service.snapshot().error.unwrap().code, "io");
}
