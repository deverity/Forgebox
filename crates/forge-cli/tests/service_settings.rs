//! `forge service set` (M2-E2-T7, ADR-0041 §7): without pairs it lists the quick settings
//! (`services.settings`), with `key=value` pairs it changes them (`services.set`), in
//! `--json` and in human mode, against a real `forgeboxd` with the daemon tests' fake
//! PHP (`crates/forge-daemon/tests/fixtures/fake_php.rs`) found by `discover`.
#![cfg(windows)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use common::*;

/// `fake_php.exe`, compiled once per test binary.
fn fake_php() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli_fake_php");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("fake_php.exe");
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../forge-daemon/tests/fixtures/fake_php.rs");
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let output = Command::new(rustc)
            .args(["--edition", "2024", "-O", "-o"])
            .arg(&exe)
            .arg(&source)
            .output()
            .expect("running rustc");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        exe
    })
    .clone()
}

#[test]
fn service_set_lists_and_changes_quick_settings() {
    let root = TestRoot::new(true);
    let dir = root.path.join("bin").join("php").join("8.4.27");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fake_php(), dir.join("php.exe")).unwrap();
    std::fs::copy(fake_php(), dir.join("php-cgi.exe")).unwrap();
    let _daemon = start_daemon(&root);

    // List (JSON): the `ServiceSettingsDto`.
    let listed = root.json(&["service", "set", "php@8.4"]);
    assert_eq!(listed["target"], "php@8.4");
    let settings = listed["settings"].as_array().unwrap();
    assert_eq!(settings.len(), 5);
    assert_eq!(settings[0]["key"], "memory_limit");
    assert_eq!(settings[0]["value"], "512M");
    assert_eq!(settings[0]["is_default"], true);
    assert_eq!(settings[2]["value"], 120);
    assert_eq!(settings[3]["value"], true);

    // List (human): one row per key, with the hint naming the target.
    let human = root
        .forge()
        .env("FORGE_LANG", "en")
        .run(&["service", "set", "php@8.4"]);
    human.assert_success();
    for key in [
        "memory_limit",
        "upload_max_filesize",
        "display_errors",
        "opcache",
    ] {
        assert!(human.stdout.contains(key), "{key}: {human:#?}");
    }
    assert!(
        format!("{}{}", human.stdout, human.stderr).contains("forge service set php@8.4 key=value"),
        "{human:#?}"
    );

    // Change (JSON): the job's `ServiceStatusDto`, then the new values are listed.
    let changed = root.json(&[
        "service",
        "set",
        "php@8.4",
        "memory_limit=1G",
        "display_errors=off",
    ]);
    assert!(changed["instances"].is_array() && changed["pools"].is_array());
    let listed = root.json(&["service", "set", "php@8.4"]);
    assert_eq!(listed["settings"][0]["value"], "1G");
    assert_eq!(listed["settings"][0]["is_default"], false);
    assert_eq!(listed["settings"][3]["value"], false);
    let overlay = std::fs::read_to_string(
        root.path
            .join("etc/php/8.4/conf.d/90-forgebox-settings.ini"),
    )
    .unwrap();
    assert!(
        overlay.contains("\nmemory_limit = 1G\n") && overlay.contains("\ndisplay_errors = Off\n"),
        "{overlay}"
    );

    // Change (human), back to the default with an empty value.
    let human =
        root.forge()
            .env("FORGE_LANG", "en")
            .run(&["service", "set", "php@8.4", "memory_limit="]);
    human.assert_success();
    assert!(
        format!("{}{}", human.stdout, human.stderr).contains("Settings applied: php@8.4"),
        "{human:#?}"
    );
    let listed = root.json(&["service", "set", "php@8.4"]);
    assert_eq!(listed["settings"][0]["value"], "512M");
    assert_eq!(listed["settings"][0]["is_default"], true);

    // A bad value: exit 2 (`invalid_input`) naming the key; a bad pair: usage error.
    let bad = root.run(&["--json", "service", "set", "php@8.4", "memory_limit=3G"]);
    assert_eq!(bad.code, 2, "{bad:#?}");
    assert_eq!(bad.error_code(), "invalid_input");
    assert!(bad.stdout.contains("memory_limit"), "{bad:#?}");
    let usage = root.run(&["service", "set", "php@8.4", "memory_limit"]);
    assert_eq!(usage.code, 2, "{usage:#?}");
    // A series that is not installed.
    let missing = root.run(&["--json", "service", "set", "php@7.4", "memory_limit=1G"]);
    assert_eq!(missing.error_code(), "not_found", "{missing:#?}");

    root.stop_daemon();
}
