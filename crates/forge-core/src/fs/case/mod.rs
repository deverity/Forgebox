//! Linux parity: case sensitivity of folders (docs/15, ADR-0010).
//!
//! - [`status`]: what `forge fs case-sensitive status` shows (flag, volume support, file
//!   system, Windows build).
//! - [`convert::convert_tree`]: reversible conversion of an existing tree (blocking), and
//!   [`convert::recover`] for a conversion interrupted by a crash.
//! - [`CaseConversions`]: the async side. [`CaseConversions::convert`] returns a
//!   [`JobId`](crate::ids::JobId) at once; the work runs in `spawn_blocking`, reports
//!   `job.progress` and ends with `fs.case_sensitivity_applied|unavailable`.
//! - [`fold_case`]: the case folding used to compare names "ignoring case" the way NTFS
//!   does, shared with the Case Mismatch Finder (`forge_sites::lint_case`).

pub mod convert;
mod job;
pub mod messages;

use std::path::Path;

use serde::{Deserialize, Serialize};

pub use self::convert::{
    ConvertOptions, ConvertPhase, ConvertProgress, ConvertReport, OLD_SUFFIX, RecoverOutcome,
    TMP_SUFFIX, TreeFlags, TreeStats, convert_tree, recover, tree_flags,
};
pub use self::job::{
    CASE_EVENT_VERSION, CaseConversions, CaseReasonCode, STATUS_TIMEOUT, case_event_payload,
    convert_dir_case_sensitive,
};
use crate::error::{ForgeError, IoResultExt, Result};
use crate::paths::posix_string;
use crate::platform::{CaseSensitiveFs, CaseSensitivitySupport, MIN_CASE_SENSITIVE_BUILD};

/// Folds `name` for case-insensitive comparison: each character is replaced by its
/// simple uppercase mapping when that is a single character (what the NTFS upcase table
/// does), so `ß` stays `ß` and `ǆ` becomes `Ǆ`. Two names collide in an insensitive
/// folder when their folds are equal.
pub fn fold_case(name: &str) -> String {
    name.chars()
        .map(|c| {
            let mut upper = c.to_uppercase();
            match (upper.next(), upper.next()) {
                (Some(u), None) => u,
                _ => c,
            }
        })
        .collect()
}

/// Case sensitivity of one folder (docs/15 §3, `forge fs case-sensitive status`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseStatus {
    /// The folder (`/` separators).
    pub dir: String,
    /// The volume and OS can use the flag here.
    pub supported: bool,
    /// The folder itself is case-sensitive; `None` when unsupported or unreadable.
    pub enabled: Option<bool>,
    /// File system of the volume (`NTFS`, `FAT32`...), when known.
    pub fs_type: Option<String>,
    /// OS build number, when known.
    pub os_build: Option<u32>,
    /// The OS is new enough (Windows 10 1803+); `None` when unknown (other OSes).
    pub win_version_ok: Option<bool>,
    /// Why it is unsupported, or why the flag could not be read.
    pub reason: Option<String>,
}

/// Status of the existing folder `dir`. `NotFound`/`InvalidInput` when `dir` is missing
/// or not a folder; everything else is reported inside [`CaseStatus`]. Blocking.
pub fn status(cs: &dyn CaseSensitiveFs, dir: &Path) -> Result<CaseStatus> {
    let meta = std::fs::metadata(dir).at("inspecting", dir)?;
    if !meta.is_dir() {
        return Err(ForgeError::InvalidInput(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    let info = cs.info(dir);
    let win_version_ok = if cfg!(windows) {
        info.os_build.map(|b| b >= MIN_CASE_SENSITIVE_BUILD)
    } else {
        None
    };
    let (supported, enabled, reason) = match cs.support(dir) {
        CaseSensitivitySupport::Supported => match cs.get(dir) {
            Ok(enabled) => (true, Some(enabled), None),
            Err(err) => (true, None, Some(err.to_string())),
        },
        CaseSensitivitySupport::Unsupported(reason) => (false, None, Some(reason)),
    };
    Ok(CaseStatus {
        dir: posix_string(dir),
        supported,
        enabled,
        fs_type: info.fs_type,
        os_build: info.os_build,
        win_version_ok,
        reason,
    })
}

#[cfg(test)]
pub(crate) mod fake {
    //! Path-keyed flag store for tests that never move directories (status, refusals
    //! before the first rename). A new directory inherits the flag of its closest
    //! ancestor that was set explicitly, as on NTFS. Renames are invisible to it, so the
    //! conversion itself is tested against the real NTFS implementation
    //! (`tests/case_convert.rs`).

    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use crate::error::{ForgeError, Result};
    use crate::platform::{CaseFsInfo, CaseSensitiveFs, CaseSensitivitySupport};

    #[derive(Default)]
    pub struct FakeCs {
        pub unsupported: Option<String>,
        /// `set` on this exact path fails with `NeedsElevation`.
        pub deny: Option<PathBuf>,
        pub flags: Mutex<HashMap<PathBuf, bool>>,
    }

    fn key(dir: &Path) -> PathBuf {
        std::path::absolute(dir).unwrap()
    }

    impl CaseSensitiveFs for FakeCs {
        fn support(&self, _dir: &Path) -> CaseSensitivitySupport {
            match &self.unsupported {
                Some(r) => CaseSensitivitySupport::Unsupported(r.clone()),
                None => CaseSensitivitySupport::Supported,
            }
        }
        fn get(&self, dir: &Path) -> Result<bool> {
            if !dir.is_dir() {
                return Err(ForgeError::NotFound(dir.display().to_string()));
            }
            let flags = self.flags.lock().unwrap();
            let mut cur = Some(key(dir));
            while let Some(p) = cur {
                if let Some(v) = flags.get(&p) {
                    return Ok(*v);
                }
                cur = p.parent().map(Path::to_path_buf);
            }
            Ok(false)
        }
        fn set(&self, dir: &Path, enabled: bool) -> Result<()> {
            if self.unsupported.is_some() {
                return Err(ForgeError::UnsupportedPlatform("fake".into()));
            }
            if self.deny.as_deref().map(key) == Some(key(dir)) {
                return Err(ForgeError::NeedsElevation("fs.set_case_sensitive".into()));
            }
            self.flags.lock().unwrap().insert(key(dir), enabled);
            Ok(())
        }
        fn info(&self, _dir: &Path) -> CaseFsInfo {
            CaseFsInfo {
                fs_type: Some("FAKEFS".into()),
                os_build: Some(19045),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeCs;
    use super::*;

    #[test]
    fn fold_case_matches_ntfs_style() {
        assert_eq!(fold_case("Header.php"), fold_case("header.PHP"));
        assert_ne!(fold_case("Header.php"), fold_case("Headers.php"));
        assert_eq!(fold_case("Ação.txt"), "AÇÃO.TXT");
        // Full mappings that change the length are not applied (NTFS does not).
        assert_eq!(fold_case("straße"), "STRAßE");
    }

    #[test]
    fn status_reports_flag_and_details() {
        let base = tempfile::tempdir().unwrap();
        let cs = FakeCs::default();
        let st = status(&cs, base.path()).unwrap();
        assert!(st.supported);
        assert_eq!(st.enabled, Some(false));
        assert_eq!(st.fs_type.as_deref(), Some("FAKEFS"));
        assert_eq!(st.os_build, Some(19045));
        if cfg!(windows) {
            assert_eq!(st.win_version_ok, Some(true));
        }
        assert!(!st.dir.contains('\\'));
        cs.set(base.path(), true).unwrap();
        assert_eq!(status(&cs, base.path()).unwrap().enabled, Some(true));
    }

    #[test]
    fn status_of_unsupported_volume_and_missing_dir() {
        let base = tempfile::tempdir().unwrap();
        let cs = FakeCs {
            unsupported: Some("FAT32".into()),
            ..FakeCs::default()
        };
        let st = status(&cs, base.path()).unwrap();
        assert!(!st.supported);
        assert_eq!(st.enabled, None);
        assert_eq!(st.reason.as_deref(), Some("FAT32"));
        let err = status(&cs, &base.path().join("missing")).unwrap_err();
        assert_eq!(err.code(), "not_found");
        let file = base.path().join("f");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(status(&cs, &file).unwrap_err().code(), "invalid_input");
    }
}
