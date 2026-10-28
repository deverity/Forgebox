//! From the staging directory to the destination: `strip_root`, case sensitivity of the
//! staging directory, and the move that makes the destination appear all at once.
//!
//! The move is a directory rename (same volume, `MoveFileEx` semantics). When the staging
//! directory is on another volume the tree is copied into a hidden sibling of the
//! destination (`.<name>.forgebox-partial-<id>`), checked file by file (paths and sizes)
//! and only then renamed into place, so `dest` is never seen half written. An existing
//! destination is replaced only on request: it is renamed aside first
//! (`.<name>.forgebox-old-<id>`) and put back if the new tree cannot be moved in.
//! Renames are retried briefly on access/sharing violations (antivirus scanners hold new
//! executables open for a moment after they are written).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::platform::CaseSensitiveFs;
use forge_manifest::schema::tool::StripRoot;

/// Attempts of a rename that fails with an access or sharing violation.
const RENAME_ATTEMPTS: u32 = 8;
/// Wait before the n-th retry is `RENAME_BACKOFF * n`.
const RENAME_BACKOFF: Duration = Duration::from_millis(50);

/// Removes the staging directory when dropped (success, failure or panic).
pub(crate) struct StageGuard(pub PathBuf);

impl Drop for StageGuard {
    fn drop(&mut self) {
        remove_tree_logged(&self.0);
    }
}

/// `remove_dir_all` that only logs failures (cleanup must never mask the real result).
pub(crate) fn remove_tree_logged(path: &Path) {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(path = %path.display(), "could not remove directory: {e}"),
    }
}

/// Gives the staging directory the case sensitivity of the destination's parent, so that
/// the flag moving with the directory does not make `bin/<tool>/<version>` case-sensitive
/// (`tmp/` is case-sensitive by default, `bin/` is not; docs/15). Best effort: volumes or
/// OSes without the flag are left alone.
pub(crate) fn match_case_sensitivity(cs: &dyn CaseSensitiveFs, stage: &Path, dest_parent: &Path) {
    let Ok(wanted) = cs.get(dest_parent) else {
        return;
    };
    match cs.get(stage) {
        Ok(current) if current == wanted => {}
        Ok(_) => {
            if let Err(e) = cs.set(stage, wanted) {
                tracing::warn!(
                    stage = %stage.display(),
                    wanted,
                    "could not match the case sensitivity of the destination: {e}"
                );
            }
        }
        Err(_) => {}
    }
}

/// Directory to move into place and the top-level folder removed, if any.
pub(crate) fn resolve_strip(stage: &Path, mode: StripRoot) -> Result<(PathBuf, Option<String>)> {
    if mode == StripRoot::No {
        return Ok((stage.to_path_buf(), None));
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(stage).at("listing", stage)? {
        entries.push(entry.at("listing", stage)?);
    }
    if let [single] = entries.as_slice() {
        if single
            .file_type()
            .at("inspecting", &single.path())?
            .is_dir()
        {
            let name = single.file_name().to_string_lossy().into_owned();
            return Ok((single.path(), Some(name)));
        }
    }
    match mode {
        StripRoot::Yes => Err(ForgeError::InvalidInput(format!(
            "strip_root is true but the archive does not have a single top-level folder \
             ({} top-level entries)",
            entries.len()
        ))),
        _ => Ok((stage.to_path_buf(), None)),
    }
}

fn is_transient(e: &io::Error) -> bool {
    // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION.
    e.kind() == io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

fn is_cross_device(e: &io::Error) -> bool {
    // ERROR_NOT_SAME_DEVICE on Windows, EXDEV elsewhere.
    e.kind() == io::ErrorKind::CrossesDevices || (cfg!(windows) && e.raw_os_error() == Some(17))
}

/// `fs::rename` retried on transient access/sharing violations.
pub(crate) fn rename_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 1;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if is_transient(&e) && attempt < RENAME_ATTEMPTS => {
                tracing::debug!(from = %from.display(), attempt, "rename blocked, retrying: {e}");
                std::thread::sleep(RENAME_BACKOFF * attempt);
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// `.<name>.forgebox-<what>-<id>` next to `dest`.
fn sibling(dest: &Path, what: &str, id: &str) -> Result<PathBuf> {
    let parent = dest.parent().ok_or_else(|| {
        ForgeError::InvalidInput(format!("{} has no parent directory", dest.display()))
    })?;
    let name = dest
        .file_name()
        .ok_or_else(|| ForgeError::InvalidInput(format!("{} has no folder name", dest.display())))?
        .to_string_lossy();
    Ok(parent.join(format!(".{name}.forgebox-{what}-{id}")))
}

/// Moves the tree `source` to `dest` (see the module docs). Returns whether a copy was
/// needed. `force_copy` exercises the cross-volume path in tests.
pub(crate) fn install(
    source: &Path,
    dest: &Path,
    replace_existing: bool,
    id: &str,
    force_copy: bool,
) -> Result<bool> {
    let old = if fs::symlink_metadata(dest).is_ok() {
        if !replace_existing {
            return Err(ForgeError::Conflict(format!(
                "{} already exists",
                dest.display()
            )));
        }
        let old = sibling(dest, "old", id)?;
        rename_retry(dest, &old).map_err(|e| {
            ForgeError::Conflict(format!(
                "cannot move the existing {} aside (files in use?): {e}",
                dest.display()
            ))
        })?;
        Some(old)
    } else {
        None
    };

    match move_tree(source, dest, id, force_copy) {
        Ok(copied) => {
            if let Some(old) = old {
                remove_tree_logged(&old);
            }
            Ok(copied)
        }
        Err(e) => {
            if let Some(old) = old {
                if let Err(back) = rename_retry(&old, dest) {
                    tracing::error!(
                        dest = %dest.display(),
                        kept = %old.display(),
                        "could not restore the previous installation: {back}"
                    );
                }
            }
            Err(e)
        }
    }
}

fn move_tree(source: &Path, dest: &Path, id: &str, force_copy: bool) -> Result<bool> {
    if !force_copy {
        match rename_retry(source, dest) {
            Ok(()) => return Ok(false),
            Err(e) if is_cross_device(&e) => {
                tracing::info!(
                    source = %source.display(),
                    dest = %dest.display(),
                    "staging is on another volume: copying"
                );
            }
            Err(e) => {
                return Err(ForgeError::io(
                    format!("moving {} to {}", source.display(), dest.display()),
                    e,
                ));
            }
        }
    }
    let partial = sibling(dest, "partial", id)?;
    let copied = copy_tree(source, &partial).and_then(|()| same_tree(source, &partial));
    if let Err(e) = copied {
        remove_tree_logged(&partial);
        return Err(e);
    }
    if let Err(e) = rename_retry(&partial, dest) {
        remove_tree_logged(&partial);
        return Err(ForgeError::io(
            format!("moving {} to {}", partial.display(), dest.display()),
            e,
        ));
    }
    remove_tree_logged(source);
    Ok(true)
}

/// Recursive copy of directories and regular files (`fs::copy` keeps the modification
/// time on Windows). Anything else is refused: staging never holds links.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir(to).at("creating", to)?;
    for entry in fs::read_dir(from).at("listing", from)? {
        let entry = entry.at("listing", from)?;
        let kind = entry.file_type().at("inspecting", &entry.path())?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target).at("copying", &entry.path())?;
        } else {
            return Err(ForgeError::Internal(format!(
                "unexpected link in staging: {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

/// Relative path → size (`None` for directories) of every entry under `root`.
fn inventory(root: &Path) -> Result<BTreeMap<PathBuf, Option<u64>>> {
    let mut out = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).at("listing", &dir)? {
            let entry = entry.at("listing", &dir)?;
            let path = entry.path();
            let meta = entry.metadata().at("inspecting", &path)?;
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            if meta.is_dir() {
                out.insert(rel, None);
                pending.push(path);
            } else {
                out.insert(rel, Some(meta.len()));
            }
        }
    }
    Ok(out)
}

/// Checks that `copy` has exactly the entries and file sizes of `original`.
fn same_tree(original: &Path, copy: &Path) -> Result<()> {
    let a = inventory(original)?;
    let b = inventory(copy)?;
    if a != b {
        let missing = a.keys().find(|k| b.get(*k) != a.get(*k));
        return Err(ForgeError::Internal(format!(
            "copy of {} into {} does not match the original{}",
            original.display(),
            copy.display(),
            missing
                .map(|m| format!(" (first difference: {})", m.display()))
                .unwrap_or_default()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(root: &Path) {
        fs::create_dir_all(root.join("bin/sub")).unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::write(root.join("bin/tool.exe"), b"MZ tool").unwrap();
        fs::write(root.join("bin/sub/lib.dll"), b"MZ lib").unwrap();
        fs::write(root.join("README"), b"").unwrap();
    }

    #[test]
    fn copy_fallback_produces_identical_tree_and_cleans_up() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("stage");
        tree(&source);
        let reference = inventory(&source).unwrap();
        let dest = base.path().join("bin").join("tool").join("1.0.0");
        fs::create_dir_all(dest.parent().unwrap()).unwrap();

        let copied = install(&source, &dest, false, "ID1", true).unwrap();
        assert!(copied);
        assert_eq!(inventory(&dest).unwrap(), reference);
        assert!(!source.exists(), "staging removed after the copy");
        let leftovers: Vec<_> = fs::read_dir(dest.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("1.0.0")]);
    }

    #[test]
    fn rename_moves_and_refuses_existing_destination() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("stage");
        tree(&source);
        let dest = base.path().join("dest");
        assert!(!install(&source, &dest, false, "ID2", false).unwrap());
        assert!(dest.join("bin/tool.exe").is_file());

        let again = base.path().join("stage2");
        tree(&again);
        let err = install(&again, &dest, false, "ID3", false).unwrap_err();
        assert_eq!(err.code(), "conflict");
        assert!(again.exists(), "nothing moved");
    }

    #[test]
    fn replace_swaps_the_whole_tree() {
        let base = tempfile::tempdir().unwrap();
        let dest = base.path().join("dest");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("old-only.txt"), b"old").unwrap();
        let source = base.path().join("stage");
        tree(&source);
        install(&source, &dest, true, "ID4", false).unwrap();
        assert!(!dest.join("old-only.txt").exists());
        assert!(dest.join("bin/tool.exe").is_file());
        let names: Vec<_> = fs::read_dir(base.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["dest"], "old tree removed");
    }

    #[test]
    fn failed_replacement_restores_the_previous_tree() {
        let base = tempfile::tempdir().unwrap();
        let dest = base.path().join("dest");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("keep.txt"), b"old").unwrap();
        // Source that does not exist: the move fails after the old tree was set aside.
        let err = install(&base.path().join("missing"), &dest, true, "ID5", false).unwrap_err();
        assert_eq!(err.code(), "io", "{err}");
        assert_eq!(fs::read(dest.join("keep.txt")).unwrap(), b"old");
    }

    #[test]
    fn copy_check_detects_differences() {
        let base = tempfile::tempdir().unwrap();
        let a = base.path().join("a");
        let b = base.path().join("b");
        tree(&a);
        tree(&b);
        same_tree(&a, &b).unwrap();
        fs::write(b.join("bin/tool.exe"), b"MZ").unwrap();
        let err = same_tree(&a, &b).unwrap_err();
        assert!(err.to_string().contains("tool.exe"), "{err}");
    }

    #[test]
    fn strip_modes() {
        let base = tempfile::tempdir().unwrap();
        let single = base.path().join("single");
        fs::create_dir_all(single.join("nginx-1.30.6/conf")).unwrap();
        let (dir, stripped) = resolve_strip(&single, StripRoot::Auto).unwrap();
        assert_eq!(dir, single.join("nginx-1.30.6"));
        assert_eq!(stripped.as_deref(), Some("nginx-1.30.6"));
        assert_eq!(
            resolve_strip(&single, StripRoot::No).unwrap(),
            (single.clone(), None)
        );
        assert!(resolve_strip(&single, StripRoot::Yes).unwrap().1.is_some());

        let flat = base.path().join("flat");
        fs::create_dir_all(flat.join("ext")).unwrap();
        fs::write(flat.join("php.exe"), b"MZ").unwrap();
        assert_eq!(
            resolve_strip(&flat, StripRoot::Auto).unwrap(),
            (flat.clone(), None)
        );
        let err = resolve_strip(&flat, StripRoot::Yes).unwrap_err();
        assert_eq!(err.code(), "invalid_input");

        // A single top-level *file* is never stripped.
        let one_file = base.path().join("one-file");
        fs::create_dir_all(&one_file).unwrap();
        fs::write(one_file.join("tool.exe"), b"MZ").unwrap();
        assert_eq!(resolve_strip(&one_file, StripRoot::Auto).unwrap().1, None);
    }
}
