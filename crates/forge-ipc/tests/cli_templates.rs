//! `cli_template` column of the method table (ADR-0037): coverage, the golden file shared
//! with the TypeScript renderer (`apps/desktop/src/ipc/cli.ts`, M2-E1-T13) and the
//! generated `methods.ts`.

use std::collections::BTreeSet;
use std::path::Path;

use forge_ipc::api::cli_template::{CliTemplate, quote_arg, split_command_line, validate};
use forge_ipc::api::{METHODS, method_info};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct Golden {
    method: String,
    params: Value,
    expected: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

fn golden() -> Vec<Golden> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cli_templates.json");
    let text = std::fs::read_to_string(&path).expect("golden file");
    serde_json::from_str(&text).expect("golden file is valid")
}

fn cli_entries(cli: &str) -> Vec<&str> {
    cli.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

#[test]
fn every_row_with_a_cli_has_a_valid_template() {
    for m in METHODS {
        let has_cli = !cli_entries(m.cli).is_empty();
        match (has_cli, m.cli_template) {
            (true, None) => panic!("{} serves `{}` but has no cli_template", m.name, m.cli),
            (false, Some(t)) => panic!("{} has no CLI but the template `{t}`", m.name),
            (true, Some(template)) => {
                validate(template).unwrap_or_else(|e| panic!("{}: {e}", m.name));
            }
            (false, None) => {}
        }
    }
}

#[test]
fn templates_start_with_the_commands_of_the_row() {
    // Every alternative begins with `forge` and the group word(s) of a served command.
    for m in METHODS {
        let Some(template) = m.cli_template else {
            continue;
        };
        let served: BTreeSet<String> = cli_entries(m.cli)
            .into_iter()
            .filter_map(|c| c.split_whitespace().nth(1).map(str::to_owned))
            .collect();
        for alternative in template.split(" | ") {
            let words: Vec<&str> = alternative
                .split_whitespace()
                .skip_while(|w| w.starts_with('['))
                .collect();
            assert_eq!(words[0], "forge", "{}: {alternative}", m.name);
            // `{secure?secure:unsecure}` style choices are the only non-literal second word.
            if !words[1].starts_with('{') {
                assert!(
                    served.contains(words[1]),
                    "{}: `{alternative}` does not start with a served command ({served:?})",
                    m.name
                );
            }
        }
    }
}

#[test]
fn render_cli_matches_the_golden_file() {
    let entries = golden();
    assert!(entries.len() > 100, "the golden file is the contract");
    let mut failures = Vec::new();
    for entry in &entries {
        let info = method_info(&entry.method)
            .unwrap_or_else(|| panic!("golden names an unknown method {}", entry.method));
        let got = info.render_cli(&entry.params);
        if got != entry.expected {
            failures.push(format!(
                "{} {} ({}):\n  expected {:?}\n  got      {:?}",
                entry.method,
                entry.params,
                entry.note.as_deref().unwrap_or("-"),
                entry.expected,
                got
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_golden_file_covers_every_template_and_the_required_cases() {
    let entries = golden();
    let covered: BTreeSet<&str> = entries.iter().map(|e| e.method.as_str()).collect();
    for m in METHODS.iter().filter(|m| m.cli_template.is_some()) {
        assert!(covered.contains(m.name), "{} has no golden entry", m.name);
    }
    let has = |pred: &dyn Fn(&Golden) -> bool| entries.iter().any(pred);
    // A path with spaces, a false bool, an empty list and a null optional (acceptance).
    assert!(has(&|e| e
        .expected
        .as_deref()
        .is_some_and(|c| c.contains("\"C:\\") && c.contains(' '))));
    assert!(has(&|e| e
        .params
        .as_object()
        .is_some_and(|o| o.values().any(|v| v == &Value::Bool(false)))));
    assert!(has(&|e| e.params.as_object().is_some_and(|o| o
        .values()
        .any(|v| v == &serde_json::json!([])))));
    assert!(has(&|e| e
        .params
        .as_object()
        .is_some_and(|o| o.values().any(Value::is_null))));
    // Every alternative of every template is hit at least once.
    for m in METHODS {
        let Some(template) = m.cli_template else {
            continue;
        };
        let parsed = CliTemplate::parse(template).unwrap();
        let hit: BTreeSet<usize> = entries
            .iter()
            .filter(|e| e.method == m.name)
            .filter_map(|e| parsed.selected(&e.params))
            .collect();
        let all: BTreeSet<usize> = (0..parsed.alternatives()).collect();
        assert_eq!(hit, all, "{}: golden entries miss an alternative", m.name);
    }
}

#[test]
fn expected_commands_split_into_the_values_that_were_quoted() {
    // The golden strings follow the quoting rule: re-splitting gives plain words.
    for e in golden() {
        let Some(expected) = &e.expected else {
            continue;
        };
        let args = split_command_line(expected);
        assert_eq!(args[0], "forge", "{expected}");
        for arg in &args {
            // quoting again is the identity on what splitting produced
            let again = quote_arg(arg).expect("argument is quotable");
            assert_eq!(
                split_command_line(&again),
                std::slice::from_ref(arg),
                "{expected}"
            );
        }
    }
}

#[test]
fn methods_ts_carries_the_templates() {
    let ts = forge_ipc::api::contracts::methods_ts();
    assert!(ts.contains("readonly cli_template: string | null;"));
    for m in METHODS {
        let line = ts
            .lines()
            .find(|l| {
                l.trim_start()
                    .starts_with(&format!("\"{}\": {{ kind:", m.name))
            })
            .unwrap_or_else(|| panic!("{} missing in methods.ts", m.name));
        let expected = match m.cli_template {
            Some(t) => format!("cli_template: {}", serde_json::to_string(t).unwrap()),
            None => "cli_template: null".to_owned(),
        };
        assert!(line.contains(&expected), "{}: {line}", m.name);
    }
}
