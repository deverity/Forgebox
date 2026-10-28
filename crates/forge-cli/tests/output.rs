//! What `forge` prints: human output (snapshots with `NO_COLOR`), colors when forced,
//! `--json` documents, `--quiet`, the language switch and where errors go.

mod common;

use common::*;
use pretty_assertions::assert_eq;

/// A root with a running daemon and the guard that stops it.
struct Fixture {
    root: TestRoot,
    _daemon: DaemonProcess,
}

impl Fixture {
    fn new() -> Self {
        let root = TestRoot::new(true);
        let daemon = start_daemon(&root);
        Self {
            root,
            _daemon: daemon,
        }
    }

    /// stdout with the root's path replaced by `<ROOT>` (it differs on every run).
    fn normalized(&self, run: &Run) -> String {
        let long = forge_ipc::base::platform::long_path(&self.root.path);
        let posix = |p: &std::path::Path| p.to_string_lossy().replace('\\', "/");
        run.stdout
            .replace(&posix(&long), "<ROOT>")
            .replace(&posix(&self.root.path), "<ROOT>")
    }
}

#[test]
fn status_of_an_empty_root_in_both_languages() {
    let fx = Fixture::new();
    let run = fx.root.run(&["status"]);
    run.assert_success();
    assert_eq!(
        run.stdout,
        "Nenhum serviço instalado ainda. Use `forge tool install`.\n"
    );
    assert_eq!(run.stderr, "");

    let english = fx.root.forge().env("FORGE_LANG", "en").run(&["status"]);
    english.assert_success();
    assert_eq!(
        english.stdout,
        "No services installed yet. Use `forge tool install`.\n"
    );
    // en_US spelling works too.
    let english = fx.root.forge().env("FORGE_LANG", "en_US").run(&["status"]);
    assert!(english.stdout.starts_with("No services"), "{english:?}");
}

#[test]
fn status_json_is_the_dto_of_the_api() {
    let fx = Fixture::new();
    let run = fx.root.run(&["status", "--json"]);
    run.assert_success();
    assert_eq!(run.stdout, "{\"instances\":[],\"pools\":[]}\n");
}

#[test]
fn site_list_snapshot() {
    let fx = Fixture::new();
    let dir = fx.root.www("demo");
    std::fs::write(dir.join("index.html"), "<h1>x</h1>").unwrap();
    let empty = fx.root.run(&["site", "list"]);
    empty.assert_success();
    assert_eq!(
        empty.stdout,
        "Nenhum site. Use `forge site add <pasta>` ou `forge park <pasta>`.\n"
    );

    let add = fx
        .root
        .run(&["site", "add", dir.to_str().unwrap(), "--host", "demo.test"]);
    add.assert_success();
    let text = fx.normalized(&add);
    assert!(text.starts_with("ok Site criado: demo.test\n"), "{text}");

    let list = fx.root.run(&["site", "list"]);
    list.assert_success();
    let text = fx.normalized(&list);
    // Header and row share the column widths; the last column is the project folder.
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(
        lines[0].split_whitespace().collect::<Vec<_>>(),
        ["HOST", "TIPO", "PHP", "SERVIDOR", "URL", "CASE", "DESTINO"]
    );
    let cells: Vec<&str> = lines[1].split_whitespace().collect();
    assert_eq!(cells[0], "demo.test");
    assert_eq!(cells[3], "nginx");
    assert_eq!(cells[5], "sim");
    assert_eq!(cells[6], "<ROOT>/www/demo");
    // Columns are aligned: every cell starts under its header (by characters: the type
    // "estático" has an accent).
    let header: Vec<char> = lines[0].chars().collect();
    let row: Vec<char> = lines[1].chars().collect();
    for word in ["TIPO", "PHP", "SERVIDOR", "URL", "CASE", "DESTINO"] {
        let at = lines[0][..lines[0].find(word).unwrap()].chars().count();
        assert!(
            header[at - 1] == ' ' && row[at - 1] == ' ' && row[at] != ' ',
            "{word} is not aligned:\n{text}"
        );
    }
    assert!(!text.contains('\u{1b}'));

    let json = fx.root.json(&["site", "list"]);
    assert_eq!(json["sites"][0]["host"], "demo.test");
    assert_eq!(json["sites"][0]["case_sensitive"], true);
}

#[test]
fn colors_follow_no_color_and_clicolor_force() {
    let fx = Fixture::new();
    // The harness sets NO_COLOR; a pipe is not a terminal either way.
    let plain = fx.root.run(&["daemon", "status"]);
    plain.assert_success();
    assert!(!plain.stdout.contains('\u{1b}'), "{plain:?}");

    // CLICOLOR_FORCE turns colors on for pipes...
    let forced = fx
        .root
        .forge()
        .env_remove("NO_COLOR")
        .env("CLICOLOR_FORCE", "1")
        .run(&["daemon", "status"]);
    forced.assert_success();
    assert!(forced.stdout.contains("\u{1b}["), "{forced:?}");
    // ... but NO_COLOR wins over it.
    let both = fx
        .root
        .forge()
        .env("NO_COLOR", "1")
        .env("CLICOLOR_FORCE", "1")
        .run(&["daemon", "status"]);
    assert!(!both.stdout.contains('\u{1b}'), "{both:?}");
    // JSON never has colors.
    let json = fx
        .root
        .forge()
        .env_remove("NO_COLOR")
        .env("CLICOLOR_FORCE", "1")
        .run(&["daemon", "status", "--json"]);
    assert!(!json.stdout.contains('\u{1b}'), "{json:?}");
    json.json();
}

#[test]
fn errors_go_to_stderr_in_human_mode_and_to_stdout_as_json() {
    let fx = Fixture::new();
    let human = fx.root.run(&["tool", "info", "nonexistent"]);
    assert_eq!(human.code, 3);
    assert_eq!(human.stdout, "");
    assert!(human.stderr.starts_with("erro: "), "{human:?}");
    assert!(human.stderr.contains("nonexistent"), "{human:?}");

    let json = fx.root.run(&["tool", "info", "nonexistent", "--json"]);
    assert_eq!(json.code, 3);
    assert_eq!(json.stderr, "");
    let doc = json.json();
    assert_eq!(doc["error"]["code"], "not_found");
    assert!(
        doc["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nonexistent")
    );

    let english = fx
        .root
        .forge()
        .env("FORGE_LANG", "en")
        .run(&["site", "remove", "demo.test"]);
    assert_eq!(english.code, 2);
    assert!(english.stderr.starts_with("error: "), "{english:?}");
    assert!(english.stderr.contains("--yes"), "{english:?}");
}

#[test]
fn quiet_keeps_results_and_drops_status_lines() {
    let fx = Fixture::new();
    let dir = fx.root.www("demo");
    let add = fx.root.run(&[
        "site",
        "add",
        dir.to_str().unwrap(),
        "--host",
        "demo.test",
        "--quiet",
    ]);
    add.assert_success();
    assert_eq!(
        add.stdout,
        "demo.test
",
        "{add:?}"
    );

    // The result of a query is still printed.
    let list = fx.root.run(&["site", "list", "--quiet"]);
    list.assert_success();
    assert!(list.stdout.contains("demo.test"), "{list:?}");

    let remove = fx.root.run(&["site", "remove", "demo.test", "--yes", "-q"]);
    remove.assert_success();
    assert_eq!(remove.stdout, "");
    assert_eq!(remove.stderr, "");
}

#[test]
fn help_and_version_work_without_a_daemon() {
    let root = TestRoot::new(true);
    let version = root.run(&["--version"]);
    version.assert_success();
    assert_eq!(
        version.stdout.trim(),
        format!("forge {}", env!("CARGO_PKG_VERSION"))
    );
    let help = root.run(&["--help"]);
    help.assert_success();
    for group in [
        "daemon",
        "tool",
        "site",
        "db",
        "tls",
        "fs",
        "stats",
        "remote",
        "completions",
    ] {
        assert!(help.stdout.contains(group), "{group} missing from --help");
    }
    let sub = root.run(&["site", "add", "--help"]);
    sub.assert_success();
    assert!(sub.stdout.contains("--host"), "{sub:?}");
    // None of this started a daemon.
    assert!(root.daemon_pid().is_none());
}

#[test]
fn doctor_reports_every_area() {
    let fx = Fixture::new();
    let run = fx.root.run(&["doctor", "--json"]);
    assert!(run.code == 0 || run.code == 1, "{run:?}");
    let report = run.json();
    let ids: Vec<&str> = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    for wanted in [
        "root.writable",
        "store.schema",
        "dns.server",
        "tls.ca",
        "port.80",
        "runtime.vcredist",
        "path.user",
        "defender.exclusion",
        "versions.daemon",
        "versions.cli",
        "registry.official",
    ] {
        assert!(ids.contains(&wanted), "{wanted} missing from {ids:?}");
    }
    // The root is portable: the user PATH is not the daemon's business.
    let path = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "path.user")
        .unwrap();
    assert_eq!(path["status"], "skipped");
    // Exit code 1 only with a failed check.
    let failed = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["status"] == "fail");
    assert_eq!(run.code == 1, failed, "{report}");

    let human = fx.root.run(&["doctor"]);
    assert!(human.stdout.contains("versions.cli"), "{human:?}");
    assert!(human.stdout.contains("ok"), "{human:?}");
}
