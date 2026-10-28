//! Drift detection for managed blocks (docs/03 §3.1).
//!
//! A block drifted when the hash recorded in its `begin` line (what Forgebox wrote) no
//! longer matches the hash of its body (what is on disk). This module only detects;
//! whoever calls it (the daemon's config actor) publishes `config.drift_detected`
//! ([`forge_core::event::EventKind::ConfigDriftDetected`]) and the UI asks the user to
//! keep the edit or regenerate (`upsert_block(.., force = true)`).

use std::fs;
use std::path::{Path, PathBuf};

use forge_core::error::{ForgeError, Result};
use serde::{Deserialize, Serialize};

use crate::managed::{CommentStyle, ParsedBlock, parse_blocks};

/// A managed block edited by the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drift {
    /// File that holds the block.
    pub file: PathBuf,
    /// Block name.
    pub block: String,
    /// Hash Forgebox recorded when it wrote the block.
    pub expected_hash: String,
    /// Hash of the body found on disk.
    pub actual_hash: String,
}

impl Drift {
    pub(crate) fn new(file: &Path, block: &ParsedBlock) -> Self {
        Self {
            file: file.to_path_buf(),
            block: block.name.clone(),
            expected_hash: block.recorded_hash.clone(),
            actual_hash: block.actual_hash.clone(),
        }
    }
}

/// Drifts in `text`, which is the content of `file`.
pub fn detect_in_text(file: &Path, text: &str, style: CommentStyle) -> Result<Vec<Drift>> {
    Ok(parse_blocks(text, style)?
        .iter()
        .filter(|b| b.is_drifted())
        .map(|b| Drift::new(file, b))
        .collect())
}

/// Drifts in the file at `path`. A missing file has none.
pub fn detect_in_file(path: &Path, style: CommentStyle) -> Result<Vec<Drift>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(ForgeError::io_at("reading", path, e)),
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| ForgeError::ConfigInvalid(format!("{} is not valid UTF-8", path.display())))?;
    detect_in_text(path, &text, style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed::upsert_block;

    #[test]
    fn clean_file_has_no_drift_and_edit_is_reported() {
        let file = Path::new("etc/nginx/nginx.conf");
        let up = upsert_block(file, None, CommentStyle::Hash, "main", "a\n", false).unwrap();
        assert!(
            detect_in_text(file, &up.text, CommentStyle::Hash)
                .unwrap()
                .is_empty()
        );
        let edited = up.text.replace("\na\n", "\nb\n");
        let drifts = detect_in_text(file, &edited, CommentStyle::Hash).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].file, file);
        assert_eq!(drifts[0].block, "main");
        assert_ne!(drifts[0].expected_hash, drifts[0].actual_hash);
    }

    #[test]
    fn missing_file_has_no_drift() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            detect_in_file(&dir.path().join("nope.conf"), CommentStyle::Hash)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn edit_on_disk_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("php.ini");
        crate::managed::write_block_file(&path, CommentStyle::Semicolon, "x", "a=1\n", false)
            .unwrap();
        let text = fs::read_to_string(&path).unwrap().replace("a=1", "a=2");
        fs::write(&path, text).unwrap();
        assert_eq!(
            detect_in_file(&path, CommentStyle::Semicolon)
                .unwrap()
                .len(),
            1
        );
    }
}
