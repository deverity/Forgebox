//! `forge php ext list|enable|disable` and `forge php xdebug` (M2-E2-T9) against a real
//! `forgeboxd` with the daemon tests' fake PHP (`crates/forge-daemon/tests/fixtures/
//! fake_php.rs`) registered as `8.4.27-nts-vs17-x64` and Xdebug seeded into it the way
//! `tools.install xdebug` leaves it (ADR-0043), so nothing is downloaded. The store is
//! seeded through `forge_ipc::base` (the CLI's only workspace dependency).
#![cfg(windows)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use forge_ipc::base::ids::ToolId;
use forge_ipc::base::paths::ForgeRoot;
use forge_ipc::base::store::{
    NewToolVersion, Store, ToolVersionRepo, ToolVersionRow, VersionSource, VersionState,
};

use common::*;

/// `fake_php.exe`, compiled once per test binary.
fn fake_php() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli_fake_php_ext");
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

/// PHP 8.4.27 NTS VS17 with `php_intl.dll` and `php_xsl.dll`, registered, and Xdebug
/// installed into it (DLL, disabled overlay, linked row).
fn seed(root: &TestRoot) -> PathBuf {
    let folder = "8.4.27-nts-vs17-x64";
    let dir = root.path.join("bin").join("php").join(folder);
    std::fs::create_dir_all(dir.join("ext")).unwrap();
    std::fs::copy(fake_php(), dir.join("php.exe")).unwrap();
    std::fs::copy(fake_php(), dir.join("php-cgi.exe")).unwrap();
    for name in ["intl", "xsl", "curl"] {
        std::fs::write(dir.join(format!("ext/php_{name}.dll")), b"").unwrap();
    }
    let dll = dir.join("ext/php_xdebug.dll");
    std::fs::write(&dll, b"xdebug").unwrap();
    let overlay = root.path.join("etc/php/8.4/conf.d/20-xdebug.ini");
    std::fs::create_dir_all(overlay.parent().unwrap()).unwrap();
    std::fs::write(&overlay, ";zend_extension = xdebug\n").unwrap();

    let forge_root = ForgeRoot::new(&root.path).unwrap();
    std::fs::create_dir_all(forge_root.db().parent().unwrap()).unwrap();
    let store = Store::open(&forge_root.db()).unwrap();
    let repo = ToolVersionRepo::new(store.conn());
    let record = |row: NewToolVersion| -> ToolVersionRow {
        let (id, _) = repo.record_install(&row, None, None).unwrap();
        repo.get(id).unwrap().unwrap()
    };
    let php = record(NewToolVersion {
        tool_id: ToolId::new("php").unwrap(),
        tool_name: "PHP".into(),
        version: "8.4.27".into(),
        canonical_name: folder.into(),
        variant: BTreeMap::from([
            ("ts".to_owned(), "nts".to_owned()),
            ("compiler".to_owned(), "vs17".to_owned()),
            ("arch".to_owned(), "x64".to_owned()),
        ]),
        path: forge_ipc::base::paths::posix_string(&dir),
        state: VersionState::Installed,
        source: VersionSource::Registry,
    });
    record(NewToolVersion {
        tool_id: ToolId::new("xdebug").unwrap(),
        tool_name: "Xdebug".into(),
        version: "3.5.3".into(),
        canonical_name: format!("php-{folder}"),
        variant: BTreeMap::from([
            ("php_series".to_owned(), "8.4".to_owned()),
            ("ts".to_owned(), "nts".to_owned()),
            ("compiler".to_owned(), "vs17".to_owned()),
            ("target_tool".to_owned(), "php".to_owned()),
            ("target_id".to_owned(), php.id.to_string()),
            ("target_name".to_owned(), folder.to_owned()),
        ]),
        path: forge_ipc::base::paths::posix_string(&dll),
        state: VersionState::Installed,
        source: VersionSource::Registry,
    });
    root.path.join("etc/php/8.4/conf.d")
}

fn ext<'a>(list: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    list["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == name)
        .unwrap_or_else(|| panic!("{name} not listed: {list}"))
}

#[test]
fn php_ext_and_xdebug_commands() {
    let root = TestRoot::new(true);
    let conf = seed(&root);
    let _daemon = start_daemon(&root);

    // `forge php ext list 8.4` (JSON): bundled DLLs plus the registry extension.
    let list = root.json(&["php", "ext", "list", "8.4"]);
    assert_eq!(list["version"], "8.4.27");
    assert_eq!(ext(&list, "intl")["enabled"], true, "defaults_on");
    assert_eq!(ext(&list, "xsl")["enabled"], false);
    let xdebug = ext(&list, "xdebug");
    assert_eq!(
        (&xdebug["enabled"], &xdebug["bundled"], &xdebug["zend"]),
        (
            &serde_json::json!(false),
            &serde_json::json!(false),
            &serde_json::json!(true)
        )
    );
    // Human: one row per extension.
    let human = root
        .forge()
        .env("FORGE_LANG", "en")
        .run(&["php", "ext", "list", "8.4"]);
    human.assert_success();
    for name in ["intl", "xsl", "xdebug"] {
        assert!(human.stdout.contains(name), "{name}: {human:#?}");
    }

    // `forge php ext disable|enable 8.4 intl`: the job's list, and the overlay follows.
    let off = root.json(&["php", "ext", "disable", "8.4", "intl"]);
    assert_eq!(ext(&off, "intl")["enabled"], false);
    let base = std::fs::read_to_string(conf.join("10-forgebox.ini")).unwrap();
    assert!(!base.contains("extension = intl"), "{base}");
    let on = root.json(&["php", "ext", "enable", "8.4", "intl"]);
    assert_eq!(ext(&on, "intl")["enabled"], true);
    let base = std::fs::read_to_string(conf.join("10-forgebox.ini")).unwrap();
    assert!(base.contains("\nextension = intl\n"), "{base}");
    let human = root
        .forge()
        .env("FORGE_LANG", "en")
        .run(&["php", "ext", "enable", "8.4", "xsl"]);
    human.assert_success();
    assert!(human.stdout.contains("xsl"), "{human:#?}");
    // A DLL that is not there: not_found (exit 3).
    let missing = root.run(&["--json", "php", "ext", "enable", "8.4", "imagick"]);
    assert_eq!(missing.error_code(), "not_found", "{missing:#?}");

    // `forge php xdebug 8.4 on --port 9010` (a free port when 9010 is taken here).
    let port = if std::net::TcpListener::bind("127.0.0.1:9010").is_ok() {
        9010
    } else {
        free_port()
    };
    let port_text = port.to_string();
    let pool = root.json(&["php", "xdebug", "8.4", "on", "--port", &port_text]);
    assert_eq!(pool["series"], "8.4");
    assert_eq!(pool["xdebug"], true);
    let overlay = std::fs::read_to_string(conf.join("20-xdebug.ini")).unwrap();
    assert!(
        overlay.contains("\nzend_extension=php_xdebug.dll\n")
            && overlay.contains("\nxdebug.start_with_request = \"yes\"\n")
            && overlay.contains(&format!("\nxdebug.client_port = \"{port}\"\n")),
        "{overlay}"
    );
    let ports = root.json(&["service", "ports"]);
    assert!(
        ports["ports"].as_array().unwrap().iter().any(|p| {
            p["owner_name"] == "php@8.4/xdebug" && p["port"] == port && p["name"] == "xdebug"
        }),
        "{ports}"
    );
    let versions = root.json(&["php", "list"]);
    assert_eq!(versions["versions"][0]["xdebug"], "on", "{versions}");

    // Human, trigger then off; the message names the mode.
    let human = root
        .forge()
        .env("FORGE_LANG", "en")
        .run(&["php", "xdebug", "8.4", "trigger"]);
    human.assert_success();
    assert!(
        format!("{}{}", human.stdout, human.stderr).contains("Xdebug of PHP 8.4: trigger"),
        "{human:#?}"
    );
    let versions = root.json(&["php", "list"]);
    assert_eq!(versions["versions"][0]["xdebug"], "trigger");
    let pool = root.json(&["php", "xdebug", "8.4", "off"]);
    assert_eq!(pool["xdebug"], false);
    let overlay = std::fs::read_to_string(conf.join("20-xdebug.ini")).unwrap();
    assert!(
        overlay.contains("\n;zend_extension=php_xdebug.dll\n"),
        "{overlay}"
    );

    root.stop_daemon();
}
