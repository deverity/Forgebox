//! Operations on one registry are serialized (docs/03 §7, ADR-0011): `load`, `refresh`,
//! `add` and `remove` never race on the cache or on the policy a catalog was built under.
//!
//! The HTTP server freezes requests for `index.yaml` until the test releases them, so every
//! interleaving below is forced, not hoped for.

mod support;

use std::time::Duration;

use forge_core::jobs::JobState;
use forge_registry::{LoadSource, RegistryConfig, RegistrySource};
use support::*;

/// A signed registry folder with the given tools (`(id, extra spec lines)`), served by HTTP.
async fn served_registry(
    env: &Env,
    folder: &str,
    tools: &[(&str, &str)],
) -> (TestServer, minisign::KeyPair) {
    let dir = env.scratch(folder);
    std::fs::create_dir_all(dir.join("tools")).unwrap();
    for (id, extra) in tools {
        std::fs::write(dir.join(format!("tools/{id}.yaml")), tool_yaml(id, extra)).unwrap();
    }
    let key = new_key();
    index_registry(&dir, folder, Some(&key));
    (TestServer::start(&dir).await, key)
}

fn remote_config(name: &str, server: &TestServer, key: &minisign::KeyPair) -> RegistryConfig {
    let mut config = RegistryConfig::official(
        name,
        RegistrySource::Http(server.index_url()),
        key.pk.to_base64(),
    );
    config.ttl = Duration::from_secs(3600);
    config
}

/// Lets queued tasks run so a (wrongly) unserialized operation would show itself.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

#[tokio::test]
async fn load_and_refresh_at_the_same_time_write_the_cache_once() {
    let env = Env::new();
    let (server, key) = served_registry(&env, "remote", &[("alpha", "")]).await;
    env.registry
        .add(remote_config("remote", &server, &key))
        .await
        .unwrap();

    server.hold_index();
    let loader = {
        let registry = env.registry.clone();
        tokio::spawn(async move { registry.load("remote").await })
    };
    server.index_requested().await;
    // The load is stuck downloading the index. A refresh asked now must wait for its turn.
    let job = env.registry.refresh("remote").unwrap();
    settle().await;
    assert_eq!(
        server.hits(),
        1,
        "the refresh did not start fetching in parallel"
    );
    assert_eq!(env.jobs.get(job).unwrap().state, JobState::Running);
    assert_eq!(env.registry.cache_writes(), 0);

    server.release_all();
    let report = loader.await.unwrap().unwrap();
    assert_eq!(report.source, LoadSource::Source);
    let info = env.wait_job(job).await;
    assert_eq!(info.state, JobState::Done, "{:?}", info.error);
    // The refresh ran after the load and found nothing new (ETag answered 304).
    assert_eq!(info.result.unwrap()["source"], "not_modified");
    assert_eq!(env.registry.cache_writes(), 1, "a single cache write");
    assert!(env.registry.find("alpha").is_some());
    let listed = &env.registry.list()[0];
    assert!(listed.last_error.is_none(), "{:?}", listed.last_error);
    assert!(listed.refreshing.is_none());
}

#[tokio::test]
async fn two_concurrent_loads_do_not_both_hit_the_source() {
    let env = Env::new();
    let (server, key) = served_registry(&env, "remote", &[("alpha", "")]).await;
    env.registry
        .add(remote_config("remote", &server, &key))
        .await
        .unwrap();
    server.hold_index();
    let first = {
        let registry = env.registry.clone();
        tokio::spawn(async move { registry.load("remote").await })
    };
    server.index_requested().await;
    let second = {
        let registry = env.registry.clone();
        tokio::spawn(async move { registry.load("remote").await })
    };
    settle().await;
    server.release_all();
    assert_eq!(first.await.unwrap().unwrap().source, LoadSource::Source);
    // The second one waited its turn and found the catalog in memory, within the TTL.
    assert_eq!(second.await.unwrap().unwrap().source, LoadSource::Memory);
    assert_eq!(env.registry.cache_writes(), 1);
    // index + signature + 1 manifest, once.
    assert_eq!(server.hits(), 3);
}

#[tokio::test]
async fn demoting_a_registry_during_a_refresh_never_serves_run_manifests_as_official() {
    let env = Env::new();
    let (server, key) =
        served_registry(&env, "remote", &[("plain", ""), ("runner", RUN_INSTALL)]).await;
    let official = remote_config("remote", &server, &key);
    env.registry.add(official.clone()).await.unwrap();

    server.hold_index();
    let job = env.registry.refresh("remote").unwrap();
    server.index_requested().await;

    // While the refresh is mid-download, the registry stops being official.
    let mut third_party = official;
    third_party.official = false;
    tokio::time::timeout(Duration::from_secs(5), env.registry.add(third_party))
        .await
        .expect("add does not wait for the slow refresh")
        .unwrap();
    server.release_all();

    let info = env.wait_job(job).await;
    assert_eq!(info.state, JobState::Done, "{:?}", info.error);
    // The result built under the old policy was dropped and the work redone under the new.
    assert!(env.registry.find("plain").is_some());
    assert!(
        env.registry.find("runner").is_none(),
        "a post_install.run manifest must not be served by a third-party registry"
    );
    assert!(
        env.registry.entries().iter().all(|entry| !entry.official),
        "nothing is marked official any more"
    );
    let listed = &env.registry.list()[0];
    assert!(!listed.official);
    assert_eq!(listed.skipped.len(), 1);
    assert_eq!(listed.skipped[0].id, "runner");
    assert_eq!(
        env.registry.cache_writes(),
        1,
        "the stale result never reached the cache"
    );
}

#[tokio::test]
async fn removing_a_registry_cancels_its_refresh_and_leaves_no_cache() {
    let env = Env::new();
    let (server, key) = served_registry(&env, "remote", &[("alpha", "")]).await;
    env.registry
        .add(remote_config("remote", &server, &key))
        .await
        .unwrap();

    server.hold_index();
    let job = env.registry.refresh("remote").unwrap();
    server.index_requested().await;

    tokio::time::timeout(Duration::from_secs(10), env.registry.remove("remote"))
        .await
        .expect("remove does not wait for the frozen request")
        .unwrap();
    server.release_all();

    assert_eq!(env.wait_job(job).await.state, JobState::Cancelled);
    assert!(env.registry.list().is_empty());
    assert_no_leftovers(&env, "remote");
    assert_eq!(env.registry.cache_writes(), 0);
}

#[tokio::test]
async fn removing_a_registry_during_a_load_waits_for_it_and_deletes_what_it_wrote() {
    let env = Env::new();
    let (server, key) = served_registry(&env, "remote", &[("alpha", "")]).await;
    env.registry
        .add(remote_config("remote", &server, &key))
        .await
        .unwrap();

    server.hold_index();
    let loader = {
        let registry = env.registry.clone();
        tokio::spawn(async move { registry.load("remote").await })
    };
    server.index_requested().await;
    let remover = {
        let registry = env.registry.clone();
        tokio::spawn(async move { registry.remove("remote").await })
    };
    settle().await;
    assert!(
        !remover.is_finished(),
        "remove waits for the load in progress"
    );

    server.release_all();
    loader.await.unwrap().unwrap();
    remover.await.unwrap().unwrap();
    assert!(env.registry.list().is_empty());
    assert_no_leftovers(&env, "remote");
    assert!(env.registry.find("alpha").is_none());
    assert_eq!(
        env.registry.load("remote").await.unwrap_err().code(),
        "not_found"
    );
}

fn assert_no_leftovers(env: &Env, name: &str) {
    let base = env.root.state_registry();
    if !base.exists() {
        return;
    }
    let leftovers: Vec<String> = std::fs::read_dir(&base)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(name))
        .collect();
    assert!(
        leftovers.is_empty(),
        "left in state/registry: {leftovers:?}"
    );
}
