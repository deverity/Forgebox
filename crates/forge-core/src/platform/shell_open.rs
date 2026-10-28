//! Opening things with the program the user's shell associates with them: a URL in the
//! default browser, a folder in the file manager (docs/07 §4, ADR-0039). The daemon uses
//! it for `sites.open`; later tasks reuse it (database GUI, `php.ini`, mail UI).
//!
//! On Windows this is `ShellExecuteExW` with the default verb on a COM-initialized
//! thread ([`super::windows::WindowsShellOpener`]). The calls are **blocking** (the shell
//! may load a handler first): async code calls them from `spawn_blocking`
//! ([`crate::runtime::spawn_blocking_timeout`], ADR-0011).
//!
//! The inputs are checked here, before any OS call, because the shell would happily run
//! whatever it is handed: only `http`/`https` URLs, and paths that exist, are absolute and
//! are folders or documents (never something the shell would execute, see
//! [`is_executable_extension`]).

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::error::{ForgeError, Result};

/// Opens URLs and paths with the user's default applications.
pub trait ShellOpener: Send + Sync + fmt::Debug {
    /// Opens an `http`/`https` URL in the default browser. `InvalidInput` for anything
    /// else. Blocking.
    fn open_url(&self, url: &str) -> Result<()>;

    /// Opens an existing absolute folder in the file manager, or a document with its
    /// associated program. `InvalidInput` for relative paths and for files the shell would
    /// execute; `NotFound` when the path does not exist. Blocking.
    fn open_path(&self, path: &Path) -> Result<()>;
}

/// File extensions the shell runs instead of showing (`open_path` refuses them).
const EXECUTABLE_EXTENSIONS: &[&str] = &[
    "exe", "com", "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "msi",
    "msp", "scr", "pif", "lnk", "url", "cpl", "hta", "jar", "reg", "msc", "appx", "msix", "dll",
];

/// Whether the shell would run a file with this extension (case-insensitive).
pub fn is_executable_extension(ext: &str) -> bool {
    EXECUTABLE_EXTENSIONS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(ext))
}

/// Checks that `url` is an `http`/`https` URL without spaces or control characters;
/// returns it trimmed.
pub fn validate_url(url: &str) -> Result<&str> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    let has_scheme = lower.starts_with("http://") || lower.starts_with("https://");
    let rest_len = url.split_once("://").map_or(0, |(_, rest)| rest.len());
    if !has_scheme || rest_len == 0 || url.chars().any(|c| c.is_control() || c == ' ') {
        return Err(ForgeError::InvalidInput(format!(
            "only http:// and https:// URLs can be opened, got {url:?}"
        )));
    }
    Ok(url)
}

/// Checks that `path` is absolute, exists and is not something the shell would execute.
pub fn validate_path(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(ForgeError::InvalidInput(format!(
            "{} must be an absolute path",
            path.display()
        )));
    }
    let meta = std::fs::metadata(path).map_err(|e| ForgeError::io_at("opening", path, e))?;
    if meta.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(is_executable_extension)
    {
        return Err(ForgeError::InvalidInput(format!(
            "{} is a program or shortcut; the shell opener only opens folders and documents",
            path.display()
        )));
    }
    Ok(())
}

/// The opener of the running OS.
#[cfg(windows)]
pub fn shell_opener() -> Arc<dyn ShellOpener> {
    Arc::new(super::windows::WindowsShellOpener)
}

/// The opener of the running OS (not implemented outside Windows yet: `xdg-open`/`open`
/// arrive with the Linux/macOS ports, docs/13 §4).
#[cfg(not(windows))]
pub fn shell_opener() -> Arc<dyn ShellOpener> {
    Arc::new(Unsupported)
}

#[cfg(not(windows))]
#[derive(Debug)]
struct Unsupported;

#[cfg(not(windows))]
impl ShellOpener for Unsupported {
    fn open_url(&self, _: &str) -> Result<()> {
        Err(unsupported())
    }
    fn open_path(&self, _: &Path) -> Result<()> {
        Err(unsupported())
    }
}

#[cfg(not(windows))]
fn unsupported() -> ForgeError {
    ForgeError::UnsupportedPlatform(format!(
        "opening files and URLs is not implemented on {}",
        std::env::consts::OS
    ))
}

/// What a [`RecordingShellOpener`] was asked to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellOpen {
    Url(String),
    Path(std::path::PathBuf),
}

/// A [`ShellOpener`] that opens nothing and remembers the requests: tests, and callers
/// that must not touch the desktop. Applies the same input checks as the real one unless
/// built with [`RecordingShellOpener::lenient`].
#[derive(Debug, Default, Clone)]
pub struct RecordingShellOpener {
    calls: Arc<Mutex<Vec<ShellOpen>>>,
    skip_validation: bool,
}

impl RecordingShellOpener {
    /// Records after validating the input like the real opener.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records without checking the input (paths need not exist).
    pub fn lenient() -> Self {
        Self {
            skip_validation: true,
            ..Self::default()
        }
    }

    /// The requests so far, oldest first.
    pub fn calls(&self) -> Vec<ShellOpen> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }

    fn record(&self, call: ShellOpen) {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(call);
        }
    }
}

impl ShellOpener for RecordingShellOpener {
    fn open_url(&self, url: &str) -> Result<()> {
        let url = if self.skip_validation {
            url
        } else {
            validate_url(url)?
        };
        self.record(ShellOpen::Url(url.to_owned()));
        Ok(())
    }

    fn open_path(&self, path: &Path) -> Result<()> {
        if !self.skip_validation {
            validate_path(path)?;
        }
        self.record(ShellOpen::Path(path.to_path_buf()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_urls_pass() {
        assert_eq!(
            validate_url(" https://loja.test/ ").unwrap(),
            "https://loja.test/"
        );
        assert!(validate_url("HTTP://a.test").is_ok());
        for bad in [
            "file:///C:/Windows/notepad.exe",
            "calc.exe",
            "javascript:alert(1)",
            "ms-msdt:/id x",
            "https://",
            "https://a.test/ b",
            "https://a.test/\u{0}",
            "",
        ] {
            let err = validate_url(bad).unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{bad:?}");
        }
    }

    #[test]
    fn paths_must_exist_and_not_be_programs() {
        let dir = tempfile::tempdir().unwrap();
        validate_path(dir.path()).unwrap();
        let doc = dir.path().join("readme.txt");
        std::fs::write(&doc, b"x").unwrap();
        validate_path(&doc).unwrap();
        for name in ["setup.EXE", "run.bat", "x.ps1", "link.lnk", "web.url"] {
            let file = dir.path().join(name);
            std::fs::write(&file, b"x").unwrap();
            assert_eq!(
                validate_path(&file).unwrap_err().code(),
                "invalid_input",
                "{name}"
            );
        }
        assert_eq!(
            validate_path(&dir.path().join("missing"))
                .unwrap_err()
                .code(),
            "not_found"
        );
        assert_eq!(
            validate_path(Path::new("relative/dir")).unwrap_err().code(),
            "invalid_input"
        );
        assert!(is_executable_extension("Cmd") && !is_executable_extension("php"));
    }

    #[test]
    fn the_recording_opener_remembers_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        let opener = RecordingShellOpener::new();
        opener.open_url("https://a.test").unwrap();
        opener.open_path(dir.path()).unwrap();
        assert!(opener.open_url("ftp://a.test").is_err());
        assert!(opener.open_path(&dir.path().join("nope")).is_err());
        assert_eq!(
            opener.calls(),
            vec![
                ShellOpen::Url("https://a.test".into()),
                ShellOpen::Path(dir.path().to_path_buf())
            ]
        );
        let lenient = RecordingShellOpener::lenient();
        lenient.open_path(Path::new("C:/missing")).unwrap();
        assert_eq!(lenient.calls().len(), 1);
    }
}
