//! Every command of docs/09 against a real `forgeboxd` on a temporary root.
//!
//! The table below is the contract: command line (minimal arguments) -> expected outcome,
//! which is either success with valid JSON, or a stable error code with the exit code of
//! docs/09 (`not_implemented` = 1 for the methods of later roadmap tasks). Nothing may hang
//! (30 s per run, enforced by the harness) or panic. The same table runs with `--json` and
//! in human mode; a clap-tree walk and the method table check that no command is missing.

mod common;

use clap::CommandFactory;
use common::*;
use forge_cli::cli::Cli;

/// What a command must do.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// Exit 0 and valid JSON.
    Ok,
    /// The stable error code (and so the exit code).
    Err(&'static str),
    /// `not_implemented`: exit 1, naming the roadmap task.
    NotImplemented,
    /// Exit 0 or 1 (environment dependent), valid JSON either way.
    OkOrFail,
}

struct Case {
    args: &'static [&'static str],
    expect: Expect,
    /// Output is not JSON even with `--json` (scripts, child output).
    raw: bool,
}

const fn case(args: &'static [&'static str], expect: Expect) -> Case {
    Case {
        args,
        expect,
        raw: false,
    }
}

const fn raw(args: &'static [&'static str], expect: Expect) -> Case {
    Case {
        args,
        expect,
        raw: true,
    }
}

use Expect::{Err as E, NotImplemented as NI, Ok as OK, OkOrFail};

/// The table. Placeholders: `{WWW}` = `<root>/www`, `{JOB}` = a finished job id,
/// `{TMP}` = a scratch folder, `{REGISTRY}` = the repository's registry folder,
/// `{ULID}` = a well-formed id that exists nowhere.
fn table() -> Vec<Case> {
    vec![
        // daemon
        case(&["daemon", "status"], OK),
        case(&["daemon", "start"], OK),
        case(&["daemon", "logs", "-n", "5"], OK),
        case(&["daemon", "doctor"], OkOrFail),
        case(&["doctor"], OkOrFail),
        // tool / catalog
        case(&["search", "nginx"], OK),
        case(&["search", "--category", "db"], OK),
        case(&["tool", "list"], OK),
        case(&["tool", "list", "--installed", "--updates"], OK),
        case(&["tool", "info", "php"], OK),
        case(&["tool", "info", "nonexistent"], E("not_found")),
        // The E2 contract (ADR-0041): releases and plans are M2-E2-T5, history M2-E2-T4,
        // extension targets M2-E2-T8.
        case(&["tool", "info", "--versions", "php"], NI),
        case(
            &["tool", "info", "--versions", "php", "--channel", "preview"],
            NI,
        ),
        case(&["tool", "info", "--history", "php"], NI),
        case(&["tool", "info", "--history", "php", "--series", "8.3"], NI),
        case(&["tool", "install", "--dry-run", "php@8.4", "nginx"], NI),
        case(&["tool", "install", "xdebug", "--for", "php@8.4"], NI),
        case(
            &[
                "tool",
                "remove",
                "xdebug@3.4.1",
                "--for",
                "php@8.4",
                "--yes",
            ],
            NI,
        ),
        case(
            &["tool", "install", "nonexistent@1.0", "--yes"],
            E("not_found"),
        ),
        case(&["tool", "remove", "php@8.2.29"], E("invalid_input")),
        case(&["tool", "remove", "php@8.2.29", "--yes"], E("not_found")),
        case(&["tool", "update", "--check"], OK),
        case(&["tool", "update"], E("invalid_input")),
        case(&["tool", "update", "nonexistent"], E("not_found")),
        case(&["tool", "rollback", "php@8.3"], E("not_found")),
        case(&["tool", "policy", "php", "notify"], E("not_found")),
        case(&["tool", "add-local", "{TMP}"], NI),
        case(&["tool", "discover"], OK),
        case(&["use", "php", "8.4"], E("not_found")),
        case(&["which", "php"], OK),
        // services
        case(&["start"], OK),
        case(&["stop"], OK),
        case(&["restart"], OK),
        case(&["start", "nginx"], E("not_found")),
        case(&["reload", "nginx"], E("not_found")),
        case(&["status"], OK),
        case(
            &["service", "create", "mysql@8.4", "legacy", "--port", "3307"],
            E("not_found"),
        ),
        case(
            &["service", "remove", "mysql/legacy", "--yes"],
            E("not_found"),
        ),
        case(&["service", "config", "nginx"], E("not_found")),
        case(&["service", "set", "php@8.3", "memory_limit=1G"], NI),
        // Without pairs: services.settings (M2-E2-T7).
        case(&["service", "set", "mysql/default"], NI),
        case(
            &[
                "service",
                "create",
                "mysql@8.4",
                "legacy",
                "--data-dir",
                "{TMP}/mysql-legacy",
            ],
            NI,
        ),
        case(
            &["service", "update", "mysql/default", "--no-autostart"],
            NI,
        ),
        case(
            &[
                "service",
                "update",
                "mysql/default",
                "--arg",
                "--skip-name-resolve",
                "--restart",
                "always",
            ],
            NI,
        ),
        case(&["service", "ports"], OK),
        // Port 1 is free: without --pid there is nothing to terminate; with one, the CLI
        // asks (no terminal: usage error) and the daemon answers not_implemented (E3-T11).
        case(&["service", "kill-port", "1"], E("not_found")),
        case(
            &["service", "kill-port", "1", "--pid", "4242"],
            E("invalid_input"),
        ),
        case(&["service", "kill-port", "1", "--pid", "4242", "--yes"], NI),
        case(&["logs", "daemon"], OK),
        case(&["logs", "nginx"], E("not_found")),
        case(&["logs", "--list"], NI),
        case(&["logs", "daemon", "--before", "100"], NI),
        // php
        case(&["php", "list"], OK),
        case(&["php", "pool", "status"], OK),
        case(&["php", "pool", "status", "8.3"], E("not_found")),
        case(&["php", "ext", "list", "8.3"], E("not_found")),
        case(&["php", "ext", "enable", "8.3", "intl"], NI),
        case(&["php", "ext", "disable", "8.3", "intl"], NI),
        case(&["php", "xdebug", "8.3", "on"], NI),
        case(&["php", "ini", "8.3"], E("not_found")),
        // sites
        case(&["site", "list"], OK),
        case(&["site", "add", "{WWW}/demo", "--host", "demo.test"], OK),
        case(&["site", "info", "demo.test"], OK),
        case(&["site", "env", "demo.test"], NI),
        case(&["site", "env", "demo.test", "--reveal"], NI),
        case(&["site", "php", "demo.test", "8.4"], E("invalid_input")),
        case(&["site", "secure", "demo.test"], OK),
        case(&["site", "unsecure", "demo.test"], OK),
        case(&["site", "server", "demo.test", "apache"], OK),
        case(&["site", "driver", "demo.test", "auto"], NI),
        case(&["site", "driver", "demo.test", "laravel"], NI),
        case(&["site", "alias", "demo.test", "add", "www.demo.test"], OK),
        // Plain `site open` would launch a real browser; the editor target stays not_implemented.
        case(&["site", "open", "demo.test", "--editor"], NI),
        case(&["site", "case-sensitive", "demo.test", "on"], OK),
        case(&["site", "lint-case", "demo.test"], OK),
        case(&["site", "remove", "demo.test"], E("invalid_input")),
        case(&["site", "remove", "demo.test", "--yes"], OK),
        case(&["site", "info", "demo.test"], E("not_found")),
        case(&["park", "{WWW}/parked", "--tld", ".test"], OK),
        case(&["parked"], OK),
        case(&["unpark", "{WWW}/parked"], OK),
        case(&["link", "{WWW}/linked", "--host", "linked.test"], OK),
        case(&["unlink", "{WWW}/linked", "--yes"], OK),
        case(
            &["proxy", "add", "app.test", "http://127.0.0.1:3000", "--ws"],
            OK,
        ),
        case(&["proxy", "remove", "app.test", "--yes"], OK),
        case(&["up", "{WWW}/proj"], E("not_found")),
        case(&["down", "{WWW}/proj"], E("not_found")),
        case(&["ps"], OK),
        case(&["ps", "nope.test"], E("not_found")),
        case(&["run", "nope.test", "web", "start"], E("not_found")),
        case(&["run", "nope.test", "web", "logs"], E("not_found")),
        case(&["share", "nope.test"], NI),
        case(&["new", "laravel", "loja"], NI),
        // db
        case(&["db", "list"], NI),
        case(&["db", "create", "loja"], NI),
        case(&["db", "drop", "loja"], E("invalid_input")),
        case(&["db", "drop", "loja", "--yes"], NI),
        case(&["db", "export", "loja", "-o", "{TMP}/loja.sql"], NI),
        case(&["db", "import", "loja", "{TMP}/loja.sql"], NI),
        case(&["db", "shell", "mysql/default"], NI),
        case(&["db", "url", "mysql/default", "loja"], NI),
        case(&["db", "open", "mysql/default", "--with", "heidisql"], NI),
        // net / tls
        case(&["dns", "status"], OK),
        case(&["dns", "enable", "--no-elevate"], E("needs_elevation")),
        case(&["dns", "disable", "--no-elevate"], E("needs_elevation")),
        case(&["dns", "test", "demo.test"], OkOrFail),
        case(&["tls", "ca", "status"], OK),
        case(
            &["tls", "ca", "install", "--scope", "machine", "--no-elevate"],
            E("needs_elevation"),
        ),
        case(&["tls", "ca", "rotate"], E("invalid_input")),
        case(&["tls", "ca", "uninstall"], NI),
        case(
            &[
                "tls",
                "ca",
                "uninstall",
                "--scope",
                "machine",
                "--no-elevate",
            ],
            E("needs_elevation"),
        ),
        case(&["tls", "issue", "cert.test"], OK),
        case(&["tls", "list"], NI),
        case(&["tls", "ca", "export"], OK),
        case(&["tls", "renew", "--all"], OK),
        case(&["tls", "export", "cert.test"], OK),
        case(&["lan", "expose", "nope.test"], NI),
        case(&["lan", "unexpose", "nope.test"], NI),
        case(&["lan", "qr", "nope.test"], NI),
        // mail
        case(&["mail", "status"], NI),
        case(&["mail", "open"], NI),
        case(&["mail", "list", "--unread"], NI),
        case(
            &["mail", "list", "--search", "pedido", "--offset", "10"],
            NI,
        ),
        case(&["mail", "show", "abc"], NI),
        case(&["mail", "show", "abc", "--keep-unread"], NI),
        case(&["mail", "show", "--attachment", "2", "abc"], NI),
        case(&["mail", "read", "abc", "def"], NI),
        case(&["mail", "read", "abc", "--unread"], NI),
        case(&["mail", "delete", "abc"], E("invalid_input")),
        case(&["mail", "delete", "abc", "--yes"], NI),
        case(&["mail", "clear"], E("invalid_input")),
        case(&["mail", "clear", "--yes"], NI),
        case(&["mail", "env"], NI),
        // shell / env
        case(&["shell"], NI),
        case(&["env"], OK),
        raw(&["env", "--cmd"], OK),
        raw(&["env", "--bash"], OK),
        raw(&["exec", "--", "cmd", "/c", "exit", "0"], OK),
        case(&["path", "show"], OK),
        case(&["path", "add"], E("conflict")),
        case(&["path", "remove"], OK),
        // fs / Linux parity
        case(&["fs", "case-sensitive", "status", "{WWW}"], OK),
        case(&["fs", "case-sensitive", "enable", "{WWW}/casedir"], OK),
        case(&["fs", "case-sensitive", "disable", "{WWW}/casedir"], OK),
        case(&["fs", "case-sensitive", "convert", "{WWW}/casedir"], OK),
        case(
            &[
                "fs",
                "case-sensitive",
                "convert",
                "{WWW}/casedir",
                "--revert",
            ],
            OK,
        ),
        // jobs
        case(&["jobs"], OK),
        case(&["jobs", "--active"], OK),
        case(&["jobs", "watch", "{JOB}"], OK),
        case(&["jobs", "cancel", "{JOB}"], OK),
        case(&["jobs", "watch", "not-a-job"], E("invalid_input")),
        // stats / remote
        case(&["stats"], NI),
        case(&["stats", "demo.test", "--window", "1h"], NI),
        case(
            &["stats", "export", "demo.test", "--from", "24h", "-o", "csv"],
            NI,
        ),
        case(&["top"], NI),
        case(
            &[
                "remote",
                "add",
                "hml",
                "wss://10.0.0.5:7788",
                "--code",
                "1234-5678",
            ],
            OK,
        ),
        case(&["remote", "list"], OK),
        case(&["remote", "use", "hml"], OK),
        case(&["status"], NI),
        case(&["remote", "use", "local"], OK),
        case(&["--context", "hml", "status"], NI),
        case(&["--context", "nope", "status"], E("not_found")),
        case(&["remote", "remove", "hml"], OK),
        case(&["remote", "pair"], NI),
        case(&["remote", "listen", "--addr", "127.0.0.1:7788"], NI),
        case(&["remote", "clients"], NI),
        case(&["remote", "revoke", "{ULID}", "--yes"], NI),
        case(&["remote", "revoke", "nope"], E("invalid_input")),
        // registry / system
        case(&["registry", "list"], OK),
        case(&["registry", "add", "{REGISTRY}", "--name", "extra"], OK),
        case(&["registry", "remove", "extra"], OK),
        case(&["registry", "update"], OK),
        case(&["import", "laragon", "{TMP}/laragon"], E("not_found")),
        case(
            &["import", "laragon", "{TMP}", "--dry-run"],
            E("invalid_input"),
        ),
        case(&["bundle", "export", "nginx", "-o", "{TMP}/stack.fbx"], NI),
        case(&["bundle", "import", "{TMP}/stack.fbx"], NI),
        case(&["backup", "now"], NI),
        case(&["backup", "schedule", "daily", "02:00"], NI),
        case(
            &["backup", "schedule", "daily", "25:00"],
            E("invalid_input"),
        ),
        case(&["config", "list"], NI),
        case(&["config", "get", "tld"], OK),
        case(&["config", "set", "tld", ".test"], OK),
        case(&["self-update", "--check"], NI),
        raw(&["completions", "powershell"], OK),
        raw(&["completions", "bash"], OK),
        raw(&["completions", "zsh"], OK),
        raw(&["mcp", "serve"], OkOrFail),
        // The daemon's own lifecycle goes last: a new daemon (restart), then none (stop).
        case(&["daemon", "restart"], OK),
        case(&["status"], OK),
        case(&["daemon", "stop"], OK),
        case(&["daemon", "status"], OkOrFail),
    ]
}

/// Values substituted into the arguments.
struct Vars {
    www: String,
    tmp: String,
    registry: String,
}

impl Vars {
    fn new(root: &TestRoot) -> Self {
        let tmp = root.path.join("scratch");
        std::fs::create_dir_all(&tmp).unwrap();
        Self {
            www: root.path.join("www").display().to_string(),
            tmp: tmp.display().to_string(),
            registry: repo_registry().display().to_string(),
        }
    }

    fn apply(&self, arg: &str, job: &str) -> String {
        arg.replace("{WWW}", &self.www)
            .replace("{TMP}", &self.tmp)
            .replace("{REGISTRY}", &self.registry)
            .replace("{JOB}", job)
            .replace("{ULID}", "01J0000000000000000000ZZZZ")
    }
}

/// Checks one run against the expectation.
fn check(run: &Run, case: &Case, json_mode: bool) {
    run.assert_no_panic();
    let what = format!("{} (expected {:?})", run.command_line(), case.expect);
    match case.expect {
        Expect::Ok => assert_eq!(run.code, 0, "{what}: {run:#?}"),
        Expect::Err(code) => {
            let exit = forge_ipc::base::error::exit_code_for(code);
            assert_eq!(run.code, exit, "{what}: {run:#?}");
            if json_mode {
                assert_eq!(run.error_code(), code, "{what}");
            }
        }
        Expect::NotImplemented => {
            assert_eq!(run.code, 1, "{what}: {run:#?}");
            if json_mode {
                assert_eq!(run.error_code(), "not_implemented", "{what}");
                let message = run.json()["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                assert!(message.contains("roadmap M"), "{what}: {message}");
            } else {
                assert!(run.stderr.contains("roadmap M"), "{what}: {run:#?}");
            }
        }
        Expect::OkOrFail => assert!(run.code == 0 || run.code == 1, "{what}: {run:#?}"),
    }
    if json_mode && !case.raw {
        // Success or failure, stdout is one JSON document.
        run.json();
    }
    if !json_mode {
        assert!(
            !run.stdout.contains('\u{1b}') && !run.stderr.contains('\u{1b}'),
            "{what}: colors with NO_COLOR: {run:#?}"
        );
    }
}

/// Runs the whole table in one root, in order, in the given mode.
fn run_table(json_mode: bool) {
    let root = TestRoot::new(true);
    let vars = Vars::new(&root);
    let _daemon = start_daemon(&root);
    // Whatever happens (a failed assertion, the restart), no daemon is left behind.
    struct StopOnDrop<'a>(&'a TestRoot);
    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.stop_daemon();
        }
    }
    let _stop = StopOnDrop(&root);
    std::fs::write(root.www("demo").join("index.html"), "<h1>demo</h1>").unwrap();
    root.www("parked");
    root.www("linked");
    root.www("casedir");
    let mut job = String::new();
    let mut failures: Vec<String> = Vec::new();
    for case in table() {
        if case.args.contains(&"{JOB}") && job.is_empty() {
            // A finished job to watch: the registry refresh of the boot, or any other.
            let jobs = root.json(&["jobs"]);
            job = jobs["jobs"][0]["id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            assert!(!job.is_empty(), "no job to watch: {jobs}");
        }
        let args: Vec<String> = case.args.iter().map(|a| vars.apply(a, &job)).collect();
        let mut full: Vec<&str> = Vec::new();
        if json_mode {
            full.push("--json");
        }
        full.extend(args.iter().map(String::as_str));
        let run = root.run(&full);
        // Every mismatch is reported at once, not only the first.
        let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            check(&run, &case, json_mode);
        }));
        if let Err(payload) = checked {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default();
            failures.push(message);
        }
    }
    assert!(
        failures.is_empty(),
        "{} commands differ:
{}",
        failures.len(),
        failures.join(
            "

"
        )
    );
}

#[test]
fn every_command_with_json() {
    run_table(true);
}

#[test]
fn every_command_in_human_mode() {
    run_table(false);
}

// --- coverage -------------------------------------------------------------------------

/// Paths of every runnable command in the clap tree (leaves, and groups whose subcommand
/// is optional, like `jobs` and `stats`).
fn command_paths(command: &clap::Command, prefix: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    let subs: Vec<&clap::Command> = command
        .get_subcommands()
        .filter(|c| c.get_name() != "help")
        .collect();
    if subs.is_empty() || (!command.is_subcommand_required_set() && !prefix.is_empty()) {
        out.push(prefix.clone());
    }
    for sub in subs {
        prefix.push(sub.get_name().to_owned());
        command_paths(sub, prefix, out);
        prefix.pop();
    }
}

/// The words of a row without the leading global flags (`--context hml status`).
fn command_words(args: &[&'static str]) -> Vec<&'static str> {
    let mut words = args;
    while let Some(first) = words.first() {
        if *first == "--context" {
            words = &words[2.min(words.len())..];
        } else if first.starts_with('-') {
            words = &words[1..];
        } else {
            break;
        }
    }
    words.to_vec()
}

#[test]
fn the_table_covers_every_command_of_the_clap_tree() {
    let mut paths = Vec::new();
    command_paths(&Cli::command(), &mut Vec::new(), &mut paths);
    assert!(paths.len() > 120, "only {} commands found", paths.len());
    let cases = table();
    let missing: Vec<String> = paths
        .iter()
        .filter(|path| {
            !cases.iter().any(|case| {
                let words = command_words(case.args);
                words.len() >= path.len() && words.iter().zip(path.iter()).all(|(a, b)| a == b)
            })
        })
        .map(|p| p.join(" "))
        .collect();
    assert!(
        missing.is_empty(),
        "commands without a row in the table: {missing:#?}"
    );
}

#[test]
fn every_cli_command_of_the_method_table_is_covered() {
    let cases = table();
    let lines: Vec<String> = cases
        .iter()
        .map(|c| format!("forge {}", command_words(c.args).join(" ")))
        .collect();
    // Client-side commands and the streams, which are tested in streams.rs.
    let tested_elsewhere = [
        "forge logs -f",
        "forge daemon logs -f",
        "forge status --watch",
        "forge stats --watch",
        "forge daemon stop",
        "forge daemon restart",
        "forge php ini --open",
    ];
    for method in forge_ipc::METHODS {
        for command in method
            .cli
            .split("; ")
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            if tested_elsewhere.contains(&command) {
                continue;
            }
            assert!(
                lines.iter().any(|line| line.starts_with(command)),
                "no row for `{command}` ({})",
                method.name
            );
        }
    }
}
