//! `kind: Extension` (docs/04 §2): installed inside a version of another tool.

use std::collections::BTreeMap;

use forge_core::ids::ToolId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{ApiVersion, Apply, ExtensionKind, is_default};
use super::tool::{
    ArchiveFormat, Config, PostInstallAction, Source, StripRoot, ToolMetadata, Uninstall,
};

/// A complete `kind: Extension` manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    #[serde(rename = "apiVersion")]
    pub api_version: ApiVersion,
    pub kind: ExtensionKind,
    pub metadata: ToolMetadata,
    pub spec: ExtensionSpec,
}

/// `spec` of an Extension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionSpec {
    /// The tool this extension goes into (`php`).
    pub extends: ToolId,
    /// Resolved against the target tool version; the first matching entry wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compat: Vec<Compat>,
    pub source: Source,
    pub install: ExtensionInstall,
    #[serde(default, skip_serializing_if = "is_default")]
    pub config: Config,
    /// Lines written to enable/disable the extension in the target's config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toggle: Option<Toggle>,
}

/// `spec.compat[]`: which extension versions fit which target builds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Compat {
    /// Constraint on the version of the extended tool (`"~8.3"`).
    pub tool: String,
    /// Required variant values of the target build (`{ ts: nts, compiler: vs17 }`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variant: BTreeMap<String, String>,
    /// Constraint on the extension version (`"3.4.*"`).
    pub version: String,
}

/// `spec.install` of an Extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionInstall {
    /// Default: inferred from the file name (a `.dll` is a single `file`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<ArchiveFormat>,
    /// Only for archives. Default `auto`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub strip_root: StripRoot,
    /// Destination inside the target version: the file itself (single file) or the folder
    /// where the archive is extracted. Must start with `{{ paths.version }}`.
    pub target: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_install: Vec<PostInstallAction>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub uninstall: Uninstall,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_estimate_mb: Option<u32>,
}

/// `spec.toggle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Toggle {
    pub enable: String,
    pub disable: String,
    /// Default `restart`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub apply: Apply,
}
