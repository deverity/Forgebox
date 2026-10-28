//! `fs.read|write|tail|list`: file access restricted to `etc/`, `logs/` and `www/`
//! (ADR-0009, rule 5; docs/11 §4).
//!
//! Validation happens twice: syntactically (root-relative, `/`-separated, no `.`/`..`,
//! no drive, no `\`, no `:` (alternate data streams), no device names, first segment
//! one of [`FS_ROOTS`]) and physically: the path is canonicalized (links and junctions
//! resolved) and must still be inside the canonical `etc/`, `logs/` or `www/`. Anything
//! else is `forbidden`. All I/O runs in `spawn_blocking`.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use forge_core::Timestamp;
use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::paths::{ForgeRoot, write_atomic};

use crate::api::{
    FS_ROOTS, FsEncoding, FsEntryDto, FsEntryKind, FsListDto, FsListParams, FsReadDto,
    FsReadParams, FsTailDto, FsTailParams, FsWriteDto, FsWriteParams,
};

/// Default and maximum bytes returned by one `fs.read`.
pub const FS_READ_MAX: u32 = 4 * 1024 * 1024;
/// Maximum size accepted by `fs.write`.
pub const FS_WRITE_MAX: usize = 4 * 1024 * 1024;
/// Maximum lines returned by `fs.tail`.
pub const FS_TAIL_MAX: u32 = 10_000;

const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// The folders `fs.*` may touch, for one installation root.
#[derive(Debug, Clone)]
pub struct FsScope {
    root: PathBuf,
}

impl FsScope {
    pub fn new(root: &ForgeRoot) -> Self {
        Self {
            root: root.path().to_path_buf(),
        }
    }

    /// Syntactic check: returns the segments of a valid root-relative path.
    pub fn validate(rel: &str) -> Result<Vec<&str>> {
        let invalid = |why: &str| Err(ForgeError::InvalidInput(format!("fs path {rel:?}: {why}")));
        if rel.is_empty() {
            return invalid("empty");
        }
        if rel.starts_with('/') || rel.contains('\\') || rel.contains(':') {
            return invalid("must be relative to the root and use '/' separators");
        }
        let segments: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
        for segment in &segments {
            if *segment == "." || *segment == ".." {
                return invalid("'.' and '..' are not allowed");
            }
            if segment
                .chars()
                .any(|c| c.is_control() || "*?\"<>|".contains(c))
            {
                return invalid("contains characters not allowed in file names");
            }
            if segment.ends_with(['.', ' ']) {
                return invalid("names must not end with '.' or space");
            }
            let stem = segment.split('.').next().unwrap_or("").to_ascii_lowercase();
            if RESERVED_NAMES.contains(&stem.as_str()) {
                return invalid("reserved device name");
            }
        }
        match segments.first() {
            Some(first) if FS_ROOTS.contains(first) => Ok(segments),
            _ => Err(ForgeError::Forbidden(format!(
                "fs path {rel:?}: only {} are accessible",
                FS_ROOTS
                    .iter()
                    .map(|r| format!("{r}/"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    fn joined(&self, segments: &[&str]) -> PathBuf {
        let mut path = self.root.clone();
        path.extend(segments);
        path
    }

    /// Checks that the canonical `path` lies in the canonical scope folder `segments[0]`.
    fn ensure_inside(&self, rel: &str, first: &str, canonical: &Path) -> Result<()> {
        let base = self.root.join(first);
        let base = fs::canonicalize(&base).at("resolving", &base)?;
        if canonical.starts_with(&base)
            && canonical
                .components()
                .all(|c| !matches!(c, Component::ParentDir))
        {
            Ok(())
        } else {
            Err(ForgeError::Forbidden(format!(
                "fs path {rel:?} resolves outside {first}/"
            )))
        }
    }

    /// Resolves an existing file or folder. Blocking.
    pub fn resolve_existing(&self, rel: &str) -> Result<PathBuf> {
        let segments = Self::validate(rel)?;
        let path = self.joined(&segments);
        let canonical = fs::canonicalize(&path).at("resolving", &path)?;
        self.ensure_inside(rel, segments[0], &canonical)?;
        Ok(canonical)
    }

    /// Resolves a file to write: its folder must exist and be inside the scope; the file
    /// itself may not exist yet (and must not be a link). Blocking.
    pub fn resolve_for_write(&self, rel: &str) -> Result<PathBuf> {
        let segments = Self::validate(rel)?;
        if segments.len() < 2 {
            return Err(ForgeError::Forbidden(format!(
                "fs path {rel:?}: cannot write the scope folder itself"
            )));
        }
        let path = self.joined(&segments);
        let parent = path
            .parent()
            .ok_or_else(|| ForgeError::InvalidInput(format!("fs path {rel:?} has no folder")))?;
        let parent = fs::canonicalize(parent).at("resolving", parent)?;
        self.ensure_inside(rel, segments[0], &parent)?;
        let target = parent.join(segments[segments.len() - 1]);
        if let Ok(meta) = fs::symlink_metadata(&target) {
            if meta.file_type().is_symlink() || meta.is_dir() {
                return Err(ForgeError::Forbidden(format!(
                    "fs path {rel:?} is a folder or a link"
                )));
            }
        }
        Ok(target)
    }
}

fn modified(meta: &fs::Metadata) -> Option<Timestamp> {
    Timestamp::try_from(meta.modified().ok()?).ok()
}

fn etag(meta: &fs::Metadata) -> String {
    let nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!("{:x}-{nanos:x}", meta.len())
}

pub(crate) fn read(scope: &FsScope, params: &FsReadParams) -> Result<FsReadDto> {
    let path = scope.resolve_existing(&params.path)?;
    let meta = fs::metadata(&path).at("inspecting", &path)?;
    if !meta.is_file() {
        return Err(ForgeError::InvalidInput(format!(
            "{} is not a file",
            params.path
        )));
    }
    let offset = params.offset.unwrap_or(0).min(meta.len());
    let max = params.max_bytes.unwrap_or(FS_READ_MAX).min(FS_READ_MAX) as u64;
    let mut file = File::open(&path).at("opening", &path)?;
    file.seek(SeekFrom::Start(offset)).at("seeking", &path)?;
    let mut bytes = Vec::new();
    file.take(max)
        .read_to_end(&mut bytes)
        .at("reading", &path)?;
    let truncated = offset + (bytes.len() as u64) < meta.len();
    let (encoding, content) = match String::from_utf8(bytes) {
        Ok(text) => (FsEncoding::Utf8, text),
        Err(err) => (FsEncoding::Base64, BASE64.encode(err.into_bytes())),
    };
    Ok(FsReadDto {
        path: params.path.clone(),
        encoding,
        content,
        size: meta.len(),
        truncated,
        modified: modified(&meta),
        etag: etag(&meta),
    })
}

pub(crate) fn write(scope: &FsScope, params: &FsWriteParams) -> Result<FsWriteDto> {
    let path = scope.resolve_for_write(&params.path)?;
    let bytes = match params.encoding {
        FsEncoding::Utf8 => params.content.as_bytes().to_vec(),
        FsEncoding::Base64 => BASE64
            .decode(params.content.as_bytes())
            .map_err(|e| ForgeError::InvalidInput(format!("invalid base64 content: {e}")))?,
    };
    if bytes.len() > FS_WRITE_MAX {
        return Err(ForgeError::InvalidInput(format!(
            "content of {} bytes exceeds the fs.write limit of {FS_WRITE_MAX}",
            bytes.len()
        )));
    }
    match fs::metadata(&path) {
        Ok(meta) => {
            if let Some(expected) = &params.if_match {
                let current = etag(&meta);
                if &current != expected {
                    return Err(ForgeError::Conflict(format!(
                        "{} changed since it was read (etag {current}, expected {expected})",
                        params.path
                    )));
                }
            }
        }
        Err(_) if params.create => {}
        Err(err) => return Err(ForgeError::io_at("inspecting", &path, err)),
    }
    write_atomic(&path, &bytes)?;
    let meta = fs::metadata(&path).at("inspecting", &path)?;
    Ok(FsWriteDto {
        path: params.path.clone(),
        size: meta.len(),
        etag: etag(&meta),
    })
}

pub(crate) fn tail(scope: &FsScope, params: &FsTailParams) -> Result<FsTailDto> {
    let path = scope.resolve_existing(&params.path)?;
    let wanted = params.lines.unwrap_or(100).clamp(1, FS_TAIL_MAX) as usize;
    let mut file = File::open(&path).at("opening", &path)?;
    let size = file.metadata().at("inspecting", &path)?.len();
    // Read backwards in chunks until enough newlines (bounded to 8 MiB).
    let mut start = size;
    let mut buffer: Vec<u8> = Vec::new();
    const CHUNK: u64 = 64 * 1024;
    const LIMIT: u64 = 8 * 1024 * 1024;
    while start > 0 && size - start < LIMIT {
        let read_from = start.saturating_sub(CHUNK);
        let mut chunk = vec![0u8; (start - read_from) as usize];
        file.seek(SeekFrom::Start(read_from)).at("seeking", &path)?;
        file.read_exact(&mut chunk).at("reading", &path)?;
        chunk.extend_from_slice(&buffer);
        buffer = chunk;
        start = read_from;
        if buffer.iter().filter(|b| **b == b'\n').count() > wanted {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buffer);
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    if lines.len() > wanted {
        lines.drain(..lines.len() - wanted);
    }
    Ok(FsTailDto {
        path: params.path.clone(),
        lines,
        size,
    })
}

pub(crate) fn list(scope: &FsScope, params: &FsListParams) -> Result<FsListDto> {
    let path = scope.resolve_existing(&params.path)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(&path).at("listing", &path)? {
        let entry = entry.at("listing", &path)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let file_type = meta.file_type();
        let kind = if file_type.is_symlink() || is_reparse_dir(&meta) {
            FsEntryKind::Link
        } else if file_type.is_dir() {
            FsEntryKind::Dir
        } else {
            FsEntryKind::File
        };
        entries.push(FsEntryDto {
            name,
            kind,
            size: (kind == FsEntryKind::File).then_some(meta.len()),
            modified: modified(&meta),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(FsListDto {
        path: params.path.clone(),
        entries,
    })
}

#[cfg(windows)]
fn is_reparse_dir(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_dir(_: &fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_rules() {
        for ok in [
            "etc/php/8.3/php.ini",
            "logs/nginx/error.log",
            "www",
            "www/a b/c.txt",
        ] {
            FsScope::validate(ok).unwrap();
        }
        for bad in [
            "",
            "/etc/x",
            "C:/Forgebox/etc/x",
            "etc\\x",
            "etc/../state/ipc.token",
            "etc/./x",
            "etc/x:stream",
            "etc/CON",
            "etc/nul.txt",
            "etc/x.",
            "etc/a*b",
        ] {
            let err = FsScope::validate(bad).unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{bad}");
        }
        for outside in [
            "state/ipc.token",
            "bin/php/current/php.exe",
            "data/mysql",
            "Etc/x",
        ] {
            let err = FsScope::validate(outside).unwrap_err();
            assert_eq!(err.code(), "forbidden", "{outside}");
        }
    }
}
