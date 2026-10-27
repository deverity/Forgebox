//! Exit codes of docs/09: 2 invalid use, 3 not found, 4 conflict, 5 needs elevation,
//! 1 for everything else (including `not_implemented`), with the stable code in `--json`.

mod common;

use common::*;

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

    /// Runs with `--json` and checks the exit code and the error code.
    fn expect(&self, args: &[&str], exit: i32, code: &str) {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let run = self.root.run(&all);
        run.assert_no_panic();
        assert_eq!(run.code, exit, "{}: {run:#?}", run.command_line());
        assert_eq!(run.error_code(), code, "{}: {run:#?}", run.command_line());
    }
}

#[test]
fn invalid_use_is_2() {
    let fx = Fixture::new();
    // Rejected by the command line parser.
    fx.expect(&["site", "add"], 2, "invalid_input");
    fx.expect(&["bogus"], 2, "invalid_input");
    fx.expect(&["tool", "info", "NOT A TOOL"], 2, "invalid_input");
    fx.expect(
        &["site", "alias", "x.test", "toggle", "y.test"],
        2,
        "invalid_input",
    );
    // Rejected by the CLI before any request.
    fx.expect(&["tool", "update"], 2, "invalid_input");
    fx.expect(&["tool", "install", "PHP@8.3"], 2, "invalid_input");
    fx.expect(
        &["backup", "schedule", "daily", "99:99"],
        2,
        "invalid_input",
    );
    fx.expect(
        &["stats", "export", "a.test", "--from", "yesterday"],
        2,
        "invalid_input",
    );
    fx.expect(
        &["service", "set", "php@8.3", "no-equals-sign"],
        2,
        "invalid_input",
    );
    fx.expect(&["jobs", "watch", "xyz"], 2, "invalid_input");
    // Destructive actions ask for confirmation, and without a terminal that is `--yes`.
    fx.expect(&["site", "remove", "a.test"], 2, "invalid_input");
    fx.expect(&["tool", "remove", "php@8.3"], 2, "invalid_input");
    fx.expect(&["service", "remove", "mysql/default"], 2, "invalid_input");
    fx.expect(&["db", "drop", "loja"], 2, "invalid_input");
    fx.expect(&["tls", "ca", "rotate"], 2, "invalid_input");
    fx.expect(&["mail", "clear"], 2, "invalid_input");
    fx.expect(
        &["remote", "revoke", "01J0000000000000000000ZZZZ"],
        2,
        "invalid_input",
    );
}

#[test]
fn a_parse_error_without_json_is_text_on_stderr() {
    let root = TestRoot::new(true);
    let run = root.run(&["site", "add"]);
    assert_eq!(run.code, 2);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("error"), "{run:?}");
}

#[test]
fn not_found_is_3() {
    let fx = Fixture::new();
    fx.expect(&["site", "info", "nope.test"], 3, "not_found");
    fx.expect(&["tool", "info", "nope"], 3, "not_found");
    fx.expect(&["start", "nginx"], 3, "not_found");
    fx.expect(&["use", "php", "9.9"], 3, "not_found");
    fx.expect(&["tool", "install", "nope@1", "--yes"], 3, "not_found");
    // A job that fails keeps the code of its error: `rollback` runs inside a job.
    fx.expect(&["tool", "rollback", "php@8.3"], 3, "not_found");
    fx.expect(&["logs", "nope"], 3, "not_found");
    fx.expect(&["logs", "site:nope.test"], 3, "not_found");
    fx.expect(
        &["exec", "--", "definitely-not-a-program-xyz"],
        3,
        "not_found",
    );
}

#[test]
fn conflict_is_4() {
    let fx = Fixture::new();
    let dir = fx.root.www("demo");
    let dir = dir.to_str().unwrap();
    fx.root
        .run(&["site", "add", dir, "--host", "demo.test"])
        .assert_success();
    // The host is taken.
    fx.expect(&["site", "add", dir, "--host", "demo.test"], 4, "conflict");
    // Portable mode never touches the user PATH.
    fx.expect(&["path", "add"], 4, "conflict");
}

#[test]
fn needs_elevation_is_5_and_never_reaches_the_daemon() {
    let fx = Fixture::new();
    fx.expect(&["dns", "enable", "--no-elevate"], 5, "needs_elevation");
    fx.expect(&["dns", "disable", "--no-elevate"], 5, "needs_elevation");
    fx.expect(
        &["tls", "ca", "install", "--scope", "machine", "--no-elevate"],
        5,
        "needs_elevation",
    );
    fx.expect(
        &["lan", "expose", "a.test", "--no-elevate"],
        5,
        "needs_elevation",
    );
    // The DNS policy of the daemon was not touched.
    let dns = fx.root.json(&["dns", "status"]);
    assert_eq!(dns["enabled"], false);
    // Without --no-elevate the notice is announced on stderr before the request.
    let run = fx.root.run(&["lan", "unexpose", "a.test"]);
    assert_eq!(run.code, 1, "{run:?}");
    assert!(run.stderr.contains("UAC"), "{run:?}");
}

#[test]
fn not_implemented_is_1_and_names_the_roadmap_task() {
    let fx = Fixture::new();
    for (args, task) in [
        (&["share", "a.test"][..], "M3-E3"),
        (&["db", "list"][..], "M2-E3"),
        (&["stats"][..], "M2-E5"),
        (&["remote", "pair"][..], "M6-E3"),
        (&["backup", "now"][..], "M3-E3"),
        (&["self-update", "--check"][..], "M4-E1"),
    ] {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let run = fx.root.run(&all);
        run.assert_no_panic();
        assert_eq!(run.code, 1, "{run:#?}");
        assert_eq!(run.error_code(), "not_implemented");
        let message = run.json()["error"]["message"].as_str().unwrap().to_owned();
        assert!(message.contains(task), "{message}");
        let human = fx.root.run(args);
        assert_eq!(human.code, 1);
        assert!(human.stderr.starts_with("erro: "), "{human:?}");
        assert!(human.stderr.contains(task), "{human:?}");
    }
}

#[test]
fn every_stable_error_code_has_the_documented_exit_code() {
    for code in forge_ipc::base::error::ERROR_CODES {
        let expected = match *code {
            "invalid_input" | "config_invalid" => 2,
            "not_found" => 3,
            "conflict" | "port_in_use" => 4,
            "needs_elevation" => 5,
            "verification_failed" => 6,
            _ => 1,
        };
        assert_eq!(
            forge_ipc::base::error::exit_code_for(code),
            expected,
            "{code}"
        );
        assert_eq!(
            forge_cli::error::CliError::new(*code, "x").exit,
            expected,
            "{code}"
        );
    }
}
