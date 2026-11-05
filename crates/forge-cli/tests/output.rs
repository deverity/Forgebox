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
fn doctor_export_writes_a_support_bundle() {
    let fx = Fixture::new();
    let file = fx.root.path.join("support.json");
    let run = fx
        .root
        .run(&["doctor", "--export", file.to_str().unwrap(), "--json"]);
    assert!(run.code == 0 || run.code == 1, "{run:?}");
    let bundle: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).expect("the bundle file")).unwrap();
    assert_eq!(bundle["v"], 1);
    assert!(bundle["doctor"]["checks"].as_array().unwrap().len() > 5);
    assert!(bundle["status"]["pid"].is_number(), "{bundle}");
    assert!(bundle["tools"].is_array());
    let log = bundle["daemon_log"].as_array().unwrap();
    assert!(log.len() <= 500);
    assert!(!log.is_empty(), "the daemon logged while booting");
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

fn words(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

#[test]
fn stats_table_snapshots_in_both_languages_and_wide() {
    let fx = Fixture::new();
    let dir = fx.root.www("snap");
    std::fs::write(dir.join("index.html"), "<h1>s</h1>").unwrap();
    fx.root.json(&[
        "site",
        "add",
        &dir.display().to_string(),
        "--host",
        "snap.test",
    ]);

    // A portable root collects nothing until `metrics.enabled` is set: the row says so.
    let pt = fx.root.run(&["stats", "snap.test"]);
    pt.assert_success();
    assert_eq!(pt.stderr, "");
    let lines: Vec<&str> = pt.stdout.lines().collect();
    assert_eq!(
        words(lines[0]),
        [
            "SITE", "REQS", "RPS", "P50", "P90", "P99", "ERR%", "CPU", "RAM"
        ]
    );
    assert_eq!(
        words(lines[1]),
        ["snap.test", "desligadas", "-", "-", "-", "-", "-", "-", "-"]
    );
    assert!(!pt.stdout.contains('\u{1b}'), "NO_COLOR: {pt:?}");

    let en = fx
        .root
        .forge()
        .env("FORGE_LANG", "en")
        .run(&["stats", "snap.test"]);
    assert_eq!(words(en.stdout.lines().nth(1).unwrap())[1], "off");

    let wide = fx.root.run(&["stats", "snap.test", "--wide"]);
    wide.assert_success();
    let header = words(wide.stdout.lines().next().unwrap()).join(" ");
    assert_eq!(
        header,
        "SITE REQS RPS P50 P90 P99 ERR% CPU RAM 2XX 3XX 4XX 5XX BYTES PHP P50 PHP P90 PHP P99 WORKERS ÚLTIMA REQ."
    );

    // The window and the range are labels and bounds, not formats.
    let window = fx.root.json(&["stats", "snap.test", "--window", "1h"]);
    assert_eq!(window["window"], "1h");
    assert_eq!(window["sites"][0]["metrics_enabled"], false);
    let ranged = fx
        .root
        .json(&["stats", "snap.test", "--from", "2h", "--to", "1h"]);
    let sample = &ranged["sites"][0];
    let (from, at) = (
        sample["from"]
            .as_str()
            .unwrap()
            .parse::<forge_ipc::base::Timestamp>()
            .unwrap(),
        sample["at"]
            .as_str()
            .unwrap()
            .parse::<forge_ipc::base::Timestamp>()
            .unwrap(),
    );
    assert_eq!(at.duration_since(from).as_secs(), 3600, "{sample}");

    // --series: a dense series of the documented size; --routes and export are empty.
    for (window, points, step_ms) in [
        ("1h", 360, 10_000),
        ("24h", 1_440, 60_000),
        ("7d", 168, 3_600_000),
    ] {
        let series = fx
            .root
            .json(&["stats", "snap.test", "--series", "--window", window]);
        assert_eq!(series["points"].as_array().unwrap().len(), points);
        assert_eq!(series["step_ms"], step_ms);
    }
    let routes = fx.root.run(&["stats", "snap.test", "--routes"]);
    routes.assert_success();
    assert!(
        routes.stdout.contains("Sem métricas ainda.")
            && routes.stdout.contains("Nenhum erro recente."),
        "{routes:?}"
    );
    let csv = fx
        .root
        .run(&["stats", "export", "snap.test", "--from", "24h", "-o", "csv"]);
    csv.assert_success();
    assert!(csv.stdout.starts_with("timestamp,requests,rps,"), "{csv:?}");
    assert_eq!(csv.stdout.lines().count(), 1, "no traffic, only the header");
    let json = fx.root.json(&[
        "stats",
        "export",
        "snap.test",
        "--from",
        "24h",
        "-o",
        "json",
    ]);
    let doc: serde_json::Value = serde_json::from_str(json["content"].as_str().unwrap()).unwrap();
    assert_eq!(doc["site"], "snap.test");
    assert_eq!(doc["rows"].as_array().unwrap().len(), 0);
}

#[test]
fn top_prints_the_machine_header_and_the_daemon() {
    let fx = Fixture::new();
    let first = fx.root.run(&["top"]);
    first.assert_success();
    let lines: Vec<&str> = first.stdout.lines().collect();
    // The first call has no earlier sample: the CPU shares are unknown.
    assert!(
        lines[0].starts_with("CPU: - da máquina, - do Forgebox ("),
        "{first:?}"
    );
    assert!(lines[1].starts_with("RAM: ") && lines[1].contains("em uso de"));
    assert!(lines[2].starts_with("Disco raiz: ") && lines[2].contains("livres de"));
    assert_eq!(lines[3], "");
    assert_eq!(
        words(lines[4]),
        ["PID", "PROCESSO", "TIPO", "DONO", "CPU", "RAM"]
    );
    let daemon = lines.iter().find(|l| l.contains("forgeboxd")).unwrap();
    assert!(daemon.contains("daemon"), "{daemon}");
    assert!(daemon.to_lowercase().contains("forgeboxd.exe"), "{daemon}");

    let en = fx.root.forge().env("FORGE_LANG", "en").run(&["top"]);
    let lines: Vec<&str> = en.stdout.lines().collect();
    assert!(
        lines[0].starts_with("CPU: ") && lines[0].contains(" machine, "),
        "{en:?}"
    );
    // The second call compares with the first: a number.
    assert!(
        lines[0].split(' ').nth(1).unwrap().ends_with('%'),
        "{}",
        lines[0]
    );
    assert!(lines[2].starts_with("Disk root: "), "{en:?}");

    let json = fx.root.json(&["top"]);
    let machine = &json["machine"];
    assert!(machine["mem_total"].as_u64().unwrap() > 0, "{json}");
    assert!(machine["logical_cpus"].as_u64().unwrap() > 0);
    assert_eq!(machine["disks"][0]["role"], "root");
    let process = json["processes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["owner_kind"] == "daemon")
        .expect("the daemon");
    assert_eq!(process["owner_name"], "forgeboxd");
}
