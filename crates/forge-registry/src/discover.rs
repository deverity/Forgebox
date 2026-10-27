//! Discovery of tools placed by hand in `bin/` (docs/03 §5.3, docs/08 §5-§6).
//!
//! Users copy folders from other environments (`php-8.3-Win32-vs16-x64` from Laragon)
//! into `bin/<tool>/` or straight into `bin/`. [`discover`] finds them without renaming
//! anything and reports, for each folder a manifest recognizes, the tool, version and
//! build variants. [`import_discovered`] registers them as `source: manual`.
//!
//! Everything is driven by the manifests (`spec.discover`), never by tool ids:
//!
//! 1. **Recognition:** the folder holds every file of `discover.match_files`. Under
//!    `bin/<tool>/` only that tool's manifest is tried; a loose folder is tried against
//!    all of them and, when several match, the one whose name, id or `match_files` stem
//!    equals the prefix of the folder name wins (`httpd-2.4.62-...` -> the manifest with
//!    `bin/httpd.exe`).
//! 2. **Version:** `discover.version_command` is run inside the folder (5 s timeout, no
//!    console window, in `spawn_blocking`); `version_regex` group 1 (or the whole trimmed
//!    output) is the version. If it fails, the version in the folder name is used as a
//!    secondary hint and the result gets lower confidence.
//! 3. **Variants:** `discover.variant_detect` commands first; for keys they do not
//!    settle, a value of `versioning.variants[].values` that appears as a token of the
//!    folder name (`vs16`, `x64`, `nts`) is used.
//!
//! The first token of a command must be a file inside the folder (no absolute paths, no
//! `..`, no `PATH` lookup): a manifest cannot make discovery run arbitrary programs.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};

use forge_core::ForgeError;
use forge_core::error::Result;
use forge_core::ids::ToolId;
use forge_core::paths::ForgeRoot;
use forge_core::platform::{PROBE_TIMEOUT, ProbeCommand, run_probe};
use forge_core::store::{
    NewToolVersion, StoreWriter, ToolVersionRepo, VersionSource, VersionState,
};
use forge_manifest::schema::ToolManifest;
use forge_manifest::schema::tool::Discover;
use forge_manifest::version::canonical_name;
use regex::Regex;
use tokio::task::JoinSet;

/// How sure discovery is about [`DiscoveredVersion::version`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// Only the files matched; no version could be determined.
    Low,
    /// The version comes from the folder name (the version command is missing or failed).
    Medium,
    /// The version was printed by the tool itself.
    High,
}

/// A folder in `bin/` recognized as a version of a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredVersion {
    pub tool_id: ToolId,
    /// Display name of the tool (from the manifest).
    pub tool_name: String,
    /// The folder, exactly where the user put it.
    pub path: PathBuf,
    /// `None` when neither the command nor the folder name gave one.
    pub version: Option<String>,
    /// Variant values by key (`ts`, `compiler`, `arch`).
    pub variants: BTreeMap<String, String>,
    /// What Forgebox would name this build (`8.3.21-nts-vs16-x64`); only when the version
    /// is known and every variant of the manifest was settled. Informational: the folder
    /// is never renamed.
    pub canonical_name: Option<String>,
    pub source: VersionSource,
    pub confidence: Confidence,
}

/// Folder names that are never candidates.
fn skipped_name(name: &str) -> bool {
    name.starts_with('.') || name.eq_ignore_ascii_case("current") || name.ends_with(".tmp")
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            !skipped_name(&e.file_name().to_string_lossy())
                && e.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink())
                && e.path().is_dir()
        })
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

fn folder_name(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ------------------------------------------------------------------- name hints

/// What can be read from a folder name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct NameHint {
    /// Lowercase text before the version (`php`, `httpd`, `node`); empty when absent.
    prefix: String,
    /// `8.3`, `2.4.62`, `18`.
    version: Option<String>,
    /// Lowercase words after the version (`win32`, `vs16`, `x64`).
    tokens: Vec<String>,
}

fn name_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Laragon and upstream archives: php-8.3-Win32-vs16-x64, httpd-2.4.62-win64-VS17,
        // nginx-1.22.0, mysql-8.0.30-winx64, node-v18, python-3.10; Forgebox's own
        // 8.3.21-nts-vs17-x64 has no prefix.
        Regex::new(r"(?i)^(?:(?P<prefix>[a-z][a-z_+]*)[-_ ]+)?v?(?P<ver>\d+(?:\.\d+)*)(?P<rest>[-_ .+].*)?$")
            .expect("static regex")
    })
}

fn parse_folder_name(name: &str) -> NameHint {
    let Some(caps) = name_regex().captures(name) else {
        // No version: keep the words so variants and prefixes can still be read.
        return NameHint {
            prefix: String::new(),
            version: None,
            tokens: tokens(name),
        };
    };
    NameHint {
        prefix: caps
            .name("prefix")
            .map(|m| m.as_str().to_lowercase())
            .unwrap_or_default(),
        version: Some(caps["ver"].to_owned()),
        tokens: caps
            .name("rest")
            .map(|m| tokens(m.as_str()))
            .unwrap_or_default(),
    }
}

fn tokens(s: &str) -> Vec<String> {
    s.split(['-', '_', ' ', '.', '+'])
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Words that identify `manifest` in a folder name: id, display name and the stems of its
/// `match_files` (`bin/httpd.exe` -> `httpd`, `node.exe` -> `node`).
fn aliases(manifest: &ToolManifest, discover: &Discover) -> Vec<String> {
    let mut out = vec![
        manifest.metadata.id.as_str().to_lowercase(),
        manifest.metadata.name.to_lowercase(),
    ];
    for file in &discover.match_files {
        if let Some(stem) = Path::new(file).file_stem() {
            out.push(stem.to_string_lossy().to_lowercase());
        }
    }
    out
}

// ---------------------------------------------------------------- commands

/// Splits a manifest command: whitespace separates words, double quotes group them and
/// `\"` inside quotes is a quote. Other backslashes are literal (Windows paths).
fn split_command(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut in_quotes = false;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                in_word = true;
            }
            '\\' if in_quotes && chars.peek() == Some(&'"') => {
                current.push('"');
                chars.next();
            }
            c if c.is_whitespace() && !in_quotes => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            c => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(current);
    }
    words
}

/// Builds the probe for `command` inside `dir`; `None` if the program is not a plain
/// relative path to an existing file of the folder.
fn probe_for(dir: &Path, command: &str) -> Option<ProbeCommand> {
    let mut words = split_command(command).into_iter();
    let program = words.next()?;
    let relative = Path::new(&program);
    if relative.is_absolute()
        || !relative
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    let exe = dir.join(relative);
    if !exe.is_file() {
        return None;
    }
    Some(ProbeCommand {
        exe,
        args: words.map(Into::into).collect(),
        cwd: dir.to_owned(),
    })
}

/// Runs `command` in `dir`; `None` on any failure or non-zero exit.
fn run_command(dir: &Path, command: &str) -> Option<String> {
    let probe = probe_for(dir, command)?;
    match run_probe(&probe, PROBE_TIMEOUT) {
        Ok(out) if out.exit_code == Some(0) => Some(out.combined()),
        Ok(out) => {
            tracing::debug!(dir = %dir.display(), command, code = ?out.exit_code, "discover command failed");
            None
        }
        Err(e) => {
            tracing::debug!(dir = %dir.display(), command, error = %e, "discover command failed");
            None
        }
    }
}

fn looks_like_version(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_digit()) && !s.chars().any(char::is_whitespace)
}

/// Version from the command output: group 1 of `regex`, or the trimmed output.
fn extract_version(output: &str, regex: Option<&Regex>) -> Option<String> {
    let version = match regex {
        Some(re) => re.captures(output)?.get(1)?.as_str().trim().to_owned(),
        None => output.trim().to_owned(),
    };
    looks_like_version(&version).then_some(version)
}

// ------------------------------------------------------------------- scanning

/// A folder plus the manifest (index) that recognized it.
#[derive(Debug, Clone)]
struct Candidate {
    dir: PathBuf,
    manifest: usize,
}

/// Whether every `match_files` entry exists under `dir`.
fn files_match(dir: &Path, discover: &Discover) -> bool {
    !discover.match_files.is_empty()
        && discover.match_files.iter().all(|f| {
            let rel = Path::new(f);
            rel.components().all(|c| matches!(c, Component::Normal(_))) && dir.join(rel).is_file()
        })
}

/// Picks the manifest for a loose folder among those whose files match.
fn choose_loose(dir: &Path, manifests: &[ToolManifest], matching: &[usize]) -> Option<usize> {
    match matching {
        [] => None,
        [one] => Some(*one),
        many => {
            let hint = parse_folder_name(&folder_name(dir));
            let named: Vec<usize> = many
                .iter()
                .copied()
                .filter(|&i| {
                    manifests[i]
                        .spec
                        .discover
                        .as_ref()
                        .is_some_and(|d| aliases(&manifests[i], d).contains(&hint.prefix))
                })
                .collect();
            match named.as_slice() {
                [one] => Some(*one),
                _ => {
                    tracing::debug!(dir = %dir.display(), "ambiguous folder, several manifests match");
                    None
                }
            }
        }
    }
}

fn candidates(bin: &Path, manifests: &[ToolManifest]) -> Vec<Candidate> {
    let matching = |dir: &Path| -> Vec<usize> {
        manifests
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                m.spec
                    .discover
                    .as_ref()
                    .is_some_and(|d| files_match(dir, d))
            })
            .map(|(i, _)| i)
            .collect()
    };
    let loose = |dir: &Path| choose_loose(dir, manifests, &matching(dir));

    let mut out = Vec::new();
    for entry in subdirs(bin) {
        let name = folder_name(&entry);
        let tool_dir = manifests
            .iter()
            .position(|m| m.metadata.id.as_str() == name && m.spec.discover.is_some());
        if let Some(index) = tool_dir {
            // bin/<tool>/<folder>: only this tool's manifest applies.
            let discover = manifests[index]
                .spec
                .discover
                .as_ref()
                .expect("checked above");
            out.extend(
                subdirs(&entry)
                    .into_iter()
                    .filter(|d| files_match(d, discover))
                    .map(|dir| Candidate {
                        dir,
                        manifest: index,
                    }),
            );
        } else if manifests.iter().any(|m| m.metadata.id.as_str() == name) {
            // A tool without discover rules: nothing to recognize under it.
        } else if let Some(manifest) = loose(&entry) {
            out.push(Candidate {
                dir: entry,
                manifest,
            });
        } else {
            // Unknown folder (a Laragon style `bin/php-pack/` or similar): look one level down.
            out.extend(
                subdirs(&entry)
                    .into_iter()
                    .filter_map(|dir| loose(&dir).map(|manifest| Candidate { dir, manifest })),
            );
        }
    }
    out
}

// ----------------------------------------------------------------- evaluation

fn evaluate(candidate: &Candidate, manifest: &ToolManifest) -> Option<DiscoveredVersion> {
    let discover = manifest.spec.discover.as_ref()?;
    let dir = &candidate.dir;
    let hint = parse_folder_name(&folder_name(dir));

    let regex = discover.version_regex.as_deref().and_then(|r| {
        Regex::new(r)
            .inspect_err(|e| tracing::warn!(tool = %manifest.metadata.id, error = %e, "invalid version_regex"))
            .ok()
    });
    let from_tool = discover
        .version_command
        .as_deref()
        .and_then(|cmd| run_command(dir, cmd))
        .and_then(|out| extract_version(&out, regex.as_ref()));
    let (version, confidence) = match (from_tool, hint.version.clone()) {
        (Some(v), _) => (Some(v), Confidence::High),
        (None, Some(v)) => (Some(v), Confidence::Medium),
        (None, None) => (None, Confidence::Low),
    };

    let order: Vec<&str> = manifest
        .spec
        .versioning
        .variants
        .iter()
        .map(|v| v.key.as_str())
        .collect();
    let mut variants = BTreeMap::new();
    for variant in &manifest.spec.versioning.variants {
        let detected = discover
            .variant_detect
            .get(&variant.key)
            .and_then(|detect| {
                let re = Regex::new(&detect.regex).ok()?;
                let out = run_command(dir, &detect.command)?;
                let raw = re.captures(&out)?.get(1)?.as_str().to_owned();
                detect.map.get(&raw).cloned()
            });
        let value = detected.or_else(|| {
            variant
                .values
                .iter()
                .find(|v| hint.tokens.iter().any(|t| t.eq_ignore_ascii_case(v)))
                .cloned()
        });
        if let Some(value) = value {
            variants.insert(variant.key.clone(), value);
        }
    }

    let canonical = version
        .as_deref()
        .filter(|_| order.iter().all(|k| variants.contains_key(*k)))
        .map(|v| canonical_name(v, &variants, &order));
    Some(DiscoveredVersion {
        tool_id: manifest.metadata.id.clone(),
        tool_name: manifest.metadata.name.clone(),
        path: dir.clone(),
        version,
        variants,
        canonical_name: canonical,
        source: VersionSource::Manual,
        confidence,
    })
}

/// Finds tools placed by hand in `<root>/bin` (see the module docs). Never fails: folders
/// that cannot be read or recognized are skipped. Version commands run in
/// `spawn_blocking`, in parallel, each with a 5 s timeout. Sorted by tool and path.
pub async fn discover(root: &ForgeRoot, manifests: &[ToolManifest]) -> Vec<DiscoveredVersion> {
    let manifests: Arc<[ToolManifest]> = manifests.to_vec().into();
    let bin = root.bin();
    let found = {
        let manifests = Arc::clone(&manifests);
        tokio::task::spawn_blocking(move || candidates(&bin, &manifests)).await
    };
    let Ok(found) = found else {
        tracing::warn!("discover scan panicked");
        return Vec::new();
    };

    let mut set = JoinSet::new();
    for candidate in found {
        let manifests = Arc::clone(&manifests);
        set.spawn_blocking(move || evaluate(&candidate, &manifests[candidate.manifest]));
    }
    let mut out = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Some(found)) => out.push(found),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "discover task failed"),
        }
    }
    out.sort_by(|a, b| (&a.tool_id, &a.path).cmp(&(&b.tool_id, &b.path)));
    out
}

// --------------------------------------------------------------------- import

/// Outcome of [`import_discovered`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Newly registered folders.
    pub imported: Vec<PathBuf>,
    /// Folders that were already registered (same path or folder name for the tool).
    pub already_registered: Vec<PathBuf>,
    /// Recognized folders without a version, which cannot be registered.
    pub unknown_version: Vec<PathBuf>,
}

/// Registers `discovered` as `source: manual` versions. Idempotent: registered folders
/// (same path or same folder name for the tool, whatever their source) are left alone.
/// The folder keeps its name; `tool_versions.canonical_name` holds the folder name and
/// `path` where it is. One atomic operation (savepoint) on the store's writer thread.
pub async fn import_discovered(
    writer: &StoreWriter,
    discovered: &[DiscoveredVersion],
) -> Result<ImportReport> {
    let discovered = discovered.to_vec();
    writer
        .execute(move |conn| {
            let repo = ToolVersionRepo::new(conn);
            let mut report = ImportReport::default();
            for found in &discovered {
                let Some(version) = found.version.clone() else {
                    report.unknown_version.push(found.path.clone());
                    continue;
                };
                let new = NewToolVersion {
                    tool_id: found.tool_id.clone(),
                    tool_name: found.tool_name.clone(),
                    version,
                    canonical_name: folder_name(&found.path),
                    variant: found.variants.clone(),
                    path: found.path.to_string_lossy().into_owned(),
                    state: VersionState::Installed,
                    source: VersionSource::Manual,
                };
                if new.canonical_name.is_empty() {
                    return Err(ForgeError::InvalidInput(format!(
                        "discovered path {} has no folder name",
                        found.path.display()
                    )));
                }
                if repo.register_if_absent(&new)?.is_some() {
                    report.imported.push(found.path.clone());
                } else {
                    report.already_registered.push(found.path.clone());
                }
            }
            Ok(report)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_manifest_commands() {
        assert_eq!(
            split_command(r#"php.exe -r "echo PHP_VERSION;""#),
            ["php.exe", "-r", "echo PHP_VERSION;"]
        );
        assert_eq!(split_command("bin/httpd.exe -v"), ["bin/httpd.exe", "-v"]);
        assert_eq!(
            split_command(r#"x.exe "a \"b\" c" ''"#),
            ["x.exe", r#"a "b" c"#, "''"]
        );
        assert!(split_command("   ").is_empty());
    }

    #[test]
    fn program_must_stay_inside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.exe"), b"").unwrap();
        assert!(probe_for(dir.path(), "a.exe -v").is_some());
        assert!(probe_for(dir.path(), "../a.exe").is_none());
        assert!(probe_for(dir.path(), "missing.exe").is_none());
        let abs = dir.path().join("a.exe");
        assert!(probe_for(dir.path(), &abs.to_string_lossy()).is_none());
        assert!(probe_for(dir.path(), "cmd").is_none());
    }

    #[test]
    fn parses_laragon_and_upstream_folder_names() {
        let cases = [
            (
                "php-8.3-Win32-vs16-x64",
                "php",
                "8.3",
                &["win32", "vs16", "x64"][..],
            ),
            (
                "php-7.3-Win32-VC15-x64",
                "php",
                "7.3",
                &["win32", "vc15", "x64"],
            ),
            (
                "httpd-2.4.62-win64-VS17",
                "httpd",
                "2.4.62",
                &["win64", "vs17"],
            ),
            ("nginx-1.22.0", "nginx", "1.22.0", &[]),
            ("mysql-8.0.30-winx64", "mysql", "8.0.30", &["winx64"]),
            ("mariadb-11.8.9-winx64", "mariadb", "11.8.9", &["winx64"]),
            ("node-v18", "node", "18", &[]),
            ("python-3.10", "python", "3.10", &[]),
            ("8.3.21-nts-vs17-x64", "", "8.3.21", &["nts", "vs17", "x64"]),
        ];
        for (name, prefix, version, toks) in cases {
            let hint = parse_folder_name(name);
            assert_eq!(hint.prefix, prefix, "{name}");
            assert_eq!(hint.version.as_deref(), Some(version), "{name}");
            assert_eq!(hint.tokens, toks, "{name}");
        }
        let none = parse_folder_name("my tools");
        assert_eq!(none.version, None);
    }

    #[test]
    fn version_extraction() {
        let re = Regex::new(r"nginx/(\d+\.\d+\.\d+)").unwrap();
        assert_eq!(
            extract_version("nginx version: nginx/1.22.0\n", Some(&re)).as_deref(),
            Some("1.22.0")
        );
        assert_eq!(
            extract_version("8.3.21\r\n", None).as_deref(),
            Some("8.3.21")
        );
        assert_eq!(extract_version("", None), None);
        assert_eq!(extract_version("error: not found", None), None);
        assert_eq!(extract_version("no match", Some(&re)), None);
    }
}
