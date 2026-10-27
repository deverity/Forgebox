//! The on-disk cache of a registry: `state/registry/<name>/` (docs/02 §3, docs/05 §7).
//!
//! It holds exactly what was verified: `index.yaml` plus every manifest file the index
//! lists. Writing is atomic at folder level (stage, then rename), so a crash never leaves a
//! half-written cache. Reading goes through [`crate::index::load_tree`] and therefore
//! verifies the signature and the hashes again.
//!
//! Metadata (ETag, last refresh, verification state) lives in SQLite (`registries`), not in
//! files; see [`crate::service`].
//!
//! All functions are blocking: call from `spawn_blocking`.

use std::path::{Path, PathBuf};

use forge_core::error::{ForgeError, IoResultExt, Result};

use crate::index::{INDEX_FILE, LoadedRegistry};

/// Folder of the cache of registry `name` under `base` (`state/registry/`).
pub fn cache_dir(base: &Path, name: &str) -> PathBuf {
    base.join(name)
}

/// Folder of the git checkout of registry `name` under `base`. The `.git` suffix cannot
/// collide with a registry name (names are `[a-z0-9-]`).
pub fn git_checkout_dir(base: &Path, name: &str) -> PathBuf {
    base.join(format!("{name}.git"))
}

/// Whether a cache exists for `name`.
pub fn exists(base: &Path, name: &str) -> bool {
    cache_dir(base, name).join(INDEX_FILE).is_file()
}

/// Removes leftovers of interrupted writes (`<name>.new-*`, `<name>.old-*`).
fn clean_stale(base: &Path, name: &str) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let new = format!("{name}.new-");
    let old = format!("{name}.old-");
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if file_name.starts_with(&new) || file_name.starts_with(&old) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Replaces the cache of `name` with the verified `loaded` registry.
///
/// Only one writer per name may run at a time (the service deduplicates refresh jobs).
pub fn write(base: &Path, name: &str, loaded: &LoadedRegistry) -> Result<()> {
    std::fs::create_dir_all(base).at("creating", base)?;
    clean_stale(base, name);
    let stamp = ulid::Ulid::generate();
    let stage = base.join(format!("{name}.new-{stamp}"));
    let result = write_stage(&stage, loaded).and_then(|()| swap(base, name, &stage, stamp));
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&stage);
    }
    result
}

fn write_stage(stage: &Path, loaded: &LoadedRegistry) -> Result<()> {
    std::fs::create_dir_all(stage).at("creating", stage)?;
    let index = stage.join(INDEX_FILE);
    std::fs::write(&index, &loaded.index_bytes).at("writing", &index)?;
    for (path, bytes) in &loaded.files {
        // Paths come from `validate_entry_path`: relative, no `..`, `/` separators.
        let target = stage.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).at("creating", parent)?;
        }
        std::fs::write(&target, bytes).at("writing", &target)?;
    }
    Ok(())
}

fn swap(base: &Path, name: &str, stage: &Path, stamp: ulid::Ulid) -> Result<()> {
    let target = cache_dir(base, name);
    let old = base.join(format!("{name}.old-{stamp}"));
    let had_old = target.exists();
    if had_old {
        std::fs::rename(&target, &old).at("moving aside", &target)?;
    }
    if let Err(e) = std::fs::rename(stage, &target) {
        if had_old {
            let _ = std::fs::rename(&old, &target);
        }
        return Err(ForgeError::Io {
            context: format!("activating the cache of registry {name}"),
            source: e,
        });
    }
    if had_old {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(())
}

/// Deletes the cache and the git checkout of `name`.
pub fn remove(base: &Path, name: &str) -> Result<()> {
    for dir in [cache_dir(base, name), git_checkout_dir(base, name)] {
        if dir.exists() {
            remove_dir_all_retry(&dir)?;
        }
    }
    clean_stale(base, name);
    Ok(())
}

/// `remove_dir_all` that clears the read-only attribute git sets on `.git/objects`.
fn remove_dir_all_retry(dir: &Path) -> Result<()> {
    if std::fs::remove_dir_all(dir).is_ok() {
        return Ok(());
    }
    clear_readonly(dir);
    std::fs::remove_dir_all(dir).at("removing", dir)
}

fn clear_readonly(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(meta) = std::fs::metadata(&path) {
            let mut perms = meta.permissions();
            if perms.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
                let _ = std::fs::set_permissions(&path, perms);
            }
            if meta.is_dir() {
                clear_readonly(&path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::index::{Verification, VerifiedIndex};

    fn loaded(files: BTreeMap<String, Vec<u8>>) -> LoadedRegistry {
        let text = "apiVersion: forgebox.dev/v1\nkind: Registry\nmetadata: { name: t }\nspec:\n  schema_version: 1\n  updated_at: 2026-01-01T00:00:00Z\n";
        let manifest = forge_manifest::load_str(text).unwrap();
        let forge_manifest::Manifest::Registry(index) = manifest else {
            unreachable!()
        };
        let verified = VerifiedIndex {
            bytes: text.as_bytes().to_vec(),
            index,
            verification: Verification::Unverified,
        };
        LoadedRegistry {
            index: verified.index,
            index_sha256: forge_manifest::index::sha256_hex(&verified.bytes),
            index_bytes: verified.bytes,
            verification: Verification::Unverified,
            files,
            manifests: Vec::new(),
            skipped: Vec::new(),
        }
    }

    #[test]
    fn write_replaces_the_previous_cache_and_leaves_no_stage() {
        let base = tempfile::tempdir().unwrap();
        let first = BTreeMap::from([("tools/a.yaml".to_owned(), b"a".to_vec())]);
        write(base.path(), "r", &loaded(first)).unwrap();
        assert!(exists(base.path(), "r"));
        assert_eq!(
            std::fs::read(base.path().join("r/tools/a.yaml")).unwrap(),
            b"a"
        );
        let second = BTreeMap::from([("stacks/s.yaml".to_owned(), b"s".to_vec())]);
        write(base.path(), "r", &loaded(second)).unwrap();
        assert!(!base.path().join("r/tools/a.yaml").exists());
        assert!(base.path().join("r/stacks/s.yaml").is_file());
        let names: Vec<String> = std::fs::read_dir(base.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["r"]);
        remove(base.path(), "r").unwrap();
        assert!(!exists(base.path(), "r"));
    }
}
