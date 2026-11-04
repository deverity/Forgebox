//! `ProviderRegistry::resolve_traced` (M2-E2-T5): whether an answer came from stale cached
//! metadata, the per-provider time limit, and the clear errors of a cold cache.

mod provider_support;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use forge_manifest::schema::ToolManifest;
use forge_provider::provider::{Endpoints, ResolveRequest};
use provider_support::{Env, PHP_VERSIONING, tool};

fn php_tool() -> ToolManifest {
    tool(
        "php",
        PHP_VERSIONING,
        "    providers:\n      - type: php-windows\n        include_archives: true\n        qa: false",
    )
}

async fn php_env(tweak: impl FnOnce(&mut forge_provider::provider::ProviderConfig)) -> Env {
    let env = Env::with(tweak).await;
    env.upstream
        .serve_fixture("/php/releases/releases.json", "php-windows/releases.json");
    env.upstream
        .serve_fixture("/php/releases/archives/", "php-windows/archives.html");
    env.upstream
        .serve_fixture("/php/qa/", "php-windows/qa.html");
    env
}

async fn traced(
    env: &Env,
    php: &ToolManifest,
    limit: Duration,
) -> (
    forge_core::error::Result<Vec<forge_provider::provider::Release>>,
    forge_provider::provider::FetchTrace,
) {
    let variants = BTreeMap::new();
    env.registry
        .resolve_traced(
            ResolveRequest {
                tool: php,
                constraint: None,
                variants: &variants,
                channel: Some("stable"),
            },
            limit,
        )
        .await
}

#[tokio::test]
async fn a_warm_cache_offline_is_stale_and_a_fresh_answer_is_not() {
    let env = php_env(|_| {}).await;
    let php = php_tool();
    let (fresh, trace) = traced(&env, &php, Duration::from_secs(10)).await;
    assert!(!fresh.unwrap().is_empty());
    assert!(!trace.stale);

    env.registry.set_offline(true);
    let (cached, trace) = traced(&env, &php, Duration::from_secs(10)).await;
    assert!(!cached.unwrap().is_empty());
    assert!(trace.stale, "offline serves the cache and says so");
    let age = jiff::Timestamp::now().as_second() - trace.fetched_at.as_second();
    assert!(
        (0..60).contains(&age),
        "fetched_at is when the cache was written: {age}s"
    );
}

#[tokio::test]
async fn an_expired_cache_is_stale_when_the_upstream_fails() {
    let env = php_env(|c| c.ttl = Duration::ZERO).await;
    let php = php_tool();
    let (first, trace) = traced(&env, &php, Duration::from_secs(10)).await;
    first.unwrap();
    assert!(!trace.stale);
    env.upstream.fail("/php/releases/releases.json", 502);
    let (second, trace) = traced(&env, &php, Duration::from_secs(10)).await;
    assert!(!second.unwrap().is_empty());
    assert!(trace.stale);
}

#[tokio::test]
async fn a_cold_cache_without_network_fails_fast_with_an_io_error() {
    // Nothing listens on the endpoint (the port was just released).
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let env = php_env(|c| {
        c.endpoints = Endpoints {
            php_windows: format!("http://127.0.0.1:{port}/php"),
            ..c.endpoints.clone()
        };
    })
    .await;
    let started = Instant::now();
    let (result, trace) = traced(&env, &php_tool(), Duration::from_secs(12)).await;
    let err = result.unwrap_err();
    assert_eq!(err.code(), "io", "{err}");
    assert!(!trace.stale);
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_provider_that_never_answers_times_out_within_the_limit() {
    // Accepts connections and stays silent.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = silent.local_addr().unwrap().port();
    let keep = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = silent.accept().await {
            held.push(socket);
        }
    });
    let env = php_env(|c| {
        c.endpoints = Endpoints {
            php_windows: format!("http://127.0.0.1:{port}/php"),
            ..c.endpoints.clone()
        };
    })
    .await;
    let started = Instant::now();
    let (result, _) = traced(&env, &php_tool(), Duration::from_millis(400)).await;
    let err = result.unwrap_err();
    assert_eq!(err.code(), "timeout", "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    keep.abort();
}

#[tokio::test]
async fn offline_with_a_cold_cache_is_a_clear_conflict() {
    let env = php_env(|c| c.offline = true).await;
    let (result, trace) = traced(&env, &php_tool(), Duration::from_secs(10)).await;
    let err = result.unwrap_err();
    assert_eq!(err.code(), "conflict");
    assert!(err.to_string().contains("offline"), "{err}");
    assert!(!trace.stale);
}
