//! Import of a Laragon installation (docs/08 §6, `forge import laragon`).
//!
//! Two steps, so a dry run and the real thing run the same code:
//!
//! 1. [`LaragonImporter::scan`] reads the installation (never writes in it) and returns an
//!    [`ImportPlan`]: tool versions of `bin/` (recognized by the registry manifests via
//!    `forge_registry::discover_in`, no per-tool code), database folders of `data/`,
//!    virtual hosts of `etc/apache2/sites-enabled/auto.*.conf` and
//!    `etc/nginx/sites-enabled/*.conf`, the parked folders and the preferences of
//!    `usr/laragon.ini`.
//! 2. [`LaragonImporter::apply`] (module `apply`) registers what the plan holds.
//!
//! **Where it lives.** The importer needs site drivers (this crate), tool discovery
//! (`forge-registry`) and the store (`forge-core`), and `forge-sites` is the only one of
//! the three that depends on the other two without a cycle; the daemon only wires the
//! pieces (`system.import_laragon`).
//!
//! **Hosts.** Laragon writes one vhost file per name (`app.test` and, from older
//! versions, `app.local`). Files that share a document root become one site: the name
//! with the TLD of `HostnameFormat` is the host and the others are aliases (the importer
//! keeps the first whose TLD Forgebox serves, see `apply`). `_` becomes `-`
//! (`SIMPROS_MATRIZ.test` → `simpros-matriz.test`); wildcard aliases are left out.
//!
//! **Moved installations.** `laragon.ini` and the vhost files hold absolute paths of
//! where Laragon was installed. When the folder being scanned is somewhere else (a copy,
//! a backup, a test fixture) the paths below the recorded root are read from the scanned
//! folder.
//!
//! **Active PHP.** The junction `bin/php/php` (Laragon's `switch-php`) is what actually
//! runs; `[php] Version` of the ini is the fallback. Sites pin that series.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::paths::posix_string;
use forge_core::platform::is_reparse_point;
use forge_core::runtime::spawn_blocking_timeout;
use forge_core::store::validate_instance_name;
use forge_manifest::schema::ToolManifest;
use forge_manifest::schema::tool::ServiceKind;
use forge_manifest::version::Version;
use forge_registry::{Confidence, DiscoveredVersion, discover_in, parse_folder_name};

use super::conf::{ParsedConf, parse_apache, parse_nginx};
use super::ini::Ini;
use super::messages as msg;
use super::plan::{
    ImportPlan, PlannedDatabase, PlannedParked, PlannedPrefs, PlannedSite, PlannedTool, Skipped,
    kind,
};
use crate::drivers;
use crate::site::normalize_alias;

/// Budget of the file-reading part of a scan (the version commands have their own).
const SCAN_TIMEOUT: Duration = Duration::from_secs(120);

/// Reads Laragon installations and imports them (see the module docs).
#[derive(Debug, Clone)]
pub struct LaragonImporter {
    pub(super) manifests: Arc<[ToolManifest]>,
}

impl LaragonImporter {
    /// An importer that recognizes tools with `manifests` (the loaded registries).
    pub fn new(manifests: impl Into<Arc<[ToolManifest]>>) -> Self {
        Self {
            manifests: manifests.into(),
        }
    }

    /// Reads the Laragon installation at `path` without changing anything in it. Runs the
    /// version command of each recognized tool inside its own folder (as discovery does,
    /// 5 s timeout each) and reads files in `spawn_blocking`.
    pub async fn scan(&self, path: &Path) -> Result<ImportPlan> {
        let root = path.to_path_buf();
        let check = root.clone();
        spawn_blocking_timeout("checking the Laragon folder", SCAN_TIMEOUT, move || {
            check_laragon(&check)
        })
        .await?;
        let discovered = discover_in(&root.join("bin"), &self.manifests).await;
        let manifests = Arc::clone(&self.manifests);
        spawn_blocking_timeout(
            "reading the Laragon installation",
            SCAN_TIMEOUT,
            move || build_plan(&root, &manifests, discovered),
        )
        .await
    }
}

fn check_laragon(root: &Path) -> Result<()> {
    if !root.is_dir() {
        return Err(ForgeError::NotFound(format!("folder {}", root.display())));
    }
    let looks_like_laragon = root.join("usr").join("laragon.ini").is_file()
        || root.join("bin").is_dir()
        || root.join("www").is_dir();
    if looks_like_laragon {
        Ok(())
    } else {
        Err(ForgeError::InvalidInput(
            msg::not_laragon(&posix_string(root)).1,
        ))
    }
}

// --------------------------------------------------------------------- paths

/// Comparison form of a path: `/`, no trailing `/`, no verbatim prefix, lowercase ASCII.
pub(super) fn norm(path: &str) -> String {
    let text = path.replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    text.trim_end_matches('/').to_ascii_lowercase()
}

/// The scanned folder and the root recorded inside its files.
struct Source {
    root: PathBuf,
    /// [`norm`] of the folder Laragon was installed in, when it is not `root`.
    original: Option<String>,
}

impl Source {
    /// Path of a file as Laragon wrote it, read from the scanned folder.
    fn map(&self, raw: &str) -> PathBuf {
        let unified = raw.trim().replace('\\', "/");
        let unified = unified.trim_end_matches('/');
        let Some(original) = &self.original else {
            return PathBuf::from(unified);
        };
        let lower = unified.to_ascii_lowercase();
        if lower == *original {
            return self.root.clone();
        }
        match lower.strip_prefix(&format!("{original}/")) {
            Some(rest) => {
                // ASCII lowercase keeps lengths, so the same tail of `unified` is the
                // path with its original case.
                let tail = &unified[unified.len() - rest.len()..];
                self.root.join(tail)
            }
            None => PathBuf::from(unified),
        }
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map_or_else(|_| posix_string(path), posix_string)
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.') && e.path().is_dir())
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

fn read_text(path: &Path) -> std::io::Result<String> {
    // Delphi writes ANSI; names and paths of interest are ASCII.
    Ok(String::from_utf8_lossy(&std::fs::read(path)?).into_owned())
}

fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| is_reparse_point(&m))
}

// ---------------------------------------------------------------------- scan

fn build_plan(
    root: &Path,
    manifests: &[ToolManifest],
    discovered: Vec<DiscoveredVersion>,
) -> Result<ImportPlan> {
    let ini = read_text(&root.join("usr").join("laragon.ini"))
        .map(|t| Ini::parse(&t))
        .unwrap_or_default();
    let original = original_root(&ini).filter(|o| *o != norm(&posix_string(root)));
    let source = Source {
        root: root.to_path_buf(),
        original: original.clone(),
    };
    let mut skipped: Vec<Skipped> = Vec::new();

    let tools = plan_tools(&source, &ini, discovered, &mut skipped);
    let (active_php, php_series) = active_pool_series(manifests, &tools);
    let prefs_roots = document_roots(&source, &ini);
    let tld = ini
        .get("preferences", "HostnameFormat")
        .and_then(|f| f.rsplit_once('.'))
        .map(|(_, tld)| tld.trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty() && !t.contains('{'));

    let databases = plan_databases(&source, manifests, &tools, &mut skipped);
    let sites = plan_sites(
        &source,
        &prefs_roots,
        tld.as_deref(),
        php_series.as_deref(),
        &mut skipped,
    );

    let mut parked = Vec::new();
    let mut document_roots = Vec::new();
    for dir in &prefs_roots {
        if dir.is_dir() {
            let projects = subdirs(dir).len();
            let has_content = std::fs::read_dir(dir).is_ok_and(|mut e| e.next().is_some());
            document_roots.push(posix_string(dir));
            parked.push(PlannedParked {
                dir: posix_string(dir),
                projects,
                offer_case_conversion: has_content,
            });
        } else {
            skipped.push(Skipped {
                kind: kind::PARKED_DIR,
                name: posix_string(dir),
                reason: msg::docroot_missing().1,
            });
        }
    }

    skipped.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
    skipped.dedup();
    Ok(ImportPlan {
        source: posix_string(root),
        original_root: original.and_then(|_| original_root_text(&ini)),
        prefs: PlannedPrefs {
            tld,
            document_roots,
            active_php,
            php_series,
        },
        tools,
        databases,
        sites,
        parked,
        skipped,
    })
}

/// [`norm`] of the folder `laragon.ini` says Laragon lives in (the parent of `DataDir`).
fn original_root(ini: &Ini) -> Option<String> {
    let data = norm(ini.get("preferences", "DataDir")?);
    let parent = data.strip_suffix("/data").unwrap_or(&data);
    (!parent.is_empty()).then(|| parent.to_owned())
}

/// The recorded root as written in the ini, with `/`.
fn original_root_text(ini: &Ini) -> Option<String> {
    let data = ini
        .get("preferences", "DataDir")?
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_owned();
    let cut = data.len().checked_sub("/data".len())?;
    if data[cut..].eq_ignore_ascii_case("/data") {
        Some(data[..cut].to_owned())
    } else {
        Some(data)
    }
}

/// Folders Laragon serves projects from: `DocumentRootList`, else `[apache] DocumentRoot`,
/// else `<root>/www`.
fn document_roots(source: &Source, ini: &Ini) -> Vec<PathBuf> {
    let mut raw: Vec<String> = ini
        .get("preferences", "DocumentRootList")
        .map(|list| {
            list.split('|')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if raw.is_empty() {
        if let Some(apache) = ini.get("apache", "DocumentRoot") {
            raw.push(apache.to_owned());
        }
    }
    let mut out: Vec<PathBuf> = raw.iter().map(|r| source.map(r)).collect();
    if out.is_empty() {
        out.push(source.root.join("www"));
    }
    let mut seen = BTreeSet::new();
    out.retain(|p| seen.insert(norm(&posix_string(p))));
    out
}

// --------------------------------------------------------------------- tools

/// Links (`bin/php/php` → `php-8.2-...`) by lowercase tool folder: `(link name, target
/// folder name)`.
fn links_in_bin(bin: &Path) -> BTreeMap<String, Vec<(String, String)>> {
    let mut out: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for tool_dir in subdirs(bin) {
        if is_link(&tool_dir) {
            continue;
        }
        for child in subdirs(&tool_dir) {
            if !is_link(&child) {
                continue;
            }
            if let Ok(target) = std::fs::read_link(&child) {
                out.entry(file_name(&tool_dir).to_ascii_lowercase())
                    .or_default()
                    .push((file_name(&child).to_ascii_lowercase(), file_name(&target)));
            }
        }
    }
    out
}

fn plan_tools(
    source: &Source,
    ini: &Ini,
    discovered: Vec<DiscoveredVersion>,
    skipped: &mut Vec<Skipped>,
) -> Vec<PlannedTool> {
    let bin = source.root.join("bin");
    let links = links_in_bin(&bin);
    let mut tools = Vec::new();
    let mut recognized_dirs: BTreeSet<String> = BTreeSet::new();
    for found in discovered {
        let folder = file_name(&found.path);
        let tool_dir = found
            .path
            .parent()
            .map(file_name)
            .unwrap_or_default()
            .to_ascii_lowercase();
        // The first folder below `bin/` that holds it, for the "unknown folder" report.
        if let Ok(rel) = found.path.strip_prefix(&bin) {
            if let Some(first) = rel.components().next() {
                recognized_dirs.insert(first.as_os_str().to_string_lossy().to_ascii_lowercase());
            }
        }
        let label = format!("{}/{folder}", found.tool_id);
        if is_link(&found.path) {
            let target = links
                .get(&tool_dir)
                .and_then(|l| l.iter().find(|(n, _)| n.eq_ignore_ascii_case(&folder)))
                .map(|(_, t)| t.clone())
                .unwrap_or_default();
            skipped.push(Skipped {
                kind: kind::TOOL_VERSION,
                name: label,
                reason: msg::tool_link(&target).1,
            });
            continue;
        }
        let Some(version) = found.version.clone() else {
            skipped.push(Skipped {
                kind: kind::TOOL_VERSION,
                name: label,
                reason: msg::tool_no_version().1,
            });
            continue;
        };
        if found.confidence < Confidence::High {
            skipped.push(Skipped {
                kind: kind::TOOL_VERSION,
                name: label,
                reason: msg::tool_unconfirmed(&version).1,
            });
            continue;
        }
        let by_link = links.get(&tool_dir).map(|list| {
            // `bin/php/php` is the junction of `switch-php`; with several links the one
            // named like the tool folder is the live one.
            list.iter()
                .find(|(name, _)| *name == tool_dir)
                .or_else(|| list.first())
                .is_some_and(|(_, target)| target.eq_ignore_ascii_case(&folder))
        });
        let by_ini = || {
            [found.tool_id.as_str(), tool_dir.as_str()]
                .iter()
                .any(|section| {
                    ini.get(section, "Version")
                        .is_some_and(|v| v.eq_ignore_ascii_case(&folder))
                })
        };
        tools.push(PlannedTool {
            tool_id: found.tool_id.clone(),
            tool_name: found.tool_name.clone(),
            version,
            active: by_link.unwrap_or_else(by_ini),
            folder,
            path: posix_string(&found.path),
            variants: found.variants.clone(),
            canonical_name: found.canonical_name.clone(),
        });
    }
    // `bin/` folders nothing was recognized in.
    for dir in subdirs(&bin) {
        let name = file_name(&dir);
        if recognized_dirs.contains(&name.to_ascii_lowercase()) {
            continue;
        }
        skipped.push(Skipped {
            kind: kind::TOOL,
            name,
            reason: msg::bin_unknown().1,
        });
    }
    tools.sort_by(|a, b| (&a.tool_id, &a.path).cmp(&(&b.tool_id, &b.path)));
    tools
}

/// Series of the active version of the tool that runs PHP sites (the manifest with
/// `service.kind: pool`, so no tool id is spelled here): `(folder, series)`.
fn active_pool_series(
    manifests: &[ToolManifest],
    tools: &[PlannedTool],
) -> (Option<String>, Option<String>) {
    let mut pools: Vec<&ToolManifest> = manifests
        .iter()
        .filter(|m| m.spec.service.kind == ServiceKind::Pool)
        .collect();
    pools.sort_by(|a, b| a.metadata.id.cmp(&b.metadata.id));
    for manifest in pools {
        let active = tools
            .iter()
            .find(|t| t.tool_id == manifest.metadata.id && t.active);
        if let Some(tool) = active {
            let versioning = &manifest.spec.versioning;
            let series = Version::parse(versioning.scheme, &tool.version)
                .ok()
                .and_then(|v| v.series(&versioning.series_key).ok());
            return (Some(tool.folder.clone()), series);
        }
    }
    (None, None)
}

// ----------------------------------------------------------------- databases

/// `8.0.46` serves a data folder of `8` and of `8.0`, not of `8.4`.
fn version_serves(version: &str, wanted: &str) -> bool {
    version == wanted
        || version
            .strip_prefix(wanted)
            .is_some_and(|rest| rest.starts_with('.'))
}

fn plan_databases(
    source: &Source,
    manifests: &[ToolManifest],
    tools: &[PlannedTool],
    skipped: &mut Vec<Skipped>,
) -> Vec<PlannedDatabase> {
    let mut out = Vec::new();
    for dir in subdirs(&source.root.join("data")) {
        let name = file_name(&dir);
        let hint = parse_folder_name(&name);
        let manifest = manifests.iter().find(|m| {
            m.spec
                .service
                .daemon
                .as_ref()
                .is_some_and(|d| d.data.is_some())
                && (m.metadata.id.as_str() == hint.prefix
                    || m.metadata.name.to_ascii_lowercase() == hint.prefix)
        });
        let (Some(manifest), Some(wanted)) = (manifest, hint.version.as_deref()) else {
            skipped.push(Skipped {
                kind: kind::DATABASE_DIR,
                name,
                reason: msg::data_unknown_tool().1,
            });
            continue;
        };
        if std::fs::read_dir(&dir).is_ok_and(|mut e| e.next().is_none()) {
            skipped.push(Skipped {
                kind: kind::DATABASE_DIR,
                name,
                reason: msg::data_empty().1,
            });
            continue;
        }
        if validate_instance_name(&name).is_err() {
            skipped.push(Skipped {
                kind: kind::DATABASE_DIR,
                name,
                reason: msg::data_unknown_tool().1,
            });
            continue;
        }
        let scheme = manifest.spec.versioning.scheme;
        let mut candidates: Vec<&PlannedTool> = tools
            .iter()
            .filter(|t| t.tool_id == manifest.metadata.id && version_serves(&t.version, wanted))
            .collect();
        // Active first, then the newest.
        candidates.sort_by(|a, b| {
            b.active.cmp(&a.active).then_with(|| {
                match (
                    Version::parse(scheme, &a.version),
                    Version::parse(scheme, &b.version),
                ) {
                    (Ok(x), Ok(y)) => y.cmp(&x),
                    _ => b.version.cmp(&a.version),
                }
            })
        });
        let Some(chosen) = candidates.first() else {
            skipped.push(Skipped {
                kind: kind::DATABASE_DIR,
                name,
                reason: msg::data_no_version(manifest.metadata.id.as_str(), wanted).1,
            });
            continue;
        };
        out.push(PlannedDatabase {
            name,
            tool_id: manifest.metadata.id.clone(),
            data_dir: posix_string(&dir),
            version_folder: chosen.folder.clone(),
            version: chosen.version.clone(),
        });
    }
    out
}

// --------------------------------------------------------------------- sites

/// A virtual host with names already normalized.
struct RawVhost {
    hosts: Vec<String>,
    root: PathBuf,
    ssl: bool,
    file: String,
    notes: Vec<String>,
}

/// `Name.TEST` → `name.test`, `SIMPROS_MATRIZ.test` → `simpros-matriz.test`; `None` when
/// no valid host can be made. The note says what changed beyond case.
fn clean_name(raw: &str) -> Option<(String, Option<String>)> {
    let lower = raw.trim().to_ascii_lowercase();
    let fixed = lower.replace('_', "-");
    let value = normalize_alias(&fixed).ok()?.value;
    let note = (fixed != lower).then(|| msg::host_renamed(raw.trim(), &value).1);
    Some((value, note))
}

fn read_vhosts(source: &Source, skipped: &mut Vec<Skipped>) -> Vec<RawVhost> {
    let mut out = Vec::new();
    let mut files: Vec<(PathBuf, bool)> = Vec::new();
    for (dir, apache) in [
        (
            source
                .root
                .join("etc")
                .join("apache2")
                .join("sites-enabled"),
            true,
        ),
        (
            source.root.join("etc").join("nginx").join("sites-enabled"),
            false,
        ),
    ] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("conf"))
            })
            .collect();
        found.sort();
        files.extend(found.into_iter().map(|p| (p, apache)));
    }
    for (path, apache) in files {
        let name = file_name(&path);
        let relative = source.relative(&path);
        if apache && !name.to_ascii_lowercase().starts_with("auto.") {
            skipped.push(Skipped {
                kind: kind::VHOST,
                name: relative,
                reason: msg::vhost_not_auto().1,
            });
            continue;
        }
        let text = match read_text(&path) {
            Ok(t) => t,
            Err(e) => {
                skipped.push(Skipped {
                    kind: kind::VHOST,
                    name: relative,
                    reason: msg::vhost_unreadable(&e.to_string()).1,
                });
                continue;
            }
        };
        let ParsedConf { vhosts, ignored } = if apache {
            parse_apache(&text)
        } else {
            parse_nginx(&text)
        };
        for why in ignored {
            skipped.push(Skipped {
                kind: kind::VHOST,
                name: relative.clone(),
                reason: msg::vhost_no_site(&why).1,
            });
        }
        for vhost in vhosts {
            let mut notes = Vec::new();
            let Some((host, note)) = clean_name(&vhost.server_name) else {
                skipped.push(Skipped {
                    kind: kind::VHOST,
                    name: vhost.server_name.clone(),
                    reason: msg::host_unusable(&vhost.server_name).1,
                });
                continue;
            };
            notes.extend(note);
            let mut hosts = vec![host];
            for alias in &vhost.aliases {
                match clean_name(alias) {
                    Some((a, note)) => {
                        notes.extend(note);
                        if !hosts.contains(&a) {
                            hosts.push(a);
                        }
                    }
                    None => notes.push(msg::host_unusable(alias).1),
                }
            }
            if !vhost.wildcards.is_empty() {
                notes.push(msg::wildcards_dropped().1);
            }
            out.push(RawVhost {
                hosts,
                root: source.map(&vhost.document_root),
                ssl: vhost.ssl,
                file: relative.clone(),
                notes,
            });
        }
    }
    out
}

/// The project folder of a document root: the first folder below the parked folder that
/// holds it, else the root itself.
fn project_dir(root: &Path, document_roots: &[PathBuf]) -> PathBuf {
    let key = norm(&posix_string(root));
    for base in document_roots {
        let base_key = norm(&posix_string(base));
        if let Some(rest) = key.strip_prefix(&format!("{base_key}/")) {
            if let Some(first) = rest.split('/').next().filter(|f| !f.is_empty()) {
                // Original case of that component.
                let text = posix_string(root);
                let start = base_key.len() + 1;
                if text.len() >= start + first.len() {
                    return base.join(&text[start..start + first.len()]);
                }
            }
        }
    }
    root.to_path_buf()
}

struct Group {
    root: PathBuf,
    hosts: Vec<String>,
    ssl: bool,
    sources: Vec<String>,
    notes: Vec<String>,
}

fn plan_sites(
    source: &Source,
    document_roots: &[PathBuf],
    tld: Option<&str>,
    php_series: Option<&str>,
    skipped: &mut Vec<Skipped>,
) -> Vec<PlannedSite> {
    let vhosts = read_vhosts(source, skipped);
    let mut groups: Vec<(String, Group)> = Vec::new();
    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    for vhost in vhosts {
        let key = norm(&posix_string(&vhost.root));
        // A name that an earlier file already declared belongs to that file.
        let (fresh, repeated): (Vec<String>, Vec<String>) = vhost
            .hosts
            .iter()
            .cloned()
            .partition(|h| !owner.contains_key(h));
        for host in &repeated {
            let first = owner.get(host).cloned().unwrap_or_default();
            skipped.push(Skipped {
                kind: kind::VHOST,
                name: host.clone(),
                reason: msg::duplicate_vhost(host, &first).1,
            });
        }
        if fresh.is_empty() {
            continue;
        }
        for host in &fresh {
            owner.insert(host.clone(), vhost.file.clone());
        }
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, group)) => {
                group.hosts.extend(fresh);
                group.ssl |= vhost.ssl;
                if !group.sources.contains(&vhost.file) {
                    group.sources.push(vhost.file);
                }
                for note in vhost.notes {
                    if !group.notes.contains(&note) {
                        group.notes.push(note);
                    }
                }
            }
            None => groups.push((
                key,
                Group {
                    root: vhost.root,
                    hosts: fresh,
                    ssl: vhost.ssl,
                    sources: vec![vhost.file],
                    notes: vhost.notes,
                },
            )),
        }
    }

    let mut sites = Vec::new();
    for (_, mut group) in groups {
        // Preferred TLD first (stable: the rest keeps the order of the files).
        if let Some(tld) = tld {
            let suffix = format!(".{tld}");
            group.hosts.sort_by_key(|h| !h.ends_with(&suffix));
        }
        if !group.root.is_dir() {
            skipped.push(Skipped {
                kind: kind::SITE,
                name: group.hosts[0].clone(),
                reason: msg::root_missing(&posix_string(&group.root)).1,
            });
            continue;
        }
        let project = project_dir(&group.root, document_roots);
        let Ok(detected) = drivers::detect(&project, None) else {
            continue;
        };
        // A document root that is the project folder itself gets the driver's own root
        // (Laravel `public/`); an explicit sub-folder is the user's choice.
        let root = if norm(&posix_string(&group.root)) == norm(&posix_string(&project)) {
            detected.root.clone()
        } else {
            group.root.clone()
        };
        let driver = detected.driver;
        sites.push(PlannedSite {
            hosts: group.hosts,
            root: posix_string(&root),
            project_dir: posix_string(&project),
            driver: driver.id().to_owned(),
            secure: group.ssl,
            php_series: (driver.kind() == forge_core::store::SiteKind::Php)
                .then(|| php_series.map(str::to_owned))
                .flatten(),
            sources: group.sources,
            notes: group.notes,
        });
    }
    sites.sort_by(|a, b| a.hosts[0].cmp(&b.hosts[0]));
    sites
}
