//! `update.migrate` (docs/04 §1, docs/05 §4): what is carried from the replaced version
//! to the new one in the same line, e.g. extensions the user added to `ext/` (Xdebug).
//!
//! `copy_dir {from, to, only_missing}`: `from` is relative to the old version folder and
//! `to` to the new one (no absolute paths, drives, `..` or variables). Files are copied
//! recursively; with `only_missing` a file that the new version already has is kept (the
//! new build's own `php_curl.dll` wins over the old one). Links and junctions are not
//! followed. A missing source folder copies nothing. Blocking: run in `spawn_blocking`.

use std::path::Path;

use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::paths::posix_string;
use forge_manifest::schema::tool::{CopyDirAction, MigrateAction};

use crate::layout::relative_in;

/// Runs `actions` from `old_dir` to `new_dir`. Returns the copied files relative to the
/// new version folder, with `/`.
pub(crate) fn run(
    old_dir: &Path,
    new_dir: &Path,
    actions: &[MigrateAction],
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<String>> {
    let mut copied = Vec::new();
    for (index, action) in actions.iter().enumerate() {
        match action {
            MigrateAction::CopyDir(copy) => {
                copy_dir(old_dir, new_dir, copy, cancelled, &mut copied).map_err(|e| {
                    crate::layout::annotate(e, &format!("update.migrate[{index}] (copy_dir)"))
                })?;
            }
        }
    }
    Ok(copied)
}

fn copy_dir(
    old_dir: &Path,
    new_dir: &Path,
    copy: &CopyDirAction,
    cancelled: &dyn Fn() -> bool,
    copied: &mut Vec<String>,
) -> Result<()> {
    let invalid = |what: &str, value: &str| {
        ForgeError::InvalidInput(format!(
            "{what} {value:?} must be a relative path inside the version"
        ))
    };
    let from = relative_in(old_dir, &copy.from).ok_or_else(|| invalid("from", &copy.from))?;
    let to = relative_in(new_dir, &copy.to).ok_or_else(|| invalid("to", &copy.to))?;
    if !from.is_dir() {
        tracing::debug!(from = %from.display(), "update.migrate: nothing to copy");
        return Ok(());
    }
    let mut stack = vec![from.clone()];
    while let Some(dir) = stack.pop() {
        if cancelled() {
            return Err(ForgeError::Cancelled("update.migrate".into()));
        }
        for entry in std::fs::read_dir(&dir).at("listing", &dir)? {
            let entry = entry.at("listing", &dir)?;
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).at("reading", &path)?;
            if meta.file_type().is_symlink() {
                continue;
            }
            let rel = path.strip_prefix(&from).map_err(|_| {
                ForgeError::Internal(format!("{} escaped the copy", path.display()))
            })?;
            let dest = to.join(rel);
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if copy.only_missing && std::fs::symlink_metadata(&dest).is_ok() {
                continue;
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).at("creating", parent)?;
            }
            std::fs::copy(&path, &dest).at("copying", &path)?;
            let shown = dest
                .strip_prefix(new_dir)
                .map_or_else(|_| posix_string(&dest), posix_string);
            copied.push(shown);
        }
    }
    copied.sort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(from: &str, to: &str, only_missing: bool) -> MigrateAction {
        MigrateAction::CopyDir(CopyDirAction {
            from: from.into(),
            to: to.into(),
            only_missing,
        })
    }

    #[test]
    fn copies_only_missing_files_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (dir.path().join("8.3.36"), dir.path().join("8.3.37"));
        std::fs::create_dir_all(old.join("ext/sub")).unwrap();
        std::fs::create_dir_all(new.join("ext")).unwrap();
        std::fs::write(old.join("ext/php_xdebug.dll"), "xdebug").unwrap();
        std::fs::write(old.join("ext/php_curl.dll"), "old curl").unwrap();
        std::fs::write(old.join("ext/sub/data.bin"), "data").unwrap();
        std::fs::write(new.join("ext/php_curl.dll"), "new curl").unwrap();

        let never = || false;
        let copied = run(&old, &new, &[action("ext/", "ext/", true)], &never).unwrap();
        assert_eq!(copied, ["ext/php_xdebug.dll", "ext/sub/data.bin"]);
        assert_eq!(
            std::fs::read_to_string(new.join("ext/php_curl.dll")).unwrap(),
            "new curl"
        );
        assert_eq!(
            std::fs::read_to_string(new.join("ext/php_xdebug.dll")).unwrap(),
            "xdebug"
        );

        // Without only_missing the old file overwrites.
        let copied = run(&old, &new, &[action("ext", "ext", false)], &never).unwrap();
        assert_eq!(copied.len(), 3);
        assert_eq!(
            std::fs::read_to_string(new.join("ext/php_curl.dll")).unwrap(),
            "old curl"
        );

        // Missing source: nothing.
        assert!(
            run(&old, &new, &[action("nope/", "nope/", true)], &never)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn refuses_paths_outside_the_versions() {
        let dir = tempfile::tempdir().unwrap();
        let never = || false;
        for (from, to) in [
            ("../x", "ext"),
            ("ext", "C:/Windows"),
            ("%TEMP%", "ext"),
            ("ext", "{{ paths.etc }}"),
        ] {
            let err = run(dir.path(), dir.path(), &[action(from, to, true)], &never).unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{from} -> {to}: {err}");
        }
    }
}
