//! Manifests as the daemon sees them: the loaded registries (`forge_registry::Registry`),
//! with lookups by tool id and the installed versions next to them.

use std::sync::Arc;

use forge_core::error::{ForgeError, Result};
use forge_core::ids::{ToolId, VersionId};
use forge_core::store::{StoreReader, ToolVersionRepo, ToolVersionRow, VersionState};
use forge_manifest::schema::tool::{ServiceKind, ToolManifest};
use forge_manifest::schema::{ExtensionManifest, Manifest, Platform, StackManifest};
use forge_manifest::version::{Build, Version, cmp_builds};
use forge_registry::Registry;

/// A tool manifest and whether its `post_install.run` may execute (official registry).
#[derive(Debug, Clone)]
pub struct ToolEntry {
    pub manifest: Arc<ToolManifest>,
    pub official: bool,
    pub registry: String,
    pub latest: Option<String>,
}

/// The manifest of `id`, if a loaded registry has it.
pub fn tool(registry: &Registry, id: &ToolId) -> Option<ToolEntry> {
    let entry = registry.find(id.as_str())?;
    match entry.manifest.as_ref() {
        Manifest::Tool(tool) => Some(ToolEntry {
            manifest: Arc::new(tool.clone()),
            official: entry.official,
            registry: entry.registry.clone(),
            latest: entry.latest.clone(),
        }),
        _ => None,
    }
}

/// [`tool`] or `not_found`.
pub fn require_tool(registry: &Registry, id: &ToolId) -> Result<ToolEntry> {
    tool(registry, id).ok_or_else(|| {
        ForgeError::NotFound(format!(
            "no manifest for tool {id} in the loaded registries (forge registry update)"
        ))
    })
}

/// An extension manifest (`kind: Extension`, docs/04 §2) of a loaded registry.
#[derive(Debug, Clone)]
pub struct ExtensionEntry {
    pub manifest: Arc<ExtensionManifest>,
    pub official: bool,
    pub registry: String,
    pub latest: Option<String>,
}

/// The extension manifest of `id`, if a loaded registry has one.
pub fn extension(registry: &Registry, id: &ToolId) -> Option<ExtensionEntry> {
    let entry = registry.find(id.as_str())?;
    match entry.manifest.as_ref() {
        Manifest::Extension(ext) => Some(ExtensionEntry {
            manifest: Arc::new(ext.clone()),
            official: entry.official,
            registry: entry.registry.clone(),
            latest: entry.latest.clone(),
        }),
        _ => None,
    }
}

/// Every extension manifest of the loaded registries (first registry wins per id), sorted
/// by id.
pub fn extensions(registry: &Registry) -> Vec<ExtensionEntry> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out: Vec<ExtensionEntry> = registry
        .entries()
        .into_iter()
        .filter_map(|entry| match entry.manifest.as_ref() {
            Manifest::Extension(ext) if seen.insert(entry.id.clone()) => Some(ExtensionEntry {
                manifest: Arc::new(ext.clone()),
                official: entry.official,
                registry: entry.registry.clone(),
                latest: entry.latest.clone(),
            }),
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| a.manifest.metadata.id.cmp(&b.manifest.metadata.id));
    out
}

/// What a spec of `tools.install`/`tools.remove` names: a tool or an extension.
#[derive(Debug, Clone)]
pub enum Installable {
    Tool(ToolEntry),
    Extension(ExtensionEntry),
}

/// [`tool`] or [`extension`], else `not_found`.
pub fn require_installable(registry: &Registry, id: &ToolId) -> Result<Installable> {
    if let Some(tool) = tool(registry, id) {
        return Ok(Installable::Tool(tool));
    }
    extension(registry, id)
        .map(Installable::Extension)
        .ok_or_else(|| {
            ForgeError::NotFound(format!(
                "no manifest for tool {id} in the loaded registries (forge registry update)"
            ))
        })
}

/// Every tool manifest of the loaded registries (first registry wins per id).
pub fn tools(registry: &Registry) -> Vec<ToolEntry> {
    let mut seen = std::collections::BTreeSet::new();
    registry
        .entries()
        .into_iter()
        .filter_map(|entry| match entry.manifest.as_ref() {
            Manifest::Tool(tool) if seen.insert(entry.id.clone()) => Some(ToolEntry {
                manifest: Arc::new(tool.clone()),
                official: entry.official,
                registry: entry.registry.clone(),
                latest: entry.latest.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Tools whose service runs as a FastCGI pool (`service.kind: pool`): the runtime of
/// `php` sites. Sorted by id, so the choice is stable when several exist.
pub fn pool_tools(registry: &Registry) -> Vec<ToolEntry> {
    let mut out: Vec<ToolEntry> = tools(registry)
        .into_iter()
        .filter(|t| t.manifest.spec.service.kind == ServiceKind::Pool)
        .collect();
    out.sort_by(|a, b| a.manifest.metadata.id.cmp(&b.manifest.metadata.id));
    out
}

/// Installed versions (`state = installed`) of `tool`, newest first by the manifest's
/// build order when it is known.
pub async fn installed(
    reader: &StoreReader,
    tool: &ToolId,
    manifest: Option<&ToolManifest>,
) -> Result<Vec<ToolVersionRow>> {
    let id = tool.clone();
    let mut rows = reader
        .query(move |conn| Ok(long_paths(ToolVersionRepo::new(conn).list(Some(&id))?)))
        .await?;
    rows.retain(|r| r.state == VersionState::Installed);
    if let Some(manifest) = manifest {
        sort_newest_first(manifest, &mut rows);
    }
    Ok(rows)
}

/// Version folders spelled with long names (rows written before the daemon normalized its
/// root, or imported, may hold 8.3 short names that servers refuse, docs/03 §3.1). Runs
/// inside the reader's blocking task.
fn long_paths(mut rows: Vec<ToolVersionRow>) -> Vec<ToolVersionRow> {
    for row in &mut rows {
        if row.path.contains('~') {
            row.path = forge_core::paths::posix_string(&forge_core::platform::long_path(
                std::path::Path::new(&row.path),
            ));
        }
    }
    rows
}

/// Every installed version of every tool.
pub async fn all_installed(reader: &StoreReader) -> Result<Vec<ToolVersionRow>> {
    let mut rows = reader
        .query(|conn| Ok(long_paths(ToolVersionRepo::new(conn).list(None)?)))
        .await?;
    rows.retain(|r| r.state == VersionState::Installed);
    Ok(rows)
}

/// One version by id.
pub async fn version(reader: &StoreReader, id: VersionId) -> Result<ToolVersionRow> {
    reader
        .query(move |conn| {
            Ok(ToolVersionRepo::new(conn)
                .get(id)?
                .map(|row| long_paths(vec![row]).remove(0)))
        })
        .await?
        .ok_or_else(|| ForgeError::NotFound(format!("version {id}")))
}

/// Sorts rows newest first with the manifest's ordering (version, then variant
/// preference); unparsable versions go last.
pub fn sort_newest_first(manifest: &ToolManifest, rows: &mut [ToolVersionRow]) {
    let variants = &manifest.spec.versioning.variants;
    let scheme = manifest.spec.versioning.scheme;
    rows.sort_by(|a, b| {
        let va = Version::parse(scheme, &a.version);
        let vb = Version::parse(scheme, &b.version);
        match (va, vb) {
            (Ok(va), Ok(vb)) => {
                let ba = Build {
                    version: &va,
                    variants: &a.variant,
                };
                let bb = Build {
                    version: &vb,
                    variants: &b.variant,
                };
                cmp_builds(&ba, &bb, variants)
            }
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            (Err(_), Err(_)) => b.version.cmp(&a.version),
        }
    });
}

/// Series (`8.3`) of a version under the manifest's `series_key`; the version itself when
/// it does not parse.
pub fn series_of(manifest: &ToolManifest, version: &str) -> String {
    let versioning = &manifest.spec.versioning;
    Version::parse(versioning.scheme, version)
        .ok()
        .and_then(|v| v.series(&versioning.series_key).ok())
        .unwrap_or_else(|| version.to_owned())
}

/// Picks an installed version by a user spec: folder name, exact version, or series
/// (`8.3` = newest `8.3.*`). Rows must be sorted newest first.
pub fn pick<'a>(
    manifest: &ToolManifest,
    rows: &'a [ToolVersionRow],
    spec: &str,
) -> Option<&'a ToolVersionRow> {
    let spec = spec.trim().trim_start_matches('v');
    rows.iter()
        .find(|r| r.canonical_name == spec)
        .or_else(|| rows.iter().find(|r| r.version == spec))
        .or_else(|| {
            rows.iter()
                .find(|r| series_of(manifest, &r.version) == spec)
        })
        .or_else(|| {
            rows.iter()
                .find(|r| r.version.starts_with(&format!("{spec}.")))
        })
}

/// A stack manifest (`kind: Stack`, docs/04 §3) of a loaded registry.
#[derive(Debug, Clone)]
pub struct StackEntry {
    pub manifest: Arc<StackManifest>,
    pub official: bool,
    pub registry: String,
}

/// The stack manifest of `id`, if a loaded registry has one.
pub fn stack(registry: &Registry, id: &ToolId) -> Option<StackEntry> {
    let entry = registry.find(id.as_str())?;
    match entry.manifest.as_ref() {
        Manifest::Stack(stack) => Some(StackEntry {
            manifest: Arc::new(stack.clone()),
            official: entry.official,
            registry: entry.registry.clone(),
        }),
        _ => None,
    }
}

/// What a catalog card shows besides the manifest text: read from manifests only (no
/// network, no database).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// `ui.color`.
    pub color: Option<String>,
    /// `spec.platforms` (stacks: the platforms every one of their tools supports;
    /// extensions: those of the tool they extend).
    pub platforms: Vec<Platform>,
    /// `install.size_estimate_mb` in bytes (stacks: the sum of their tools; `None` when no
    /// tool has an estimate).
    pub size_estimate_bytes: Option<u64>,
}

const MB: u64 = 1024 * 1024;

fn tool_facts(tool: &ToolManifest) -> Facts {
    Facts {
        color: tool.spec.ui.color.clone(),
        platforms: tool.spec.platforms.clone(),
        size_estimate_bytes: tool
            .spec
            .install
            .size_estimate_mb
            .map(|mb| u64::from(mb) * MB),
    }
}

/// [`Facts`] of a catalog entry.
pub fn facts(registry: &Registry, manifest: &Manifest) -> Facts {
    match manifest {
        Manifest::Tool(tool) => tool_facts(tool),
        Manifest::Extension(ext) => Facts {
            color: None,
            platforms: self::tool(registry, &ext.spec.extends)
                .map(|t| t.manifest.spec.platforms.clone())
                .unwrap_or_default(),
            size_estimate_bytes: None,
        },
        Manifest::Stack(stack) => {
            let mut platforms: Option<Vec<Platform>> = None;
            let mut size: Option<u64> = None;
            for item in &stack.spec.tools {
                let Some(tool) = self::tool(registry, &item.id) else {
                    continue;
                };
                let f = tool_facts(&tool.manifest);
                platforms = Some(match platforms {
                    None => f.platforms,
                    Some(p) => p.into_iter().filter(|x| f.platforms.contains(x)).collect(),
                });
                if let Some(bytes) = f.size_estimate_bytes {
                    *size.get_or_insert(0) += bytes;
                }
            }
            Facts {
                color: None,
                platforms: platforms.unwrap_or_default(),
                size_estimate_bytes: size,
            }
        }
        _ => Facts::default(),
    }
}

/// The newer version of an installed series that the registry already knows, without any
/// network: the index `latest` when it belongs to a series with an installed version older
/// than it. (`tools.update` / `tools.check_updates` ask the providers for the real answer.)
pub fn offline_update(tool: &ToolEntry, rows: &[ToolVersionRow]) -> Option<String> {
    let latest_text = tool.latest.as_deref()?;
    let versioning = &tool.manifest.spec.versioning;
    let latest = Version::parse(versioning.scheme, latest_text).ok()?;
    let series = latest.series(&versioning.series_key).ok()?;
    let newest_installed = rows
        .iter()
        .filter_map(|r| Version::parse(versioning.scheme, &r.version).ok())
        .filter(|v| v.series(&versioning.series_key).is_ok_and(|s| s == series))
        .max()?;
    (newest_installed < latest).then(|| latest_text.to_owned())
}
