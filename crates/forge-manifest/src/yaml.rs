//! YAML access (ADR-0012): the only module that names `serde_yaml_ng`.
//!
//! Externally tagged enums of the schema (`post_install: [- copy: {...}]`) are written as
//! single-key maps, the form used by docs/04, through `singleton_map_recursive` in both
//! directions (the crate's default form is a YAML tag, `!copy`).

use std::fmt;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_yaml_ng::with::singleton_map_recursive;

/// 1-based position in the source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, column {}", self.line, self.column)
    }
}

/// A YAML syntax error or a mismatch with the target type.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct YamlError {
    /// Includes the YAML path and, when known, the position (`... at line 3 column 5`).
    pub message: String,
    pub location: Option<Location>,
}

impl From<serde_yaml_ng::Error> for YamlError {
    fn from(err: serde_yaml_ng::Error) -> Self {
        Self {
            message: err.to_string(),
            location: err.location().map(|l| Location {
                line: l.line(),
                column: l.column(),
            }),
        }
    }
}

/// Parses one YAML document into `T`.
pub fn from_str<T: DeserializeOwned>(text: &str) -> Result<T, YamlError> {
    let deserializer = serde_yaml_ng::Deserializer::from_str(text);
    singleton_map_recursive::deserialize(deserializer).map_err(YamlError::from)
}

/// Writes `value` as YAML (LF line endings, no BOM).
pub fn to_string<T: Serialize>(value: &T) -> Result<String, YamlError> {
    let mut out = Vec::new();
    let mut serializer = serde_yaml_ng::Serializer::new(&mut out);
    singleton_map_recursive::serialize(value, &mut serializer).map_err(YamlError::from)?;
    String::from_utf8(out).map_err(|e| YamlError {
        message: e.to_string(),
        location: None,
    })
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Action {
        Mkdir(String),
        Copy { from: String },
    }

    #[test]
    fn enums_are_single_key_maps() {
        let actions: Vec<Action> = from_str("- mkdir: a\n- copy: { from: b }\n").unwrap();
        assert_eq!(
            actions,
            [Action::Mkdir("a".into()), Action::Copy { from: "b".into() }]
        );
        let text = to_string(&actions).unwrap();
        assert_eq!(text, "- mkdir: a\n- copy:\n    from: b\n");
    }

    #[test]
    fn errors_have_location() {
        let err = from_str::<Vec<Action>>("- mkdir: a\n- nope: 1\n").unwrap_err();
        let loc = err.location.unwrap();
        assert_eq!(loc.line, 2, "{err}");
        assert!(err.message.contains("nope"), "{err}");
    }
}
