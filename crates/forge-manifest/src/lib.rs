//! Forgebox manifest schema (`forgebox.yaml`), parser, semantic validation, version rules
//! and registry lint (docs/04, ADR-0006).
//!
//! - [`schema`]: serde + schemars structs of the five kinds (`Tool`, `Extension`, `Stack`,
//!   `Project`, `Registry`) and [`schema::json_schemas`] (`cargo xtask schema`).
//! - [`parse`]: [`parse::load_str`] (errors with line/column) and [`parse::validate`].
//! - [`version`]: versions, constraints, channels, series and build ordering.
//! - [`lint`]: the seven rules of docs/04 §6 over a registry folder.
//! - [`yaml`]: the only module that touches the YAML crate (ADR-0012).
//!
//! No tool-specific logic lives here: everything is data from the manifests.

pub mod issue;
pub mod lint;
pub mod parse;
pub mod schema;
pub mod template;
pub mod version;
pub mod yaml;

pub use issue::{IssueCode, ValidationIssue};
pub use parse::{ManifestError, ValidateOptions, load_file, load_str, validate, validate_with};
pub use schema::Manifest;

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-manifest");
    }
}
