//! Manifest structs (serde) and their JSON Schemas (schemars).

use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};

// Expanded in M1-E1-T1
/// Manifest of a tool or extension (`kind: Tool | Extension`).
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
pub struct ToolManifestV1 {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
}

// Expanded in M1-E1-T1
/// Manifest of a stack (`kind: Stack`).
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
pub struct StackManifestV1 {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
}

// Expanded in M1-E1-T1
/// Per-project manifest (`kind: Project`).
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
pub struct ProjectManifestV1 {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
}

/// Returns `(file name, schema)` for every manifest kind, in a stable order.
/// File names follow docs/02 (`registry/schema/*.v1.json`).
pub fn json_schemas() -> Vec<(String, Schema)> {
    vec![
        ("tool.v1.json".to_owned(), schema_for!(ToolManifestV1)),
        ("stack.v1.json".to_owned(), schema_for!(StackManifestV1)),
        ("project.v1.json".to_owned(), schema_for!(ProjectManifestV1)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_three_named_schemas() {
        let names: Vec<_> = json_schemas().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["tool.v1.json", "stack.v1.json", "project.v1.json"]);
    }

    #[test]
    fn schemas_are_valid_json_with_api_version() {
        for (name, schema) in json_schemas() {
            let text = serde_json::to_string_pretty(&schema).unwrap();
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert!(v["properties"]["apiVersion"].is_object(), "{name}");
            assert!(v["properties"]["kind"].is_object(), "{name}");
        }
    }
}
