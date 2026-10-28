//! Choosing the version behind `bin/<tool>/current` (`forge use`, ADR-0005).
//!
//! [`select`] maps what the user typed to an installed version: a folder name
//! (`8.3.37-nts-vs17-x64`), an exact version (`8.3.37`, `v22.12.0`) or a series / version
//! constraint in the tool's scheme (`8.3` = `8.3.*`, `22`, `>=8.3 <8.5`). Several matches
//! resolve to the preferred build ([`cmp_builds`]: newest version, then the manifest's
//! variant preference). Only `installed` rows count.

use std::path::PathBuf;

use forge_core::error::{ForgeError, Result};
use forge_core::ids::{ToolId, VersionId};
use forge_core::store::{ToolVersionRow, VersionState};
use forge_manifest::schema::ToolManifest;
use forge_manifest::version::{Build, Constraint, Version, cmp_builds};
use serde::{Deserialize, Serialize};

/// The version behind `current`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentVersion {
    pub tool: ToolId,
    /// Folder name the junction points at.
    pub canonical_name: String,
    /// Absolute target of the junction.
    pub path: PathBuf,
}

/// Result of [`super::Layout::use_version`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseReport {
    pub tool: ToolId,
    /// Folder `current` pointed at before (`None`: there was no junction).
    pub from: Option<String>,
    /// Folder it points at now.
    pub to: String,
    pub version: String,
    pub version_id: VersionId,
    /// Absolute folder of the new current version.
    pub path: PathBuf,
    /// Duration of the junction switch (`current.tmp` + rename), microseconds.
    pub switch_us: u64,
    /// The junction already pointed there: nothing was changed.
    pub unchanged: bool,
}

/// Payload of `tool.current_changed` (same fields as `forge_ipc`'s `ToolCurrentPayload`,
/// docs/03 §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCurrentEventPayload {
    pub v: u32,
    pub tool: ToolId,
    pub from: Option<String>,
    pub to: String,
    pub version_id: Option<VersionId>,
    pub elapsed_us: u64,
}

/// Version of [`ToolCurrentEventPayload`].
pub const TOOL_CURRENT_EVENT_VERSION: u32 = 1;

/// Normalizes user input: trims and drops a `v` before a digit (`v22` → `22`).
fn normalize(spec: &str) -> &str {
    let spec = spec.trim();
    match spec.strip_prefix(['v', 'V']) {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => rest,
        _ => spec,
    }
}

/// The installed row of `manifest`'s tool that `spec` designates (module docs).
pub fn select<'r>(
    manifest: &ToolManifest,
    rows: &'r [ToolVersionRow],
    spec: &str,
) -> Result<&'r ToolVersionRow> {
    let tool = manifest.metadata.id.as_str();
    let installed: Vec<&ToolVersionRow> = rows
        .iter()
        .filter(|r| r.state == VersionState::Installed && r.tool_id == manifest.metadata.id)
        .collect();
    let wanted = normalize(spec);
    if wanted.is_empty() {
        return Err(ForgeError::InvalidInput("empty version".into()));
    }
    if let Some(row) = installed
        .iter()
        .find(|r| r.canonical_name.eq_ignore_ascii_case(wanted))
    {
        return Ok(row);
    }
    let scheme = manifest.spec.versioning.scheme;
    let parsed: Vec<(&ToolVersionRow, Option<Version>)> = installed
        .iter()
        .map(|r| (*r, Version::parse(scheme, &r.version).ok()))
        .collect();
    let exact: Vec<&(&ToolVersionRow, Option<Version>)> = parsed
        .iter()
        .filter(|(r, _)| r.version == wanted || r.version.split('+').next() == Some(wanted))
        .collect();
    let mut matches: Vec<&(&ToolVersionRow, Option<Version>)> = if exact.is_empty() {
        match Constraint::parse(scheme, wanted) {
            Ok(constraint) => parsed
                .iter()
                .filter(|(_, v)| v.as_ref().is_some_and(|v| constraint.matches(v)))
                .collect(),
            Err(_) => Vec::new(),
        }
    } else {
        exact
    };
    let variants = &manifest.spec.versioning.variants;
    matches.sort_by(|(ra, va), (rb, vb)| match (va, vb) {
        (Some(va), Some(vb)) => cmp_builds(
            &Build {
                version: va,
                variants: &ra.variant,
            },
            &Build {
                version: vb,
                variants: &rb.variant,
            },
            variants,
        ),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => ra.canonical_name.cmp(&rb.canonical_name),
    });
    matches.first().map(|(row, _)| *row).ok_or_else(|| {
        let names: Vec<&str> = installed
            .iter()
            .map(|r| r.canonical_name.as_str())
            .collect();
        ForgeError::NotFound(if names.is_empty() {
            format!("{tool} {wanted}: no version of {tool} is installed")
        } else {
            format!(
                "{tool} {wanted} is not installed (installed: {})",
                names.join(", ")
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use forge_core::store::VersionSource;
    use forge_manifest::Manifest;

    use super::*;

    fn php() -> ToolManifest {
        let text = "apiVersion: forgebox.dev/v1\nkind: Tool\n\
                    metadata: { id: php, name: PHP, category: runtime }\n\
                    spec:\n  platforms: [windows-x64]\n\
                    \x20 versioning:\n    variants:\n      - { key: ts, values: [nts, ts], default: nts }\n\
                    \x20 source:\n    providers:\n      - type: php-windows\n";
        match forge_manifest::load_str(text).unwrap() {
            Manifest::Tool(t) => t,
            _ => unreachable!(),
        }
    }

    fn row(version: &str, ts: &str, state: VersionState) -> ToolVersionRow {
        ToolVersionRow {
            id: VersionId::new(),
            tool_id: ToolId::new("php").unwrap(),
            version: version.into(),
            canonical_name: format!("{version}-{ts}"),
            variant: BTreeMap::from([("ts".into(), ts.into())]),
            path: format!("C:/fb/bin/php/{version}-{ts}"),
            state,
            source: VersionSource::Registry,
        }
    }

    #[test]
    fn selects_by_folder_version_and_series() {
        let m = php();
        let rows = vec![
            row("8.3.36", "nts", VersionState::Installed),
            row("8.3.37", "ts", VersionState::Installed),
            row("8.3.37", "nts", VersionState::Installed),
            row("8.4.14", "nts", VersionState::Installed),
            row("8.5.0", "nts", VersionState::Broken),
        ];
        let pick = |spec: &str| select(&m, &rows, spec).map(|r| r.canonical_name.clone());
        assert_eq!(pick("8.3.37-ts").unwrap(), "8.3.37-ts");
        assert_eq!(pick("8.3.37").unwrap(), "8.3.37-nts", "nts preferred");
        assert_eq!(pick("8.3").unwrap(), "8.3.37-nts", "newest of the series");
        assert_eq!(pick(" v8.4 ").unwrap(), "8.4.14-nts");
        assert_eq!(pick("8").unwrap(), "8.4.14-nts");
        assert_eq!(pick(">=8.3 <8.4").unwrap(), "8.3.37-nts");
        let err = pick("8.5").unwrap_err();
        assert_eq!(err.code(), "not_found", "broken rows do not count");
        assert!(err.to_string().contains("8.3.36-nts"), "{err}");
        assert_eq!(pick("").unwrap_err().code(), "invalid_input");
        assert_eq!(select(&m, &[], "8.3").unwrap_err().code(), "not_found");
    }
}
