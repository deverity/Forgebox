//! Catalog icons (M2-E2-T4): the signed index lists them with their sha256, every source
//! delivers them, the cache keeps them and a hash mismatch refuses the whole registry.
//! Works on `tests/fixtures/registry-icons` (signed with the development key).

mod support;

use std::path::{Path, PathBuf};

use forge_registry::{RegistryConfig, RegistrySource, icon::sanitize_svg, search::MatchKind};
use support::*;

fn icons_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/registry-icons")
}

fn config(name: &str, source: RegistrySource) -> RegistryConfig {
    RegistryConfig::official(name, source, dev_public_key())
}

const WEB_SVG_SHA: &str = "hello-web.svg";

fn sha_of(dir: &Path, icon: &str) -> String {
    forge_manifest::index::sha256_hex(&std::fs::read(dir.join("icons").join(icon)).unwrap())
}

#[tokio::test]
async fn dir_source_delivers_icons_and_the_cache_keeps_them() {
    let env = Env::new();
    env.registry
        .add(config("icons", RegistrySource::Dir(icons_fixture())))
        .await
        .unwrap();
    let report = env.registry.load("icons").await.unwrap();
    assert!(report.verification.is_verified(), "{report:?}");
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);

    let web = env.registry.find("hello-web").unwrap();
    let icon = web.icon.as_ref().expect("hello-web has an icon");
    assert_eq!(icon.path, "icons/hello-web.svg");
    assert_eq!(icon.sha256, sha_of(&icons_fixture(), WEB_SVG_SHA));
    // The accepted document is byte-identical to the file the index hashed.
    let svg = sanitize_svg(&icon.bytes).unwrap();
    assert_eq!(
        forge_manifest::index::sha256_hex(svg.as_bytes()),
        icon.sha256
    );
    assert!(env.registry.find("hello-db").unwrap().icon.is_none());

    // Cache: the verified icons are written next to the manifests.
    let cache = env.registry.cache_path("icons");
    assert!(cache.join("icons/hello-web.svg").is_file());
    assert!(cache.join("icons/evil-script.svg").is_file());

    // A restart (new service over the same cache, source gone) still has them.
    let again = env.restarted();
    again
        .add(config("icons", RegistrySource::Dir(env.scratch("missing"))))
        .await
        .unwrap();
    let report = again.load("icons").await.unwrap();
    assert!(report.tools > 0);
    let web = again.find("hello-web").unwrap();
    assert_eq!(
        web.icon.map(|i| i.sha256),
        Some(sha_of(&icons_fixture(), WEB_SVG_SHA))
    );
}

#[tokio::test]
async fn the_six_malicious_icons_load_but_are_refused_when_served() {
    let env = Env::new();
    env.registry
        .add(config("icons", RegistrySource::Dir(icons_fixture())))
        .await
        .unwrap();
    env.registry.load("icons").await.unwrap();
    let mut refused = 0;
    for id in [
        "evil-script",
        "evil-foreign",
        "evil-onload",
        "evil-href",
        "evil-external",
        "evil-style",
    ] {
        let icon = env.registry.find(id).unwrap().icon.expect(id);
        let err = sanitize_svg(&icon.bytes).unwrap_err();
        assert_eq!(err.code(), "verification_failed", "{id}: {err}");
        refused += 1;
    }
    assert_eq!(refused, 6);
}

#[tokio::test]
async fn a_changed_icon_fails_the_hash_check_on_every_source() {
    let env = Env::new();
    let dir = env.scratch("tampered");
    copy_dir(&icons_fixture(), &dir);
    // The signature of the index is still valid: only the icon changed.
    std::fs::write(
        dir.join("icons/hello-web.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><circle r=\"1\"/></svg>\n",
    )
    .unwrap();
    env.registry
        .add(config("dir", RegistrySource::Dir(dir.clone())))
        .await
        .unwrap();
    let err = env.registry.load("dir").await.unwrap_err();
    assert_eq!(err.code(), "verification_failed");
    assert!(err.to_string().contains("icons/hello-web.svg"), "{err}");

    let server = TestServer::start(&dir).await;
    env.registry
        .add(config("http", RegistrySource::Http(server.index_url())))
        .await
        .unwrap();
    let err = env.registry.load("http").await.unwrap_err();
    assert_eq!(err.code(), "verification_failed");
}

#[tokio::test]
async fn http_source_fetches_icons_and_tolerates_a_missing_one() {
    let env = Env::new();
    let dir = env.scratch("http-icons");
    copy_dir(&icons_fixture(), &dir);
    // Listed in the index but not served: the icon is not offered, the registry loads.
    std::fs::remove_file(dir.join("icons/evil-style.svg")).unwrap();
    let server = TestServer::start(&dir).await;
    env.registry
        .add(config("http", RegistrySource::Http(server.index_url())))
        .await
        .unwrap();
    let report = env.registry.load("http").await.unwrap();
    assert!(report.verification.is_verified(), "{report:?}");
    let web = env.registry.find("hello-web").unwrap();
    assert_eq!(
        web.icon.map(|i| i.sha256),
        Some(sha_of(&icons_fixture(), WEB_SVG_SHA))
    );
    assert!(env.registry.find("evil-style").unwrap().icon.is_none());
    assert!(
        server
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|p| p == "icons/hello-web.svg")
    );
    assert!(
        env.registry
            .cache_path("http")
            .join("icons/hello-web.svg")
            .is_file()
    );
}

#[tokio::test]
async fn empty_search_lists_in_index_order() {
    let env = Env::new();
    env.registry
        .add(config("icons", RegistrySource::Dir(icons_fixture())))
        .await
        .unwrap();
    env.registry.load("icons").await.unwrap();
    let hits = env.registry.search("", None);
    assert!(hits.iter().all(|h| h.matched == MatchKind::All));
    let ids: Vec<&str> = hits.iter().map(|h| h.entry.id.as_str()).collect();
    // Tools by id, then the stacks: the order of index.yaml.
    let mut sorted_tools: Vec<&str> = ids[..ids.len() - 1].to_vec();
    sorted_tools.sort_unstable();
    assert_eq!(ids[..ids.len() - 1], sorted_tools[..]);
    assert_eq!(ids.last(), Some(&"hello-stack"));
    // The index order, not an id order of the whole list ("hello-stack" sorts before
    // "hello-web" by id).
    assert!(
        ids.iter().position(|i| *i == "hello-stack") > ids.iter().position(|i| *i == "hello-web")
    );
}
