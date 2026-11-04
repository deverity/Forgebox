//! `forge tool info <tool> --versions` and `forge tool install <specs> --dry-run`
//! (M2-E2-T5) against a real `forgeboxd`, in human and `--json` form.
//!
//! The providers are reached through a proxy that refuses connections (`HTTPS_PROXY` to a
//! closed port), which is "offline" for the daemon's metadata cache:
//!
//! * with a warm cache (the recorded `releases.json` of windows.php.net, seeded as an expired
//!   entry of `state/registry/cache/php-windows/`) the listing and the plan come from it,
//!   flagged `stale`;
//! * with a cold cache the command fails fast with a clear `io`/`timeout` error (never a
//!   hang), well inside 15 s;
//! * a plan writes nothing to `bin/`.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use pretty_assertions::assert_eq;
use sha2::{Digest, Sha256};

const PHP_RELEASES_URL: &str = "https://downloads.php.net/~windows/releases/releases.json";

fn fixture(rel: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../forge-provider/tests/fixtures")
        .join(rel);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Seeds one metadata document as the daemon's cache keeps it: `<key>.body` and
/// `<key>.json`, `key` = the first 24 hex digits of the sha256 of the URL.
fn seed_cache(root: &TestRoot, provider: &str, url: &str, body: &[u8], age: Duration) {
    let digest = Sha256::digest(url.as_bytes());
    let key: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    let dir: PathBuf = root.path.join("state/registry/cache").join(provider);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{key}.body")), body).unwrap();
    let fetched_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - age.as_secs();
    let meta = serde_json::json!({
        "url": url, "etag": null, "last_modified": null, "fetched_at": fetched_at
    });
    std::fs::write(dir.join(format!("{key}.json")), meta.to_string()).unwrap();
}

/// `forge` whose daemon cannot reach any provider.
fn offline(root: &TestRoot) -> Forge<'_> {
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let proxy = format!("http://127.0.0.1:{closed}");
    root.forge()
        .env("HTTPS_PROXY", proxy.clone())
        .env("HTTP_PROXY", proxy.clone())
        .env("ALL_PROXY", proxy)
        .env("NO_PROXY", "")
        .env("FORGE_LANG", "en")
}

fn warm_root() -> TestRoot {
    let root = TestRoot::new(true);
    seed_cache(
        &root,
        "php-windows",
        PHP_RELEASES_URL,
        &fixture("php-windows/releases.json"),
        Duration::from_secs(24 * 3600),
    );
    root
}

#[test]
fn versions_come_from_the_cache_when_the_providers_are_unreachable() {
    let root = warm_root();

    let json = offline(&root).run(&["--json", "tool", "info", "php", "--versions"]);
    json.assert_success();
    let doc = json.json();
    assert_eq!(doc["tool"], "php");
    assert_eq!(doc["stale"], true, "{doc}");
    assert!(doc["fetched_at"].is_string());
    let releases = doc["releases"].as_array().unwrap();
    assert!(releases.len() >= 2, "{doc}");
    let newest = &releases[0];
    assert_eq!(newest["version"], "8.5.12+1");
    assert_eq!(newest["series"], "8.5");
    assert_eq!(newest["channel"], "stable");
    assert_eq!(newest["installed"], false);
    let variants: Vec<&str> = newest["variants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["canonical_name"].as_str().unwrap())
        .collect();
    assert_eq!(variants.len(), 2);
    assert!(
        variants[0].contains("-nts-"),
        "preferred build first: {variants:?}"
    );

    let human = offline(&root).run(&["tool", "info", "php", "--versions"]);
    human.assert_success();
    let text = format!("{}{}", human.stdout, human.stderr);
    assert!(
        text.contains("Providers unreachable: cached list"),
        "{text}"
    );
    assert!(text.contains("8.5.12+1"), "{text}");
    assert!(text.contains("nts"), "{text}");
    root.stop_daemon();
}

#[test]
fn a_dry_run_plans_from_the_cache_and_writes_nothing() {
    let root = warm_root();

    let json = offline(&root).run(&["--json", "tool", "install", "php@8.4", "--dry-run"]);
    json.assert_success();
    let doc = json.json();
    let items = doc["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{doc}");
    let php = &items[0];
    assert_eq!(php["tool"], "php");
    assert_eq!(php["reason"], "requested");
    assert_eq!(php["already_installed"], false);
    assert!(
        php["version"].as_str().unwrap().starts_with("8.4."),
        "{doc}"
    );
    assert_eq!(php["variant"]["ts"], "nts", "the default variant");
    // releases.json only has a rounded size, so the manifest estimate (35 MB) stands in.
    assert_eq!(php["size_bytes"], 35 * 1024 * 1024);
    assert_eq!(php["size_estimated"], true);
    assert_eq!(doc["total_bytes"], 35 * 1024 * 1024);
    assert_eq!(doc["unknown_size"], 0);

    let human = offline(&root).run(&["tool", "install", "php@8.4", "--dry-run"]);
    human.assert_success();
    assert!(human.stdout.contains("php"), "{}", human.stdout);
    assert!(human.stdout.contains("download"), "{}", human.stdout);
    assert!(human.stdout.contains("To download: 35"), "{}", human.stdout);
    assert!(human.stdout.contains("≈"), "{}", human.stdout);

    // --variant narrows the build, as in the real install.
    let ts = offline(&root).run(&[
        "--json",
        "tool",
        "install",
        "php@8.4",
        "--variant",
        "ts",
        "--dry-run",
    ]);
    ts.assert_success();
    assert_eq!(ts.json()["items"][0]["variant"]["ts"], "ts");

    // Nothing was downloaded or created.
    assert!(!root.path.join("bin/php").exists());
    assert!(
        std::fs::read_dir(root.path.join("tmp/downloads"))
            .map(|d| d.count() == 0)
            .unwrap_or(true)
    );
    root.stop_daemon();
}

#[test]
fn a_cold_cache_without_network_fails_fast_and_clearly() {
    let root = TestRoot::new(true);
    for args in [
        &["--json", "tool", "info", "nginx", "--versions"][..],
        &["--json", "tool", "install", "nginx", "--dry-run"][..],
    ] {
        let run = offline(&root).run(args);
        assert_ne!(run.code, 0, "{}", run.command_line());
        run.assert_no_panic();
        let code = run.error_code();
        assert!(
            code == "io" || code == "timeout",
            "{}: {code}: {}",
            run.command_line(),
            run.stdout
        );
        assert!(
            run.elapsed < Duration::from_secs(15),
            "{} took {:?}",
            run.command_line(),
            run.elapsed
        );
    }
    root.stop_daemon();
}
