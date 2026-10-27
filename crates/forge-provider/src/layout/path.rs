//! The user `PATH` (docs/03 §5.2, docs/08 §3, ADR-0005, ADR-0025): added **once**
//! (onboarding, or when a CLI tool is installed after it), never touched by `forge use`.
//!
//! [`wanted_entries`] lists what Forgebox wants on the `PATH` for a set of tools and the
//! `[path]` preferences: `bin/<tool>/current` plus each `install.path` subfolder
//! (`"."` is the junction itself) of every tool with `current_link` and a non-empty
//! `install.path`, and `bin/shims` when `prefs.shims`. [`ensure`] adds the missing ones
//! through [`UserEnv`] (each entry at most once; equivalent spellings count as present)
//! and is idempotent: a second call changes nothing and broadcasts nothing. In portable
//! mode (`portable.flag`) nothing outside the root is written (docs/08 §2.2).
//! [`remove_root_entries`] takes every entry under `<root>\bin` away again (uninstall).
//!
//! Blocking (registry and broadcast through [`UserEnv`]): call from `spawn_blocking`.

use forge_core::config::PathConfig;
use forge_core::error::Result;
use forge_core::paths::ForgeRoot;
use forge_core::platform::{UserEnv, normalize_path_entry};
use forge_manifest::schema::ToolManifest;
use forge_manifest::schema::tool::Layout;
use serde::{Deserialize, Serialize};

/// What [`ensure`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathReport {
    /// Entries added by this call.
    pub added: Vec<String>,
    /// Entries that were already there.
    pub present: Vec<String>,
    /// Portable mode: nothing was written.
    pub skipped_portable: bool,
}

/// `entry` as a Windows path string (`\` separators).
fn windows_string(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('/', "\\")
}

/// Entries Forgebox wants on the user `PATH`, in order (module docs).
pub fn wanted_entries(
    root: &ForgeRoot,
    manifests: &[&ToolManifest],
    prefs: &PathConfig,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |entry: String| {
        let key = normalize_path_entry(&entry, &|_| None);
        if !out
            .iter()
            .any(|e| normalize_path_entry(e, &|_| None) == key)
        {
            out.push(entry);
        }
    };
    if prefs.current_links {
        for manifest in manifests {
            let install = &manifest.spec.install;
            if !install.current_link || install.layout != Layout::Versioned {
                continue;
            }
            let current = root.tool_current_link(&manifest.metadata.id);
            for sub in &install.path {
                let sub = sub.trim_matches('/');
                let dir = if sub.is_empty() || sub == "." {
                    current.clone()
                } else {
                    current.join(sub.replace('/', "\\"))
                };
                push(windows_string(&dir));
            }
        }
    }
    if prefs.shims {
        push(windows_string(&root.bin_shims()));
    }
    out
}

/// Adds the [`wanted_entries`] that are missing (module docs).
pub fn ensure(
    root: &ForgeRoot,
    manifests: &[&ToolManifest],
    prefs: &PathConfig,
    env: &dyn UserEnv,
) -> Result<PathReport> {
    let mut report = PathReport::default();
    if root.is_portable() {
        report.skipped_portable = true;
        tracing::info!("portable mode: the user PATH is not modified");
        return Ok(report);
    }
    let wanted = wanted_entries(root, manifests, prefs);
    // Entries are put first; adding in reverse keeps the wanted order at the front.
    for entry in wanted.iter().rev() {
        if env.add_path_entry(entry)? {
            report.added.push(entry.clone());
        } else {
            report.present.push(entry.clone());
        }
    }
    report.added.reverse();
    report.present.reverse();
    Ok(report)
}

/// Removes every user `PATH` entry under `<root>\bin` (uninstall, docs/08 §3). Returns
/// the removed entries. In portable mode nothing is touched.
pub fn remove_root_entries(root: &ForgeRoot, env: &dyn UserEnv) -> Result<Vec<String>> {
    if root.is_portable() {
        return Ok(Vec::new());
    }
    let lookup = |name: &str| std::env::var(name).ok();
    let bin = normalize_path_entry(&windows_string(&root.bin()), &lookup);
    let prefix = format!("{bin}\\");
    env.remove_path_entries(&|entry| {
        let entry = normalize_path_entry(entry, &lookup);
        entry == bin || entry.starts_with(&prefix)
    })
}

#[cfg(test)]
mod tests {
    use forge_core::platform::MemoryUserEnv;
    use forge_manifest::Manifest;

    use super::*;

    fn tool(id: &str, install: &str) -> ToolManifest {
        let text = format!(
            "apiVersion: forgebox.dev/v1\nkind: Tool\n\
             metadata: {{ id: {id}, name: X, category: devtool }}\n\
             spec:\n  platforms: [windows-x64]\n  source:\n    providers:\n      - type: nodejs-dist\n\
             \x20 install:\n{install}"
        );
        match forge_manifest::load_str(&text).unwrap() {
            Manifest::Tool(t) => t,
            _ => unreachable!(),
        }
    }

    #[test]
    fn wanted_entries_follow_manifests_and_prefs() {
        let root = ForgeRoot::new(r"C:\fb").unwrap();
        let php = tool("php", "    path: [\".\"]\n");
        let git = tool("git", "    path: [cmd, \"usr/bin\"]\n");
        let nginx = tool("nginx", "    current_link: false\n    path: [\".\"]\n");
        let lib = tool("lib", "    path: []\n");
        let all = [&php, &git, &nginx, &lib];
        let prefs = PathConfig::default();
        assert_eq!(
            wanted_entries(&root, &all, &prefs),
            [
                r"C:\fb\bin\php\current",
                r"C:\fb\bin\git\current\cmd",
                r"C:\fb\bin\git\current\usr\bin"
            ]
        );
        let shims_only = PathConfig {
            current_links: false,
            shims: true,
        };
        assert_eq!(
            wanted_entries(&root, &all, &shims_only),
            [r"C:\fb\bin\shims"]
        );
    }

    #[test]
    fn ensure_is_idempotent_and_keeps_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let php = tool("php", "    path: [\".\"]\n");
        let node = tool("nodejs", "    path: [\".\"]\n");
        let env = MemoryUserEnv::with_path(r"C:\Windows;C:\tools");
        let prefs = PathConfig {
            current_links: true,
            shims: true,
        };
        let first = ensure(&root, &[&php, &node], &prefs, &env).unwrap();
        assert_eq!(first.added.len(), 3);
        let entries = env.path_entries().unwrap();
        assert_eq!(
            entries[0],
            windows_string(&root.tool_current_link(&php.metadata.id))
        );
        assert_eq!(entries[2], windows_string(&root.bin_shims()));
        assert_eq!(&entries[3..], [r"C:\Windows", r"C:\tools"]);
        let notified = env.notifications();

        let second = ensure(&root, &[&php, &node], &prefs, &env).unwrap();
        assert!(second.added.is_empty());
        assert_eq!(second.present, first.added);
        assert_eq!(env.notifications(), notified, "nothing broadcast");
        assert_eq!(env.path_entries().unwrap(), entries, "single entries");

        let removed = remove_root_entries(&root, &env).unwrap();
        assert_eq!(removed.len(), 3);
        assert_eq!(env.raw_path(), r"C:\Windows;C:\tools");
    }

    #[test]
    fn portable_mode_never_touches_the_user_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("portable.flag"), "").unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let php = tool("php", "    path: [\".\"]\n");
        let env = MemoryUserEnv::with_path(r"C:\Windows");
        let report = ensure(&root, &[&php], &PathConfig::default(), &env).unwrap();
        assert!(report.skipped_portable && report.added.is_empty());
        assert_eq!(env.raw_path(), r"C:\Windows");
        assert_eq!(env.notifications(), 0);
        assert!(remove_root_entries(&root, &env).unwrap().is_empty());
    }
}
