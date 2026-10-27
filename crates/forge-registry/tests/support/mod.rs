//! Helpers shared by the integration tests of forge-registry.
#![allow(dead_code)]

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use forge_core::Timestamp;
use forge_core::event::EventBus;
use forge_core::ids::JobId;
use forge_core::jobs::{JobInfo, JobRegistry};
use forge_core::paths::ForgeRoot;
use forge_core::store::Store;
use forge_manifest::index::{build_index, render_index, sha256_hex};
use forge_manifest::lint::RegistryDir;
use forge_manifest::schema::registry::{Signature, SignatureType};
use forge_registry::index::{encode_signature, key_id_of_public_key, signing_payload};
use forge_registry::{Registry, ServiceOptions};

/// `tests/fixtures/registry`: signed with the development key.
pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/registry")
}

/// Same registry, but the index lists a wrong sha256 for `hello-db` (signature is valid).
pub fn bad_sha_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/registry-bad-sha")
}

pub fn dev_public_key() -> String {
    forge_registry::index::DEV_PUBLIC_KEY.to_owned()
}

/// Recursively copies `from` into `to`.
pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

pub fn new_key() -> minisign::KeyPair {
    minisign::KeyPair::generate_unencrypted_keypair().unwrap()
}

/// Regenerates `dir/index.yaml` from the manifests in `dir`; signs it with `key` if given.
pub fn index_registry(dir: &Path, name: &str, key: Option<&minisign::KeyPair>) {
    let registry = RegistryDir::new(dir);
    let now = Timestamp::from_second(Timestamp::now().as_second()).unwrap();
    let mut index = build_index(&registry, name, None, now).unwrap();
    index.spec.signature = None;
    if let Some(pair) = key {
        let payload = signing_payload(&index).unwrap();
        let signature = minisign::sign(
            Some(&pair.pk),
            &pair.sk,
            Cursor::new(payload),
            Some("trusted comment"),
            Some("untrusted comment"),
        )
        .unwrap();
        index.spec.signature = Some(Signature {
            kind: SignatureType::Minisign,
            key_id: key_id_of_public_key(&pair.pk.to_base64()).unwrap(),
            sig: encode_signature(&signature.into_string()),
        });
    }
    std::fs::write(registry.index_path(), render_index(&index).unwrap()).unwrap();
}

/// A minimal valid tool manifest.
pub fn tool_yaml(id: &str, extra_install: &str) -> String {
    format!(
        "apiVersion: forgebox.dev/v1\nkind: Tool\nmetadata: {{ id: {id}, name: {id}, category: devtool }}\nspec:\n  platforms: [windows-x64]\n  source:\n    providers:\n      - type: nodejs-dist\n{extra_install}"
    )
}

/// `install.post_install` with a `run` action (refused for third-party registries).
pub const RUN_INSTALL: &str =
    "  install:\n    post_install:\n      - run: { command: \"bin/setup.exe --init\" }\n";

pub struct Env {
    pub tmp: tempfile::TempDir,
    pub root: ForgeRoot,
    pub store: Store,
    pub jobs: JobRegistry,
    pub registry: Registry,
}

impl Env {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(tmp.path().join("forgebox")).unwrap();
        let store = Store::open_in_memory().unwrap();
        let jobs = JobRegistry::new(EventBus::new());
        let registry = Registry::new(
            root.clone(),
            &store,
            jobs.clone(),
            ServiceOptions::default(),
        )
        .unwrap();
        Self {
            tmp,
            root,
            store,
            jobs,
            registry,
        }
    }

    /// A second service over the same root and database, as after a daemon restart.
    pub fn restarted(&self) -> Registry {
        Registry::new(
            self.root.clone(),
            &self.store,
            self.jobs.clone(),
            ServiceOptions::default(),
        )
        .unwrap()
    }

    /// A scratch folder inside the temp dir.
    pub fn scratch(&self, name: &str) -> PathBuf {
        let dir = self.tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Polls the job until it ends.
    pub async fn wait_job(&self, id: JobId) -> JobInfo {
        for _ in 0..500 {
            if let Some(info) = self.jobs.get(id) {
                if info.state.is_terminal() {
                    return info;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("job {id} did not finish");
    }
}

/// Local HTTP server serving a folder, with ETags and a request log.
pub struct TestServer {
    pub base: String,
    pub hits: Arc<AtomicUsize>,
    pub log: Arc<std::sync::Mutex<Vec<String>>>,
}

struct ServerState {
    dir: PathBuf,
    hits: Arc<AtomicUsize>,
    log: Arc<std::sync::Mutex<Vec<String>>>,
}

async fn serve(State(state): State<Arc<ServerState>>, uri: Uri, headers: HeaderMap) -> Response {
    state.hits.fetch_add(1, Ordering::SeqCst);
    let rel = uri.path().trim_start_matches('/').to_owned();
    state.log.lock().unwrap().push(rel.clone());
    let Ok(bytes) = std::fs::read(state.dir.join(&rel)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let etag = format!("\"{}\"", &sha256_hex(&bytes)[..16]);
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    (StatusCode::OK, [(header::ETAG, etag)], bytes).into_response()
}

impl TestServer {
    pub async fn start(dir: &Path) -> Self {
        let hits = Arc::new(AtomicUsize::new(0));
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let state = Arc::new(ServerState {
            dir: dir.to_owned(),
            hits: Arc::clone(&hits),
            log: Arc::clone(&log),
        });
        let app = Router::new().fallback(serve).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base: format!("http://{addr}"),
            hits,
            log,
        }
    }

    pub fn index_url(&self) -> url::Url {
        url::Url::parse(&format!("{}/index.yaml", self.base)).unwrap()
    }

    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}
