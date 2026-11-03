//! `build_entries` orders stacks by `metadata.order` (curated display order, docs/04 §5):
//! stacks with an order come first, by it; the others follow, by id.

use forge_manifest::index::build_entries;
use forge_manifest::lint::RegistryDir;

fn stack(id: &str, order: Option<u32>) -> String {
    let order = order.map(|o| format!(", order: {o}")).unwrap_or_default();
    format!(
        "apiVersion: forgebox.dev/v1\nkind: Stack\nmetadata: {{ id: {id}, name: {id}{order} }}\nspec:\n  tools: []\n"
    )
}

#[test]
fn ordered_stacks_come_first_then_the_rest_by_id() {
    let dir = tempfile::tempdir().unwrap();
    let stacks = dir.path().join("stacks");
    std::fs::create_dir_all(&stacks).unwrap();
    std::fs::write(stacks.join("alpha.yaml"), stack("alpha", None)).unwrap();
    std::fs::write(stacks.join("zeta.yaml"), stack("zeta", Some(2))).unwrap();
    std::fs::write(stacks.join("beta.yaml"), stack("beta", Some(1))).unwrap();
    std::fs::write(stacks.join("gamma.yaml"), stack("gamma", None)).unwrap();

    let (_, stack_entries) = build_entries(&RegistryDir::new(dir.path())).unwrap();

    let stack_ids: Vec<&str> = stack_entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(stack_ids, ["beta", "zeta", "alpha", "gamma"]);
    assert_eq!(stack_entries[0].path, "stacks/beta.yaml");
}

#[test]
fn stacks_without_order_keep_the_id_order() {
    let dir = tempfile::tempdir().unwrap();
    let stacks = dir.path().join("stacks");
    std::fs::create_dir_all(&stacks).unwrap();
    for id in ["mid", "alpha", "omega"] {
        std::fs::write(stacks.join(format!("{id}.yaml")), stack(id, None)).unwrap();
    }
    let (_, stack_entries) = build_entries(&RegistryDir::new(dir.path())).unwrap();
    let stack_ids: Vec<&str> = stack_entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(stack_ids, ["alpha", "mid", "omega"]);
}

fn tool_with_icon(id: &str, icon: &str) -> String {
    format!(
        "apiVersion: forgebox.dev/v1\nkind: Tool\nmetadata: {{ id: {id}, name: {id}, category: devtool, icon: {icon}, changelog_url: \"https://example.org/changes\" }}\nspec:\n  platforms: [windows-x64]\n  source:\n    providers:\n      - type: nodejs-dist\n"
    )
}

#[test]
fn icons_of_the_registry_are_listed_with_their_hash() {
    use forge_manifest::index::sha256_hex;
    use forge_manifest::lint::{LintContext, lint_registry};

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("tools")).unwrap();
    std::fs::create_dir_all(dir.path().join("icons")).unwrap();
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>\n";
    std::fs::write(dir.path().join("icons/a.svg"), svg).unwrap();
    std::fs::write(
        dir.path().join("tools/a.yaml"),
        tool_with_icon("a", "a.svg"),
    )
    .unwrap();
    // A URL is never fetched, a missing file is for the linter: neither is indexed.
    std::fs::write(
        dir.path().join("tools/b.yaml"),
        tool_with_icon("b", "https://example.org/b.svg"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("tools/c.yaml"),
        tool_with_icon("c", "c.svg"),
    )
    .unwrap();

    let registry = RegistryDir::new(dir.path());
    let (tools, _) = build_entries(&registry).unwrap();
    let icon = tools[0].icon.as_ref().expect("a has an icon entry");
    assert_eq!(icon.path, "icons/a.svg");
    assert_eq!(icon.sha256.as_str(), sha256_hex(svg));
    assert!(tools[1].icon.is_none() && tools[2].icon.is_none());

    // The linter checks the signed hash against the file.
    let index = forge_manifest::index::build_index(
        &registry,
        "t",
        None,
        forge_core::Timestamp::from_second(1_700_000_000).unwrap(),
    )
    .unwrap();
    std::fs::write(
        registry.index_path(),
        forge_manifest::index::render_index(&index).unwrap(),
    )
    .unwrap();
    let ctx = LintContext::new(Vec::new(), Vec::new());
    let clean = lint_registry(&registry, &ctx);
    let icon_issues: Vec<String> = clean
        .issues
        .iter()
        .map(ToString::to_string)
        .filter(|m| m.contains("icon.sha256"))
        .collect();
    assert!(icon_issues.is_empty(), "{icon_issues:?}");
    std::fs::write(dir.path().join("icons/a.svg"), b"<svg/>").unwrap();
    let dirty = lint_registry(&registry, &ctx);
    assert!(
        dirty
            .issues
            .iter()
            .any(|i| i.to_string().contains("spec.tools[0].icon.sha256")),
        "{:?}",
        dirty.issues
    );
}

#[test]
fn changelog_url_must_be_https() {
    let text = tool_with_icon("a", "a.svg").replace("https://example.org/changes", "http://x.test");
    let manifest = forge_manifest::load_str(&text).unwrap();
    let issues = forge_manifest::validate(&manifest);
    assert!(
        issues.iter().any(|i| i.field == "metadata.changelog_url"),
        "{issues:?}"
    );
}
