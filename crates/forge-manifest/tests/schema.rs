//! Snapshots of the JSON Schemas written by `cargo xtask schema` (registry/schema/).

use forge_manifest::schema::json_schemas;

#[test]
fn json_schemas_snapshot() {
    for (name, schema) in json_schemas() {
        let text = serde_json::to_string_pretty(&schema).unwrap();
        let snapshot = name.trim_end_matches(".json").replace('.', "_");
        insta::assert_snapshot!(snapshot, text);
    }
}

#[test]
fn tool_schema_describes_tagged_unions() {
    let (_, tool) = json_schemas().into_iter().next().unwrap();
    let value = serde_json::to_value(&tool).unwrap();
    let defs = &value["$defs"];
    // Providers are selected by `type`, post_install actions are single-key maps.
    let providers = defs["Provider"]["oneOf"].as_array().unwrap();
    assert_eq!(providers.len(), 11);
    assert!(
        providers
            .iter()
            .all(|p| p["properties"]["type"].is_object())
    );
    let actions = defs["PostInstallAction"]["oneOf"].as_array().unwrap();
    let keys: Vec<&str> = actions
        .iter()
        .map(|a| a["required"][0].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["copy", "render", "mkdir", "run"]);
    assert_eq!(defs["StripRoot"]["oneOf"][1]["const"], "auto");
    assert_eq!(defs["Duration"]["type"], "string");
}
