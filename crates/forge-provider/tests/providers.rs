//! Offline tests of the providers: recorded upstream responses (tests/fixtures, fetched
//! 26/10/2026) served by a local axum server.

mod provider_support;

use std::collections::BTreeMap;

use forge_core::error::ForgeError;
use forge_manifest::schema::ToolManifest;
use forge_manifest::schema::VersionScheme;
use forge_manifest::version::Constraint;
use forge_provider::provider::{Release, ResolveRequest, SigKind, SigSubject, resolve_checksum};
use provider_support::{Env, PHP_VERSIONING, tool};

fn no_variants() -> BTreeMap<String, String> {
    BTreeMap::new()
}

fn constraint(text: &str) -> Constraint {
    Constraint::parse(VersionScheme::Semver, text).unwrap()
}

async fn resolve(
    env: &Env,
    tool: &ToolManifest,
    constraint: Option<&Constraint>,
    variants: &BTreeMap<String, String>,
    channel: Option<&str>,
) -> Result<Vec<Release>, ForgeError> {
    env.registry
        .resolve(ResolveRequest {
            tool,
            constraint,
            variants,
            channel,
        })
        .await
}

fn versions(releases: &[Release]) -> Vec<String> {
    let mut v: Vec<String> = releases.iter().map(|r| r.version.clone()).collect();
    v.dedup();
    v
}

fn variant<'a>(release: &'a Release, key: &str) -> &'a str {
    release.variants.get(key).map_or("", String::as_str)
}

// ------------------------------------------------------------------------- php

fn php_tool(qa: bool) -> ToolManifest {
    tool(
        "php",
        PHP_VERSIONING,
        &format!(
            "    providers:\n      - type: php-windows\n        include_archives: true\n        qa: {qa}"
        ),
    )
}

async fn php_env() -> Env {
    let env = Env::new().await;
    env.upstream
        .serve_fixture("/php/releases/releases.json", "php-windows/releases.json");
    env.upstream
        .serve_fixture("/php/releases/archives/", "php-windows/archives.html");
    env.upstream
        .serve_fixture("/php/qa/", "php-windows/qa.html");
    env
}

#[tokio::test]
async fn php_lists_every_variant_and_the_manifest_default_ranks_first() {
    let env = php_env().await;
    let tool = php_tool(false);
    let all = resolve(&env, &tool, None, &no_variants(), Some("stable"))
        .await
        .unwrap();

    // 8.5 from releases.json: nts and ts, vs17, x64 only (x86 builds are ignored).
    let newest: Vec<&Release> = all.iter().filter(|r| r.version == "8.5.12+1").collect();
    assert_eq!(newest.len(), 2);
    assert_eq!(variant(newest[0], "ts"), "nts", "manifest default first");
    assert_eq!(variant(newest[1], "ts"), "ts");
    assert!(newest.iter().all(|r| variant(r, "compiler") == "vs17"));
    assert!(newest.iter().all(|r| variant(r, "arch") == "x64"));
    assert_eq!(all[0].version, "8.5.12+1");

    let nts = newest[0];
    assert_eq!(nts.file_name, "php-8.5.12-1-nts-Win32-vs17-x64.zip");
    assert_eq!(
        nts.url,
        format!("{}/php/releases/{}", env.upstream.base(), nts.file_name)
    );
    assert_eq!(nts.sha256.as_ref().unwrap().len(), 64);
    assert!(nts.published_at.is_some());
    assert_eq!(nts.size, None, "releases.json only has rounded sizes");

    // The recorded releases.json: sha256 equals the one in the file.
    let json: serde_json::Value =
        serde_json::from_slice(&provider_support::fixture("php-windows/releases.json")).unwrap();
    assert_eq!(
        nts.sha256.as_deref(),
        json["8.5"]["nts-vs17-x64"]["zip"]["sha256"].as_str()
    );

    // Archives: older versions, no hash published.
    let old = all.iter().find(|r| r.version == "8.4.1").unwrap();
    assert!(old.sha256.is_none());
    assert!(old.tags.contains(&"archive".to_owned()));
    assert_eq!(
        old.published_at.unwrap().to_string(),
        "2024-12-17T20:00:00Z"
    );

    // vc15 is a declared compiler value: 7.4.33 is listed once, with the releases.json hash.
    let seven: Vec<_> = all.iter().filter(|r| r.version == "7.4.33").collect();
    assert_eq!(seven.len(), 2);
    assert!(
        seven
            .iter()
            .all(|r| r.sha256.is_some() && variant(r, "compiler") == "vc15")
    );

    // Not declared (VC6) or x86: never listed.
    assert!(all.iter().all(|r| !r.version.starts_with("5.")));
    assert!(all.iter().all(|r| variant(r, "arch") == "x64"));
    // QA is off: no pre-releases.
    assert!(all.iter().all(|r| !r.version.contains('-')));
}

#[tokio::test]
async fn php_constraint_and_variant_filters() {
    let env = php_env().await;
    let tool = php_tool(false);
    let c = constraint("~8.4");
    let r = resolve(&env, &tool, Some(&c), &no_variants(), Some("stable"))
        .await
        .unwrap();
    assert!(r.iter().all(|x| x.version.starts_with("8.4.")));
    assert!(versions(&r).contains(&"8.4.27+1".to_owned()));
    assert!(versions(&r).contains(&"8.4.0".to_owned()));

    let wanted: BTreeMap<String, String> = [("ts".to_owned(), "ts".to_owned())].into();
    let r = resolve(&env, &tool, Some(&c), &wanted, Some("stable"))
        .await
        .unwrap();
    assert!(!r.is_empty() && r.iter().all(|x| variant(x, "ts") == "ts"));
}

#[tokio::test]
async fn php_qa_builds_only_in_a_prerelease_channel_and_when_enabled() {
    let env = php_env().await;
    let stable_ch = resolve(&env, &php_tool(true), None, &no_variants(), Some("stable"))
        .await
        .unwrap();
    assert!(stable_ch.iter().all(|r| !r.tags.contains(&"qa".to_owned())));
    assert!(
        env.upstream.hits("/php/qa/").is_empty(),
        "QA not even fetched"
    );

    let preview = resolve(&env, &php_tool(true), None, &no_variants(), Some("preview"))
        .await
        .unwrap();
    let rc = preview.iter().find(|r| r.version == "8.5.12-RC1").unwrap();
    assert!(rc.tags.contains(&"qa".to_owned()));
    assert!(rc.sha256.is_none());

    // qa: false keeps them out even for the preview channel.
    let off = resolve(
        &env,
        &php_tool(false),
        None,
        &no_variants(),
        Some("preview"),
    )
    .await
    .unwrap();
    assert!(off.iter().all(|r| !r.tags.contains(&"qa".to_owned())));
}

#[tokio::test]
async fn php_survives_a_missing_archive_listing_but_not_a_missing_releases_json() {
    let env = Env::new().await;
    env.upstream
        .serve_fixture("/php/releases/releases.json", "php-windows/releases.json");
    // archives/ answers 404.
    let r = resolve(&env, &php_tool(false), None, &no_variants(), Some("stable"))
        .await
        .unwrap();
    assert!(r.iter().all(|x| x.tags.contains(&"current".to_owned())));

    let env = Env::new().await;
    let err = resolve(&env, &php_tool(false), None, &no_variants(), Some("stable"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found");
}

#[tokio::test]
async fn unknown_channel_is_invalid_input() {
    let env = php_env().await;
    let err = resolve(&env, &php_tool(false), None, &no_variants(), Some("nope"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_input");
}

// ------------------------------------------------------------------------ node

const NODE_V: &str = "  versioning:\n    channels:\n      current: { constraint: \">=0\" }\n      lts: { constraint: \">=0\" }";
const NODE_SRC: &str = "    providers:\n      - type: nodejs-dist";

async fn node_env(max_checksums: usize) -> Env {
    let env = Env::with(|c| c.max_checksum_fetches = max_checksums).await;
    env.upstream
        .serve_fixture("/node/dist/index.json", "nodejs-dist/index.json");
    for v in ["v26.12.1", "v24.22.1"] {
        env.upstream.serve_fixture(
            &format!("/node/dist/{v}/SHASUMS256.txt"),
            &format!("nodejs-dist/{v}/SHASUMS256.txt"),
        );
    }
    env
}

#[tokio::test]
async fn node_resolves_zip_urls_and_sha256_from_shasums() {
    let env = node_env(50).await;
    let tool = tool("node", NODE_V, NODE_SRC);
    let r = resolve(&env, &tool, None, &no_variants(), Some("current"))
        .await
        .unwrap();
    // 30 entries + 2 LTS recorded.
    assert_eq!(r.len(), 32);
    let newest = &r[0];
    assert_eq!(newest.version, "26.12.1");
    assert_eq!(
        newest.url,
        format!(
            "{}/node/dist/v26.12.1/node-v26.12.1-win-x64.zip",
            env.upstream.base()
        )
    );
    assert_eq!(
        newest.sha256.as_deref(),
        Some("b3efd5c70cba3308ed46e7e699a9b8aac075bbc6b027fcb39c84c31acc25e2b4")
    );
    assert_eq!(
        newest.published_at.unwrap().to_string(),
        "2026-10-21T00:00:00Z"
    );
    let sig = newest.signature.as_ref().unwrap();
    assert_eq!(sig.kind, SigKind::Pgp);
    assert!(sig.url.ends_with("/v26.12.1/SHASUMS256.txt.sig"));
    assert!(
        matches!(&sig.subject, SigSubject::ChecksumFile { url } if url.ends_with("SHASUMS256.txt"))
    );
    // Only the versions that have a SHASUMS256.txt on the fake server could be filled.
    let lts = r.iter().find(|x| x.version == "24.22.1").unwrap();
    assert_eq!(
        lts.sha256.as_deref(),
        Some("d2046d28c2c3dcd211ac5145c0bf30e2047ef9dfb306693c8c6e2eccd8b95fa5")
    );
    assert!(lts.tags.contains(&"lts:Krypton".to_owned()));
}

#[tokio::test]
async fn node_lts_channel_and_constraint() {
    let env = node_env(20).await;
    let tool = tool("node", NODE_V, NODE_SRC);
    let r = resolve(&env, &tool, None, &no_variants(), Some("lts"))
        .await
        .unwrap();
    assert_eq!(versions(&r), ["24.22.1", "24.22.0"]);
    let c = constraint("^26.12");
    let r = resolve(&env, &tool, Some(&c), &no_variants(), Some("current"))
        .await
        .unwrap();
    assert_eq!(versions(&r), ["26.12.1", "26.12.0"]);
}

#[tokio::test]
async fn node_reads_only_the_newest_checksum_documents_and_resolves_the_rest_lazily() {
    let env = node_env(1).await;
    let tool = tool("node", NODE_V, NODE_SRC);
    let c = constraint(">=26.12");
    let mut r = resolve(&env, &tool, Some(&c), &no_variants(), Some("current"))
        .await
        .unwrap();
    assert_eq!(versions(&r), ["26.12.1", "26.12.0"]);
    assert!(r[0].sha256.is_some());
    assert!(r[1].sha256.is_none());
    let url = r[1].checksum_url.clone().unwrap();
    assert!(url.ends_with("/v26.12.0/SHASUMS256.txt"));
    assert_eq!(
        env.upstream
            .hits("/node/dist/v26.12.0/SHASUMS256.txt")
            .len(),
        0
    );

    // Lazy lookup: the document does not exist on the fake server -> error, release intact.
    let err = resolve_checksum(env.registry.fetcher(), "nodejs-dist", &mut r[1])
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found");
    assert_eq!(r[1].checksum_url.as_deref(), Some(url.as_str()));

    // And when it exists.
    env.upstream.serve(
        "/node/dist/v26.12.0/SHASUMS256.txt",
        format!("{}  node-v26.12.0-win-x64.zip\n", "ab".repeat(32)),
    );
    assert!(
        resolve_checksum(env.registry.fetcher(), "nodejs-dist", &mut r[1])
            .await
            .unwrap()
    );
    assert_eq!(r[1].sha256.as_deref(), Some("ab".repeat(32).as_str()));
    assert!(r[1].checksum_url.is_none());
}

// ---------------------------------------------------------------------- apache

const APACHE_V: &str = "  versioning:\n    scheme: semver\n    variants:\n      - key: compiler\n        values: [vs18, vs17]\n        default: vs18\n      - key: arch\n        values: [x64]";

async fn apache_env() -> Env {
    let env = Env::new().await;
    env.upstream
        .serve_fixture("/download/VS17/", "apache-lounge/vs17.html");
    env.upstream
        .serve_fixture("/download/VS18/", "apache-lounge/vs18.html");
    env.upstream.serve_fixture(
        "/download/VS17/binaries/httpd-2.4.66-251206-Win64-VS17.zip.txt",
        "apache-lounge/httpd-2.4.66-251206-Win64-VS17.zip.txt",
    );
    env.upstream.serve_fixture(
        "/download/VS18/binaries/httpd-2.4.69-261002-Win64-VS18.zip.txt",
        "apache-lounge/httpd-2.4.69-261002-Win64-VS18.zip.txt",
    );
    env
}

#[tokio::test]
async fn apache_returns_vs17_and_vs18_as_compiler_variants() {
    let env = apache_env().await;
    let tool = tool(
        "apache",
        APACHE_V,
        "    providers:\n      - type: apache-lounge",
    );
    let r = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap();
    assert_eq!(r.len(), 2, "win32 builds are skipped: {r:#?}");
    assert_eq!(
        (r[0].version.as_str(), variant(&r[0], "compiler")),
        ("2.4.69", "vs18")
    );
    assert_eq!(
        (r[1].version.as_str(), variant(&r[1], "compiler")),
        ("2.4.66", "vs17")
    );
    assert_eq!(variant(&r[0], "arch"), "x64");
    assert_eq!(
        r[1].sha256.as_deref(),
        Some("2cd1f349b6705e43e784e876a233eeb1a859fa6b8abc693a91f87b35a368f7f9")
    );
    assert_eq!(r[1].sha512.as_ref().map(String::len), Some(128));
    assert_eq!(
        r[0].sha256.as_deref(),
        Some("77d715d0d9cee731e2251e7ddc2090a7a8e6d41fedef2e9827cd918593c5a652")
    );
    assert_eq!(
        r[1].url,
        format!(
            "{}/download/VS17/binaries/httpd-2.4.66-251206-Win64-VS17.zip",
            env.upstream.base()
        )
    );
    assert_eq!(r[1].file_name, "httpd-2.4.66-251206-Win64-VS17.zip");
    assert_eq!(
        r[1].published_at.unwrap().to_string(),
        "2025-12-10T00:00:00Z"
    );
    assert!(r[1].tags.contains(&"build:251206".to_owned()));
    assert!(r[1].signature.is_some());

    let wanted: BTreeMap<String, String> = [("compiler".to_owned(), "vs17".to_owned())].into();
    let only17 = resolve(&env, &tool, None, &wanted, None).await.unwrap();
    assert_eq!(only17.len(), 1);
}

#[tokio::test]
async fn apache_builds_list_limits_the_pages_and_a_custom_asset_finds_modules() {
    let env = apache_env().await;
    let tool = tool(
        "mod-fcgid",
        APACHE_V,
        "    providers:\n      - type: apache-lounge\n        builds: [vs17]\n        asset: '^mod_fcgid-(?P<version>[0-9.]+)-win64-VS[0-9]+\\.zip$'",
    );
    let r = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].version, "2.3.10");
    assert_eq!(variant(&r[0], "compiler"), "vs17");
    assert!(r[0].sha256.is_none(), "modules have no checksum document");
    assert!(env.upstream.hits("/download/VS18/").is_empty());
}

#[tokio::test]
async fn apache_falls_back_to_the_static_releases_when_the_site_is_down() {
    let env = Env::new().await;
    env.upstream.fail("/download/VS17/", 503);
    env.upstream.fail("/download/VS18/", 503);
    let sha = "1".repeat(64);
    let tool = tool(
        "apache",
        APACHE_V,
        &format!(
            "    providers:\n      - type: apache-lounge\n    releases:\n      - version: 2.4.66\n        url: https://example.test/httpd-2.4.66.zip\n        sha256: \"{sha}\"\n        variants: {{ compiler: vs17, arch: x64 }}"
        ),
    );
    let r = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].sha256.as_deref(), Some(sha.as_str()));

    let no_fallback = tool_without_releases();
    let err = resolve(&env, &no_fallback, None, &no_variants(), None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "io");
}

fn tool_without_releases() -> ToolManifest {
    tool(
        "apache",
        APACHE_V,
        "    providers:\n      - type: apache-lounge",
    )
}

// ----------------------------------------------------------------------- nginx

const NGINX_V: &str = "  versioning:\n    scheme: semver";

#[tokio::test]
async fn nginx_parses_mainline_stable_and_legacy_with_pgp_signatures() {
    let env = Env::new().await;
    env.upstream
        .serve_fixture("/en/download.html", "nginx-org/download.html");
    let tool = tool("nginx", NGINX_V, "    providers:\n      - type: nginx-org");
    let r = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap();
    assert_eq!(r[0].version, "1.31.8");
    assert_eq!(r[0].tags, ["mainline"]);
    let stable = r.iter().find(|x| x.version == "1.30.6").unwrap();
    assert_eq!(stable.tags, ["stable"]);
    assert_eq!(
        stable.url,
        format!("{}/download/nginx-1.30.6.zip", env.upstream.base())
    );
    let sig = stable.signature.as_ref().unwrap();
    assert!(sig.url.ends_with("/download/nginx-1.30.6.zip.asc"));
    assert_eq!(sig.subject, SigSubject::Archive);
    assert!(
        r.iter()
            .any(|x| x.version == "1.28.3" && x.tags == ["legacy"])
    );
    // 0.6 and 0.5 have no Windows build.
    assert!(
        r.iter()
            .all(|x| !x.version.starts_with("0.6") && !x.version.starts_with("0.5"))
    );
    // No upstream checksum.
    assert!(r.iter().all(|x| x.sha256.is_none()));

    let c = constraint("~1.30");
    let r = resolve(&env, &tool, Some(&c), &no_variants(), None)
        .await
        .unwrap();
    assert_eq!(versions(&r), ["1.30.6"]);
}

#[tokio::test]
async fn nginx_takes_the_curated_sha256_from_the_manifest() {
    let env = Env::new().await;
    env.upstream
        .serve_fixture("/en/download.html", "nginx-org/download.html");
    let sha = "e".repeat(64);
    let tool = tool(
        "nginx",
        NGINX_V,
        &format!(
            "    providers:\n      - type: nginx-org\n    releases:\n      - version: 1.30.6\n        sha256: \"{sha}\"\n        size: 1234"
        ),
    );
    let r = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap();
    let curated = r.iter().find(|x| x.version == "1.30.6").unwrap();
    assert_eq!(curated.sha256.as_deref(), Some(sha.as_str()));
    assert_eq!(curated.size, Some(1234));
    assert!(
        r.iter()
            .find(|x| x.version == "1.31.8")
            .unwrap()
            .sha256
            .is_none()
    );
}

#[tokio::test]
async fn nginx_layout_change_is_reported() {
    let env = Env::new().await;
    env.upstream
        .serve("/en/download.html", "<html>nothing</html>");
    let tool = tool("nginx", NGINX_V, "    providers:\n      - type: nginx-org");
    let err = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_input");
}

// --------------------------------------------------------------------- mariadb

const MARIADB_V: &str = "  versioning:\n    scheme: semver\n    channels:\n      stable: { constraint: \">=10\" }\n      preview: { constraint: \">=10\", prerelease: true }";
const MARIADB_SRC: &str = "    providers:\n      - type: mariadb";

#[tokio::test]
async fn mariadb_resolves_a_series_range() {
    let env = Env::new().await;
    env.upstream
        .serve_fixture("/rest-api/mariadb/", "mariadb/all.json");
    env.upstream
        .serve_fixture("/rest-api/mariadb/11.8/", "mariadb/series.json");
    let tool = tool("mariadb", MARIADB_V, MARIADB_SRC);
    let c = constraint("~11.8");
    let r = resolve(&env, &tool, Some(&c), &no_variants(), Some("stable"))
        .await
        .unwrap();
    assert_eq!(r[0].version, "11.8.9");
    // 11.8.7 has no Windows files in the recorded response.
    assert!(r.iter().all(|x| x.version != "11.8.7"));
    assert_eq!(
        r[0].sha256.as_deref(),
        Some("830c46727d9278eae212ae3eca44eeb9e71b2a68704e95f344a64fba7b1963f5")
    );
    assert_eq!(r[0].sha512.as_ref().map(String::len), Some(128));
    assert_eq!(r[0].file_name, "mariadb-11.8.9-winx64.zip");
    assert_eq!(
        r[0].url,
        format!(
            "{}/rest-api/mariadb/11.8.9/mariadb-11.8.9-winx64.zip",
            env.upstream.base()
        )
    );
    assert_eq!(
        r[0].published_at.unwrap().to_string(),
        "2026-08-22T00:00:00Z"
    );
    assert_eq!(r[0].signature.as_ref().unwrap().kind, SigKind::Pgp);
}

#[tokio::test]
async fn mariadb_resolves_an_exact_pin_inside_a_series() {
    for pin in ["11.8.9", "=11.8.9"] {
        let env = Env::new().await;
        env.upstream
            .serve_fixture("/rest-api/mariadb/", "mariadb/all.json");
        env.upstream
            .serve_fixture("/rest-api/mariadb/11.8/", "mariadb/series.json");
        let tool = tool("mariadb", MARIADB_V, MARIADB_SRC);
        let c = constraint(pin);
        let r = resolve(&env, &tool, Some(&c), &no_variants(), Some("stable"))
            .await
            .unwrap();
        assert_eq!(r.len(), 1, "pin {pin}");
        assert_eq!(r[0].version, "11.8.9");
    }
}

#[tokio::test]
async fn mariadb_resolves_a_range_strictly_inside_a_series() {
    let env = Env::new().await;
    env.upstream
        .serve_fixture("/rest-api/mariadb/", "mariadb/all.json");
    env.upstream
        .serve_fixture("/rest-api/mariadb/11.8/", "mariadb/series.json");
    let tool = tool("mariadb", MARIADB_V, MARIADB_SRC);
    let c = constraint(">=11.8.5, <11.8.9");
    let r = resolve(&env, &tool, Some(&c), &no_variants(), Some("stable"))
        .await
        .unwrap();
    assert!(!r.is_empty());
    assert!(
        r.iter()
            .all(|x| x.version.as_str() >= "11.8.5" && x.version.as_str() < "11.8.9")
    );
    assert!(r.iter().all(|x| x.version != "11.8.9"));
}

#[tokio::test]
async fn mariadb_skips_failing_series_and_marks_non_stable_ones_as_prereleases() {
    let env = Env::new().await;
    env.upstream
        .serve_fixture("/rest-api/mariadb/", "mariadb/all.json");
    env.upstream
        .serve_fixture("/rest-api/mariadb/11.8/", "mariadb/series.json");
    // Every other series answers 404: warned and skipped.
    let tool = tool("mariadb", MARIADB_V, MARIADB_SRC);
    let r = resolve(&env, &tool, None, &no_variants(), Some("stable"))
        .await
        .unwrap();
    assert!(!r.is_empty() && r.iter().all(|x| x.version.starts_with("11.8.")));

    // A series with status RC is a pre-release.
    let rc_series = std::fs::read_to_string(provider_support::fixture_path("mariadb/series.json"))
        .unwrap()
        .replace("11.8.", "13.1.");
    env.upstream.serve("/rest-api/mariadb/13.1/", rc_series);
    let r = resolve(&env, &tool, None, &no_variants(), Some("preview"))
        .await
        .unwrap();
    let rc = r.iter().find(|x| x.version == "13.1.9-rc").unwrap();
    assert!(rc.tags.contains(&"rc".to_owned()));
    let stable = resolve(&env, &tool, None, &no_variants(), Some("stable"))
        .await
        .unwrap();
    assert!(stable.iter().all(|x| !x.version.contains('-')));
}

#[tokio::test]
async fn mariadb_fails_when_every_series_fails() {
    let env = Env::new().await;
    env.upstream
        .serve_fixture("/rest-api/mariadb/", "mariadb/all.json");
    let tool = tool("mariadb", MARIADB_V, MARIADB_SRC);
    let c = constraint("~11.8");
    let err = resolve(&env, &tool, Some(&c), &no_variants(), Some("stable"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found");
}

// ---------------------------------------------------------------------- github

const GH_SRC: &str = "    providers:\n      - type: github-release\n        repo: axllent/mailpit\n        asset: '^mailpit-windows-amd64\\.zip$'";

async fn gh_env() -> Env {
    let env = Env::new().await;
    env.upstream.serve_fixture(
        "/gh/repos/axllent/mailpit/releases",
        "github-release/mailpit-releases.json",
    );
    env
}

#[tokio::test]
async fn github_picks_the_asset_by_regex_and_uses_the_api_digest() {
    let env = gh_env().await;
    let tool = tool("mailpit", "", GH_SRC);
    let r = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap();
    assert!(r.len() >= 5);
    assert!(r.windows(2).all(|w| w[0].version != w[1].version));
    let newest = &r[0];
    assert_eq!(newest.file_name, "mailpit-windows-amd64.zip");
    assert!(newest.url.ends_with("/mailpit-windows-amd64.zip"));
    assert!(!newest.version.starts_with('v'));
    assert_eq!(newest.sha256.as_ref().map(String::len), Some(64));
    assert!(newest.size.unwrap() > 1_000_000);
    assert!(newest.published_at.is_some());
    assert!(
        env.upstream.hits("/gh/repos/axllent/mailpit/releases")[0]
            .authorization
            .is_none()
    );
}

#[tokio::test]
async fn github_token_is_sent_only_when_configured() {
    let env = Env::with(|c| c.github_token = Some("ghp_test".to_owned())).await;
    env.upstream.serve_fixture(
        "/gh/repos/axllent/mailpit/releases",
        "github-release/mailpit-releases.json",
    );
    let tool = tool("mailpit", "", GH_SRC);
    resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap();
    assert_eq!(
        env.upstream.hits("/gh/repos/axllent/mailpit/releases")[0]
            .authorization
            .as_deref(),
        Some("Bearer ghp_test")
    );
}

#[tokio::test]
async fn github_version_regex_prerelease_flag_and_checksum_asset() {
    let env = Env::new().await;
    let body = serde_json::json!([
        {"tag_name": "release-2.0.0-rc1", "draft": false, "prerelease": true,
         "published_at": "2026-01-02T03:04:05Z",
         "assets": [{"name": "tool-win.zip", "size": 10, "browser_download_url": format!("{}/dl/rc/tool-win.zip", env.upstream.base())}]},
        {"tag_name": "release-1.5.0", "draft": false, "prerelease": false,
         "published_at": "2025-01-02T03:04:05Z",
         "assets": [
            {"name": "tool-win.zip", "size": 11, "browser_download_url": format!("{}/dl/1.5.0/tool-win.zip", env.upstream.base())},
            {"name": "checksums.txt", "size": 100, "browser_download_url": format!("{}/dl/1.5.0/checksums.txt", env.upstream.base())}]},
        {"tag_name": "release-1.4.0", "draft": true, "prerelease": false, "assets": [
            {"name": "tool-win.zip", "size": 11, "browser_download_url": "https://example.test/x.zip"}]},
        {"tag_name": "weird", "draft": false, "prerelease": false, "assets": [
            {"name": "tool-win.zip", "size": 11, "browser_download_url": "https://example.test/y.zip"}]}
    ]);
    env.upstream
        .serve("/gh/repos/o/tool/releases", body.to_string());
    let sha = "c".repeat(64);
    env.upstream.serve(
        "/dl/1.5.0/checksums.txt",
        format!(
            "{sha}  tool-win.zip\n{}  tool-linux.tar.gz\n",
            "d".repeat(64)
        ),
    );
    let tool = tool(
        "tool",
        "  versioning:\n    channels:\n      preview: { constraint: \">=0.0.0-0\", prerelease: true }",
        "    providers:\n      - type: github-release\n        repo: o/tool\n        asset: '^tool-win\\.zip$'\n        version_regex: '^release-(.+)$'\n        checksum: { asset: checksums.txt }\n        prerelease: true",
    );
    let r = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap();
    // No channel: pre-releases are out; draft and non-matching tags too.
    assert_eq!(versions(&r), ["1.5.0"]);
    assert_eq!(r[0].sha256.as_deref(), Some(sha.as_str()));
    assert!(r[0].checksum_url.is_none());

    let r = resolve(&env, &tool, None, &no_variants(), Some("preview"))
        .await
        .unwrap();
    assert_eq!(versions(&r), ["2.0.0-rc1", "1.5.0"]);
    assert!(r[0].tags.contains(&"prerelease".to_owned()));
    assert!(r[0].sha256.is_none());
}

#[tokio::test]
async fn github_uses_etag_revalidation_and_survives_the_rate_limit() {
    let env = Env::with(|c| c.ttl = std::time::Duration::ZERO).await;
    env.upstream.serve_fixture(
        "/gh/repos/axllent/mailpit/releases",
        "github-release/mailpit-releases.json",
    );
    let tool = tool("mailpit", "", GH_SRC);
    let first = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap();
    let second = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap();
    assert_eq!(first, second);
    let hits = env.upstream.hits("/gh/repos/axllent/mailpit/releases");
    assert_eq!(hits.len(), 2);
    assert!(hits[0].if_none_match.is_none());
    assert!(
        hits[1].if_none_match.is_some(),
        "second request is conditional"
    );

    // Quota exhausted: the stale cache answers.
    env.upstream.fail_with(
        "/gh/repos/axllent/mailpit/releases",
        403,
        &[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1893456000"),
        ],
    );
    let third = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap();
    assert_eq!(first, third);

    // Without a cache the error says what happened.
    let fresh = Env::new().await;
    fresh.upstream.fail_with(
        "/gh/repos/axllent/mailpit/releases",
        403,
        &[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1893456000"),
        ],
    );
    let err = resolve(&fresh, &tool, None, &no_variants(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("limite de requisições"), "{err}");
}

#[tokio::test]
async fn github_invalid_asset_regex_is_a_config_error() {
    let env = gh_env().await;
    let tool = tool(
        "mailpit",
        "",
        "    providers:\n      - type: github-release\n        repo: axllent/mailpit\n        asset: '('",
    );
    let err = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "config_invalid");
}

// ------------------------------------------------------------------------ http

#[tokio::test]
async fn http_static_releases_render_placeholders_and_read_the_checksum_document() {
    let env = Env::new().await;
    let base = env.upstream.base();
    let sum = "9".repeat(64);
    env.upstream.serve(
        "/files/tool-1.2.3-vs17-nts.zip.sha256",
        format!("{sum}  tool-1.2.3-vs17-nts.zip\n"),
    );
    let tool = tool(
        "tool",
        "  versioning:\n    series_key: \"{major}.{minor}\"\n    variants:\n      - key: compiler\n        values: [vs17, vs16]\n      - key: ts\n        values: [nts, ts]",
        &format!(
            "    providers:\n      - type: http\n        url: \"{base}/files/tool-{{version}}-{{compiler}}{{flag}}.zip\"\n        vars:\n          flag: {{ nts: \"-nts\", ts: \"\" }}\n        checksum: {{ type: sha256, url: \"{{url}}.sha256\" }}\n    releases:\n      - version: 1.2.3\n        variants: {{ compiler: vs17, ts: nts }}\n      - version: 1.2.3\n        variants: {{ compiler: vs16, ts: ts }}\n        sha256: \"{}\"",
            "a".repeat(64)
        ),
    );
    let r = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap();
    assert_eq!(r.len(), 2);
    let vs17 = r.iter().find(|x| variant(x, "compiler") == "vs17").unwrap();
    assert_eq!(vs17.url, format!("{base}/files/tool-1.2.3-vs17-nts.zip"));
    assert_eq!(vs17.sha256.as_deref(), Some(sum.as_str()));
    let vs16 = r.iter().find(|x| variant(x, "compiler") == "vs16").unwrap();
    assert_eq!(vs16.url, format!("{base}/files/tool-1.2.3-vs16.zip"));
    assert_eq!(
        vs16.sha256.as_deref(),
        Some("a".repeat(64).as_str()),
        "static hash wins"
    );
    assert!(
        env.upstream
            .hits("/files/tool-1.2.3-vs16.zip.sha256")
            .is_empty()
    );
}

#[tokio::test]
async fn http_version_source_regex_and_json_path() {
    let env = Env::new().await;
    let base = env.upstream.base();
    env.upstream.serve(
        "/versions.html",
        "<a>tool-2.1.0.zip</a> <a>tool-2.0.1.zip</a> <a>tool-2.1.0.zip</a>",
    );
    env.upstream.serve(
        "/versions.json",
        r#"{"releases":[{"v":"3.0.0"},{"v":"3.1.0"}]}"#,
    );
    let v = "  versioning:\n    series_key: \"{major}.{minor}\"";
    let by_regex = tool(
        "tool",
        v,
        &format!(
            "    providers:\n      - type: http\n        url: \"{base}/dl/{{series}}/tool-{{version}}.zip\"\n        version_source: {{ url: \"{base}/versions.html\", regex: 'tool-([0-9.]+)\\.zip' }}"
        ),
    );
    let r = resolve(&env, &by_regex, None, &no_variants(), None)
        .await
        .unwrap();
    assert_eq!(versions(&r), ["2.1.0", "2.0.1"], "de-duplicated");
    assert_eq!(r[0].url, format!("{base}/dl/2.1/tool-2.1.0.zip"));

    let by_json = tool(
        "tool",
        v,
        &format!(
            "    providers:\n      - type: http\n        url: \"{base}/dl/tool-{{version}}.zip\"\n        version_source: {{ url: \"{base}/versions.json\", json_path: \"$.releases[*].v\" }}"
        ),
    );
    let c = constraint("^3");
    let r = resolve(&env, &by_json, Some(&c), &no_variants(), None)
        .await
        .unwrap();
    assert_eq!(versions(&r), ["3.1.0", "3.0.0"]);
}

#[tokio::test]
async fn http_unknown_placeholder_is_a_config_error() {
    let env = Env::new().await;
    let tool = tool(
        "tool",
        "",
        "    providers:\n      - type: http\n        url: \"https://example.test/{nope}.zip\"\n    releases:\n      - version: 1.0.0\n        sha256: \"1111111111111111111111111111111111111111111111111111111111111111\"",
    );
    let err = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "config_invalid");
}

// -------------------------------------------------------------- mysql / edb

fn curated_tool(kind: &str, releases: &str) -> ToolManifest {
    tool(
        kind,
        "",
        &format!("    providers:\n      - type: {kind}\n    releases:\n{releases}"),
    )
}

#[tokio::test]
async fn curated_providers_return_the_manifest_releases_without_network() {
    for kind in ["mysql", "postgresql-edb"] {
        let env = Env::new().await;
        let tool = curated_tool(
            kind,
            &format!(
                "      - version: 8.4.0\n        url: https://cdn.example.test/{kind}-8.4.0.zip\n        sha256: \"{}\"\n        size: 99\n      - version: 8.0.40\n        url: https://cdn.example.test/{kind}-8.0.40.zip\n        sha256: \"{}\"",
                "1".repeat(64),
                "2".repeat(64)
            ),
        );
        let r = resolve(&env, &tool, None, &no_variants(), None)
            .await
            .unwrap();
        assert_eq!(versions(&r), ["8.4.0", "8.0.40"]);
        assert_eq!(r[0].size, Some(99));
        assert_eq!(r[0].file_name, format!("{kind}-8.4.0.zip"));
        assert_eq!(env.upstream.total_hits(), 0);
        let c = constraint("~8.0");
        let r = resolve(&env, &tool, Some(&c), &no_variants(), None)
            .await
            .unwrap();
        assert_eq!(versions(&r), ["8.0.40"]);
    }
}

#[tokio::test]
async fn curated_providers_validate_the_format() {
    let env = Env::new().await;
    // Missing sha256 and non-http URL are reported together.
    let tool = curated_tool(
        "mysql",
        "      - version: 8.4.0\n        url: https://cdn.example.test/a.zip\n      - version: 8.0.40\n        url: ftp://cdn.example.test/b.zip\n        sha256: \"2222222222222222222222222222222222222222222222222222222222222222\"",
    );
    let err = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "config_invalid");
    let text = err.to_string();
    assert!(text.contains("8.4.0") && text.contains("sha256"), "{text}");
    assert!(text.contains("8.0.40") && text.contains("http"), "{text}");
}

// ------------------------------------------------------------ registry / cache

#[tokio::test]
async fn providers_are_tried_in_order_until_one_has_releases() {
    let env = Env::new().await;
    // First provider (github) is empty (404 -> error), second (mysql curated) answers.
    let tool = tool(
        "x",
        "",
        &format!(
            "    providers:\n      - type: github-release\n        repo: o/missing\n        asset: 'a'\n      - type: mysql\n    releases:\n      - version: 1.0.0\n        url: https://cdn.example.test/x.zip\n        sha256: \"{}\"",
            "3".repeat(64)
        ),
    );
    let r = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap();
    assert_eq!(versions(&r), ["1.0.0"]);
}

#[tokio::test]
async fn unsupported_providers_are_explicit() {
    let env = Env::new().await;
    let py = tool("python", "", "    providers:\n      - type: python-embed");
    let err = resolve(&env, &py, None, &no_variants(), None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_implemented");
}

#[tokio::test]
async fn cache_ttl_offline_and_stale_fallback() {
    let env = php_env().await;
    let php = php_tool(false);
    resolve(&env, &php, None, &no_variants(), Some("stable"))
        .await
        .unwrap();
    resolve(&env, &php, None, &no_variants(), Some("stable"))
        .await
        .unwrap();
    assert_eq!(
        env.upstream.hits("/php/releases/releases.json").len(),
        1,
        "fresh cache: one request"
    );
    // The cache lives under <cache_dir>/<provider>/.
    let dir = env.cache.path().join("cache").join("php-windows");
    assert!(std::fs::read_dir(dir).unwrap().count() >= 4);

    // Offline: served from the cache without a request.
    env.registry.set_offline(true);
    let r = resolve(&env, &php, None, &no_variants(), Some("stable"))
        .await
        .unwrap();
    assert!(!r.is_empty());
    assert_eq!(env.upstream.hits("/php/releases/releases.json").len(), 1);
    // ...but a document never fetched is a conflict.
    let nginx = tool("nginx", NGINX_V, "    providers:\n      - type: nginx-org");
    let err = resolve(&env, &nginx, None, &no_variants(), None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "conflict");
    assert!(err.to_string().contains("offline"), "{err}");
}

#[tokio::test]
async fn expired_cache_is_served_when_the_upstream_fails() {
    let env = Env::with(|c| c.ttl = std::time::Duration::ZERO).await;
    env.upstream
        .serve_fixture("/en/download.html", "nginx-org/download.html");
    let tool = tool("nginx", NGINX_V, "    providers:\n      - type: nginx-org");
    let first = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap();
    env.upstream.fail("/en/download.html", 502);
    let second = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap();
    assert_eq!(first, second);
    // A 404 is not masked by the cache.
    env.upstream.fail("/en/download.html", 404);
    let err = resolve(&env, &tool, None, &no_variants(), None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found");

    let probe = env
        .registry
        .fetcher()
        .get(forge_provider::provider::FetchRequest {
            provider: "nginx-org",
            url: &format!("{}/en/download.html", env.upstream.base()),
            ..Default::default()
        })
        .await;
    assert!(matches!(probe, Err(ForgeError::NotFound(_))));
}

#[tokio::test]
async fn concurrent_fetches_of_one_url_do_not_corrupt_the_cache() {
    let env = Env::with(|c| c.ttl = std::time::Duration::ZERO).await;
    env.upstream
        .serve_fixture("/en/download.html", "nginx-org/download.html");
    let expected = provider_support::fixture("nginx-org/download.html");
    let url = format!("{}/en/download.html", env.upstream.base());

    let mut tasks = Vec::new();
    for _ in 0..24 {
        let fetcher = env.registry.fetcher().clone();
        let url = url.clone();
        tasks.push(tokio::spawn(async move {
            fetcher
                .get(forge_provider::provider::FetchRequest {
                    provider: "nginx-org",
                    url: &url,
                    ..Default::default()
                })
                .await
                .map(|f| f.body.to_vec())
        }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap().unwrap(), expected);
    }

    env.registry.set_offline(true);
    let cached = env
        .registry
        .fetcher()
        .get(forge_provider::provider::FetchRequest {
            provider: "nginx-org",
            url: &url,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(cached.body.to_vec(), expected);
    let dir = env.cache.path().join("cache").join("nginx-org");
    for entry in std::fs::read_dir(dir).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(!name.ends_with(".tmp"), "leftover temp file {name}");
    }
}
