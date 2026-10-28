//! The two exchange files of a run (ADR-0022): `<root>/state/locks/helper-<id>.request.json`
//! (read) and `.response.json` (written). Both are created by the daemon before the
//! helper starts; the helper only opens them, never creates them, and refuses links and
//! multiply linked files ([`forge_core::platform::open_no_follow`]).

use std::io::{Read, Write};
use std::path::Path;

use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::helper::{MAX_DOCUMENT_BYTES, request_path, response_path};
use forge_core::ids::RequestId;
use forge_core::platform::{NoFollow, open_no_follow};

const PREFIX: &str = "helper-";
const REQUEST_SUFFIX: &str = ".request.json";

/// The request id written in the request file name (`helper-<id>.request.json`).
fn id_from_name(request: &Path) -> Result<RequestId> {
    let name = request
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    name.strip_prefix(PREFIX)
        .and_then(|rest| rest.strip_suffix(REQUEST_SUFFIX))
        .and_then(|id| id.parse().ok())
        .ok_or_else(|| {
            ForgeError::Forbidden(format!(
                "{} is not named helper-<request id>{REQUEST_SUFFIX}",
                request.display()
            ))
        })
}

/// `a` and `b` name the same file: same canonical parent directory, same file name
/// (Windows names compare without case).
fn same_file_location(a: &Path, b: &Path) -> Result<bool> {
    let parent = |p: &Path| -> Result<std::path::PathBuf> {
        let dir = p
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::canonicalize(dir).at("resolving", dir)
    };
    let names_match = match (a.file_name(), b.file_name()) {
        (Some(x), Some(y)) => {
            let (x, y) = (x.to_string_lossy(), y.to_string_lossy());
            if cfg!(windows) {
                x.eq_ignore_ascii_case(&y)
            } else {
                x == y
            }
        }
        _ => false,
    };
    Ok(names_match && parent(a)? == parent(b)?)
}

/// Checks the paths given on the command line against the layout of ADR-0022 for the
/// helper's `root`; the request id of the run.
///
/// Errors (`forbidden`): a request file not named `helper-<id>.request.json`, or not in
/// `<root>/state/locks`; a response file that is not its sibling `.response.json`.
pub fn check_paths(root: &Path, request: &Path, response: Option<&Path>) -> Result<RequestId> {
    let id = id_from_name(request)?;
    if !same_file_location(request, &request_path(root, id))? {
        return Err(ForgeError::Forbidden(format!(
            "{} is not in {}",
            request.display(),
            root.join("state").join("locks").display()
        )));
    }
    if let Some(response) = response {
        let expected = response_path(root, id);
        if !same_file_location(response, &expected)? {
            return Err(ForgeError::Forbidden(format!(
                "response file {} must be {}",
                response.display(),
                expected.display()
            )));
        }
    }
    Ok(id)
}

/// Reads the request file (at most [`MAX_DOCUMENT_BYTES`] + 1 bytes: a bigger one is
/// refused by the parser).
pub fn read_request(path: &Path) -> Result<String> {
    let file = open_no_follow(path, NoFollow::Read)?.file;
    read_limited(file, path)
}

/// Reads at most [`MAX_DOCUMENT_BYTES`] + 1 bytes of UTF-8 text.
pub fn read_limited(reader: impl Read, what: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .at("reading", what)?;
    String::from_utf8(bytes)
        .map_err(|_| ForgeError::InvalidInput(format!("{} is not UTF-8 text", what.display())))
}

/// Writes the response into the existing response file.
pub fn write_response(path: &Path, text: &str) -> Result<()> {
    let mut file = open_no_follow(path, NoFollow::OverwriteExisting)?.file;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .at("writing", path)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Root {
        _dir: tempfile::TempDir,
        root: std::path::PathBuf,
    }

    fn root() -> Root {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Forge box");
        std::fs::create_dir_all(root.join("state").join("locks")).unwrap();
        Root { _dir: dir, root }
    }

    #[test]
    fn exchange_files_must_follow_the_layout() {
        let r = root();
        let id = RequestId::new();
        let req = request_path(&r.root, id);
        let resp = response_path(&r.root, id);
        std::fs::write(&req, "{}").unwrap();
        std::fs::write(&resp, "").unwrap();
        assert_eq!(check_paths(&r.root, &req, Some(&resp)).unwrap(), id);
        assert_eq!(check_paths(&r.root, &req, None).unwrap(), id);
        // Different spelling of the same directory.
        let upper =
            std::path::PathBuf::from(req.parent().unwrap().to_string_lossy().to_uppercase())
                .join(req.file_name().unwrap());
        if cfg!(windows) {
            assert_eq!(check_paths(&r.root, &upper, Some(&resp)).unwrap(), id);
        }

        // Response of another run, or anywhere else.
        let other = response_path(&r.root, RequestId::new());
        std::fs::write(&other, "").unwrap();
        for bad in [
            other.clone(),
            r.root.join("x.json"),
            r.root.join("state").join(resp.file_name().unwrap()),
        ] {
            let _ = std::fs::write(&bad, "");
            let err = check_paths(&r.root, &req, Some(&bad)).unwrap_err();
            assert_eq!(err.code(), "forbidden", "{}: {err}", bad.display());
        }

        // A request outside state/locks, or badly named.
        let elsewhere = r.root.join(req.file_name().unwrap());
        std::fs::write(&elsewhere, "{}").unwrap();
        assert_eq!(
            check_paths(&r.root, &elsewhere, None).unwrap_err().code(),
            "forbidden"
        );
        let badly_named = r.root.join("state").join("locks").join("request.json");
        std::fs::write(&badly_named, "{}").unwrap();
        assert_eq!(
            check_paths(&r.root, &badly_named, None).unwrap_err().code(),
            "forbidden"
        );
        let bad_id = r
            .root
            .join("state")
            .join("locks")
            .join("helper-xyz.request.json");
        assert_eq!(
            check_paths(&r.root, &bad_id, None).unwrap_err().code(),
            "forbidden"
        );
    }

    #[test]
    fn response_is_written_in_place_and_never_created() {
        let r = root();
        let id = RequestId::new();
        let resp = response_path(&r.root, id);
        assert_eq!(write_response(&resp, "x").unwrap_err().code(), "not_found");
        assert!(!resp.exists(), "never created");
        std::fs::write(&resp, "old content that is longer").unwrap();
        write_response(&resp, "{\"ok\":true}").unwrap();
        assert_eq!(std::fs::read_to_string(&resp).unwrap(), "{\"ok\":true}");

        // A hard link planted as the response file is refused and its target untouched.
        let precious = r.root.join("precious.txt");
        std::fs::write(&precious, "keep").unwrap();
        let planted = response_path(&r.root, RequestId::new());
        std::fs::hard_link(&precious, &planted).unwrap();
        assert_eq!(
            write_response(&planted, "pwned").unwrap_err().code(),
            "forbidden"
        );
        assert_eq!(std::fs::read_to_string(&precious).unwrap(), "keep");
    }

    #[test]
    fn requests_are_read_with_a_limit() {
        let r = root();
        let id = RequestId::new();
        let req = request_path(&r.root, id);
        std::fs::write(&req, "{\"v\":1}").unwrap();
        assert_eq!(read_request(&req).unwrap(), "{\"v\":1}");
        std::fs::write(&req, vec![b'a'; MAX_DOCUMENT_BYTES + 10]).unwrap();
        assert_eq!(read_request(&req).unwrap().len(), MAX_DOCUMENT_BYTES + 1);
        std::fs::write(&req, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(read_request(&req).unwrap_err().code(), "invalid_input");
    }
}
