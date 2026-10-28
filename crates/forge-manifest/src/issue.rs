//! Problems found by validation (`parse::validate`) and lint (`lint::lint_registry`).
//!
//! [`IssueCode::as_str`] is stable: the CLI/GUI use it as the i18n key of the message
//! shown to the user; `message` is the English detail for developers and logs.

use std::fmt;

/// What is wrong. Each code belongs to one lint rule of docs/04 §6
/// ([`IssueCode::lint_rule`]); problems that are not covered by rules 2–7 (schema,
/// identity, consistency) are reported under rule 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum IssueCode {
    // Rule 1: valid manifest, unique ids, platforms.
    /// YAML syntax, unknown key, wrong type, missing field, wrong `apiVersion`/`kind`.
    Schema,
    /// Malformed identifier (tool slug, variant key, port name, env var...).
    InvalidId,
    /// `spec.platforms` is empty.
    EmptyPlatforms,
    /// Two entries with the same key inside one manifest.
    Duplicate,
    InvalidVersion,
    InvalidConstraint,
    InvalidSeriesKey,
    /// Variant value/key not declared, `default` outside `values`...
    InvalidVariant,
    /// Reference to something that does not exist in the manifest (`file: php_ini`).
    InvalidReference,
    InvalidRegex,
    /// Value outside what the field accepts.
    InvalidValue,
    InvalidUrl,
    InvalidPath,
    /// Host not lowercase or malformed.
    InvalidHost,
    /// Valid in the schema but not supported by this version (`archive: msi`).
    Unsupported,
    /// `service.kind` and the `daemon`/`pool`/`oneshot`/`gui` blocks disagree.
    ServiceMismatch,
    /// `source.providers` is empty.
    MissingProvider,
    /// A database tool with an automatic update policy (docs/05 §4).
    DatabaseAutoUpdate,
    /// The same id in two files of a registry.
    DuplicateId,
    /// Reference to a tool/extension/stack that the registry does not have.
    UnknownTool,
    /// `index.yaml` and the manifest files disagree.
    IndexMismatch,
    /// Manifest in the wrong folder or with a file name other than `<id>.yaml`.
    MisplacedManifest,
    /// File could not be read.
    Io,
    // Rule 2.
    /// Provider without upstream checksum and releases without `sha256`.
    MissingChecksum,
    // Rule 3.
    /// `ports[].range` / `port_range` is not a known range.
    UnknownPortRange,
    // Rule 4.
    /// `render.template` exists neither in forge-render nor in `<registry>/templates/`.
    MissingTemplate,
    // Rule 5.
    UnknownTemplateVariable,
    /// `{{ }}` that is not a plain variable reference, or a `{% %}` block.
    InvalidTemplate,
    // Rule 6.
    /// `post_install.run` executable outside `{{ paths.version }}`.
    RunOutsideVersion,
    /// `post_install.run` in a third-party registry (docs/11 §2).
    RunNotAllowed,
    /// `post_install` writes outside the Forgebox root.
    WriteOutsideRoot,
    // Rule 7.
    /// Icon not SVG, missing, larger than 20 KB or with scripts.
    InvalidIcon,
}

impl IssueCode {
    /// Stable snake_case key (i18n key, `--json` output).
    pub const fn as_str(self) -> &'static str {
        match self {
            IssueCode::Schema => "schema",
            IssueCode::InvalidId => "invalid_id",
            IssueCode::EmptyPlatforms => "empty_platforms",
            IssueCode::Duplicate => "duplicate",
            IssueCode::InvalidVersion => "invalid_version",
            IssueCode::InvalidConstraint => "invalid_constraint",
            IssueCode::InvalidSeriesKey => "invalid_series_key",
            IssueCode::InvalidVariant => "invalid_variant",
            IssueCode::InvalidReference => "invalid_reference",
            IssueCode::InvalidRegex => "invalid_regex",
            IssueCode::InvalidValue => "invalid_value",
            IssueCode::InvalidUrl => "invalid_url",
            IssueCode::InvalidPath => "invalid_path",
            IssueCode::InvalidHost => "invalid_host",
            IssueCode::Unsupported => "unsupported",
            IssueCode::ServiceMismatch => "service_mismatch",
            IssueCode::MissingProvider => "missing_provider",
            IssueCode::DatabaseAutoUpdate => "database_auto_update",
            IssueCode::DuplicateId => "duplicate_id",
            IssueCode::UnknownTool => "unknown_tool",
            IssueCode::IndexMismatch => "index_mismatch",
            IssueCode::MisplacedManifest => "misplaced_manifest",
            IssueCode::Io => "io",
            IssueCode::MissingChecksum => "missing_checksum",
            IssueCode::UnknownPortRange => "unknown_port_range",
            IssueCode::MissingTemplate => "missing_template",
            IssueCode::UnknownTemplateVariable => "unknown_template_variable",
            IssueCode::InvalidTemplate => "invalid_template",
            IssueCode::RunOutsideVersion => "run_outside_version",
            IssueCode::RunNotAllowed => "run_not_allowed",
            IssueCode::WriteOutsideRoot => "write_outside_root",
            IssueCode::InvalidIcon => "invalid_icon",
        }
    }

    /// Lint rule of docs/04 §6 (1..=7).
    pub const fn lint_rule(self) -> u8 {
        match self {
            IssueCode::MissingChecksum => 2,
            IssueCode::UnknownPortRange => 3,
            IssueCode::MissingTemplate => 4,
            IssueCode::UnknownTemplateVariable | IssueCode::InvalidTemplate => 5,
            IssueCode::RunOutsideVersion
            | IssueCode::RunNotAllowed
            | IssueCode::WriteOutsideRoot => 6,
            IssueCode::InvalidIcon => 7,
            _ => 1,
        }
    }
}

impl fmt::Display for IssueCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One problem in a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    pub code: IssueCode,
    /// Dotted path of the field (`spec.install.post_install[0].run.command`).
    pub field: String,
    pub message: String,
}

impl ValidationIssue {
    pub fn new(code: IssueCode, field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            field: field.into(),
            message: message.into(),
        }
    }

    pub fn lint_rule(&self) -> u8 {
        self.code.lint_rule()
    }
}

impl fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[rule {} {}] {}: {}",
            self.lint_rule(),
            self.code,
            self.field,
            self.message
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rule_has_codes() {
        let codes = [
            IssueCode::Schema,
            IssueCode::MissingChecksum,
            IssueCode::UnknownPortRange,
            IssueCode::MissingTemplate,
            IssueCode::UnknownTemplateVariable,
            IssueCode::RunOutsideVersion,
            IssueCode::InvalidIcon,
        ];
        let rules: Vec<u8> = codes.iter().map(|c| c.lint_rule()).collect();
        assert_eq!(rules, [1, 2, 3, 4, 5, 6, 7]);
        let issue = ValidationIssue::new(IssueCode::InvalidIcon, "metadata.icon", "x");
        assert_eq!(issue.to_string(), "[rule 7 invalid_icon] metadata.icon: x");
    }
}
