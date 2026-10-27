//! [`open_no_follow`](crate::platform::secure_file::open_no_follow) for Windows:
//! `FILE_FLAG_OPEN_REPARSE_POINT` opens a reparse point itself instead of its target, then
//! `GetFileInformationByHandle` tells whether the opened object is a reparse point, a
//! directory or a file with several hard links. Truncation (for
//! [`NoFollow::OverwriteExisting`]) happens only after those checks.

use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use ::windows::Win32::Foundation::HANDLE;
use ::windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED, FILE_SHARE_READ,
    GETFINALPATHNAMEBYHANDLE_FLAGS, GetFileInformationByHandle, GetFinalPathNameByHandleW,
    VOLUME_NAME_DOS,
};

use super::process::os_error;
use crate::error::{ForgeError, IoResultExt, Result};
use crate::paths::plain_path;
use crate::platform::secure_file::{NoFollow, NoFollowFile};

fn options(mode: NoFollow) -> OpenOptions {
    let mut opts = OpenOptions::new();
    opts.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    match mode {
        NoFollow::Read => {
            opts.read(true);
        }
        NoFollow::ReadExclusive => {
            opts.read(true).share_mode(FILE_SHARE_READ.0);
        }
        NoFollow::OverwriteExisting => {
            opts.write(true);
        }
        NoFollow::AppendOrCreate => {
            opts.append(true);
        }
    }
    opts
}

fn info(file: &File, path: &Path) -> Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: valid handle owned by `file`; `info` is writable.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(|e| ForgeError::io_at("inspecting", path, os_error(&e)))?;
    Ok(info)
}

pub(crate) fn open_no_follow(path: &Path, mode: NoFollow) -> Result<NoFollowFile> {
    let (file, created) = if mode == NoFollow::AppendOrCreate {
        let mut create = options(mode);
        create.create_new(true);
        match create.open(path) {
            Ok(file) => (file, true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                (options(mode).open(path).at("opening", path)?, false)
            }
            Err(e) => return Err(ForgeError::io_at("creating", path, e)),
        }
    } else {
        (options(mode).open(path).at("opening", path)?, false)
    };
    let info = info(&file, path)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(ForgeError::Forbidden(format!(
            "{} is a link (reparse point); refusing to follow it",
            path.display()
        )));
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
        return Err(ForgeError::InvalidInput(format!(
            "{} is a directory",
            path.display()
        )));
    }
    if info.nNumberOfLinks != 1 {
        return Err(ForgeError::Forbidden(format!(
            "{} has {} hard links; refusing to use it",
            path.display(),
            info.nNumberOfLinks
        )));
    }
    if mode == NoFollow::OverwriteExisting {
        file.set_len(0).at("truncating", path)?;
    }
    Ok(NoFollowFile { file, created })
}

pub(crate) fn final_path(file: &File) -> Result<PathBuf> {
    let handle = HANDLE(file.as_raw_handle());
    let mut buf = vec![0_u16; 512];
    loop {
        // SAFETY: valid handle; `buf` is writable and its length is passed by the slice.
        let len = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                &mut buf,
                GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_NORMALIZED.0 | VOLUME_NAME_DOS.0),
            )
        } as usize;
        if len == 0 {
            return Err(ForgeError::io(
                "resolving the path of an open file",
                std::io::Error::last_os_error(),
            ));
        }
        if len < buf.len() {
            let text = String::from_utf16_lossy(&buf[..len]);
            return Ok(plain_path(Path::new(&text)));
        }
        // Too small: `len` is the size needed, terminating NUL included.
        buf.resize(len + 1, 0);
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;

    #[test]
    fn plain_files_open_in_every_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, b"hello").unwrap();

        let mut text = String::new();
        open_no_follow(&path, NoFollow::Read)
            .unwrap()
            .file
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "hello");

        let exclusive = open_no_follow(&path, NoFollow::ReadExclusive).unwrap();
        assert!(
            std::fs::OpenOptions::new().write(true).open(&path).is_err(),
            "writers are locked out while the exclusive handle is open"
        );
        assert!(std::fs::read(&path).is_ok(), "readers are not");
        drop(exclusive);

        let mut over = open_no_follow(&path, NoFollow::OverwriteExisting).unwrap();
        assert!(!over.created);
        over.file.write_all(b"x").unwrap();
        drop(over);
        assert_eq!(std::fs::read(&path).unwrap(), b"x", "truncated");

        let log = dir.path().join("log.txt");
        let mut first = open_no_follow(&log, NoFollow::AppendOrCreate).unwrap();
        assert!(first.created);
        first.file.write_all(b"1\n").unwrap();
        drop(first);
        let mut second = open_no_follow(&log, NoFollow::AppendOrCreate).unwrap();
        assert!(!second.created);
        second.file.write_all(b"2\n").unwrap();
        drop(second);
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "1\n2\n");

        let resolved = final_path(&std::fs::File::open(&log).unwrap()).unwrap();
        assert!(
            !resolved.to_string_lossy().starts_with(r"\\?\"),
            "{resolved:?}"
        );
        assert_eq!(
            std::fs::canonicalize(&resolved).unwrap(),
            std::fs::canonicalize(&log).unwrap()
        );
    }

    #[test]
    fn missing_files_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        for mode in [
            NoFollow::Read,
            NoFollow::ReadExclusive,
            NoFollow::OverwriteExisting,
        ] {
            assert_eq!(
                open_no_follow(&missing, mode).unwrap_err().code(),
                "not_found"
            );
        }
        let err = open_no_follow(dir.path(), NoFollow::Read).unwrap_err();
        assert!(
            matches!(err.code(), "invalid_input" | "io"),
            "a directory is never opened as a file: {err}"
        );
    }

    #[test]
    fn hard_links_are_refused_before_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("protected.txt");
        std::fs::write(&target, b"precious").unwrap();
        let link = dir.path().join("response.json");
        std::fs::hard_link(&target, &link).unwrap();
        for mode in [
            NoFollow::Read,
            NoFollow::OverwriteExisting,
            NoFollow::AppendOrCreate,
        ] {
            let err = open_no_follow(&link, mode).unwrap_err();
            assert_eq!(err.code(), "forbidden", "{err}");
        }
        assert_eq!(std::fs::read(&target).unwrap(), b"precious", "untouched");
    }

    #[test]
    fn junctions_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        junction::create(&real, &link).unwrap();
        // Opening the junction object itself as a file fails (access denied) or is
        // refused as a reparse point: never a file inside `real`.
        for mode in [
            NoFollow::Read,
            NoFollow::OverwriteExisting,
            NoFollow::AppendOrCreate,
        ] {
            assert!(open_no_follow(&link, mode).is_err(), "{mode:?}");
        }
        assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
        // Through the junction (an intermediate component) the file opens; `final_path`
        // shows where it really is.
        std::fs::write(real.join("f.txt"), b"x").unwrap();
        let opened = open_no_follow(&link.join("f.txt"), NoFollow::Read).unwrap();
        let resolved = final_path(&opened.file).unwrap();
        assert_eq!(
            std::fs::canonicalize(resolved).unwrap(),
            std::fs::canonicalize(real.join("f.txt")).unwrap()
        );
    }

    #[test]
    fn symlinks_are_refused_when_they_can_be_created() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("t.txt");
        std::fs::write(&target, b"t").unwrap();
        let link = dir.path().join("s.txt");
        // Needs Developer Mode or the SeCreateSymbolicLink privilege; skip otherwise.
        if std::os::windows::fs::symlink_file(&target, &link).is_err() {
            return;
        }
        for mode in [
            NoFollow::Read,
            NoFollow::OverwriteExisting,
            NoFollow::AppendOrCreate,
        ] {
            assert_eq!(open_no_follow(&link, mode).unwrap_err().code(), "forbidden");
        }
        assert_eq!(std::fs::read(&target).unwrap(), b"t");
    }
}
