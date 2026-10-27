//! Types shared by every manifest kind (docs/04).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The only accepted `apiVersion`.
pub const API_VERSION: &str = "forgebox.dev/v1";

/// `apiVersion` of a manifest. Anything other than `forgebox.dev/v1` is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
pub enum ApiVersion {
    #[default]
    #[serde(rename = "forgebox.dev/v1")]
    V1,
}

/// Declares the single-value `kind` marker of one manifest kind.
macro_rules! kind_marker {
    ($(#[$meta:meta])* $name:ident, $variant:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema,
        )]
        pub enum $name {
            #[default]
            $variant,
        }
    };
}

kind_marker!(
    /// `kind: Tool`.
    ToolKind,
    Tool
);
kind_marker!(
    /// `kind: Extension`.
    ExtensionKind,
    Extension
);
kind_marker!(
    /// `kind: Stack`.
    StackKind,
    Stack
);
kind_marker!(
    /// `kind: Project`.
    ProjectKind,
    Project
);
kind_marker!(
    /// `kind: Registry`.
    RegistryKind,
    Registry
);

/// Value of `kind`, used to dispatch a document to its struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Tool,
    Extension,
    Stack,
    Project,
    Registry,
}

impl Kind {
    /// Every kind, in the order of docs/04.
    pub const ALL: [Kind; 5] = [
        Kind::Tool,
        Kind::Extension,
        Kind::Stack,
        Kind::Project,
        Kind::Registry,
    ];

    /// The YAML spelling (`Tool`, `Extension`...).
    pub const fn as_str(self) -> &'static str {
        match self {
            Kind::Tool => "Tool",
            Kind::Extension => "Extension",
            Kind::Stack => "Stack",
            Kind::Project => "Project",
            Kind::Registry => "Registry",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Kind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Kind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| format!("unknown kind {s:?}"))
    }
}

/// A duration written as text (`"15s"`, `"2m"`, `"1h"`, `"1h 30m"`), parsed with
/// `humantime`. Plain numbers are rejected so that the unit is always explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Duration(pub std::time::Duration);

impl Duration {
    pub const fn from_secs(secs: u64) -> Self {
        Self(std::time::Duration::from_secs(secs))
    }

    pub const fn as_std(&self) -> std::time::Duration {
        self.0
    }

    pub const fn is_zero(&self) -> bool {
        self.0.is_zero()
    }
}

impl From<std::time::Duration> for Duration {
    fn from(value: std::time::Duration) -> Self {
        Self(value)
    }
}

impl From<Duration> for std::time::Duration {
    fn from(value: Duration) -> Self {
        value.0
    }
}

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", humantime::format_duration(self.0))
    }
}

impl FromStr for Duration {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        humantime::parse_duration(s.trim()).map(Self).map_err(|e| {
            format!("invalid duration {s:?} ({e}); use values like \"15s\", \"2m\", \"1h\"")
        })
    }
}

impl Serialize for Duration {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Duration {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Duration {
    fn schema_name() -> Cow<'static, str> {
        "Duration".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "Duration with an explicit unit, e.g. \"15s\", \"2m\", \"1h\", \"1h 30m\".",
            "type": "string",
            "pattern": "^\\s*[0-9]+\\s*[a-zA-Z]+(\\s*[0-9]+\\s*[a-zA-Z]+)*\\s*$",
            "examples": ["15s", "2m", "1h"]
        })
    }
}

/// A SHA-256 digest: 64 hex digits, normalized to lowercase.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sha256(String);

impl Sha256 {
    /// Validates `value` (64 hex digits, any case) and stores it in lowercase.
    pub fn new(value: &str) -> Result<Self, String> {
        if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(Self(value.to_ascii_lowercase()))
        } else {
            Err(format!(
                "invalid sha256 {value:?}: expected 64 hexadecimal digits"
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Sha256 {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl Serialize for Sha256 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Sha256 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::new(&text).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Sha256 {
    fn schema_name() -> Cow<'static, str> {
        "Sha256".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "SHA-256 digest (64 hexadecimal digits).",
            "type": "string",
            "pattern": "^[0-9a-fA-F]{64}$"
        })
    }
}

/// Target platform of a tool (`spec.platforms`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    WindowsX64,
    WindowsArm64,
    MacosArm64,
    LinuxX64,
}

/// Catalog category (`metadata.category`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Runtime,
    Webserver,
    Database,
    Cache,
    Queue,
    Search,
    Storage,
    Mail,
    Tunnel,
    Devtool,
    Gui,
    Library,
}

/// `metadata.maintainers[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Maintainer {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// How versions of a tool are written and compared (`spec.versioning.scheme`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum VersionScheme {
    /// Semantic versioning (`8.3.37`, `1.27.0-rc1`); `X` and `X.Y` are read as `X.0.0`/`X.Y.0`.
    #[default]
    Semver,
    /// Calendar versions (`2024.10.1`): dot-separated, first segment numeric.
    Calver,
    /// Free text (`12.8.0.6908`, `2.47.1.windows.1`), compared segment by segment.
    String,
}

/// When a changed setting takes effect (`apply`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Apply {
    /// Graceful reload (recycle of the PHP pool, `nginx -s reload`...).
    Reload,
    /// Restart of the service (always correct; default).
    #[default]
    Restart,
    /// Nothing to do (read on next start or per request).
    None,
}

/// A command run by the engine (`post_install[].run`, `post_create[].run`).
///
/// In `post_install`, the executable (first token of `command`) is resolved inside
/// `{{ paths.version }}` and never through `PATH` (docs/04 §6 rule 6, docs/11 §2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunAction {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Default: no limit other than the job's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<Duration>,
}

/// Restart behaviour (`restart_policy.on`, `processes.*.restart`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RestartOn {
    Never,
    /// Restart after an unexpected exit (default).
    #[default]
    Failure,
    Always,
}

pub(crate) fn is_false(value: &bool) -> bool {
    !*value
}

pub(crate) fn is_true(value: &bool) -> bool {
    *value
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_round_trips_as_text() {
        let d: Duration = serde_json::from_str("\"120s\"").unwrap();
        assert_eq!(d.as_std(), std::time::Duration::from_secs(120));
        assert_eq!(serde_json::to_string(&d).unwrap(), "\"2m\"");
        let h: Duration = "1h 30m".parse().unwrap();
        assert_eq!(h, Duration::from_secs(5400));
        assert!(serde_json::from_str::<Duration>("15").is_err());
        assert!("soon".parse::<Duration>().is_err());
    }

    #[test]
    fn sha256_is_validated_and_lowercased() {
        let upper = "AB".repeat(32);
        assert_eq!(Sha256::new(&upper).unwrap().as_str(), "ab".repeat(32));
        assert!(Sha256::new("abc").is_err());
        assert!(Sha256::new(&"zz".repeat(32)).is_err());
    }

    #[test]
    fn kind_parses_and_prints() {
        for kind in Kind::ALL {
            assert_eq!(kind.as_str().parse::<Kind>().unwrap(), kind);
        }
        assert!("Gadget".parse::<Kind>().is_err());
    }

    #[test]
    fn api_version_only_accepts_v1() {
        let v: ApiVersion = serde_json::from_str("\"forgebox.dev/v1\"").unwrap();
        assert_eq!(v, ApiVersion::V1);
        assert!(serde_json::from_str::<ApiVersion>("\"forgebox.dev/v2\"").is_err());
    }
}
