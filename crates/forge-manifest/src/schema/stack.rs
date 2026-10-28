//! `kind: Stack` (docs/04 §3): preset of tools, instances and a site template.

use std::collections::BTreeMap;

use forge_core::ids::ToolId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{ApiVersion, RunAction, StackKind, is_false};

/// A complete `kind: Stack` manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StackManifest {
    #[serde(rename = "apiVersion")]
    pub api_version: ApiVersion,
    pub kind: StackKind,
    pub metadata: StackMetadata,
    pub spec: StackSpec,
}

/// `metadata` of a Stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StackMetadata {
    /// Unique among stacks (`[a-z0-9-]`).
    pub id: ToolId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// SVG file in `registry/icons/` or an `https://` URL (rule 7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub deprecated: bool,
}

/// `spec` of a Stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StackSpec {
    pub tools: Vec<StackTool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<StackSite>,
}

/// `spec.tools[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StackTool {
    pub id: ToolId,
    /// Series (`"8.4"`); default: latest stable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    /// Exact version, when a series is not precise enough.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Instance name for services (default `default`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Extensions installed into this tool (`[xdebug]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<ToolId>,
}

/// `spec.site`: how `forge new` creates the site.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct StackSite {
    /// Command that creates the project in `{{ dir }}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_with: Option<String>,
    /// Site driver (`laravel`, `wordpress`...), see docs/06 §1.2.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    /// PHP series of the site.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub php: Option<String>,
    /// HTTPS with the local CA. Default: global preference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
    /// Merged into `.env` after creation.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env_patch: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub post_create: Vec<PostCreateAction>,
}

/// `site.post_create[]`: single-key map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PostCreateAction {
    CreateDatabase(CreateDatabaseAction),
    /// Project command (`php artisan key:generate`), usually with `cwd: "{{ dir }}"`.
    Run(RunAction),
}

/// `post_create[].create_database`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateDatabaseAction {
    /// `<tool>/<instance>` (`mysql/default`).
    pub instance: String,
    pub name: String,
}
