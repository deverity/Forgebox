//! Junctions (`IO_REPARSE_TAG_MOUNT_POINT`) for `bin/<tool>/current` (ADR-0005).
//!
//! Creation and reading use the `junction` crate (MIT), which does exactly
//! `CreateFileW(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)` +
//! `DeviceIoControl(FSCTL_SET_REPARSE_POINT / FSCTL_GET_REPARSE_POINT)`; it is built
//! without its `unstable_admin` feature so it never asks for backup/restore privileges.
//! Junctions need neither admin nor Developer Mode, unlike symlinks.
//!
//! Atomic replacement: a new junction is created at `<link>.tmp` and renamed over
//! `<link>`. `std::fs::rename` on Windows replaces an existing junction (it falls
//! back to `FileRenameInfoEx` with POSIX semantics, Windows 10 1709+), while still
//! refusing to replace a real directory. Covered by the tests below.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::{ForgeError, IoResultExt, Result};
use crate::platform::LinkFs;

/// [`LinkFs`] backed by NTFS junctions.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsLinkFs;

fn tmp_link(link: &Path) -> PathBuf {
    let mut name = link.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

fn absolute_target(target: &Path) -> Result<PathBuf> {
    let target = std::path::absolute(target).at("resolving", target)?;
    match fs::metadata(&target) {
        Ok(meta) if meta.is_dir() => Ok(target),
        Ok(_) => Err(ForgeError::InvalidInput(format!(
            "junction target {} is not a directory",
            target.display()
        ))),
        Err(e) => Err(ForgeError::io_at("reading junction target", &target, e)),
    }
}

fn junction_exists(path: &Path) -> Result<bool> {
    // `symlink_metadata` does not follow the link; only reparse-point directories can
    // be junctions, which avoids opening ordinary folders exclusively.
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(ForgeError::io_at("inspecting", path, e)),
    };
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let attrs = meta.file_attributes();
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT == 0 || attrs & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Ok(false);
    }
    junction::exists(path).at("inspecting junction", path)
}

fn create(link: &Path, target: &Path) -> Result<()> {
    match fs::symlink_metadata(link) {
        Ok(_) => {
            return Err(ForgeError::Conflict(format!(
                "{} already exists",
                link.display()
            )));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(ForgeError::io_at("inspecting", link, e)),
    }
    if let Err(e) = junction::create(target, link) {
        // `junction::create` makes the directory first; do not leave it behind.
        if fs::symlink_metadata(link).is_ok() && !junction_exists(link).unwrap_or(false) {
            let _ = fs::remove_dir(link);
        }
        return Err(ForgeError::io_at("creating junction", link, e));
    }
    Ok(())
}

impl LinkFs for WindowsLinkFs {
    fn create_junction(&self, link: &Path, target: &Path) -> Result<()> {
        let target = absolute_target(target)?;
        create(link, &target)
    }

    fn read_junction(&self, link: &Path) -> Result<PathBuf> {
        if fs::symlink_metadata(link).is_err() {
            return Err(ForgeError::NotFound(link.display().to_string()));
        }
        if !junction_exists(link)? {
            return Err(ForgeError::InvalidInput(format!(
                "{} is not a junction",
                link.display()
            )));
        }
        junction::get_target(link).at("reading junction", link)
    }

    fn is_junction(&self, path: &Path) -> Result<bool> {
        junction_exists(path)
    }

    fn remove_junction(&self, link: &Path) -> Result<()> {
        if fs::symlink_metadata(link).is_err() {
            return Err(ForgeError::NotFound(link.display().to_string()));
        }
        if !junction_exists(link)? {
            return Err(ForgeError::InvalidInput(format!(
                "refusing to remove {}: not a junction",
                link.display()
            )));
        }
        // RemoveDirectoryW on a junction deletes the link itself, never the target.
        fs::remove_dir(link).at("removing junction", link)
    }

    fn replace_junction(&self, link: &Path, target: &Path) -> Result<()> {
        let target = absolute_target(target)?;
        match fs::symlink_metadata(link) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return create(link, &target),
            Err(e) => return Err(ForgeError::io_at("inspecting", link, e)),
            Ok(_) if !junction_exists(link)? => {
                return Err(ForgeError::Conflict(format!(
                    "{} exists and is not a junction",
                    link.display()
                )));
            }
            Ok(_) => {}
        }

        let tmp = tmp_link(link);
        if junction_exists(&tmp)? {
            // Leftover of an interrupted switch.
            fs::remove_dir(&tmp).at("removing stale junction", &tmp)?;
        }
        create(&tmp, &target)?;
        if let Err(e) = fs::rename(&tmp, link) {
            let _ = fs::remove_dir(&tmp);
            return Err(ForgeError::io_at("switching junction", link, e));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn same_dir(a: &Path, b: &Path) -> bool {
        fs::canonicalize(a).unwrap() == fs::canonicalize(b).unwrap()
    }

    struct Fixture {
        _base: tempfile::TempDir,
        v1: PathBuf,
        v2: PathBuf,
        link: PathBuf,
    }

    fn fixture() -> Fixture {
        let base = tempfile::tempdir().unwrap();
        let v1 = base.path().join("8.3.37");
        let v2 = base.path().join("8.4.14");
        fs::create_dir_all(&v1).unwrap();
        fs::create_dir_all(&v2).unwrap();
        fs::write(v1.join("php83.txt"), "8.3").unwrap();
        fs::write(v2.join("php84.txt"), "8.4").unwrap();
        let link = base.path().join("current");
        Fixture {
            _base: base,
            v1,
            v2,
            link,
        }
    }

    #[test]
    fn create_read_and_list_through_junction() {
        let fx = fixture();
        let fs_ = WindowsLinkFs;
        fs_.create_junction(&fx.link, &fx.v1).unwrap();
        assert!(fs_.is_junction(&fx.link).unwrap());
        assert!(!fs_.is_junction(&fx.v1).unwrap());
        assert!(!fs_.is_junction(&fx.link.join("missing")).unwrap());
        assert!(same_dir(&fs_.read_junction(&fx.link).unwrap(), &fx.v1));
        assert_eq!(names(&fx.link), vec!["php83.txt"]);
        assert_eq!(
            fs::read_to_string(fx.link.join("php83.txt")).unwrap(),
            "8.3"
        );

        let err = fs_.create_junction(&fx.link, &fx.v2).unwrap_err();
        assert_eq!(err.code(), "conflict");
    }

    #[test]
    fn replace_is_atomic_rename_and_keeps_targets() {
        let fx = fixture();
        let fs_ = WindowsLinkFs;
        // Creates when missing.
        fs_.replace_junction(&fx.link, &fx.v1).unwrap();
        assert!(same_dir(&fs_.read_junction(&fx.link).unwrap(), &fx.v1));

        fs_.replace_junction(&fx.link, &fx.v2).unwrap();
        assert!(same_dir(&fs_.read_junction(&fx.link).unwrap(), &fx.v2));
        assert_eq!(names(&fx.link), vec!["php84.txt"]);
        assert!(
            !tmp_link(&fx.link).exists(),
            "temporary junction renamed away"
        );
        assert_eq!(names(&fx.v1), vec!["php83.txt"], "old target untouched");

        // A stale `current.tmp` from an interrupted switch is cleaned up.
        fs_.create_junction(&tmp_link(&fx.link), &fx.v2).unwrap();
        fs_.replace_junction(&fx.link, &fx.v1).unwrap();
        assert!(same_dir(&fs_.read_junction(&fx.link).unwrap(), &fx.v1));
        assert!(!tmp_link(&fx.link).exists());
    }

    #[test]
    fn remove_deletes_only_the_link() {
        let fx = fixture();
        let fs_ = WindowsLinkFs;
        fs_.create_junction(&fx.link, &fx.v1).unwrap();
        fs_.remove_junction(&fx.link).unwrap();
        assert!(!fx.link.exists());
        assert_eq!(names(&fx.v1), vec!["php83.txt"]);
        assert_eq!(
            fs_.remove_junction(&fx.link).unwrap_err().code(),
            "not_found"
        );
    }

    #[test]
    fn refuses_to_touch_real_directories() {
        let fx = fixture();
        let fs_ = WindowsLinkFs;
        assert_eq!(
            fs_.remove_junction(&fx.v1).unwrap_err().code(),
            "invalid_input"
        );
        assert_eq!(
            fs_.read_junction(&fx.v1).unwrap_err().code(),
            "invalid_input"
        );
        let err = fs_.replace_junction(&fx.v1, &fx.v2).unwrap_err();
        assert_eq!(err.code(), "conflict");
        assert_eq!(names(&fx.v1), vec!["php83.txt"]);
    }

    #[test]
    fn target_must_be_an_existing_directory() {
        let fx = fixture();
        let fs_ = WindowsLinkFs;
        let missing = fx.v1.with_file_name("nope");
        assert_eq!(
            fs_.create_junction(&fx.link, &missing).unwrap_err().code(),
            "not_found"
        );
        let file = fx.v1.join("php83.txt");
        assert_eq!(
            fs_.create_junction(&fx.link, &file).unwrap_err().code(),
            "invalid_input"
        );
        assert!(!fx.link.exists());
    }
}
