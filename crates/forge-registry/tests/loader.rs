//! Registry loader: Dir, Http and Git sources, signature and hash verification, policy of
//! third-party registries, cache/TTL and search, against `tests/fixtures/registry`
//! (signed with the development key `registry/keys/dev.pub`).

mod support;

use std::time::Duration;

use forge_core::jobs::JobState;
use forge_manifest::schema::Category;
use forge_registry::{
    LoadSource, RegistryConfig, RegistrySource, Verification, messages, search::MatchKind,
};
use support::*;

fn fixture_config(name: &str, dir: &std::path::Path) -> RegistryConfig {
    RegistryConfig::official(name, RegistrySource::Dir(dir.to_owned()), dev_public_key())
}

// ---------- Dir source ----------

#[tokio::test]
async fn loads_the_signed_fixture_from_a_folder() {
    let env = Env::new();
    env.registry
        .add(fixture_config("fixture", &fixture_dir()))
        .await
        .unwrap();
    let report = env.registry.load("fixture").await.unwrap();
    assert_eq!(report.source, LoadSource::Source);
    assert!(report.verification.is_verified(), "{report:?}");
    assert_eq!((report.tools, report.stacks), (2, 1));
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);

    let web = env.registry.find("hello-web").expect("tool is indexed");
    assert_eq!(web.name(), "Hello Web");
    assert_eq!(web.latest.as_deref(), Some("1.10.0"));
    assert!(web.official && web.verification.is_verified());
    assert!(env.registry.find("hello-stack").is_some());
    assert!(env.registry.find("nope").is_none());

    // The cache holds exactly the verified files.
    let cache = env.registry.cache_path("fixture");
    for file in [
        "index.yaml",
        "index.yaml.minisig",
        "tools/hello-web.yaml",
        "tools/hello-db.yaml",
        "stacks/hello-stack.yaml",
    ] {
        assert!(cache.join(file).is_file(), "{file}");
    }

    // Metadata went to SQLite.
    let row: (String, Option<String>, Option<String>) = env
        .store
        .reader()
        .query(|conn| {
            Ok(conn.query_row(
                "SELECT kind, verification, index_sha256 FROM registries WHERE name = 'fixture'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(row.0, "dir");
    assert_eq!(row.1.as_deref(), Some("verified"));
    assert_eq!(row.2.as_deref().map(str::len), Some(64));

    let info = &env.registry.list()[0];
    assert!(info.official && info.last_refresh_at.is_some() && info.last_error.is_none());
    assert_eq!(
        messages::verification_label(info.verification.as_ref().unwrap()),
        "verificado"
    );
}

#[tokio::test]
async fn search_ranks_and_filters() {
    let env = Env::new();
    env.registry
        .add(fixture_config("fixture", &fixture_dir()))
        .await
        .unwrap();
    env.registry.load("fixture").await.unwrap();

    let hits = env.registry.search("hello-web", None);
    assert_eq!(hits[0].entry.id.as_str(), "hello-web");
    assert_eq!(hits[0].matched, MatchKind::ExactId);

    let hits = env.registry.search("hello", None);
    let ids: Vec<_> = hits.iter().map(|h| h.entry.id.as_str()).collect();
    assert_eq!(ids, ["hello-db", "hello-stack", "hello-web"]);
    assert!(hits.iter().all(|h| h.matched == MatchKind::PrefixId));

    // Tag-only match ranks below id matches.
    let hits = env.registry.search("sql", None);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].matched, MatchKind::ExactTag);

    let databases = env.registry.search("", Some(Category::Database));
    assert_eq!(databases.len(), 1);
    assert_eq!(databases[0].entry.id.as_str(), "hello-db");
    // Stacks have no category.
    assert!(
        env.registry
            .search("hello-stack", Some(Category::Webserver))
            .is_empty()
    );
    assert_eq!(env.registry.search("demo", None).len(), 3);
}

#[tokio::test]
async fn a_wrong_sha256_in_the_index_is_rejected() {
    let env = Env::new();
    env.registry
        .add(fixture_config("bad", &bad_sha_fixture_dir()))
        .await
        .unwrap();
    let err = env.registry.load("bad").await.unwrap_err();
    assert_eq!(err.code(), "verification_failed", "{err}");
    assert!(err.to_string().contains("hello-db.yaml"), "{err}");
    assert!(
        env.registry.find("hello-web").is_none(),
        "nothing is served"
    );
    assert!(
        !env.registry.cache_path("bad").exists(),
        "nothing is cached"
    );
    assert!(env.registry.list()[0].last_error.is_some());
}

#[tokio::test]
async fn an_edited_manifest_is_rejected() {
    let env = Env::new();
    let dir = env.scratch("edited");
    copy_dir(&fixture_dir(), &dir);
    let path = dir.join("tools/hello-web.yaml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("# sneaky edit\n");
    std::fs::write(&path, text).unwrap();
    env.registry
        .add(fixture_config("edited", &dir))
        .await
        .unwrap();
    let err = env.registry.load("edited").await.unwrap_err();
    assert_eq!(err.code(), "verification_failed");
}

#[tokio::test]
async fn an_index_edited_after_signing_is_rejected() {
    let env = Env::new();
    let dir = env.scratch("forged");
    copy_dir(&fixture_dir(), &dir);
    let path = dir.join("index.yaml");
    let text = std::fs::read_to_string(&path).unwrap();
    // Pretend a newer version exists: the signature no longer matches.
    assert!(text.contains("latest: 1.10.0"));
    std::fs::write(&path, text.replace("latest: 1.10.0", "latest: 9.9.9")).unwrap();
    env.registry
        .add(fixture_config("forged", &dir))
        .await
        .unwrap();
    let err = env.registry.load("forged").await.unwrap_err();
    assert_eq!(err.code(), "verification_failed");
    assert!(err.to_string().contains("signature"), "{err}");
}

#[tokio::test]
async fn a_signature_by_another_key_is_rejected_even_when_trusted_unsigned() {
    let env = Env::new();
    let dir = env.scratch("other-key");
    copy_dir(&fixture_dir(), &dir);
    let stranger = new_key();
    index_registry(&dir, "other-key", Some(&stranger));
    let mut config = fixture_config("other-key", &dir);
    config.trusted_unsigned = true;
    env.registry.add(config).await.unwrap();
    // The dev key is configured but the index was signed by someone else.
    let err = env.registry.load("other-key").await.unwrap_err();
    assert_eq!(err.code(), "verification_failed");

    // With the right key configured the same folder is fine.
    let mut config = fixture_config("other-key", &dir);
    config.public_key = Some(stranger.pk.to_base64());
    env.registry.add(config).await.unwrap();
    let report = env.registry.load("other-key").await.unwrap();
    assert!(report.verification.is_verified());
}

#[tokio::test]
async fn unsigned_registries_need_trusted_unsigned_and_show_as_unverified() {
    let env = Env::new();
    let dir = env.scratch("unsigned");
    std::fs::create_dir_all(dir.join("tools")).unwrap();
    std::fs::write(dir.join("tools/mine.yaml"), tool_yaml("mine", "")).unwrap();
    index_registry(&dir, "unsigned", None);

    let strict = RegistryConfig::new("unsigned", RegistrySource::Dir(dir.clone()));
    env.registry.add(strict).await.unwrap();
    let err = env.registry.load("unsigned").await.unwrap_err();
    assert_eq!(err.code(), "verification_failed");
    assert!(err.to_string().contains("not signed"), "{err}");

    let mut lenient = RegistryConfig::new("unsigned", RegistrySource::Dir(dir));
    lenient.trusted_unsigned = true;
    env.registry.add(lenient).await.unwrap();
    let report = env.registry.load("unsigned").await.unwrap();
    assert_eq!(report.verification, Verification::Unverified);
    assert_eq!(
        messages::verification_label(&report.verification),
        "não verificado"
    );
    let entry = env.registry.find("mine").unwrap();
    assert!(!entry.official && !entry.verification.is_verified());
    assert_eq!(messages::origin_label(entry.official), "terceiro");
}

#[tokio::test]
async fn third_party_manifests_with_post_install_run_are_refused() {
    let env = Env::new();
    let dir = env.scratch("thirdparty");
    std::fs::create_dir_all(dir.join("tools")).unwrap();
    std::fs::write(dir.join("tools/plain.yaml"), tool_yaml("plain", "")).unwrap();
    std::fs::write(
        dir.join("tools/runner.yaml"),
        tool_yaml("runner", RUN_INSTALL),
    )
    .unwrap();
    let key = new_key();
    index_registry(&dir, "thirdparty", Some(&key));

    let mut third = RegistryConfig::new("thirdparty", RegistrySource::Dir(dir.clone()));
    third.public_key = Some(key.pk.to_base64());
    env.registry.add(third).await.unwrap();
    let report = env.registry.load("thirdparty").await.unwrap();
    assert_eq!(report.tools, 1);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].id, "runner");
    assert!(
        report.skipped[0].reason.contains("post_install.run"),
        "{:?}",
        report.skipped
    );
    assert!(env.registry.find("runner").is_none());
    assert!(env.registry.find("plain").is_some());

    // The very same registry flagged official may declare `run`.
    let mut official = RegistryConfig::new("thirdparty", RegistrySource::Dir(dir));
    official.public_key = Some(key.pk.to_base64());
    official.official = true;
    env.registry.add(official).await.unwrap();
    let report = env.registry.load("thirdparty").await.unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(report.tools, 2);
    assert!(env.registry.find("runner").is_some());
}

#[tokio::test]
async fn paths_in_the_index_cannot_escape_the_registry() {
    let env = Env::new();
    let dir = env.scratch("escape");
    std::fs::create_dir_all(dir.join("tools")).unwrap();
    std::fs::write(dir.join("tools/ok.yaml"), tool_yaml("ok", "")).unwrap();
    index_registry(&dir, "escape", None);
    let path = dir.join("index.yaml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("tools/ok.yaml", "tools/../../ok.yaml")).unwrap();
    let mut config = RegistryConfig::new("escape", RegistrySource::Dir(dir));
    config.trusted_unsigned = true;
    env.registry.add(config).await.unwrap();
    let err = env.registry.load("escape").await.unwrap_err();
    assert_eq!(err.code(), "invalid_input", "{err}");
}

// ---------- cache and TTL ----------

#[tokio::test]
async fn the_cache_is_reused_without_reading_the_source_within_the_ttl() {
    let env = Env::new();
    let dir = env.scratch("src");
    copy_dir(&fixture_dir(), &dir);
    env.registry
        .add(fixture_config("cached", &dir))
        .await
        .unwrap();
    assert_eq!(
        env.registry.load("cached").await.unwrap().source,
        LoadSource::Source
    );

    // The source disappears: a second load inside the TTL does not need it.
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(
        env.registry.load("cached").await.unwrap().source,
        LoadSource::Memory
    );

    // After a restart the catalog comes from the (re-verified) cache.
    let restarted = env.restarted();
    assert_eq!(restarted.restore().await.unwrap(), 1);
    assert!(restarted.find("hello-web").is_none(), "not loaded yet");
    let report = restarted.load("cached").await.unwrap();
    assert_eq!(report.source, LoadSource::Cache);
    assert!(report.verification.is_verified());
    assert!(restarted.find("hello-web").is_some());
}

#[tokio::test]
async fn an_expired_cache_asks_the_source_and_survives_its_failure() {
    let env = Env::new();
    let dir = env.scratch("src");
    copy_dir(&fixture_dir(), &dir);
    let mut config = fixture_config("expiring", &dir);
    config.ttl = Duration::ZERO;
    env.registry.add(config).await.unwrap();
    env.registry.load("expiring").await.unwrap();
    // TTL zero: never fresh, so the source is read again...
    assert_eq!(
        env.registry.load("expiring").await.unwrap().source,
        LoadSource::Source
    );
    // ...and when it fails, `load` serves the last verified catalog with a warning.
    std::fs::remove_dir_all(&dir).unwrap();
    let report = env.registry.load("expiring").await.unwrap();
    assert_eq!(report.source, LoadSource::StaleCache);
    assert!(report.warning.is_some());
    assert!(env.registry.find("hello-web").is_some());
    // `refresh`, which was asked explicitly, reports the failure.
    let job = env.registry.refresh("expiring").unwrap();
    let info = env.wait_job(job).await;
    assert_eq!(info.state, JobState::Failed);
    assert!(env.registry.list()[0].last_error.is_some());
    assert!(env.registry.find("hello-web").is_some(), "still served");
}

#[tokio::test]
async fn a_tampered_cache_is_not_trusted() {
    let env = Env::new();
    let dir = env.scratch("src");
    copy_dir(&fixture_dir(), &dir);
    env.registry.add(fixture_config("t", &dir)).await.unwrap();
    env.registry.load("t").await.unwrap();
    let cached = env.registry.cache_path("t").join("tools/hello-db.yaml");
    std::fs::write(&cached, "kind: Nope\n").unwrap();

    let restarted = env.restarted();
    restarted.restore().await.unwrap();
    // The cache fails verification, so the loader goes back to the (good) source.
    let report = restarted.load("t").await.unwrap();
    assert_eq!(report.source, LoadSource::Source);
    assert_ne!(std::fs::read_to_string(&cached).unwrap(), "kind: Nope\n");
}

#[tokio::test]
async fn refresh_is_a_job_and_runs_once_at_a_time() {
    let env = Env::new();
    env.registry
        .add(fixture_config("fixture", &fixture_dir()))
        .await
        .unwrap();
    let first = env.registry.refresh("fixture").unwrap();
    let again = env.registry.refresh("fixture");
    // Either the first job is still running (same id) or it already finished (new job).
    assert!(again.is_ok());
    let info = env.wait_job(first).await;
    assert_eq!(info.state, JobState::Done, "{:?}", info.error);
    let result = info.result.unwrap();
    assert_eq!(result["registry"], "fixture");
    assert_eq!(result["tools"], 2);
    assert!(env.registry.find("hello-db").is_some());
    env.wait_job(again.unwrap()).await;
    assert!(env.registry.list()[0].refreshing.is_none());
    assert_eq!(
        env.registry.refresh("missing").unwrap_err().code(),
        "not_found"
    );
}

#[tokio::test]
async fn configuration_is_persisted_and_removal_clears_the_cache() {
    let env = Env::new();
    let mut config = fixture_config("keep", &fixture_dir());
    config.priority = 7;
    config.ttl = Duration::from_secs(90);
    env.registry.add(config.clone()).await.unwrap();
    env.registry.load("keep").await.unwrap();

    let restarted = env.restarted();
    restarted.restore().await.unwrap();
    let info = &restarted.list()[0];
    assert_eq!(
        (info.name.as_str(), info.priority, info.official),
        ("keep", 7, true)
    );
    assert!(info.last_refresh_at.is_some());
    assert!(info.verification.is_some());

    assert!(
        env.registry
            .add(RegistryConfig::new(
                "Bad Name",
                RegistrySource::Dir(fixture_dir())
            ))
            .await
            .is_err()
    );
    env.registry.remove("keep").await.unwrap();
    assert!(env.registry.list().is_empty());
    assert!(!env.registry.cache_path("keep").exists());
    assert_eq!(
        env.registry.remove("keep").await.unwrap_err().code(),
        "not_found"
    );
}

#[tokio::test]
async fn the_same_id_resolves_by_registry_priority() {
    let env = Env::new();
    let dir = env.scratch("mine");
    std::fs::create_dir_all(dir.join("tools")).unwrap();
    std::fs::write(dir.join("tools/hello-web.yaml"), tool_yaml("hello-web", "")).unwrap();
    let key = new_key();
    index_registry(&dir, "mine", Some(&key));
    let mut mine = RegistryConfig::new("mine", RegistrySource::Dir(dir));
    mine.public_key = Some(key.pk.to_base64());
    mine.priority = 5;
    env.registry.add(mine).await.unwrap();
    env.registry
        .add(fixture_config("fixture", &fixture_dir()))
        .await
        .unwrap();
    for (name, result) in env.registry.load_all().await {
        result.unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    assert_eq!(env.registry.find("hello-web").unwrap().registry, "mine");
    let names: Vec<_> = env.registry.list().into_iter().map(|i| i.name).collect();
    assert_eq!(names, ["mine", "fixture"]);
}

// ---------- Http source ----------

#[tokio::test]
async fn http_source_loads_caches_and_revalidates_with_etag() {
    let env = Env::new();
    let server = TestServer::start(&fixture_dir()).await;
    let config = RegistryConfig::official(
        "remote",
        RegistrySource::Http(server.index_url()),
        dev_public_key(),
    );
    env.registry.add(config).await.unwrap();

    let report = env.registry.load("remote").await.unwrap();
    assert_eq!(report.source, LoadSource::Source);
    assert!(report.verification.is_verified());
    assert_eq!((report.tools, report.stacks), (2, 1));
    assert!(report.etag.is_some());
    // index + its detached signature + 2 tools + 1 stack
    assert_eq!(server.hits(), 5);
    let log = server.log.lock().unwrap().clone();
    assert_eq!(
        log[0], "index.yaml",
        "the index is requested first: {log:?}"
    );

    // Inside the TTL nothing is requested.
    assert_eq!(
        env.registry.load("remote").await.unwrap().source,
        LoadSource::Memory
    );
    assert_eq!(server.hits(), 5);

    // An explicit refresh sends If-None-Match: one request, answered 304.
    let job = env.registry.refresh("remote").unwrap();
    let info = env.wait_job(job).await;
    assert_eq!(info.state, JobState::Done, "{:?}", info.error);
    assert_eq!(info.result.unwrap()["source"], "not_modified");
    assert_eq!(server.hits(), 6);
    assert!(env.registry.find("hello-web").is_some());

    // After a restart the cache is used; an ETag survived in SQLite.
    let restarted = env.restarted();
    restarted.restore().await.unwrap();
    assert_eq!(
        restarted.load("remote").await.unwrap().source,
        LoadSource::Cache
    );
    assert_eq!(server.hits(), 6);
}

#[tokio::test]
async fn http_source_verifies_the_signature_before_requesting_manifests() {
    let env = Env::new();
    let dir = env.scratch("served");
    copy_dir(&fixture_dir(), &dir);
    let path = dir.join("index.yaml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("latest: 1.10.0", "latest: 9.9.9")).unwrap();
    let server = TestServer::start(&dir).await;
    env.registry
        .add(RegistryConfig::official(
            "remote",
            RegistrySource::Http(server.index_url()),
            dev_public_key(),
        ))
        .await
        .unwrap();
    let err = env.registry.load("remote").await.unwrap_err();
    assert_eq!(err.code(), "verification_failed");
    assert_eq!(
        server.hits(),
        2,
        "only the index and its signature were requested"
    );
}

#[tokio::test]
async fn http_errors_are_reported() {
    let env = Env::new();
    let dir = env.scratch("empty");
    let server = TestServer::start(&dir).await;
    env.registry
        .add(RegistryConfig::new(
            "gone",
            RegistrySource::Http(server.index_url()),
        ))
        .await
        .unwrap();
    let err = env.registry.load("gone").await.unwrap_err();
    assert_eq!(err.code(), "not_found", "{err}");
}

// ---------- Git source ----------

fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.org"])
        .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn git_source_clones_verifies_and_detects_no_change() {
    if !git_available() {
        eprintln!("git is not installed: skipping");
        return;
    }
    let env = Env::new();
    let repo = env.scratch("repo");
    copy_dir(&fixture_dir(), &repo);
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "registry"]);

    let url = url::Url::from_directory_path(&repo).unwrap();
    env.registry
        .add(RegistryConfig::official(
            "gitreg",
            RegistrySource::Git(url),
            dev_public_key(),
        ))
        .await
        .unwrap();
    let report = env.registry.load("gitreg").await.unwrap();
    assert_eq!(report.source, LoadSource::Source, "{report:?}");
    assert!(report.verification.is_verified());
    assert_eq!(report.etag.as_deref().map(str::len), Some(40), "commit id");
    assert!(env.registry.find("hello-stack").is_some());

    // Same commit: the refresh job reports "not modified".
    let job = env.registry.refresh("gitreg").unwrap();
    let info = env.wait_job(job).await;
    assert_eq!(info.state, JobState::Done, "{:?}", info.error);
    assert_eq!(info.result.unwrap()["source"], "not_modified");

    // A new commit with a tampered manifest is rejected and the old catalog stays.
    std::fs::write(repo.join("tools/hello-db.yaml"), "kind: Nope\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "tamper"]);
    let job = env.registry.refresh("gitreg").unwrap();
    let info = env.wait_job(job).await;
    assert_eq!(info.state, JobState::Failed);
    assert!(
        env.registry.find("hello-db").is_some(),
        "previous catalog is kept"
    );
}

#[tokio::test]
async fn git_source_without_a_repository_fails_cleanly() {
    if !git_available() {
        return;
    }
    let env = Env::new();
    let missing = env.tmp.path().join("no-repo-here");
    let url = url::Url::from_directory_path(&missing).unwrap();
    env.registry
        .add(RegistryConfig::new("nogit", RegistrySource::Git(url)))
        .await
        .unwrap();
    assert!(env.registry.load("nogit").await.is_err());
    let checkout = env.root.state_registry().join("nogit.git");
    assert!(!checkout.exists(), "failed clone leaves nothing behind");
}

/// A git repository holding one signed tool registry.
fn git_registry(env: &Env, folder: &str, tool: &str, key: &minisign::KeyPair) -> url::Url {
    let repo = env.scratch(folder);
    std::fs::create_dir_all(repo.join("tools")).unwrap();
    std::fs::write(repo.join(format!("tools/{tool}.yaml")), tool_yaml(tool, "")).unwrap();
    index_registry(&repo, folder, Some(key));
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "registry"]);
    url::Url::from_directory_path(&repo).unwrap()
}

#[tokio::test]
async fn changing_the_url_of_a_git_registry_fetches_from_the_new_repository() {
    if !git_available() {
        eprintln!("git is not installed: skipping");
        return;
    }
    let env = Env::new();
    let key = new_key();
    let repo_a = git_registry(&env, "repo-a", "alpha", &key);
    let repo_b = git_registry(&env, "repo-b", "beta", &key);
    let config = |url: &url::Url| {
        let mut config = RegistryConfig::new("moving", RegistrySource::Git(url.clone()));
        config.public_key = Some(key.pk.to_base64());
        config
    };

    env.registry.add(config(&repo_a)).await.unwrap();
    env.registry.load("moving").await.unwrap();
    assert!(env.registry.find("alpha").is_some());
    assert!(env.registry.find("beta").is_none());

    // Same name, another repository with other signed content.
    env.registry.add(config(&repo_b)).await.unwrap();
    let job = env.registry.refresh("moving").unwrap();
    let info = env.wait_job(job).await;
    assert_eq!(info.state, JobState::Done, "{:?}", info.error);
    assert_eq!(info.result.unwrap()["source"], "source");
    assert!(env.registry.find("beta").is_some(), "the new URL is used");
    assert!(
        env.registry.find("alpha").is_none(),
        "nothing of the old repository is served"
    );

    // Back to A: a plain `load` (not a forced refresh) must not trust the TTL of B either.
    env.registry.add(config(&repo_a)).await.unwrap();
    env.registry.load("moving").await.unwrap();
    assert!(env.registry.find("alpha").is_some());
    assert!(env.registry.find("beta").is_none());
}
