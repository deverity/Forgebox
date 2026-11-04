//! GUI/CLI parity gate, Rust half (M2-E4-T5, docs/07 §1.7, ADR-0037).
//!
//! `apps/desktop/qa/parity/gui-commands.json` lists, for every action of the GUI registry, the
//! `forge` commands it renders from sample inputs. Here each command is parsed with the real
//! clap tree and must map back to the method of its action. Coverage: every `METHODS` row with
//! a non-empty `cli` (mutating or not) must be reached by some action or be listed in `cli-only.yaml` with a
//! reason. `pending` rows are screens that M2-E2/E3/E5 have not added yet; they are tolerated
//! until the M2 gate runs with `FORGE_PARITY_STRICT=1` (M2-E4-T11), where the list must be empty.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use clap::{ArgMatches, CommandFactory, Parser};
use forge_cli::cli::Cli;
use forge_ipc::api::cli_template::split_command_line;
use forge_ipc::api::{METHODS, MethodInfo};
use serde::Deserialize;

/// A command served by a method: the words up to the first flag and the flags.
struct Served {
    path: Vec<String>,
    flags: Vec<String>,
}

fn served(method: &MethodInfo) -> Vec<Served> {
    method
        .cli
        .split(';')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(|command| {
            let words: Vec<&str> = command.split_whitespace().skip(1).collect();
            Served {
                path: words
                    .iter()
                    .take_while(|w| !w.starts_with('-'))
                    .map(|w| (*w).to_owned())
                    .collect(),
                flags: words
                    .iter()
                    .filter(|w| w.starts_with('-'))
                    .map(|w| (*w).to_owned())
                    .collect(),
            }
        })
        .collect()
}

/// Was the flag (`--check`, `-f`) given on the command line at `command`'s level?
fn flag_given(command: &clap::Command, matches: &ArgMatches, flag: &str) -> bool {
    let arg = command.get_arguments().find(|a| {
        a.get_long().is_some_and(|l| flag == format!("--{l}"))
            || a.get_short().is_some_and(|s| flag == format!("-{s}"))
    });
    arg.is_some_and(|a| {
        matches.value_source(a.get_id().as_str()) == Some(clap::parser::ValueSource::CommandLine)
    })
}

/// Parses `args` with the real tree and returns the methods it maps to: normally one;
/// `forge jobs watch` is served by both `jobs.get` and `events.subscribe`. Same mapping as
/// `cli_templates_roundtrip.rs`, which checks the golden file of the templates.
fn methods_of(args: &[String]) -> BTreeSet<&'static str> {
    Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?} does not parse: {e}"));

    let root = Cli::command();
    let matches = root
        .clone()
        .try_get_matches_from(args)
        .expect("parses with the command factory too");
    let (mut command, mut current) = (&root, &matches);
    let mut path: Vec<String> = Vec::new();
    while let Some((name, sub)) = current.subcommand() {
        path.push(name.to_owned());
        command = command.find_subcommand(name).expect("known subcommand");
        current = sub;
    }

    // Every value on the line is bound to an argument (not swallowed by the shell words).
    let mut bound: BTreeSet<String> = BTreeSet::new();
    for arg in command.get_arguments() {
        if let Some(values) = current.get_raw(arg.get_id().as_str()) {
            bound.extend(values.map(|v| v.to_string_lossy().into_owned()));
        }
    }
    let mut words = args.iter().skip(1).filter(|a| !a.starts_with('-'));
    for expected in &path {
        assert_eq!(words.next(), Some(expected), "{args:?}");
    }
    for value in words {
        assert!(bound.contains(value), "{value:?} of {args:?} is not bound");
    }

    // Candidates: methods serving this command path whose flags were all given. The most
    // specific one wins (`forge tool update --check` over `forge tool update`).
    let mut best: Vec<(&'static MethodInfo, usize)> = Vec::new();
    for method in METHODS {
        for served in served(method) {
            if served.path == path && served.flags.iter().all(|f| flag_given(command, current, f)) {
                best.push((method, served.flags.len()));
            }
        }
    }
    let top =
        best.iter().map(|(_, n)| *n).max().unwrap_or_else(|| {
            panic!("{args:?} (path {path:?}) is served by no method of the table")
        });
    best.iter()
        .filter(|(_, n)| *n == top)
        .map(|(m, _)| m.name)
        .collect()
}

// ---- the parity data ----

#[derive(Debug, Deserialize)]
struct Sample {
    command: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GuiAction {
    action: String,
    method: String,
    samples: Vec<Sample>,
}

#[derive(Debug, Deserialize)]
struct CliOnlyEntry {
    method: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct PendingEntry {
    method: String,
    /// The roadmap task whose screen adds the action.
    task: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CliOnly {
    cli_only: Vec<CliOnlyEntry>,
    /// Pure queries and streams the GUI shows as data; `reason` names the screen.
    #[serde(default)]
    query: Vec<CliOnlyEntry>,
    #[serde(default)]
    pending: Vec<PendingEntry>,
}

fn parity_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/qa/parity")
}

fn load_actions() -> Vec<GuiAction> {
    let text = std::fs::read_to_string(parity_dir().join("gui-commands.json")).expect("json");
    serde_json::from_str(&text).expect("gui-commands.json is valid")
}

/// Parses `cli-only.yaml`; every entry needs a non-empty reason (and a task for `pending`).
fn parse_cli_only(text: &str) -> Result<CliOnly, String> {
    let parsed: CliOnly = serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
    for e in parsed.cli_only.iter().chain(&parsed.query) {
        if e.reason.trim().is_empty() {
            return Err(format!("cli-only entry {} has no reason", e.method));
        }
    }
    for e in &parsed.pending {
        if e.task.trim().is_empty() {
            return Err(format!("pending entry {} has no task", e.method));
        }
    }
    Ok(parsed)
}

/// Same overlap rule as the round-trip test: `services.set` writes when pairs follow.
fn resolve(args: &[String], mut found: BTreeSet<&'static str>) -> BTreeSet<&'static str> {
    if found == BTreeSet::from(["services.set", "services.settings"]) {
        found = BTreeSet::from([if args.len() > 4 {
            "services.set"
        } else {
            "services.settings"
        }]);
    }
    found
}

/// Checks every command of `actions`; returns the methods they reach or the problems.
fn check_actions(actions: &[GuiAction]) -> Result<BTreeSet<&'static str>, Vec<String>> {
    let mut reached = BTreeSet::new();
    let mut problems = Vec::new();
    for action in actions {
        for command in action.samples.iter().filter_map(|s| s.command.as_deref()) {
            let args = split_command_line(command);
            // `methods_of` asserts on malformed lines; a panic here is a parity failure.
            let outcome = std::panic::catch_unwind(|| resolve(&args, methods_of(&args)));
            match outcome {
                Ok(found) if found.contains(action.method.as_str()) => reached.extend(found),
                Ok(found) => problems.push(format!(
                    "{}: `{command}` maps to {found:?}, not {}",
                    action.action, action.method
                )),
                Err(_) => problems.push(format!(
                    "{}: `{command}` does not parse or maps to no method",
                    action.action
                )),
            }
        }
    }
    if problems.is_empty() {
        Ok(reached)
    } else {
        Err(problems)
    }
}

/// Methods that must be reached or listed: every row with a non-empty `cli`, mutating or not
/// (queries go under `query:` in `cli-only.yaml`, with the screen that shows their data).
fn with_cli() -> BTreeSet<&'static str> {
    METHODS
        .iter()
        .filter(|m| !m.cli.trim().is_empty())
        .map(|m| m.name)
        .collect()
}

// ---- tests ----

#[test]
fn every_gui_command_parses_and_maps_to_its_method() {
    let actions = load_actions();
    assert!(actions.len() > 20, "only {} actions", actions.len());
    let reached = check_actions(&actions).unwrap_or_else(|p| panic!("{}", p.join("\n")));
    assert!(reached.len() > 15, "only {} methods reached", reached.len());
}

#[test]
fn a_fake_invalid_command_fails_the_gate() {
    let fixture = r#"[
      {"action": "fake.bad_verb", "method": "services.start",
       "samples": [{"command": "forge frobnicate everything"}]},
      {"action": "fake.wrong_method", "method": "services.stop",
       "samples": [{"command": "forge start mysql"}]},
      {"action": "fake.ok", "method": "services.start",
       "samples": [{"command": "forge start mysql"}, {"command": null}]}
    ]"#;
    let actions: Vec<GuiAction> = serde_json::from_str(fixture).unwrap();
    let problems = check_actions(&actions).unwrap_err();
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0].contains("fake.bad_verb"), "{problems:?}");
    assert!(problems[1].contains("fake.wrong_method"), "{problems:?}");
}

#[test]
fn a_cli_only_entry_without_a_reason_fails() {
    let no_reason = "cli_only:\n  - method: env.show\n";
    assert!(parse_cli_only(no_reason).is_err());
    let blank = "cli_only:\n  - method: env.show\n    reason: \"  \"\n";
    assert!(parse_cli_only(blank).unwrap_err().contains("no reason"));
    let ok = "cli_only:\n  - method: env.show\n    reason: shell only\n";
    assert_eq!(parse_cli_only(ok).unwrap().cli_only.len(), 1);
}

#[test]
fn every_cli_method_is_reached_by_an_action_or_listed() {
    let text = std::fs::read_to_string(parity_dir().join("cli-only.yaml")).expect("yaml");
    let config = parse_cli_only(&text).unwrap_or_else(|e| panic!("cli-only.yaml: {e}"));
    let reached = check_actions(&load_actions()).unwrap_or_else(|p| panic!("{}", p.join("\n")));
    let all = with_cli();

    let mut listed: BTreeMap<&str, &str> = BTreeMap::new();
    for e in &config.cli_only {
        assert!(
            all.contains(e.method.as_str()),
            "{} has no CLI row",
            e.method
        );
        assert!(
            listed.insert(&e.method, "cli_only").is_none(),
            "{} twice",
            e.method
        );
    }
    for e in &config.query {
        assert!(
            all.contains(e.method.as_str()),
            "{} has no CLI row",
            e.method
        );
        assert!(
            listed.insert(&e.method, "query").is_none(),
            "{} twice",
            e.method
        );
    }
    for e in &config.pending {
        assert!(
            all.contains(e.method.as_str()),
            "{} has no CLI row",
            e.method
        );
        assert!(
            listed.insert(&e.method, "pending").is_none(),
            "{} twice",
            e.method
        );
    }

    // A listed method that an action reaches is stale: the list must shrink with the GUI.
    let stale: Vec<_> = listed.keys().filter(|m| reached.contains(*m)).collect();
    assert!(
        stale.is_empty(),
        "reached by an action, remove from cli-only.yaml: {stale:?}"
    );

    let missing: Vec<_> = all
        .iter()
        .filter(|m| !reached.contains(*m) && !listed.contains_key(*m))
        .collect();
    assert!(
        missing.is_empty(),
        "METHODS rows with a CLI command that no action reaches and cli-only.yaml does not list: {missing:?}"
    );

    if std::env::var("FORGE_PARITY_STRICT").as_deref() == Ok("1") {
        let pending: Vec<_> = config.pending.iter().map(|p| &p.method).collect();
        assert!(
            pending.is_empty(),
            "M2 gate: pending coverage left: {pending:?}"
        );
    }
}
