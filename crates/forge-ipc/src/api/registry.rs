//! `registry.*`: manifest registries (docs/09 §registry, docs/05).

use serde::{Deserialize, Serialize};
use specta::Type;

use forge_core::Timestamp;

/// Kind of registry source (`registries.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum RegistryKind {
    Git,
    Http,
    Dir,
}

/// A registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RegistryDto {
    pub name: String,
    pub url: String,
    pub kind: RegistryKind,
    pub official: bool,
    pub enabled: bool,
    pub priority: i32,
    pub tools: u32,
    pub last_refresh_at: Option<Timestamp>,
}

/// Result of `registry.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RegistryListDto {
    pub registries: Vec<RegistryDto>,
}

/// Params of `registry.add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RegistryAddParams {
    pub url: String,
    /// Default: derived from the URL.
    #[serde(default)]
    pub name: Option<String>,
}

/// Params of `registry.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RegistryRemoveParams {
    pub name: String,
}
