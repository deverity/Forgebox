//! Validation of archive entry names (zip-slip and Windows name hazards, docs/11 §2).
//!
//! An entry name becomes a relative path made of plain components only. Rejected:
//! `..` anywhere, absolute paths (`/x`, `\x`, `\\server\share`), drive letters and any
//! other `:` (also NTFS alternate data streams), characters Windows does not allow
//! (`<>:"|?*` and control characters), reserved device names (`CON`, `NUL.txt`, `COM1`...),
//! components ending in `.` or space (Windows would silently alias them) and components
//! longer than 255 characters. `\` counts as a separator like `/`; empty and `.`
//! components are dropped (`./a//b/` is `a/b`).

use std::path::PathBuf;

use forge_core::error::{ForgeError, Result};

/// Longest component NTFS accepts.
const MAX_COMPONENT_LEN: usize = 255;

/// Device names Windows reserves in every directory, with or without extension.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3",
    "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// Error for an entry that must not be extracted. Always `verification_failed`: the
/// archive is malicious or broken, and nothing of it is installed.
pub(crate) fn unsafe_entry(name: &str, why: &str) -> ForgeError {
    ForgeError::VerificationFailed(format!("unsafe archive entry {name:?}: {why}"))
}

/// Relative path of entry `name`, or `None` for the archive root (`./`, empty name).
pub(crate) fn sanitize(name: &str) -> Result<Option<PathBuf>> {
    if name.contains('\0') {
        return Err(unsafe_entry(name, "contains a NUL character"));
    }
    let normalized = name.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(unsafe_entry(name, "absolute path"));
    }
    let bytes = normalized.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(unsafe_entry(name, "drive letter"));
    }
    let mut path = PathBuf::new();
    for component in normalized.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            return Err(unsafe_entry(name, "parent directory component (..)"));
        }
        check_component(name, component)?;
        path.push(component);
    }
    Ok((!path.as_os_str().is_empty()).then_some(path))
}

/// Validates one path component of entry `name`.
pub(crate) fn check_component(name: &str, component: &str) -> Result<()> {
    if component.chars().count() > MAX_COMPONENT_LEN {
        return Err(unsafe_entry(
            name,
            "path component longer than 255 characters",
        ));
    }
    if let Some(c) = component
        .chars()
        .find(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || c.is_control())
    {
        return Err(unsafe_entry(
            name,
            &format!("character {c:?} is not allowed in Windows file names"),
        ));
    }
    if component.ends_with(['.', ' ']) {
        return Err(unsafe_entry(
            name,
            "component ends with '.' or space (aliased by Windows)",
        ));
    }
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches(' ');
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        return Err(unsafe_entry(name, "reserved Windows device name"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn plain_names_are_kept() {
        let ok = |name: &str, expected: &str| {
            assert_eq!(
                sanitize(name).unwrap().as_deref(),
                Some(Path::new(expected)),
                "{name}"
            );
        };
        ok("php.exe", "php.exe");
        ok("ext/php_openssl.dll", "ext/php_openssl.dll");
        ok("ext\\php_curl.dll", "ext/php_curl.dll");
        ok("./nginx-1.30.6/conf/", "nginx-1.30.6/conf");
        ok("a//b/./c", "a/b/c");
        ok("..foo/bar..baz", "..foo/bar..baz");
        ok("console.log", "console.log");
        ok("COM10.txt", "COM10.txt");
        ok(
            "auxiliary/nul-terminated.txt",
            "auxiliary/nul-terminated.txt",
        );
        ok("ünïcödé/файл.txt", "ünïcödé/файл.txt");
        assert_eq!(sanitize("./").unwrap(), None);
        assert_eq!(sanitize("").unwrap(), None);
    }

    #[test]
    fn hazards_are_rejected() {
        for bad in [
            "../evil.txt",
            "a/../../evil.txt",
            "a/..",
            "..\\evil.txt",
            "/etc/passwd",
            "\\Windows\\evil.dll",
            "\\\\server\\share\\x",
            "C:/evil.txt",
            "c:evil.txt",
            "C:\\Windows\\evil.dll",
            "file.txt:stream",
            "a/b<c",
            "what?.txt",
            "star*.txt",
            "pipe|.txt",
            "quote\".txt",
            "tab\t.txt",
            "nul\0.txt",
            "CON",
            "con.txt",
            "dir/Nul.tar.gz",
            "aux",
            "COM1.log",
            "lpt9",
            "CONOUT$",
            "COM¹",
            "trailing.",
            "trailing ",
            "dir./x",
        ] {
            let err = sanitize(bad).unwrap_err();
            assert_eq!(err.code(), "verification_failed", "{bad:?}");
            assert!(err.to_string().contains("unsafe archive entry"), "{bad:?}");
        }
        assert!(sanitize(&"a".repeat(256)).is_err());
        assert!(sanitize(&"a".repeat(255)).is_ok());
    }
}
