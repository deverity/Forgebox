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
