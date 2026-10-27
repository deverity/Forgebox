//! `kind: Project` (docs/04 §4): optional `forgebox.yaml` at the root of a project.

use std::borrow::Cow;
use std::collections::BTreeMap;

use forge_core::ids::ToolId;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::common::{ApiVersion, ProjectKind, RestartOn, is_default, is_false};
use super::tool::BoolOrWord;

/// A complete `kind: Project` manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectManifest {
    #[serde(rename = "apiVersion")]
    pub api_version: ApiVersion,
    pub kind: ProjectKind,
    pub metadata: ProjectMetadata,
    #[serde(default, skip_serializing_if = "is_default")]
    pub spec: ProjectSpec,
}

/// `metadata` of a Project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectMetadata {
    pub name: String,
}

/// `spec` of a Project. Every block is optional.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<ProjectSite>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ProjectService>,
    /// Used by `forge shell` and by `processes`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runtimes: Vec<ProjectRuntime>,
    /// Managed by the supervisor while the site is up (docs/06 §4.2).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub processes: BTreeMap<String, ProcessSpec>,
    /// Extra reverse-proxy sites.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proxies: Vec<ProjectProxy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share: Option<Share>,
}

/// `spec.site`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectSite {
    /// Lowercase host (`loja.test`). Default: folder name + TLD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Document root relative to the project (`public`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    /// Front web server tool (`apache`, `nginx`, `caddy`, `frankenphp`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<ToolId>,
    /// PHP series (`"8.3"`) or exact version (`"8.3.36"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub php: Option<String>,
    /// `php.ini` overrides for this site.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub php_ini: BTreeMap<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xdebug: Option<XdebugMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
}

/// Xdebug for a site (docs/06 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum XdebugMode {
    Inherit,
    Off,
    Trigger,
    Always,
}

/// `spec.services[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectService {
    pub tool: ToolId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Default `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Database created on `forge up` when missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
}

/// `spec.runtimes[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectRuntime {
    pub tool: ToolId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    /// Version or series prefix (`"22"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// `spec.processes.<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProcessSpec {
    /// Command line; `$PORT` receives the allocated port.
    pub cmd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Fixed port or `auto` (range `apps`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<ProcessPort>,
    /// The site host proxies to this process. Default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub proxy: bool,
    /// Path proxied to the process (`/api`); default: the whole host when `proxy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_path: Option<String>,
    /// `tool@version` put first on the PATH (`nodejs@22`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// Default `failure`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub restart: RestartOn,
    /// Default `true` (starts with the site).
    #[serde(default, skip_serializing_if = "is_default")]
    pub autostart: ProcessAutostart,
}

/// `processes.*.port`: a number or `auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessPort {
    Auto,
    Fixed(u16),
}

impl Serialize for ProcessPort {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ProcessPort::Auto => serializer.serialize_str("auto"),
            ProcessPort::Fixed(port) => serializer.serialize_u16(*port),
        }
    }
}

impl<'de> Deserialize<'de> for ProcessPort {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Port(u16),
            Word(String),
        }
        match Repr::deserialize(deserializer)? {
            Repr::Port(0) => Err(serde::de::Error::custom("port 0 is not allowed")),
            Repr::Port(port) => Ok(ProcessPort::Fixed(port)),
            Repr::Word(w) if w == "auto" => Ok(ProcessPort::Auto),
            Repr::Word(w) => Err(serde::de::Error::custom(format!(
                "invalid port {w:?}: expected a number or auto"
            ))),
        }
    }
}

impl JsonSchema for ProcessPort {
    fn schema_name() -> Cow<'static, str> {
        "ProcessPort".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "A port number or auto (allocated from the apps range).",
            "oneOf": [
                { "type": "integer", "minimum": 1, "maximum": 65535 },
                { "const": "auto" }
            ]
        })
    }
}

/// `processes.*.autostart`: `true`, `false` or `on-visit` (started on the first request).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ProcessAutostart {
    #[default]
    Yes,
    No,
    OnVisit,
}

impl Serialize for ProcessAutostart {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ProcessAutostart::Yes => serializer.serialize_bool(true),
            ProcessAutostart::No => serializer.serialize_bool(false),
            ProcessAutostart::OnVisit => serializer.serialize_str("on-visit"),
        }
    }
}

impl<'de> Deserialize<'de> for ProcessAutostart {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match BoolOrWord::deserialize(deserializer)? {
            BoolOrWord::Bool(true) => Ok(ProcessAutostart::Yes),
            BoolOrWord::Bool(false) => Ok(ProcessAutostart::No),
            BoolOrWord::Word(w) if w == "on-visit" => Ok(ProcessAutostart::OnVisit),
            BoolOrWord::Word(w) => Err(serde::de::Error::custom(format!(
                "invalid autostart {w:?}: expected true, false or on-visit"
            ))),
        }
    }
}

impl JsonSchema for ProcessAutostart {
    fn schema_name() -> Cow<'static, str> {
        "ProcessAutostart".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "true, false or on-visit (start on the first request to the site).",
            "oneOf": [{ "type": "boolean" }, { "const": "on-visit" }]
        })
    }
}

/// `spec.proxies[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectProxy {
    /// Lowercase host.
    pub host: String,
    /// `http://127.0.0.1:3000`.
    pub upstream: String,
    /// Default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub websocket: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
}

/// `spec.share`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Share {
    /// Tunnel tool (`cloudflared`, `ngrok`).
    pub provider: ToolId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_port_and_autostart_accept_words() {
        assert_eq!(
            serde_json::from_str::<ProcessPort>("\"auto\"").unwrap(),
            ProcessPort::Auto
        );
        assert_eq!(
            serde_json::from_str::<ProcessPort>("5173").unwrap(),
            ProcessPort::Fixed(5173)
        );
        assert!(serde_json::from_str::<ProcessPort>("0").is_err());
        assert!(serde_json::from_str::<ProcessPort>("\"any\"").is_err());
        assert_eq!(
            serde_json::from_str::<ProcessAutostart>("\"on-visit\"").unwrap(),
            ProcessAutostart::OnVisit
        );
        assert_eq!(
            serde_json::to_string(&ProcessAutostart::No).unwrap(),
            "false"
        );
    }
}
