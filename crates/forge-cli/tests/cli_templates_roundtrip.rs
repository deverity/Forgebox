//! Round trip of the `cli_template` column (ADR-0037): every command string of the shared
//! golden file (`forge-ipc/tests/fixtures/cli_templates.json`) must parse with the real
//! clap tree of `forge` and map back to the method that produced it, with every value
//! bound to an argument.

use std::collections::BTreeSet;
use std::path::Path;

use clap::{ArgMatches, CommandFactory, Parser};
use forge_cli::cli::Cli;
use forge_ipc::api::cli_template::split_command_line;
use forge_ipc::api::{METHODS, MethodInfo};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Golden {
    method: String,
    expected: Option<String>,
}

fn golden() -> Vec<Golden> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../forge-ipc/tests/fixtures/cli_templates.json");
    let text = std::fs::read_to_string(&path).expect("golden file");
    serde_json::from_str(&text).expect("golden file is valid")
}

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
/// `forge jobs watch` is served by both `jobs.get` and `events.subscribe`.
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

#[test]
fn every_golden_command_parses_and_maps_back_to_its_method() {
    let mut checked = 0;
    let mut methods = BTreeSet::new();
    for entry in golden() {
        let Some(command) = entry.expected else {
            continue;
        };
        let args = split_command_line(&command);
        assert_eq!(args[0], "forge", "{command}");
        let found = methods_of(&args);
        assert!(
            found.contains(entry.method.as_str()),
            "`{command}` maps to {found:?} instead of {}",
            entry.method
        );
        // Only the documented overlap may be ambiguous.
        assert!(
            found.len() == 1 || found == BTreeSet::from(["events.subscribe", "jobs.get"]),
            "`{command}` is ambiguous: {found:?}"
        );
        methods.insert(entry.method);
        checked += 1;
    }
    assert!(checked > 100, "only {checked} commands checked");

    // Every method with a template is represented.
    for method in METHODS.iter().filter(|m| m.cli_template.is_some()) {
        assert!(
            methods.contains(method.name),
            "{} not round-tripped",
            method.name
        );
    }
}

#[test]
fn quoted_paths_reach_clap_as_one_value() {
    let args =
        split_command_line(r#"forge site add "C:\Meus Projetos\loja virtual" --host loja.test"#);
    let cli = Cli::try_parse_from(&args).unwrap();
    match cli.command {
        forge_cli::cli::Command::Site(forge_cli::cli::SiteCmd::Add { dir, host, .. }) => {
            assert_eq!(dir, Path::new(r"C:\Meus Projetos\loja virtual"));
            assert_eq!(host.as_deref(), Some("loja.test"));
        }
        other => panic!("{other:?}"),
    }
}
