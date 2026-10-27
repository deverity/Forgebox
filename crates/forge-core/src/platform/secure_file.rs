//! Opening files for the elevated helper without following links planted by the user
//! (ADR-0022 obligations, ADR-0024).
//!
//! `forgebox-helper` runs as an administrator but reads and writes inside the Forgebox
//! root, which belongs to the user: a symbolic link, junction or hard link placed there
//! could turn "append to `logs/helper.log`" into "append to a system file". Every file
//! the helper touches inside the root goes through [`open_no_follow`], which never follows
//! a link in the **last** component and refuses links and multiply linked files. Links in
//! intermediate directories are the caller's business: compare [`final_path`] with the
//! expected location after opening.

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::error::Result;

/// How [`open_no_follow`] opens the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoFollow {
    /// Read an existing file (others may read and write it meanwhile).
    Read,
    /// Read an existing file; while it is open nobody may write, rename or delete it.
    ReadExclusive,
    /// Write an existing file from the start, truncating it **after** the checks.
    OverwriteExisting,
    /// Append to the file, creating it when missing.
    AppendOrCreate,
}

/// A file opened by [`open_no_follow`].
#[derive(Debug)]
pub struct NoFollowFile {
    pub file: File,
    /// Created by this call ([`NoFollow::AppendOrCreate`] only).
    pub created: bool,
}

/// Opens `path` as `mode` says, without following a reparse point (symbolic link,
/// junction, mount point) or symlink in its last component. Refused: links
/// (`forbidden`), directories (`invalid_input`), files with more than one hard link
/// (`forbidden`); a missing file is `not_found` (except for
/// [`NoFollow::AppendOrCreate`]).
pub fn open_no_follow(path: &Path, mode: NoFollow) -> Result<NoFollowFile> {
    imp::open_no_follow(path, mode)
}

/// Path of an open file as the file system resolved it (every link followed), without
/// the `\\?\` prefix.
pub fn final_path(file: &File) -> Result<PathBuf> {
    imp::final_path(file)
}

#[cfg(windows)]
mod imp {
    pub(super) use crate::platform::windows::secure_file::{final_path, open_no_follow};
}

#[cfg(not(windows))]
mod imp {
    //! Portable fallback (no Win32): checks with `symlink_metadata` before opening. Racy,
    //! acceptable until the Linux/macOS ports bring the helper (`O_NOFOLLOW`).

    use std::fs::{File, OpenOptions};
    use std::path::{Path, PathBuf};

    use super::{NoFollow, NoFollowFile};
    use crate::error::{ForgeError, IoResultExt, Result};

    pub(in crate::platform) fn open_no_follow(path: &Path, mode: NoFollow) -> Result<NoFollowFile> {
        let existing = match std::fs::symlink_metadata(path) {
            Ok(meta) => Some(meta),
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound && mode == NoFollow::AppendOrCreate =>
            {
                None
            }
            Err(e) => return Err(ForgeError::io_at("inspecting", path, e)),
        };
        if let Some(meta) = &existing {
            if meta.file_type().is_symlink() {
                return Err(ForgeError::Forbidden(format!(
                    "{} is a link",
                    path.display()
                )));
            }
            if meta.is_dir() {
                return Err(ForgeError::InvalidInput(format!(
                    "{} is a directory",
                    path.display()
                )));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if meta.nlink() != 1 {
                    return Err(ForgeError::Forbidden(format!(
                        "{} has {} hard links",
                        path.display(),
                        meta.nlink()
                    )));
                }
            }
        }
        let mut opts = OpenOptions::new();
        match mode {
            NoFollow::Read | NoFollow::ReadExclusive => opts.read(true),
            NoFollow::OverwriteExisting => opts.write(true),
            NoFollow::AppendOrCreate => opts.append(true).create(true),
        };
        let file: File = opts.open(path).at("opening", path)?;
        if mode == NoFollow::OverwriteExisting {
            file.set_len(0).at("truncating", path)?;
        }
        Ok(NoFollowFile {
            file,
            created: existing.is_none(),
        })
    }

    pub(in crate::platform) fn final_path(_file: &File) -> Result<PathBuf> {
        Err(ForgeError::UnsupportedPlatform(format!(
            "resolving the path of an open file is not implemented on {}",
            std::env::consts::OS
        )))
    }
}
