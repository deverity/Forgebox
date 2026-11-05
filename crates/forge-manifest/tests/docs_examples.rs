//! The complete examples of docs/04 (code blocks starting with `apiVersion:`) must parse
//! and validate, so the spec and the schema cannot drift apart.

use std::path::PathBuf;

use forge_manifest::schema::common::Kind;
use forge_manifest::{load_str, validate};

fn yaml_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in markdown.lines() {
        match &mut current {
            None if line.trim_start() == "```yaml" => current = Some(String::new()),
            Some(block) if line.trim_start() == "```" => {
                blocks.push(std::mem::take(block));
                current = None;
            }
            Some(block) => {
                block.push_str(line);
                block.push('\n');
            }
            None => {}
        }
    }
    blocks
}

#[test]
fn docs_04_examples_are_valid_manifests() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/04-tool-manifest-spec.md");
    let markdown = std::fs::read_to_string(&path).unwrap();
    let zero_sha = "0".repeat(64);
    let mut kinds = Vec::new();
    for block in yaml_blocks(&markdown) {
        if !block.starts_with("apiVersion:") {
            continue; // fragments such as §1.2
        }
        // §5 shows placeholders for hashes.
        let text = block.replace("<64 hex>", &zero_sha);
        let manifest = load_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        let issues = validate(&manifest);
        assert!(issues.is_empty(), "{:#?}\n{text}", issues);
        kinds.push(manifest.kind());
    }
    assert_eq!(
        kinds,
        [
            Kind::Tool,
            Kind::Extension,
            Kind::Stack,
            Kind::Project,
            Kind::Registry
        ]
    );
}

#[test]
fn docs_04_daemon_fragment_is_a_valid_service_block() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/04-tool-manifest-spec.md");
    let markdown = std::fs::read_to_string(&path).unwrap();
    let fragment = yaml_blocks(&markdown)
        .into_iter()
        .find(|b| b.starts_with("service:\n  kind: daemon"))
        .expect("§1.2 fragment");
    let indented: String = fragment.lines().map(|l| format!("  {l}\n")).collect();
    let text = format!(
        "apiVersion: forgebox.dev/v1\nkind: Tool\n\
         metadata: {{ id: mysql, name: MySQL, category: database }}\n\
         spec:\n  platforms: [windows-x64]\n  source:\n    providers:\n      - type: mariadb\n\
         \x20 config:\n    files:\n      - {{ id: my_ini, path: \"{{{{ paths.etc }}}}/my.ini\", format: ini }}\n\
         {indented}"
    );
    let manifest = load_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    let issues = validate(&manifest);
    assert!(issues.is_empty(), "{issues:#?}");
}

/// Wraps a `spec` capability fragment (§1.8, §1.9) in a daemon tool with the ports it uses.
fn capability_manifest(fragment: &str) -> String {
    let indented: String = fragment.lines().map(|l| format!("  {l}\n")).collect();
    format!(
        "apiVersion: forgebox.dev/v1\nkind: Tool\n\
         metadata: {{ id: capability, name: Capability, category: database }}\n\
         spec:\n  platforms: [windows-x64]\n  source:\n    providers:\n      - type: mariadb\n\
         \x20 service:\n    kind: daemon\n    daemon:\n      exe: bin/server.exe\n      ports:\n\
         \x20       - {{ name: main, default: 3306, range: databases }}\n\
         \x20       - {{ name: http, default: 8025, range: mail }}\n\
         \x20       - {{ name: smtp, default: 1025, range: mail }}\n\
         {indented}"
    )
}

#[test]
fn docs_04_capability_fragments_are_valid_blocks() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/04-tool-manifest-spec.md");
    let markdown = std::fs::read_to_string(&path).unwrap();
    let mut seen = Vec::new();
    for fragment in yaml_blocks(&markdown) {
        let first = fragment.lines().next().unwrap_or_default();
        if first != "database:" && first != "mail:" {
            continue;
        }
        let text = capability_manifest(&fragment);
        let manifest = load_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        let issues = validate(&manifest);
        assert!(issues.is_empty(), "{issues:#?}\n{text}");
        seen.push(first.to_owned());
    }
    assert_eq!(seen, ["database:", "mail:"]);
}
