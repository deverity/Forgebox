//! `forge service update` and `forge service create --data-dir` (M2-E2-T10, ADR-0041),
//! in `--json` and in human mode, against a real `forgeboxd` with the daemon tests' fake
//! `mysqld.exe` (`crates/forge-daemon/tests/fixtures/fake_mysqld.rs`, which appends
//! `version=… argv=… FB_T10=…` to `<datadir>/launch.log` at each start).
#![cfg(windows)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use common::*;

fn fake_mysqld() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli_fake_mysqld");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("fake_mysqld.exe");
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../forge-daemon/tests/fixtures/fake_mysqld.rs");
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

fn launch_log(root: &TestRoot) -> Vec<String> {
    std::fs::read_to_string(root.path.join("data/mysql/default/launch.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn wait_launches(root: &TestRoot, count: usize) -> Vec<String> {
    let started = Instant::now();
    loop {
        let lines = launch_log(root);
        if lines.len() >= count {
            return lines;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "launch.log: {lines:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn service_update_and_create_data_dir_in_json_and_human() {
    let root = TestRoot::new(true);
    for version in ["8.0.40", "8.4.13", "8.4.14"] {
        let bin = root.path.join("bin/mysql").join(version).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::copy(fake_mysqld(), bin.join("mysqld.exe")).unwrap();
        std::fs::copy(fake_mysqld(), bin.join("mysqladmin.exe")).unwrap();
    }
    let _daemon = start_daemon(&root);
    wait_launches(&root, 1);

    // --json: the updated InstanceDto; the running instance restarted with the new argv.
    let updated = root.json(&[
        "service",
        "update",
        "mysql/default",
        "--arg",
        "--fb-extra=1",
        "--env",
        "FB_T10=beta",
        "--restart",
        "always",
        "--restart-window",
        "90",
    ]);
    assert_eq!(updated["tool"], "mysql");
    assert_eq!(updated["name"], "default");
    let lines = wait_launches(&root, 2);
    assert!(lines[1].contains("--fb-extra=1"), "{lines:?}");
    assert!(lines[1].ends_with("FB_T10=beta"), "{lines:?}");

    // The read side (`service set <target>` is quick settings; the tab data is the
    // `advanced` block of services.settings, shown to this admin connection).
    let listed = root.json(&["service", "set", "mysql/default"]);
    let advanced = &listed["advanced"];
    assert_eq!(advanced["args"], serde_json::json!(["--fb-extra=1"]));
    assert_eq!(advanced["env"]["FB_T10"], "beta");
    assert_eq!(advanced["restart_policy"]["on"], "always");
    assert_eq!(advanced["restart_policy"]["window_secs"], 90);
    assert_eq!(
        advanced["versions"],
        serde_json::json!(["8.4.14", "8.4.13"])
    );

    // Human: the success line, and the same-series version move.
    let human = root.forge().env("FORGE_LANG", "en").run(&[
        "service",
        "update",
        "mysql/default",
        "--version",
        "8.4.13",
        "--no-autostart",
    ]);
    human.assert_success();
    assert!(
        format!("{}{}", human.stdout, human.stderr).contains("Instance updated: mysql/default"),
        "{human:#?}"
    );
    let lines = wait_launches(&root, 3);
    assert!(lines[2].starts_with("version=8.4.13 "), "{lines:?}");

    // Another series: exit 2 (`invalid_input`) with the explanation; nothing restarts.
    let cross = root.run(&[
        "--json",
        "service",
        "update",
        "mysql/default",
        "--version",
        "8.0.40",
    ]);
    assert_eq!(cross.code, 2, "{cross:#?}");
    assert_eq!(cross.error_code(), "invalid_input");
    assert!(cross.stdout.contains("series"), "{cross:#?}");
    let human = root.forge().env("FORGE_LANG", "en").run(&[
        "service",
        "update",
        "mysql/default",
        "--version",
        "8.0.40",
    ]);
    assert_eq!(human.code, 2, "{human:#?}");
    assert!(
        format!("{}{}", human.stdout, human.stderr).contains("series"),
        "{human:#?}"
    );
    // Bad environment name; unknown instance.
    let bad = root.run(&[
        "--json",
        "service",
        "update",
        "mysql/default",
        "--env",
        "PATH=C:/x",
    ]);
    assert_eq!(bad.error_code(), "invalid_input", "{bad:#?}");
    let missing = root.run(&[
        "--json",
        "service",
        "update",
        "mysql/nope",
        "--no-autostart",
    ]);
    assert_eq!(missing.error_code(), "not_found", "{missing:#?}");
    assert_eq!(launch_log(&root).len(), 3);

    // create --data-dir: refused before any job when it is not absolute or is in bin/.
    let bad = root.run(&[
        "--json",
        "service",
        "create",
        "mysql@8.4",
        "legacy",
        "--data-dir",
        root.path.join("bin/mysql/8.4.14/data").to_str().unwrap(),
    ]);
    assert_eq!(bad.code, 2, "{bad:#?}");
    assert_eq!(bad.error_code(), "invalid_input");
    // A good folder: the DTO carries it; human text names the instance.
    let outside = tempfile::tempdir().unwrap();
    let wanted = outside.path().join("mysql-legacy");
    let wanted_text = wanted.to_str().unwrap();
    let made = root.json(&[
        "service",
        "create",
        "mysql@8.4",
        "legacy",
        "--data-dir",
        wanted_text,
    ]);
    assert_eq!(made["name"], "legacy");
    assert_eq!(
        made["data_dir"].as_str().unwrap().to_lowercase(),
        wanted_text.replace('\\', "/").to_lowercase()
    );
    let human = root.forge().env("FORGE_LANG", "en").run(&[
        "service",
        "create",
        "mysql@8.4",
        "other",
        "--data-dir",
        outside.path().join("mysql-other").to_str().unwrap(),
    ]);
    human.assert_success();
    assert!(
        format!("{}{}", human.stdout, human.stderr).contains("Instance created: mysql/other"),
        "{human:#?}"
    );

    root.stop_daemon();
}
