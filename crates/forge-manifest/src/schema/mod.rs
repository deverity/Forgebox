//! Manifest structs (serde) and their JSON Schemas (schemars), docs/04.
//!
//! Every struct uses `#[serde(deny_unknown_fields)]`: an unknown key is an error, not a
//! silently ignored typo. Each kind has its own top-level struct with an explicit `kind`
//! field (a single-value enum), so that the per-kind JSON Schema describes `kind` too and
//! a document can be parsed straight into its struct (errors keep line and column).
//! [`Manifest`] dispatches on `kind`; YAML text should go through
//! [`crate::parse::load_str`], which does that dispatch without buffering the document.

pub mod common;
pub mod extension;
pub mod project;
pub mod registry;
pub mod stack;
pub mod tool;

use forge_core::ids::ToolId;
use schemars::{Schema, schema_for};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use common::{
    API_VERSION, ApiVersion, Apply, Category, Duration, Kind, Maintainer, Platform, RestartOn,
    RunAction, Sha256, VersionScheme,
};
pub use extension::ExtensionManifest;
pub use project::ProjectManifest;
pub use registry::RegistryManifest;
pub use stack::StackManifest;
pub use tool::ToolManifest;

/// Any manifest, selected by its `kind`.
///
/// Not boxed: manifests are parsed rarely and kept in small numbers (one per catalog
/// entry), so the size of the `Tool` variant is cheaper than an allocation per manifest
/// and a `Box` in every pattern of the consumers.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Manifest {
    Tool(ToolManifest),
    Extension(ExtensionManifest),
    Stack(StackManifest),
    Project(ProjectManifest),
    Registry(RegistryManifest),
}

impl Manifest {
    pub fn kind(&self) -> Kind {
        match self {
            Manifest::Tool(_) => Kind::Tool,
            Manifest::Extension(_) => Kind::Extension,
            Manifest::Stack(_) => Kind::Stack,
            Manifest::Project(_) => Kind::Project,
            Manifest::Registry(_) => Kind::Registry,
        }
    }

    /// `metadata.id` of Tool, Extension and Stack.
    pub fn id(&self) -> Option<&ToolId> {
        match self {
            Manifest::Tool(m) => Some(&m.metadata.id),
            Manifest::Extension(m) => Some(&m.metadata.id),
            Manifest::Stack(m) => Some(&m.metadata.id),
            Manifest::Project(_) | Manifest::Registry(_) => None,
        }
    }

    /// `metadata.name`.
    pub fn name(&self) -> &str {
        match self {
            Manifest::Tool(m) => &m.metadata.name,
            Manifest::Extension(m) => &m.metadata.name,
            Manifest::Stack(m) => &m.metadata.name,
            Manifest::Project(m) => &m.metadata.name,
            Manifest::Registry(m) => &m.metadata.name,
        }
    }

    /// `metadata.icon` of Tool, Extension and Stack.
    pub fn icon(&self) -> Option<&str> {
        match self {
            Manifest::Tool(m) => m.metadata.icon.as_deref(),
            Manifest::Extension(m) => m.metadata.icon.as_deref(),
            Manifest::Stack(m) => m.metadata.icon.as_deref(),
            Manifest::Project(_) | Manifest::Registry(_) => None,
        }
    }
}

impl From<ToolManifest> for Manifest {
    fn from(m: ToolManifest) -> Self {
        Manifest::Tool(m)
    }
}

impl From<ExtensionManifest> for Manifest {
    fn from(m: ExtensionManifest) -> Self {
        Manifest::Extension(m)
    }
}

impl From<StackManifest> for Manifest {
    fn from(m: StackManifest) -> Self {
        Manifest::Stack(m)
    }
}

impl From<ProjectManifest> for Manifest {
    fn from(m: ProjectManifest) -> Self {
        Manifest::Project(m)
    }
}

impl From<RegistryManifest> for Manifest {
    fn from(m: RegistryManifest) -> Self {
        Manifest::Registry(m)
    }
}

impl Serialize for Manifest {
    /// Each kind struct carries its own `kind`, so the output is the inner struct.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Manifest::Tool(m) => m.serialize(serializer),
            Manifest::Extension(m) => m.serialize(serializer),
            Manifest::Stack(m) => m.serialize(serializer),
            Manifest::Project(m) => m.serialize(serializer),
            Manifest::Registry(m) => m.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Manifest {
    /// Generic dispatch on the `kind` tag (JSON over IPC, tests). The document is buffered
    /// first, so errors lose their position; YAML text goes through
    /// [`crate::parse::load_str`] instead.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        let value = serde_json::Value::deserialize(deserializer)?;
        let kind = value
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| D::Error::missing_field("kind"))?;
        let kind: Kind = kind.parse().map_err(D::Error::custom)?;
        let result = match kind {
            Kind::Tool => serde_json::from_value(value).map(Manifest::Tool),
            Kind::Extension => serde_json::from_value(value).map(Manifest::Extension),
            Kind::Stack => serde_json::from_value(value).map(Manifest::Stack),
            Kind::Project => serde_json::from_value(value).map(Manifest::Project),
            Kind::Registry => serde_json::from_value(value).map(Manifest::Registry),
        };
        result.map_err(D::Error::custom)
    }
}

/// Returns `(file name, schema)` for every manifest kind, in a stable order.
/// File names follow docs/02 (`registry/schema/*.v1.json`).
pub fn json_schemas() -> Vec<(String, Schema)> {
    vec![
        ("tool.v1.json".to_owned(), schema_for!(ToolManifest)),
        (
            "extension.v1.json".to_owned(),
            schema_for!(ExtensionManifest),
        ),
        ("stack.v1.json".to_owned(), schema_for!(StackManifest)),
        ("project.v1.json".to_owned(), schema_for!(ProjectManifest)),
        ("registry.v1.json".to_owned(), schema_for!(RegistryManifest)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_five_named_schemas() {
        let names: Vec<_> = json_schemas().into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            names,
            [
                "tool.v1.json",
                "extension.v1.json",
                "stack.v1.json",
                "project.v1.json",
                "registry.v1.json"
            ]
        );
    }

    #[test]
    fn schemas_require_api_version_and_kind_and_reject_unknown_keys() {
        for (name, schema) in json_schemas() {
            let v = serde_json::to_value(&schema).unwrap();
            assert_eq!(v["properties"]["apiVersion"]["$ref"], "#/$defs/ApiVersion");
            assert!(v["properties"]["kind"].is_object(), "{name}");
            assert_eq!(v["additionalProperties"], false, "{name}");
            let required: Vec<_> = v["required"].as_array().unwrap().iter().collect();
            assert!(
                required.contains(&&serde_json::json!("apiVersion")),
                "{name}"
            );
            assert!(required.contains(&&serde_json::json!("kind")), "{name}");
        }
    }

    #[test]
    fn generic_deserialize_dispatches_on_kind() {
        let json = serde_json::json!({
            "apiVersion": "forgebox.dev/v1",
            "kind": "Project",
            "metadata": { "name": "loja" }
        });
        let m: Manifest = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(m.kind(), Kind::Project);
        assert_eq!(m.name(), "loja");
        assert_eq!(serde_json::to_value(&m).unwrap(), json);

        let bad = serde_json::json!({ "apiVersion": "forgebox.dev/v1", "kind": "Gadget" });
        assert!(serde_json::from_value::<Manifest>(bad).is_err());
        let missing = serde_json::json!({ "apiVersion": "forgebox.dev/v1" });
        assert!(serde_json::from_value::<Manifest>(missing).is_err());
    }
}
