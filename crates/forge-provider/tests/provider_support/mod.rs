//! Test support for the provider tests: a local HTTP server that serves recorded
//! fixtures (with `ETag`/`304`), records every request, and helpers to build manifests.

#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::Response;
use forge_manifest::schema::{Manifest, ToolManifest};
use forge_provider::provider::{Endpoints, ProviderConfig, ProviderRegistry};
use sha2::{Digest, Sha256};

pub fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(rel)
}

pub fn fixture(rel: &str) -> Vec<u8> {
    std::fs::read(fixture_path(rel)).unwrap_or_else(|e| panic!("fixture {rel}: {e}"))
}

#[derive(Clone)]
struct Route {
    body: Vec<u8>,
    status: u16,
    headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    pub if_none_match: Option<String>,
    pub authorization: Option<String>,
}

#[derive(Default)]
struct Shared {
    routes: Mutex<HashMap<String, Route>>,
    hits: Mutex<Vec<Hit>>,
}

pub struct Upstream {
    addr: SocketAddr,
    shared: Arc<Shared>,
}

impl Upstream {
    pub async fn start() -> Self {
        let shared = Arc::new(Shared::default());
        let app = Router::new().fallback(handle).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { addr, shared }
    }

    pub fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn serve(&self, path: &str, body: impl Into<Vec<u8>>) {
        self.shared.routes.lock().unwrap().insert(
            path.to_owned(),
            Route {
                body: body.into(),
                status: 200,
                headers: Vec::new(),
            },
        );
    }

    pub fn serve_fixture(&self, path: &str, rel: &str) {
        self.serve(path, fixture(rel));
    }

    /// Answers `status` (empty body) for `path`.
    pub fn fail(&self, path: &str, status: u16) {
        self.fail_with(path, status, &[]);
    }

    pub fn fail_with(&self, path: &str, status: u16, headers: &[(&str, &str)]) {
        self.shared.routes.lock().unwrap().insert(
            path.to_owned(),
            Route {
                body: Vec::new(),
                status,
                headers: headers
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            },
        );
    }

    pub fn hits(&self, path: &str) -> Vec<Hit> {
        self.shared
            .hits
            .lock()
            .unwrap()
            .iter()
            .filter(|h| h.path == path)
            .cloned()
            .collect()
    }

    pub fn total_hits(&self) -> usize {
        self.shared.hits.lock().unwrap().len()
    }
}

async fn handle(
    State(shared): State<Arc<Shared>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let path = uri.path().to_owned();
    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    shared.hits.lock().unwrap().push(Hit {
        path: path.clone(),
        if_none_match: if_none_match.clone(),
        authorization: headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    });
    let route = shared.routes.lock().unwrap().get(&path).cloned();
    let Some(route) = route else {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap();
    };
    let mut builder = Response::builder().status(route.status);
    for (k, v) in &route.headers {
        builder = builder.header(k.as_str(), HeaderValue::from_str(v).unwrap());
    }
    if route.status != 200 {
        return builder.body(Body::empty()).unwrap();
    }
    let etag = format!("\"{}\"", hex(&Sha256::digest(&route.body))[..16].to_owned());
    builder = builder.header(header::ETAG, &etag);
    if method == Method::GET && if_none_match.as_deref() == Some(etag.as_str()) {
        return builder
            .status(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .unwrap();
    }
    builder.body(Body::from(route.body)).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// All endpoints on one local server (the providers use distinct path prefixes).
pub fn endpoints(base: &str) -> Endpoints {
    Endpoints {
        php_windows: format!("{base}/php"),
        apache_lounge: base.to_owned(),
        nginx_org: base.to_owned(),
        github_api: format!("{base}/gh"),
        nodejs_dist: format!("{base}/node/dist"),
        mariadb: format!("{base}/rest-api/mariadb"),
    }
}

pub struct Env {
    pub upstream: Upstream,
    pub registry: ProviderRegistry,
    pub cache: tempfile::TempDir,
}

impl Env {
    pub async fn new() -> Self {
        Self::with(|_| {}).await
    }

    pub async fn with(tweak: impl FnOnce(&mut ProviderConfig)) -> Self {
        let upstream = Upstream::start().await;
        let cache = tempfile::tempdir().unwrap();
        let mut config = ProviderConfig::new(cache.path().join("cache"));
        config.endpoints = endpoints(&upstream.base());
        config.request_timeout = Duration::from_secs(10);
        tweak(&mut config);
        let registry = ProviderRegistry::new(config).unwrap();
        Self {
            upstream,
            registry,
            cache,
        }
    }
}

/// Parses a Tool manifest; `versioning`, `source` and `extra` are YAML fragments
/// indented by the caller (2 spaces under `spec:`).
pub fn tool(id: &str, versioning: &str, source: &str) -> ToolManifest {
    let yaml = format!(
        "apiVersion: forgebox.dev/v1\nkind: Tool\nmetadata:\n  id: {id}\n  name: {id}\n  category: runtime\nspec:\n  platforms: [windows-x64]\n{versioning}\n  source:\n{source}\n"
    );
    match forge_manifest::load_str(&yaml) {
        Ok(Manifest::Tool(tool)) => tool,
        Ok(_) => panic!("not a tool"),
        Err(err) => panic!("manifest error: {err}\n{yaml}"),
    }
}

pub const PHP_VERSIONING: &str = r#"  versioning:
    channels:
      stable: { constraint: ">=7.4 <9" }
      preview: { constraint: ">=7.4.0-0", prerelease: true }
    variants:
      - key: ts
        values: [nts, ts]
        default: nts
      - key: compiler
        values: [vs17, vs16, vc15]
        derive: from_provider
      - key: arch
        values: [x64]
        default: x64"#;
