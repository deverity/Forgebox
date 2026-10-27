//! Online smoke tests: each provider against its real upstream, validating only the
//! shape of the answer (versions move, formats should not). Run with
//! `cargo test -p forge-provider --test providers_online -- --ignored`.

use std::collections::BTreeMap;

use forge_manifest::schema::{Manifest, ToolManifest, VersionScheme};
use forge_manifest::version::Constraint;
use forge_provider::provider::{ProviderConfig, ProviderRegistry, Release, ResolveRequest};

fn tool(id: &str, versioning: &str, source: &str) -> ToolManifest {
    let yaml = format!(
        "apiVersion: forgebox.dev/v1\nkind: Tool\nmetadata:\n  id: {id}\n  name: {id}\n  category: runtime\nspec:\n  platforms: [windows-x64]\n{versioning}\n  source:\n{source}\n"
    );
    match forge_manifest::load_str(&yaml) {
        Ok(Manifest::Tool(t)) => t,
        other => panic!("manifest: {other:?}"),
    }
}

fn registry() -> (ProviderRegistry, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = ProviderConfig::new(dir.path().join("cache"));
    config.max_checksum_fetches = 3;
    (ProviderRegistry::new(config).unwrap(), dir)
}

async fn resolve(
    tool: &ToolManifest,
    constraint: Option<&str>,
    channel: Option<&str>,
) -> Vec<Release> {
    let (registry, _dir) = registry();
    let constraint = constraint.map(|c| Constraint::parse(VersionScheme::Semver, c).unwrap());
    let variants = BTreeMap::new();
    let releases = registry
        .resolve(ResolveRequest {
            tool,
            constraint: constraint.as_ref(),
            variants: &variants,
            channel,
        })
        .await
        .unwrap_or_else(|e| panic!("{} failed: {e}", tool.metadata.id));
    assert!(!releases.is_empty(), "{}: no releases", tool.metadata.id);
    for r in &releases {
        assert!(
            r.url.starts_with("https://"),
            "{}: {}",
            tool.metadata.id,
            r.url
        );
        assert!(!r.file_name.is_empty());
        if let Some(sha) = &r.sha256 {
            assert!(
                sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
                "{sha}"
            );
        }
    }
    releases
}

fn assert_hashed(releases: &[Release]) {
    assert!(
        releases[0].sha256.is_some(),
        "newest release has no sha256: {:#?}",
        releases[0]
    );
}

#[tokio::test]
#[ignore = "online"]
async fn php_windows_online() {
    let t = tool(
        "php",
        "  versioning:\n    channels:\n      stable: { constraint: \">=7.4 <9\" }\n    variants:\n      - key: ts\n        values: [nts, ts]\n        default: nts\n      - key: compiler\n        values: [vs17, vs16, vc15]\n      - key: arch\n        values: [x64]",
        "    providers:\n      - type: php-windows",
    );
    let r = resolve(&t, None, Some("stable")).await;
    assert_hashed(&r);
    assert_eq!(r[0].variants["ts"], "nts");
    assert!(r.iter().any(|x| x.variants["ts"] == "ts"));
    assert!(r.iter().any(|x| x.variants["compiler"] == "vs17"));
    assert!(
        r[0].url
            .starts_with("https://downloads.php.net/~windows/releases/")
    );
    assert!(
        r.len() > 20,
        "archives should add older versions, got {}",
        r.len()
    );
}

#[tokio::test]
#[ignore = "online"]
async fn github_release_online() {
    let t = tool(
        "mailpit",
        "",
        "    providers:\n      - type: github-release\n        repo: axllent/mailpit\n        asset: '^mailpit-windows-amd64\\.zip$'",
    );
    let r = resolve(&t, None, None).await;
    assert_hashed(&r);
    assert_eq!(r[0].file_name, "mailpit-windows-amd64.zip");
}

#[tokio::test]
#[ignore = "online"]
async fn http_online() {
    let t = tool(
        "node",
        "",
        "    providers:\n      - type: http\n        url: \"https://nodejs.org/dist/v{version}/node-v{version}-win-x64.zip\"\n        checksum: { type: sha256, url: \"https://nodejs.org/dist/v{version}/SHASUMS256.txt\" }\n        version_source: { url: \"https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt\", regex: 'node-v([0-9.]+)-win-x64\\.zip' }",
    );
    let r = resolve(&t, None, None).await;
    assert_eq!(r.len(), 1);
    assert!(r[0].version.starts_with("22."));
    assert_hashed(&r);
}

#[tokio::test]
#[ignore = "online"]
async fn nodejs_dist_online() {
    let t = tool(
        "node",
        "  versioning:\n    channels:\n      current: { constraint: \">=0\" }\n      lts: { constraint: \">=0\" }",
        "    providers:\n      - type: nodejs-dist",
    );
    let r = resolve(&t, None, Some("current")).await;
    assert_hashed(&r);
    assert!(r[0].signature.is_some());
    let lts = resolve(&t, Some("^22"), Some("lts")).await;
    assert!(lts.iter().all(|x| x.version.starts_with("22.")));
    assert!(lts[0].tags.contains(&"lts".to_owned()));
}

#[tokio::test]
#[ignore = "online"]
async fn mariadb_online() {
    let t = tool(
        "mariadb",
        "  versioning:\n    channels:\n      stable: { constraint: \">=10\" }",
        "    providers:\n      - type: mariadb",
    );
    let r = resolve(&t, Some("~11.8"), Some("stable")).await;
    assert_hashed(&r);
    assert!(r[0].file_name.ends_with("-winx64.zip"));
    assert!(r[0].sha512.is_some());
}

#[tokio::test]
#[ignore = "online"]
async fn apache_lounge_online() {
    let t = tool(
        "apache",
        "  versioning:\n    variants:\n      - key: compiler\n        values: [vs18, vs17]\n      - key: arch\n        values: [x64]",
        "    providers:\n      - type: apache-lounge",
    );
    let r = resolve(&t, None, None).await;
    assert_hashed(&r);
    let compilers: Vec<&str> = r.iter().map(|x| x.variants["compiler"].as_str()).collect();
    assert!(
        compilers.contains(&"vs17") && compilers.contains(&"vs18"),
        "{compilers:?}"
    );
}

#[tokio::test]
#[ignore = "online"]
async fn nginx_org_online() {
    let t = tool("nginx", "", "    providers:\n      - type: nginx-org");
    let r = resolve(&t, None, None).await;
    assert!(r.iter().any(|x| x.tags.contains(&"mainline".to_owned())));
    assert!(r.iter().any(|x| x.tags.contains(&"stable".to_owned())));
    assert!(r[0].signature.is_some());
    // The signature file exists upstream.
    head_ok(&r[0].signature.as_ref().unwrap().url).await;
}

/// Curated providers do no network I/O; the online test checks that the curated URLs
/// still exist upstream (the hash below is a format placeholder, not checked here).
async fn curated_url_exists(kind: &str, version: &str, url: &str) {
    let t = tool(
        kind,
        "",
        &format!(
            "    providers:\n      - type: {kind}\n    releases:\n      - version: {version}\n        url: {url}\n        sha256: \"{}\"",
            "0".repeat(64)
        ),
    );
    let r = resolve(&t, None, None).await;
    assert_eq!(r[0].version, version);
    head_ok(&r[0].url).await;
}

async fn head_ok(url: &str) {
    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .header("Range", "bytes=0-0")
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "{url} answered {}",
        resp.status()
    );
}

#[tokio::test]
#[ignore = "online"]
async fn mysql_online() {
    curated_url_exists(
        "mysql",
        "8.4.0",
        "https://cdn.mysql.com/archives/mysql-8.4/mysql-8.4.0-winx64.zip",
    )
    .await;
}

#[tokio::test]
#[ignore = "online"]
async fn postgresql_edb_online() {
    curated_url_exists(
        "postgresql-edb",
        "17.5.1",
        "https://get.enterprisedb.com/postgresql/postgresql-17.5-1-windows-x64-binaries.zip",
    )
    .await;
}
