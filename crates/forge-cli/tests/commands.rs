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
        case(&["doctor", "--export", "{TMP}/support.json"], OkOrFail),
        // tool / catalog
        case(&["search", "nginx"], OK),
        case(&["search", "--category", "db"], OK),
        case(&["tool", "list"], OK),
        case(&["tool", "list", "--installed", "--updates"], OK),
        case(&["tool", "info", "php"], OK),
        case(&["tool", "info", "nonexistent"], E("not_found")),
        // The E2 contract (ADR-0041): releases and plans are M2-E2-T5; history (M2-E2-T4) answers.
        case(&["tool", "info", "--versions", "php"], NI),
        case(
            &["tool", "info", "--versions", "php", "--channel", "preview"],
            NI,
        ),
        case(&["tool", "info", "--history", "php"], OK),
        case(&["tool", "info", "--history", "php", "--series", "8.3"], OK),
        case(&["tool", "install", "--dry-run", "php@8.4", "nginx"], NI),
        // Extensions (ADR-0043): no PHP in this root.
        case(
            &["tool", "install", "xdebug", "--for", "php@8.4", "--yes"],
            E("not_found"),
        ),
        case(
            &[
                "tool",
                "remove",
                "xdebug@3.4.1",
                "--for",
                "php@8.4",
                "--yes",
            ],
            E("not_found"),
        ),
        case(
            &["tool", "install", "nginx", "--for", "php@8.4", "--yes"],
            E("invalid_input"),
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
        // No PHP and no MySQL in this root (quick settings: tests/service_settings.rs).
        case(
            &["service", "set", "php@8.3", "memory_limit=1G"],
            E("not_found"),
        ),
        // Without pairs: services.settings.
        case(&["service", "set", "mysql/default"], E("not_found")),
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
        // asks (no terminal: usage error); with --yes the daemon opens the pid (M2-E3-T11):
        // no such process.
        case(&["service", "kill-port", "1"], E("not_found")),
        case(
            &["service", "kill-port", "1", "--pid", "4000000000"],
            E("invalid_input"),
        ),
        case(
            &["service", "kill-port", "1", "--pid", "4000000000", "--yes"],
            E("not_found"),
        ),
        case(&["logs", "daemon"], OK),
        case(&["logs", "nginx"], E("not_found")),
        case(&["logs", "--list"], NI),
        case(&["logs", "daemon", "--before", "100"], NI),
        // php
        case(&["php", "list"], OK),
        case(&["php", "pool", "status"], OK),
        case(&["php", "pool", "status", "8.3"], E("not_found")),
        case(&["php", "ext", "list", "8.3"], E("not_found")),
        // M2-E2-T9: real now; no PHP 8.3 in this root.
        case(&["php", "ext", "enable", "8.3", "intl"], E("not_found")),
        case(&["php", "ext", "disable", "8.3", "intl"], E("not_found")),
        case(&["php", "xdebug", "8.3", "on"], E("not_found")),
        case(&["php", "ini", "8.3"], E("not_found")),
        // sites
        case(&["site", "list"], OK),
        case(&["site", "add", "{WWW}/demo", "--host", "demo.test"], OK),
        case(&["site", "info", "demo.test"], OK),
        // M2-E3-T11: the folder has no .env (an empty list).
        case(&["site", "env", "demo.test"], OK),
        case(&["site", "env", "demo.test", "--reveal"], OK),
        case(&["site", "env", "nosuch.test"], E("not_found")),
        case(&["site", "php", "demo.test", "8.4"], E("invalid_input")),
        case(&["site", "secure", "demo.test"], OK),
        case(&["site", "unsecure", "demo.test"], OK),
        case(&["site", "server", "demo.test", "apache"], OK),
        // Forcing laravel moves the root to demo/public; auto brings it back.
        case(&["site", "driver", "demo.test", "laravel"], OK),
        case(&["site", "driver", "demo.test", "auto"], OK),
        case(&["site", "alias", "demo.test", "add", "www.demo.test"], OK),
        // Plain `site open` would launch a real browser; the editor target stays not_implemented.
        case(&["site", "open", "demo.test", "--editor"], NI),
        // The metrics contract (ADR-0044): pool mode and the metrics switch are M2-E5-T11.
        case(&["site", "pool", "demo.test", "dedicated"], NI),
        case(&["site", "metrics", "demo.test", "off"], NI),
        case(
            &["site", "pool", "demo.test", "private"],
            E("invalid_input"),
        ),
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
        case(&["db", "list"], OK),
        case(&["db", "create", "loja"], E("not_found")),
        case(&["db", "drop", "loja"], E("invalid_input")),
        case(&["db", "drop", "loja", "--yes"], E("not_found")),
        case(
            &["db", "export", "loja", "-o", "{TMP}/loja.sql"],
            E("not_found"),
        ),
        case(&["db", "import", "loja", "{TMP}/loja.sql"], E("not_found")),
        case(&["db", "shell", "mysql/default"], E("not_found")),
        case(&["db", "url", "mysql/default", "loja"], E("not_found")),
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
        // Portable root: the OS store is never touched (M2-E3-T11).
        case(&["tls", "ca", "uninstall"], E("forbidden")),
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
        case(&["tls", "list"], OK),
        case(&["tls", "ca", "export"], OK),
        case(&["tls", "renew", "--all"], OK),
        case(&["tls", "export", "cert.test"], OK),
        case(&["lan", "expose", "nope.test"], NI),
        case(&["lan", "unexpose", "nope.test"], NI),
        case(&["lan", "qr", "nope.test"], NI),
        // mail (no tool declares spec.mail in the test root: not_found with the install hint)
        case(&["mail", "status"], E("not_found")),
        case(&["mail", "open"], E("not_found")),
        case(&["mail", "list", "--unread"], E("not_found")),
        case(
            &["mail", "list", "--search", "pedido", "--offset", "10"],
            E("not_found"),
        ),
        case(&["mail", "show", "abc"], E("not_found")),
        case(&["mail", "show", "abc", "--keep-unread"], E("not_found")),
        case(
            &["mail", "show", "--attachment", "2", "abc"],
            E("not_found"),
        ),
        case(&["mail", "read", "abc", "def"], E("not_found")),
        case(&["mail", "read", "abc", "--unread"], E("not_found")),
        case(&["mail", "delete", "abc"], E("invalid_input")),
        case(&["mail", "delete", "abc", "--yes"], E("not_found")),
        case(&["mail", "clear"], E("invalid_input")),
        case(&["mail", "clear", "--yes"], E("not_found")),
        case(&["mail", "env"], E("not_found")),
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
        // stats / remote. ADR-0044: windows and exact ranges (M2-E5-T8, real; the root is
        // portable, so the samples say metrics are off), series and routes (M2-E5-T9).
        case(&["site", "add", "{WWW}/demo", "--host", "stats.test"], OK),
        case(&["stats"], OK),
        case(&["stats", "stats.test", "--window", "1h"], OK),
        case(&["stats", "stats.test", "--from", "2h", "--to", "1h"], OK),
        case(&["stats", "nope.test", "--window", "5m"], E("not_found")),
        case(&["stats", "--from", "1h", "--to", "2h"], E("invalid_input")),
        case(&["stats", "--series", "demo.test", "--window", "24h"], NI),
        case(&["stats", "--series", "--window", "1h"], E("invalid_input")),
        case(&["stats", "--routes", "demo.test"], NI),
        case(
            &["stats", "--routes", "demo.test", "--watch"],
            E("invalid_input"),
        ),
        case(
            &["stats", "export", "demo.test", "--from", "24h", "-o", "csv"],
            NI,
        ),
        case(&["site", "remove", "stats.test", "--yes"], OK),
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
        case(&["config", "list"], OK),
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

// ---------------------------------------------------------------- db with a fake client

/// Compiles `crates/forge-daemon/tests/fixtures/<relative>` (a single file or the `main.rs`
/// of a fixture folder) with `rustc` into this test's target tmp folder.
fn compile_daemon_fixture(name: &str, relative: &str) -> std::path::PathBuf {
    use std::sync::{Mutex, OnceLock};
    static BUILT: OnceLock<Mutex<std::collections::BTreeMap<String, std::path::PathBuf>>> =
        OnceLock::new();
    let mut built = BUILT.get_or_init(Default::default).lock().unwrap();
    built
        .entry(name.to_owned())
        .or_insert_with(|| {
            let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli_fixtures");
            std::fs::create_dir_all(&dir).unwrap();
            let exe = dir.join(format!("{name}.exe"));
            let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../forge-daemon/tests/fixtures")
                .join(relative);
            let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
            let output = std::process::Command::new(rustc)
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

/// `forge db list|create|drop|url|shell` against a real `forgeboxd` whose `mysql/default`
/// is the fake `mysqld` and whose client is the fake of `fixtures/fake_db_client/`:
/// `--json` shapes, human output, the confirmation of `drop`, and `db shell` running the
/// client attached to the console and exiting with its code.
#[test]
fn db_commands_drive_the_manifest_client() {
    let root = TestRoot::new(true);
    let bin = root.path.join("bin/mysql/8.4.13/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let mysqld = compile_daemon_fixture("fake_mysqld", "fake_mysqld.rs");
    std::fs::copy(&mysqld, bin.join("mysqld.exe")).unwrap();
    std::fs::copy(&mysqld, bin.join("mysqladmin.exe")).unwrap();
    std::fs::copy(
        compile_daemon_fixture("fake_db_client", "fake_db_client/main.rs"),
        bin.join("mysql.exe"),
    )
    .unwrap();
    let _daemon = start_daemon(&root);
    struct StopOnDrop<'a>(&'a TestRoot);
    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.stop_daemon();
        }
    }
    let _stop = StopOnDrop(&root);

    // The instance autostarts; `db list` reports it before and after it runs.
    let started = std::time::Instant::now();
    let list = loop {
        let list = root.json(&["db", "list"]);
        if list["instances"][0]["state"] == "running" {
            break list;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "mysql never ran: {list}"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    assert_eq!(list["databases"], serde_json::json!([]));
    let instance = &list["instances"][0];
    assert_eq!(instance["instance"], "mysql/default");
    assert_eq!(instance["protocol"], "mysql");
    assert_eq!(instance["version"], "8.4.13");
    assert_eq!(instance["capabilities"]["shell"], true);
    let port = instance["port"].as_u64().expect("port");

    // create: JSON shape and human text.
    let made = root.json(&["db", "create", "loja"]);
    assert_eq!(made["name"], "loja");
    assert_eq!(made["instance"], "mysql/default");
    assert_eq!(made["charset"], "utf8mb4");
    let human = root.forge().env("FORGE_LANG", "en").run(&[
        "db",
        "create",
        "Loja",
        "--charset",
        "latin1",
        "--on",
        "mysql",
    ]);
    human.assert_success();
    assert!(
        human.stdout.contains("Loja") || human.stderr.contains("Loja"),
        "{human:#?}"
    );
    let bad = root.run(&["--json", "db", "create", "a;b"]);
    assert_eq!(bad.code, 2, "{bad:#?}");
    assert_eq!(bad.error_code(), "invalid_input");

    let list = root.json(&["db", "list", "mysql/default"]);
    let names: Vec<_> = list["databases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["Loja", "loja"],
        "case-sensitive, system databases hidden"
    );
    let human = root.forge().env("FORGE_LANG", "en").run(&["db", "list"]);
    human.assert_success();
    assert!(
        human.stdout.contains("loja") && human.stdout.contains("mysql/default"),
        "{human:#?}"
    );

    // url.
    let url = root.json(&["db", "url", "mysql/default", "loja"]);
    assert_eq!(url["url"], format!("mysql://root@127.0.0.1:{port}/loja"));
    assert_eq!(url["env"]["vars"]["DB_DATABASE"], "loja");
    assert_eq!(url["env"]["vars"]["DB_PORT"], port.to_string());

    // drop asks for confirmation (`--yes` skips it; a script without it fails).
    let refused = root.run(&["--json", "db", "drop", "loja"]);
    assert_eq!(refused.code, 2, "{refused:#?}");
    assert_eq!(refused.error_code(), "invalid_input");
    let dropped = root.json(&["db", "drop", "loja", "--yes"]);
    assert_eq!(dropped["ok"], true);
    let gone = root.run(&["--json", "db", "drop", "loja", "--yes"]);
    assert_eq!(
        gone.code,
        forge_ipc::base::error::exit_code_for("not_found")
    );

    // shell: --json prints the command; otherwise the fake runs attached, exit code kept.
    let command = root.json(&["db", "shell", "mysql/default", "Loja"]);
    assert!(
        command["program"]
            .as_str()
            .unwrap()
            .ends_with(r"bin\mysql.exe"),
        "{command}"
    );
    assert_eq!(command["args"].as_array().unwrap().last().unwrap(), "Loja");
    let attached = root.run(&["db", "shell", "mysql/default", "Loja"]);
    attached.assert_success();
    assert!(
        attached.stdout.contains(&format!(
            "fake-shell --protocol=tcp --host=127.0.0.1 --port={port} --user=root Loja"
        )),
        "{attached:#?}"
    );
    std::fs::write(bin.join("shell-exit.txt"), "7").unwrap();
    let failing = root.run(&["db", "shell", "mysql/default"]);
    assert_eq!(failing.code, 7, "{failing:#?}");
    assert!(failing.stdout.contains("fake-shell"), "{failing:#?}");
}

// ---------------------------------------------------------------- mail with a fake Mailpit

/// `POST <path>` to the fake Mailpit's HTTP port (its test controls, no client needed).
fn fake_mailpit_post(http_port: u16, path: &str) {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", http_port)).unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
}

/// `forge mail ...` against a real `forgeboxd` whose `mailpit/default` is the fake of
/// `fixtures/fake_mailpit.rs`: the `--json` shapes of every subcommand, an attachment
/// written with `-o` (exact bytes) and refused without it, and one human-mode table.
#[test]
fn mail_commands_drive_the_fake_mailpit() {
    let root = TestRoot::new(true);
    let toml_path = root.path.join("etc").join("forgebox.toml");
    let toml = std::fs::read_to_string(&toml_path).unwrap();
    // The file ends inside `[ports.ranges]`.
    let (smtp_range, http_range) = (free_port(), free_port());
    let toml = format!("{toml}mail = [{smtp_range}, {http_range}]\n");
    std::fs::write(&toml_path, toml).unwrap();
    let dir = root.path.join("bin/mailpit/1.32.0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(
        compile_daemon_fixture("fake_mailpit", "fake_mailpit.rs"),
        dir.join("mailpit.exe"),
    )
    .unwrap();
    std::fs::write(dir.join("fake-mode.txt"), "polite").unwrap();
    let _daemon = start_daemon(&root);
    struct StopOnDrop<'a>(&'a TestRoot);
    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.stop_daemon();
        }
    }
    let _stop = StopOnDrop(&root);

    let started = std::time::Instant::now();
    let status = loop {
        let status = root.json(&["mail", "status"]);
        if status["running"] == true && status["messages"].is_u64() {
            break status;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "mailpit never ran: {status}"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    let smtp = status["smtp_port"].as_u64().expect("smtp_port");
    assert_eq!(status["messages"], 0);
    assert_eq!(status["unread"], 0);
    let ui_url = status["ui_url"].as_str().expect("ui_url");
    let http_port: u16 = ui_url
        .rsplit(':')
        .next()
        .unwrap()
        .trim_end_matches('/')
        .parse()
        .unwrap();

    fake_mailpit_post(http_port, "/fake/inject?count=2&subject=Order");
    fake_mailpit_post(
        http_port,
        "/fake/inject?count=1&subject=Invoice&attachment=2048",
    );
    let status = root.json(&["mail", "status"]);
    assert_eq!(
        (status["messages"].as_u64(), status["unread"].as_u64()),
        (Some(3), Some(3))
    );

    // list: newest first, paging, --unread and --search.
    let list = root.json(&["mail", "list"]);
    assert_eq!(
        (list["total"].as_u64(), list["unread"].as_u64()),
        (Some(3), Some(3))
    );
    let first = &list["messages"][0];
    assert_eq!(first["id"], "msg3");
    assert_eq!(first["subject"], "Invoice 3");
    assert_eq!(first["from"], "Loja <loja@example.test>");
    assert_eq!(first["read"], false);
    assert_eq!(first["attachments"], 1);
    assert_eq!(first["to"], serde_json::json!(["ana@example.test"]));
    let page = root.json(&["mail", "list", "--limit", "1", "--offset", "1"]);
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    assert_eq!(page["messages"][0]["id"], "msg2");
    assert_eq!(page["total"], 3);
    let found = root.json(&["mail", "list", "--search", "order"]);
    assert_eq!(found["total"], 2);
    assert_eq!(found["messages"].as_array().unwrap().len(), 2);

    // show keeps or marks; read toggles.
    let kept = root.json(&["mail", "show", "msg1", "--keep-unread"]);
    assert_eq!(kept["summary"]["id"], "msg1");
    assert_eq!(kept["text"], "body of msg1");
    assert_eq!(root.json(&["mail", "status"])["unread"], 3);
    let shown = root.json(&["mail", "show", "msg1"]);
    assert_eq!(shown["summary"]["subject"], "Order 1");
    assert_eq!(root.json(&["mail", "status"])["unread"], 2);
    assert_eq!(root.json(&["mail", "read", "msg2"])["ok"], true);
    assert_eq!(root.json(&["mail", "status"])["unread"], 1);
    assert_eq!(root.json(&["mail", "list", "--unread"])["total"], 1);
    assert_eq!(
        root.json(&["mail", "read", "msg1", "msg2", "--unread"])["ok"],
        true
    );
    assert_eq!(root.json(&["mail", "status"])["unread"], 3);

    // attachments: -o writes the exact bytes; without -o nothing is written.
    let detail = root.json(&["mail", "show", "msg3", "--keep-unread"]);
    let part = &detail["attachments"][0];
    assert_eq!(part["part_id"], "1");
    assert_eq!(part["file_name"], "a.bin");
    assert_eq!(part["size_bytes"], 2048);
    let out = root.path.join("a.bin");
    let saved = root.json(&[
        "mail",
        "show",
        "msg3",
        "--attachment",
        "1",
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(saved["filename"], "a.bin");
    assert_eq!(saved["bytes"], 2048);
    assert_eq!(saved["path"], out.display().to_string());
    assert_eq!(std::fs::read(&out).unwrap(), vec![b'x'; 2048]);
    std::fs::remove_file(&out).unwrap();
    let human =
        root.forge()
            .env("FORGE_LANG", "en")
            .run(&["mail", "show", "msg3", "--attachment", "1"]);
    human.assert_success();
    assert!(
        human.stdout.contains("a.bin is binary: use -o <file>"),
        "{human:#?}"
    );
    assert!(
        !out.exists(),
        "a binary attachment is never written without -o"
    );
    assert!(!root.path.join("a.bin").exists());

    // one human-mode table.
    let human = root.forge().env("FORGE_LANG", "en").run(&["mail", "list"]);
    human.assert_success();
    assert!(
        human.stdout.contains("Invoice 3") && human.stdout.contains("msg1"),
        "{human:#?}"
    );

    // env.
    let env = root.json(&["mail", "env"]);
    assert_eq!(env["vars"]["MAIL_PORT"], smtp.to_string());

    // delete asks for confirmation; clear too.
    let refused = root.run(&["--json", "mail", "delete", "msg1"]);
    assert_eq!(refused.code, 2, "{refused:#?}");
    assert_eq!(refused.error_code(), "invalid_input");
    assert_eq!(root.json(&["mail", "delete", "msg1", "--yes"])["ok"], true);
    assert_eq!(root.json(&["mail", "status"])["messages"], 2);
    let refused = root.run(&["--json", "mail", "clear"]);
    assert_eq!(refused.code, 2, "{refused:#?}");
    assert_eq!(root.json(&["mail", "clear", "--yes"])["ok"], true);
    let status = root.json(&["mail", "status"]);
    assert_eq!(
        (status["messages"].as_u64(), status["unread"].as_u64()),
        (Some(0), Some(0))
    );
    let empty = root.forge().env("FORGE_LANG", "en").run(&["mail", "list"]);
    empty.assert_success();
}
