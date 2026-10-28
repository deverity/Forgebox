//! `logs/helper.log`: one JSON line per request handled, accepted or refused (docs/11 §1).
//!
//! ```json
//! {"ts":"2026-10-27T12:00:00Z","request_id":"01J...","op":"nrpt.add","dry_run":false,"ok":true,"code":null,"message":"..."}
//! ```
//!
//! The file is in the user's root while the helper runs elevated, so it is opened like the
//! exchange files: never through a link in its name ([`open_no_follow`]), and the opened
//! file must really be `<root>/logs/helper.log` ([`final_path`]): a `logs` junction
//! pointing into `C:\Windows` makes the helper refuse the request instead of writing there.
//! The log is opened **before** anything is done: no log, no action.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::platform::{NoFollow, open_no_follow};
use serde::Serialize;

/// File name inside `logs/`.
pub const LOG_FILE: &str = "helper.log";

/// One line of `helper.log`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogEntry {
    /// RFC 3339 UTC.
    pub ts: String,
    pub request_id: Option<String>,
    /// Operation name (`nrpt.add`), when the request could be parsed.
    pub op: Option<String>,
    pub dry_run: bool,
    pub ok: bool,
    /// Stable error code when `ok` is false.
    pub code: Option<String>,
    pub message: String,
}

impl LogEntry {
    /// An entry stamped now.
    pub fn now(
        request_id: Option<String>,
        op: Option<String>,
        dry_run: bool,
        ok: bool,
        code: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            ts: jiff::Timestamp::now().to_string(),
            request_id,
            op,
            dry_run,
            ok,
            code,
            message: message.into(),
        }
    }
}

/// The open `helper.log` of a root.
#[derive(Debug)]
pub struct HelperLog {
    file: File,
}

impl HelperLog {
    /// Opens (creating when missing) `<root>/logs/helper.log`, refusing links.
    pub fn open(root: &Path) -> Result<Self> {
        let logs = root.join("logs");
        match std::fs::symlink_metadata(&logs) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(ForgeError::Forbidden(format!(
                    "{} is a link; the helper does not log through it",
                    logs.display()
                )));
            }
            Ok(meta) if !meta.is_dir() => {
                return Err(ForgeError::InvalidInput(format!(
                    "{} is not a directory",
                    logs.display()
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&logs).at("creating", &logs)?;
            }
            Err(e) => return Err(ForgeError::io_at("inspecting", &logs, e)),
        }
        let path = logs.join(LOG_FILE);
        let opened = open_no_follow(&path, NoFollow::AppendOrCreate)?;
        #[cfg(windows)]
        {
            let actual = forge_core::platform::final_path(&opened.file)?;
            let canonical_root = std::fs::canonicalize(root).at("resolving", root)?;
            let expected = forge_core::paths::plain_path(&canonical_root)
                .join("logs")
                .join(LOG_FILE);
            if !actual
                .to_string_lossy()
                .eq_ignore_ascii_case(&expected.to_string_lossy())
            {
                drop(opened.file);
                if opened.created {
                    let _ = std::fs::remove_file(&actual);
                }
                return Err(ForgeError::Forbidden(format!(
                    "{} resolves to {}; the helper only logs inside its root",
                    path.display(),
                    actual.display()
                )));
            }
        }
        Ok(Self { file: opened.file })
    }

    /// Appends one line.
    pub fn append(&mut self, entry: &LogEntry) -> Result<()> {
        let mut line = serde_json::to_string(entry)
            .map_err(|e| ForgeError::Internal(format!("encoding a helper.log line: {e}")))?;
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.flush())
            .map_err(|e| ForgeError::io("writing logs/helper.log", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_appended_as_json() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = HelperLog::open(dir.path()).unwrap();
        log.append(&LogEntry::now(
            Some("01J".into()),
            Some("nrpt.add".into()),
            false,
            true,
            None,
            "rule added",
        ))
        .unwrap();
        drop(log);
        let mut log = HelperLog::open(dir.path()).unwrap();
        log.append(&LogEntry::now(
            None,
            None,
            true,
            false,
            Some("invalid_input".into()),
            "bad",
        ))
        .unwrap();
        let text = std::fs::read_to_string(dir.path().join("logs").join(LOG_FILE)).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["op"], "nrpt.add");
        assert_eq!(lines[0]["ok"], true);
        assert_eq!(lines[1]["code"], "invalid_input");
        assert_eq!(lines[1]["dry_run"], true);
        assert!(lines[1]["ts"].as_str().unwrap().ends_with('Z'));
    }

    #[cfg(windows)]
    #[test]
    fn links_out_of_the_root_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // `logs` as a junction to a directory outside the root.
        junction::create(&outside, root.join("logs")).unwrap();
        assert_eq!(HelperLog::open(&root).unwrap_err().code(), "forbidden");
        assert_eq!(
            std::fs::read_dir(&outside).unwrap().count(),
            0,
            "nothing created"
        );

        // `helper.log` as a hard link to a file outside.
        let root2 = dir.path().join("root2");
        std::fs::create_dir_all(root2.join("logs")).unwrap();
        let precious = outside.join("precious.txt");
        std::fs::write(&precious, "keep").unwrap();
        std::fs::hard_link(&precious, root2.join("logs").join(LOG_FILE)).unwrap();
        assert_eq!(HelperLog::open(&root2).unwrap_err().code(), "forbidden");
        assert_eq!(std::fs::read_to_string(&precious).unwrap(), "keep");
    }
}
