//! `kind: Registry` (docs/04 §5): `index.yaml` of a registry.

use forge_core::Timestamp;
use forge_core::ids::ToolId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{ApiVersion, Category, RegistryKind, Sha256};

/// Current value of `spec.schema_version`.
pub const REGISTRY_SCHEMA_VERSION: u32 = 1;

/// A complete `kind: Registry` manifest (`index.yaml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistryManifest {
    #[serde(rename = "apiVersion")]
    pub api_version: ApiVersion,
    pub kind: RegistryKind,
    pub metadata: RegistryMetadata,
    pub spec: RegistrySpec,
}

/// `metadata` of a Registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistryMetadata {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// `spec` of a Registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistrySpec {
    /// Must be `1`.
    pub schema_version: u32,
    pub updated_at: Timestamp,
    /// Tools and extensions (`tools/*.yaml`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<RegistryEntry>,
    /// `stacks/*.yaml`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stacks: Vec<RegistryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<Signature>,
}

/// `spec.tools[]` / `spec.stacks[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistryEntry {
    pub id: ToolId,
    /// Relative to the registry root, `/` separators (`tools/php.yaml`).
    pub path: String,
    /// Of the manifest file.
    pub sha256: Sha256,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<Category>,
    /// Newest stable version, for the catalog list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
}

/// `key_id` of the placeholder signature of unsigned development indexes (`sig` may be empty).
/// Real signatures come with M1-E1-T3; a loader must only accept `dev` in development mode.
pub const DEV_KEY_ID: &str = "dev";

/// `spec.signature`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    #[serde(rename = "type")]
    pub kind: SignatureType,
    pub key_id: String,
    pub sig: String,
}

/// Signature scheme of a registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SignatureType {
    Minisign,
}
