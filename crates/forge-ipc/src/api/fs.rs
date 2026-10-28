//! `fs.*`: file transfer restricted to `etc/`, `logs/` and `www/` under the root
//! (ADR-0009, rule 5; docs/13 §2). Paths are root-relative with `/` separators; the
//! daemon validates them and checks the canonical path stays inside one of the three
//! folders, so links cannot escape. Used by the GUI to edit `php.ini` or read logs on a
//! remote daemon without SMB/SSH.

use serde::{Deserialize, Serialize};
use specta::Type;

use forge_core::Timestamp;

/// First path segments accepted by `fs.*`.
pub const FS_ROOTS: &[&str] = &["etc", "logs", "www"];

/// Encoding of file content on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum FsEncoding {
    /// UTF-8 text.
    #[default]
    Utf8,
    /// Arbitrary bytes, standard base64.
    Base64,
}

/// Params of `fs.read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct FsReadParams {
    /// `etc/php/8.3/php.ini`.
    pub path: String,
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub offset: Option<u64>,
    /// Default and maximum: 4 MiB.
    #[serde(default)]
    pub max_bytes: Option<u32>,
}

/// Result of `fs.read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct FsReadDto {
    pub path: String,
    /// `utf8` when the bytes read are valid UTF-8, otherwise `base64`.
    pub encoding: FsEncoding,
    pub content: String,
    /// Total size of the file.
    #[specta(type = specta_typescript::Number)]
    pub size: u64,
    /// More bytes follow (`offset + read < size`).
    pub truncated: bool,
    pub modified: Option<Timestamp>,
    /// Version tag for `fs.write.if_match`.
    pub etag: String,
}

/// Params of `fs.write` (whole-file replace, atomic).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct FsWriteParams {
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub encoding: FsEncoding,
    /// Refuse (`conflict`) unless the file still has this `etag` (optimistic lock).
    #[serde(default)]
    pub if_match: Option<String>,
    /// Allow creating a new file (the parent folder must exist).
    #[serde(default)]
    pub create: bool,
}

/// Result of `fs.write`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct FsWriteDto {
    pub path: String,
    #[specta(type = specta_typescript::Number)]
    pub size: u64,
    pub etag: String,
}

/// Params of `fs.tail`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct FsTailParams {
    pub path: String,
    /// Default 100, maximum 10 000.
    #[serde(default)]
    pub lines: Option<u32>,
}

/// Result of `fs.tail`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct FsTailDto {
    pub path: String,
    pub lines: Vec<String>,
    #[specta(type = specta_typescript::Number)]
    pub size: u64,
}

/// Params of `fs.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct FsListParams {
    /// `www/loja`.
    pub path: String,
}

/// Kind of directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum FsEntryKind {
    File,
    Dir,
    /// Symlink or junction (not followed by `fs.list`).
    Link,
}

/// A directory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct FsEntryDto {
    pub name: String,
    pub kind: FsEntryKind,
    #[specta(type = Option<specta_typescript::Number>)]
    pub size: Option<u64>,
    pub modified: Option<Timestamp>,
}

/// Result of `fs.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct FsListDto {
    pub path: String,
    pub entries: Vec<FsEntryDto>,
}
