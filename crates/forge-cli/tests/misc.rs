//! Streams, client-side commands (`exec`, `env`, `completions`, `remote`) and the case
//! parity commands, end to end.

mod common;

use std::time::Duration;

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
}

// --- streams --------------------------------------------------------------------------

#[test]
fn logs_follow_streams_json_lines_until_killed() {
    let fx = Fixture::new();
    let mut stream = fx
        .root
        .forge()
        .spawn(&["--json", "logs", "daemon", "-f", "-n", "5"]);
    let line = stream
        .next_line(Duration::from_secs(15))
        .expect("a line of the daemon log");
    let doc: serde_json::Value = serde_json::from_str(&line).expect("a JSON line");
    assert!(doc["text"].is_string() && doc["seq"].is_u64(), "{doc}");
    assert!(
        stream.kill(),
        "the follower keeps running until it is stopped"
    );
}

#[test]
fn status_watch_prints_snapshots_and_keeps_running() {
    let fx = Fixture::new();
    let mut stream = fx.root.forge().spawn(&["--json", "status", "--watch"]);
    let line = stream
        .next_line(Duration::from_secs(15))
        .expect("first snapshot");
    assert_eq!(line, "{\"instances\":[],\"pools\":[]}");
    assert!(
        stream.next_line(Duration::from_millis(500)).is_none(),
        "nothing changed"
    );
    assert!(stream.kill());
}

#[test]
fn stats_watch_says_it_is_not_implemented_instead_of_hanging() {
    let fx = Fixture::new();
    let run = fx.root.run(&["--json", "stats", "--watch"]);
    assert_eq!(run.code, 1, "{run:?}");
    assert_eq!(run.error_code(), "not_implemented");
}

#[test]
fn jobs_are_listed_and_watched() {
    let fx = Fixture::new();
    // A job that ends: refreshing the registry.
    fx.root.json(&["registry", "update"]);
    let jobs = fx.root.json(&["jobs"]);
    let job = jobs["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["name"] == "registry.update")
        .unwrap_or_else(|| panic!("registry.update not in {jobs}"));
    assert_eq!(job["state"], "done");
    let id = job["id"].as_str().unwrap();
    let watched = fx.root.run(&["--json", "jobs", "watch", id]);
    watched.assert_success();
    assert!(watched.json()["registries"].is_array(), "{watched:?}");
    let human = fx.root.run(&["jobs"]);
    human.assert_success();
    assert!(
        human.stdout.contains("registry.update") && human.stdout.contains("concluído"),
        "{human:?}"
    );
    let active = fx.root.json(&["jobs", "--active"]);
    assert_eq!(active["jobs"].as_array().unwrap().len(), 0);
    let cancel = fx.root.json(&["jobs", "cancel", id]);
    assert_eq!(cancel["was_running"], false);
    // A job id that does not exist.
    let missing = fx
        .root
        .run(&["--json", "jobs", "watch", "01J0000000000000000000ZZZZ"]);
    assert_eq!(missing.code, 3, "{missing:?}");
}

// --- exec / env -----------------------------------------------------------------------

#[test]
fn exec_passes_the_exit_code_and_the_output_of_the_child() {
    let fx = Fixture::new();
    let run = fx.root.run(&["exec", "--", "cmd", "/c", "exit", "7"]);
    assert_eq!(run.code, 7, "{run:?}");
    assert_eq!(
        run.stderr, "",
        "a child failure is not a forge error: {run:?}"
    );
    let hello = fx
        .root
        .run(&["exec", "--", "cmd", "/c", "echo", "hello forge"]);
    hello.assert_success();
    assert!(hello.stdout.contains("hello forge"), "{hello:?}");
    // `--php` with no PHP installed is not an error of exec itself.
    let php = fx
        .root
        .run(&["exec", "--php", "8.2", "--", "cmd", "/c", "exit", "0"]);
    php.assert_success();
    // Flags after `--` belong to the child.
    let flags = fx
        .root
        .run(&["exec", "--", "cmd", "/c", "echo", "--json", "-y"]);
    flags.assert_success();
    assert!(flags.stdout.contains("--json -y"), "{flags:?}");
}

#[test]
fn env_prints_scripts_for_each_shell() {
    let fx = Fixture::new();
    let ps = fx.root.run(&["env"]);
    ps.assert_success();
    assert!(ps.stdout.starts_with("$env:PATH = "), "{ps:?}");
    let ps_flag = fx.root.run(&["env", "--powershell"]);
    assert_eq!(ps_flag.stdout, ps.stdout);
    let cmd = fx.root.run(&["env", "--cmd"]);
    assert!(cmd.stdout.starts_with("set \"PATH="), "{cmd:?}");
    let bash = fx.root.run(&["env", "--bash"]);
    assert!(bash.stdout.starts_with("export PATH=\""), "{bash:?}");
    let json = fx.root.json(&["env"]);
    assert!(json["path_prepend"].is_array(), "{json}");
    assert!(json["script"].is_null());
    // Commands that only exist as shims (`composer`) come last.
    let last = json["path_prepend"].as_array().unwrap().last().unwrap();
    assert!(last.as_str().unwrap().ends_with(r"bin\shims"), "{json}");
    assert!(json["vars"].is_object(), "{json}");
    // Exclusive flags.
    let both = fx.root.run(&["env", "--cmd", "--bash"]);
    assert_eq!(both.code, 2, "{both:?}");
}

// --- completions ----------------------------------------------------------------------

#[test]
fn completions_cover_the_shells_and_need_no_daemon() {
    let root = TestRoot::new(true);
    let powershell = root.run(&["completions", "powershell"]);
    powershell.assert_success();
    assert!(
        powershell.stdout.contains("Register-ArgumentCompleter"),
        "{}",
        &powershell.stdout[..200.min(powershell.stdout.len())]
    );
    assert!(powershell.stdout.contains("'forge'"));
    // The script knows the commands of docs/09.
    for word in [
        "daemon",
        "doctor",
        "lint-case",
        "case-sensitive",
        "completions",
    ] {
        assert!(
            powershell.stdout.contains(word),
            "{word} missing from the PowerShell script"
        );
    }
    let bash = root.run(&["completions", "bash"]);
    bash.assert_success();
    assert!(bash.stdout.contains("_forge"));
    let zsh = root.run(&["completions", "zsh"]);
    zsh.assert_success();
    assert!(
        zsh.stdout.starts_with("#compdef forge"),
        "{}",
        &zsh.stdout[..80.min(zsh.stdout.len())]
    );
    let bad = root.run(&["completions", "tcsh"]);
    assert_eq!(bad.code, 2);
    // No command above started a daemon.
    assert!(root.daemon_pid().is_none());
}

// --- remote profiles ------------------------------------------------------------------

#[test]
fn remote_profiles_live_on_the_client_and_select_the_context() {
    let root = TestRoot::new(true);
    // No daemon is needed (or started) to manage profiles.
    let add = root.run(&[
        "remote",
        "add",
        "hml",
        "wss://10.0.0.5:7788",
        "--code",
        "1234-5678",
    ]);
    add.assert_success();
    assert!(add.stdout.contains("Perfil hml guardado"), "{add:?}");
    assert!(
        add.stderr.contains("M6-E3"),
        "the pairing code is not stored yet: {add:?}"
    );
    let file = std::fs::read_to_string(root.config.path().join("remotes.toml")).unwrap();
    assert!(file.contains("wss://10.0.0.5:7788"), "{file}");
    assert!(
        !file.contains("1234-5678"),
        "the pairing code is single use: {file}"
    );

    let ssh = root.run(&["remote", "add", "vps", "ssh://dev@host.example"]);
    ssh.assert_success();
    let bad = root.run(&["remote", "add", "x", "http://nope"]);
    assert_eq!(bad.code, 2, "{bad:?}");
    let bad_name = root.run(&["remote", "add", "Bad Name", "wss://h:1"]);
    assert_eq!(bad_name.code, 2, "{bad_name:?}");

    let list = root.json(&["remote", "list"]);
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["local", "hml", "vps"]);
    assert_eq!(list[0]["current"], true);

    root.run(&["remote", "use", "hml"]).assert_success();
    let list = root.json(&["remote", "list"]);
    assert_eq!(list[1]["current"], true);
    assert_eq!(list[0]["current"], false);

    // The selected remote profile is used by every daemon command: remote mode is M6-E3,
    // so they say so (and do not talk to the local daemon behind the user's back).
    let status = root.run(&["--json", "status"]);
    assert_eq!(status.code, 1, "{status:?}");
    assert_eq!(status.error_code(), "not_implemented");
    assert!(
        status.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("M6-E3")
    );
    assert!(root.daemon_pid().is_none(), "no local daemon was started");
    // `--context local` overrides the selection.
    let local = root.run(&["--context", "local", "--no-daemon", "--json", "status"]);
    assert_eq!(local.error_code(), "io", "{local:?}");
    // Unknown profile.
    let unknown = root.run(&["--context", "nope", "--json", "status"]);
    assert_eq!(unknown.code, 3, "{unknown:?}");

    root.run(&["remote", "remove", "hml"]).assert_success();
    let list = root.json(&["remote", "list"]);
    assert_eq!(
        list[0]["current"], true,
        "removing the selected profile goes back to local"
    );
    assert_eq!(root.run(&["remote", "remove", "hml"]).code, 3);
    assert_eq!(root.run(&["remote", "use", "hml"]).code, 3);
    root.run(&["remote", "use", "local"]).assert_success();
}

// --- case parity ----------------------------------------------------------------------

#[test]
fn fs_case_sensitive_status_and_convert() {
    let fx = Fixture::new();
    let dir = fx.root.www("cased");
    let status = fx
        .root
        .json(&["fs", "case-sensitive", "status", dir.to_str().unwrap()]);
    assert_eq!(status["supported"], true, "{status}");
    let human = fx
        .root
        .run(&["fs", "case-sensitive", "status", dir.to_str().unwrap()]);
    human.assert_success();
    assert!(human.stdout.contains("case-"), "{human:?}");
    // Outside the root and the parked folders: refused with a stable code.
    let outside = fx
        .root
        .run(&["--json", "fs", "case-sensitive", "status", r"C:\Windows"]);
    assert_eq!(outside.code, 1, "{outside:?}");
    assert_eq!(outside.error_code(), "forbidden");
}

#[test]
fn lint_case_lists_mismatches_and_exits_1() {
    let fx = Fixture::new();
    let dir = fx.root.www("shop");
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib").join("foo.php"), "<?php\n").unwrap();
    std::fs::write(
        dir.join("index.php"),
        "<?php\nrequire __DIR__ . '/Lib/Foo.php';\n",
    )
    .unwrap();
    fx.root
        .run(&["site", "add", dir.to_str().unwrap(), "--host", "shop.test"])
        .assert_success();

    let json = fx.root.run(&["--json", "site", "lint-case", "shop.test"]);
    let report = json.json();
    let issues = report["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1, "{report}");
    assert_eq!(json.code, 1, "issues left: exit 1");
    assert_eq!(issues[0]["category"], "mismatch");

    let human = fx.root.run(&["site", "lint-case", "shop.test"]);
    assert_eq!(human.code, 1, "{human:?}");
    assert!(human.stdout.contains("index.php:2"), "{human:?}");
    assert!(human.stdout.contains("Lib/Foo.php"), "{human:?}");
    assert!(human.stdout.contains("1 problema(s)"), "{human:?}");

    // --dry-run changes nothing.
    let dry = fx
        .root
        .run(&["site", "lint-case", "shop.test", "--fix", "--dry-run"]);
    assert_eq!(dry.code, 1, "a dry run fixes nothing: {dry:?}");
    let text = std::fs::read_to_string(dir.join("index.php")).unwrap();
    assert!(text.contains("/Lib/Foo.php"), "{text}");

    // --fix repairs it and the exit code goes back to 0.
    let fix = fx.root.run(&["site", "lint-case", "shop.test", "--fix"]);
    fix.assert_success();
    assert!(fix.stdout.contains("corrigido"), "{fix:?}");
    let clean = fx.root.run(&["site", "lint-case", "shop.test"]);
    clean.assert_success();
    assert!(clean.stdout.contains("0 problema(s)"), "{clean:?}");

    // --dry-run needs --fix.
    assert_eq!(
        fx.root
            .run(&["site", "lint-case", "shop.test", "--dry-run"])
            .code,
        2
    );
}
