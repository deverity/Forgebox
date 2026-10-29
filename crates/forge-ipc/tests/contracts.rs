//! Invariants of the method table against docs/09 and ADR-0009, and the snapshot of
//! the generated TypeScript (`cargo xtask gen-ts` must have been run and committed).

use std::collections::BTreeSet;
use std::path::Path;

use forge_ipc::api::contracts::{export_typescript, generated_files};
use forge_ipc::api::{METHODS, MethodKind, Role, method_info};

/// Every command of docs/09, as listed in `MethodInfo::cli`.
const DOCS_COMMANDS: &[&str] = &[
    // daemon
    "forge daemon status",
    "forge daemon stop",
    "forge daemon restart",
    "forge daemon logs",
    "forge daemon logs -f",
    "forge daemon doctor",
    "forge doctor",
    // tool / catalog
    "forge search",
    "forge tool list",
    "forge tool info",
    "forge tool install",
    "forge tool remove",
    "forge tool update",
    "forge tool update --check",
    "forge tool rollback",
    "forge tool policy",
    "forge tool add-local",
    "forge tool discover",
    "forge use",
    "forge which",
    // service / instance
    "forge start",
    "forge stop",
    "forge restart",
    "forge reload",
    "forge status",
    "forge status --watch",
    "forge service create",
    "forge service remove",
    "forge service config",
    "forge service set",
    "forge service ports",
    "forge service kill-port",
    "forge logs",
    "forge logs -f",
    "forge logs --list",
    // php
    "forge php list",
    "forge php pool status",
    "forge php ext list",
    "forge php ext enable",
    "forge php ext disable",
    "forge php xdebug",
    "forge php ini",
    "forge php ini --open",
    // site
    "forge site list",
    "forge site add",
    "forge site remove",
    "forge site php",
    "forge site secure",
    "forge site unsecure",
    "forge site server",
    "forge site alias",
    "forge site open",
    "forge site info",
    "forge site env",
    "forge park",
    "forge unpark",
    "forge parked",
    "forge link",
    "forge unlink",
    "forge proxy add",
    "forge proxy remove",
    "forge up",
    "forge down",
    "forge ps",
    "forge run",
    "forge share",
    "forge new",
    // db
    "forge db list",
    "forge db create",
    "forge db drop",
    "forge db export",
    "forge db import",
    "forge db shell",
    "forge db url",
    "forge db open",
    // net / tls
    "forge dns status",
    "forge dns enable",
    "forge dns disable",
    "forge dns test",
    "forge tls ca status",
    "forge tls ca install",
    "forge tls ca uninstall",
    "forge tls ca export",
    "forge tls ca rotate",
    "forge tls issue",
    "forge tls renew",
    "forge tls export",
    "forge lan expose",
    "forge lan unexpose",
    "forge lan qr",
    // mail
    "forge mail status",
    "forge mail open",
    "forge mail list",
    "forge mail show",
    "forge mail show --attachment",
    "forge mail read",
    "forge mail delete",
    "forge mail clear",
    "forge mail env",
    // shell / env
    "forge shell",
    "forge env",
    "forge exec",
    "forge path add",
    "forge path remove",
    "forge path show",
    // fs / Linux parity
    "forge fs case-sensitive status",
    "forge fs case-sensitive enable",
    "forge fs case-sensitive disable",
    "forge fs case-sensitive convert",
    "forge site case-sensitive",
    "forge site lint-case",
    // jobs
    "forge jobs",
    "forge jobs watch",
    "forge jobs cancel",
    // stats / remote
    "forge stats",
    "forge stats --watch",
    "forge top",
    "forge stats export",
    "forge remote pair",
    "forge remote listen",
    "forge remote clients",
    "forge remote revoke",
    // registry / system
    "forge registry list",
    "forge registry add",
    "forge registry remove",
    "forge registry update",
    "forge import laragon",
    "forge bundle export",
    "forge bundle import",
    "forge backup now",
    "forge backup schedule",
    "forge config list",
    "forge config get",
    "forge config set",
    "forge self-update",
];

/// Commands that run entirely in the client (no daemon method), see docs/09 §Mapeamento.
const CLIENT_SIDE: &[&str] = &[
    "forge daemon start",
    "forge completions",
    "forge mcp serve",
    "forge remote add",
    "forge remote list",
    "forge remote use",
    "forge remote remove",
];

/// Methods that act on the machine of the caller's OS session (open an editor, a
/// browser, a terminal, a GUI client; read a client path): `local_only` (ADR-0009, rule
/// 6). The set must match exactly, so a change is always deliberate.
const LOCAL_ONLY: &[&str] = &[
    "tools.add_local",
    "services.config",
    "services.kill_port_holder",
    "php.ini_open",
    "sites.open",
    "db.shell",
    "db.open",
    "mail.open",
    "shell.open",
    "shell.open_editor",
    "shell.env",
];

/// Methods that "open" something in the daemon, not on the caller's machine: allowed on
/// remote connections (a terminal session runs on the daemon's machine, ADR-0040).
const OPENS_ON_THE_DAEMON: &[&str] = &["terminal.open"];

/// Groups of docs/09 (plus the stream groups `logs`, `metrics`, `events`).
const GROUPS: &[&str] = &[
    "daemon",
    "tools",
    "services",
    "logs",
    "php",
    "sites",
    "db",
    "net",
    "tls",
    "mail",
    "shell",
    "terminal",
    "fs_parity",
    "jobs",
    "stats",
    "metrics",
    "remote",
    "registry",
    "system",
    "fs",
    "events",
];

fn cli_entries() -> BTreeSet<&'static str> {
    METHODS
        .iter()
        .flat_map(|m| m.cli.split(';').map(str::trim))
        .filter(|c| !c.is_empty())
        .collect()
}

#[test]
fn names_are_unique_and_well_formed() {
    let mut names = BTreeSet::new();
    for m in METHODS {
        assert!(names.insert(m.name), "duplicate {}", m.name);
        let (group, action) = m.name.split_once('.').expect("group.action");
        for part in [group, action] {
            assert!(
                !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{}",
                m.name
            );
        }
        assert_eq!(method_info(m.name), Some(m));
    }
    let groups: BTreeSet<&str> = METHODS.iter().map(|m| m.group()).collect();
    assert_eq!(groups, GROUPS.iter().copied().collect());
}

#[test]
fn every_docs_command_is_served() {
    let served = cli_entries();
    for command in DOCS_COMMANDS {
        assert!(served.contains(command), "{command} has no API method");
    }
    for entry in &served {
        assert!(entry.starts_with("forge "), "{entry}");
        assert!(
            DOCS_COMMANDS.contains(entry),
            "{entry} is not a docs/09 command (update DOCS_COMMANDS and docs/09)"
        );
    }
}

#[test]
fn every_command_word_of_docs_09_is_mapped() {
    let doc =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/09-cli.md"))
            .unwrap();
    let known: BTreeSet<&str> = DOCS_COMMANDS
        .iter()
        .chain(CLIENT_SIDE)
        .filter_map(|c| c.split_whitespace().nth(1))
        .collect();
    let mut in_code = false;
    for line in doc.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if !in_code {
            continue;
        }
        let line = line.trim();
        let Some(rest) = line.strip_prefix("forge ") else {
            continue;
        };
        let word = rest.split_whitespace().next().unwrap_or("");
        if word.starts_with('<') || word.starts_with('[') {
            continue; // the synopsis line
        }
        assert!(
            known.contains(word),
            "docs/09 command `forge {word}` is not mapped"
        );
    }
}

#[test]
fn os_dependent_methods_are_local_only() {
    for name in LOCAL_ONLY {
        let info = method_info(name).unwrap_or_else(|| panic!("{name} missing"));
        assert!(info.local_only, "{name} must be local_only");
    }
    let marked: BTreeSet<&str> = METHODS
        .iter()
        .filter(|m| m.local_only)
        .map(|m| m.name)
        .collect();
    assert_eq!(marked, LOCAL_ONLY.iter().copied().collect());
    // Anything that "opens" something on the caller's machine is local.
    for m in METHODS {
        if (m.action() == "open" || m.action().ends_with("_open"))
            && !OPENS_ON_THE_DAEMON.contains(&m.name)
        {
            assert!(m.local_only, "{}", m.name);
        }
    }
    // The commands named by M1-E5-T1: site open --editor, db open --with, service
    // config, shell.
    for command in [
        "forge site open",
        "forge db open",
        "forge service config",
        "forge shell",
    ] {
        let m = METHODS
            .iter()
            .find(|m| m.cli.split(';').any(|c| c.trim() == command))
            .unwrap();
        assert!(m.local_only, "{command} → {}", m.name);
    }
}

#[test]
fn kinds_flags_and_roles_are_consistent() {
    for m in METHODS {
        match m.kind {
            MethodKind::Call => {
                assert!(
                    !m.long_running && m.job_result.is_none() && m.item.is_none(),
                    "{}",
                    m.name
                );
            }
            MethodKind::Job => {
                assert!(m.long_running, "{}", m.name);
                assert_eq!(m.result, "JobDto", "{}", m.name);
                assert!(m.job_result.is_some(), "{}", m.name);
            }
            MethodKind::Stream => {
                assert_eq!(m.result, "SubscriptionDto", "{}", m.name);
                assert!(m.item.is_some() && !m.mutating, "{}", m.name);
                let (note, lagged) = (m.notification.unwrap(), m.lagged_notification.unwrap());
                assert!(note.starts_with(&format!("{}.", m.group())), "{note}");
                assert_eq!(lagged, format!("{}.lagged", m.group()));
                assert!(
                    method_info(&format!("{}.unsubscribe", m.group())).is_some(),
                    "{} needs an unsubscribe method",
                    m.name
                );
            }
        }
        if m.mutating {
            assert!(
                m.min_role >= Role::Operator,
                "viewers never change state: {}",
                m.name
            );
        }
    }
    // Installing/removing tools and touching secrets is admin only (docs/13 §3.3).
    for name in [
        "tools.install",
        "tools.remove",
        "tools.update",
        "tls.export",
        "fs.read",
        "fs.write",
        "remote.pair",
        "system.config_set",
    ] {
        assert_eq!(method_info(name).unwrap().min_role, Role::Admin, "{name}");
    }
}

#[test]
fn destructive_methods_require_confirm() {
    let ts = export_typescript().unwrap();
    for name in [
        "tools.remove",
        "services.remove",
        "services.kill_port_holder",
        "sites.remove",
        "db.drop",
        "mail.clear",
        "mail.delete",
        "tls.ca_rotate",
    ] {
        let params = method_info(name).unwrap().params;
        let start = ts
            .find(&format!("export type {params} = {{"))
            .unwrap_or_else(|| panic!("{params}"));
        let block = &ts[start..start + ts[start..].find("};").unwrap()];
        assert!(
            block.contains("confirm"),
            "{name} ({params}) needs `confirm`"
        );
    }
}

#[test]
fn generated_typescript_is_up_to_date() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (path, expected) in generated_files().unwrap() {
        let actual = std::fs::read_to_string(repo.join(path))
            .unwrap_or_else(|e| panic!("{path}: {e}; run `cargo xtask gen-ts`"));
        assert!(
            actual == expected,
            "{path} is stale: run `cargo xtask gen-ts` and commit the result"
        );
    }
}

/// Roles and flags of the methods added by the E3 contract (ADR-0040).
#[test]
fn e3_methods_have_the_decided_roles_and_flags() {
    let info = |name: &str| method_info(name).unwrap_or_else(|| panic!("{name} missing"));
    // Terminal sessions: admin only (a shell is full control of the machine), never
    // local_only (they run on the daemon's machine, like SSH), no CLI.
    let terminal: Vec<_> = METHODS.iter().filter(|m| m.group() == "terminal").collect();
    let names: BTreeSet<&str> = terminal.iter().map(|m| m.name).collect();
    assert_eq!(
        names,
        BTreeSet::from([
            "terminal.open",
            "terminal.list",
            "terminal.input",
            "terminal.resize",
            "terminal.close",
            "terminal.subscribe",
            "terminal.unsubscribe",
        ])
    );
    for m in &terminal {
        assert_eq!(m.min_role, Role::Admin, "{}", m.name);
        assert!(!m.local_only, "{}", m.name);
        assert!(m.cli.is_empty() && m.cli_template.is_none(), "{}", m.name);
    }
    // Keystrokes are deduplicated by input_seq, not cached by request_id.
    assert!(!info("terminal.input").mutating);
    assert!(!info("terminal.resize").mutating);
    assert!(info("terminal.open").mutating && info("terminal.close").mutating);
    let subscribe = info("terminal.subscribe");
    assert_eq!(subscribe.kind, MethodKind::Stream);
    assert_eq!(subscribe.notification, Some("terminal.output"));
    assert_eq!(subscribe.lagged_notification, Some("terminal.lagged"));
    assert_eq!(subscribe.item, Some("TerminalEnvelope"));

    let editor = info("shell.open_editor");
    assert_eq!(editor.min_role, Role::Operator);
    assert!(editor.local_only && !editor.mutating);
    assert!(editor.cli.is_empty() && editor.cli_template.is_none());

    let kill = info("services.kill_port_holder");
    assert_eq!(kill.min_role, Role::Admin);
    assert!(kill.local_only && kill.mutating);
    assert_eq!(kill.kind, MethodKind::Call);

    // sites.env: operators see masked values; `reveal` is checked against admin by the
    // handler (the table has one role per method).
    let env = info("sites.env");
    assert_eq!(env.min_role, Role::Operator);
    assert!(!env.local_only && !env.mutating);

    let uninstall = info("tls.ca_uninstall");
    assert_eq!(uninstall.min_role, Role::Admin);
    assert!(uninstall.mutating && uninstall.long_running);

    for (name, role, mutating) in [
        ("mail.mark_read", Role::Operator, true),
        ("mail.delete", Role::Operator, true),
        ("mail.attachment", Role::Viewer, false),
        ("logs.targets", Role::Viewer, false),
        ("system.config_list", Role::Viewer, false),
    ] {
        let m = info(name);
        assert_eq!((m.min_role, m.mutating), (role, mutating), "{name}");
        assert!(!m.local_only, "{name}");
    }
    assert_eq!(info("db.shell").result, "ShellCommandDto");
}
