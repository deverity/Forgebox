//! Async side of the case sensitivity operations: jobs, timeouts and events.
//!
//! The conversion is a long operation (docs/03 §7.4): [`CaseConversions::convert`]
//! returns a [`JobId`] at once and runs [`convert_tree`] in `spawn_blocking`, mapping its
//! progress to `job.progress` and polling the job's cancellation token between entries
//! (a cancelled conversion restores the original tree and ends `cancelled`). At most one
//! conversion per folder runs at a time in the process: a second one fails with
//! `conflict` (`fs.case_conversions` leaf lock, see `crate::actor`).
//!
//! Short queries ([`CaseConversions::status`], [`CaseConversions::set_flag`]) run in
//! `spawn_blocking` with [`STATUS_TIMEOUT`].
//!
//! Events (docs/03 §8, payload `forge_ipc::api::events::FsCaseSensitivityPayload`):
//! `fs.case_sensitivity_applied` after a successful conversion to case-sensitive, and
//! `fs.case_sensitivity_unavailable` after a revert (`reason_code: disabled`) or when the
//! volume cannot use the flag (`reason_code: unsupported`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::convert::{ConvertOptions, ConvertPhase, ConvertProgress, ConvertReport, convert_tree};
use super::{CaseStatus, TreeFlags, messages, status, tree_flags};
use crate::error::{ForgeError, Result};
use crate::event::{EventBus, EventKind};
use crate::ids::{JobId, RequestId, SiteId};
use crate::jobs::{JobContext, JobProgress, JobRegistry, JobSpec};
use crate::paths::posix_string;
use crate::platform::CaseSensitiveFs;
use crate::runtime::spawn_blocking_timeout;
use crate::sync::{LockLevel, OrderedMutex};

/// Version of the `fs.case_sensitivity_*` payloads.
pub const CASE_EVENT_VERSION: u32 = 1;

/// Budget of the short blocking queries (status, flag of one folder, bounded walks).
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(10);

/// Stable code of why a folder or site is not case-sensitive (`reason_code` of
/// `fs.case_sensitivity_unavailable`; also the i18n key suffix `fs.case.reason.<code>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseReasonCode {
    /// Volume or OS without support (FAT/exFAT/ReFS, Windows < 1803, policy).
    Unsupported,
    /// Turned off on purpose (reverted).
    Disabled,
    /// Never converted.
    Insensitive,
    /// The folder is flagged but some subfolders are not (only `enable` was used).
    Partial,
    /// The folder does not exist.
    Missing,
    /// The state could not be read.
    Error,
    /// A database instance keeps the table name case it was initialized with
    /// (`lower_case_table_names` cannot change on an initialized data folder, docs/15 §5).
    Initialized,
}

impl CaseReasonCode {
    /// Wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Disabled => "disabled",
            Self::Insensitive => "insensitive",
            Self::Partial => "partial",
            Self::Missing => "missing",
            Self::Error => "error",
            Self::Initialized => "initialized",
        }
    }
}

/// Payload of `fs.case_sensitivity_applied|unavailable` (wire form of
/// `forge_ipc::api::events::FsCaseSensitivityPayload`). `dir` uses `/` separators;
/// `reason` is the pt-BR text, `reason_code` its stable code.
pub fn case_event_payload(
    dir: &str,
    site_id: Option<SiteId>,
    reason: Option<(CaseReasonCode, &str)>,
) -> serde_json::Value {
    serde_json::json!({
        "v": CASE_EVENT_VERSION,
        "dir": dir,
        "site_id": site_id,
        "reason": reason.map(|(_, text)| text),
        "reason_code": reason.map(|(code, _)| code.as_str()),
    })
}

/// Folders being converted in this process (canonical paths). Leaf lock: held only to
/// insert or remove one path.
static BUSY: OrderedMutex<BTreeSet<PathBuf>> =
    OrderedMutex::new(LockLevel::CASE_CONVERSIONS, BTreeSet::new());

/// Marks a folder as being converted until dropped.
struct BusyGuard(PathBuf);

impl BusyGuard {
    fn acquire(dir: &Path) -> Result<Self> {
        // Canonical form: two spellings of the same folder (`C:\WWW`, `c:\www`) are one.
        let key = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        if BUSY.lock().insert(key.clone()) {
            Ok(Self(key))
        } else {
            Err(ForgeError::Conflict(format!(
                "a case sensitivity conversion of {} is already running",
                dir.display()
            )))
        }
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.lock().remove(&self.0);
    }
}

/// `job.progress` for a conversion step: percentage bands per phase and a pt-BR message.
fn job_progress(p: ConvertProgress) -> JobProgress {
    let ratio = if p.total > 0 {
        p.done.min(p.total) as f32 / p.total as f32
    } else {
        0.0
    };
    let (_, text) = messages::phase(p.phase);
    let message = if p.total > 0 {
        format!("{text} ({}/{})", p.done, p.total)
    } else if p.done > 0 {
        format!("{text} ({})", p.done)
    } else {
        text.to_owned()
    };
    let progress = JobProgress::new().message(message);
    match p.phase {
        ConvertPhase::Recovering | ConvertPhase::Scanning => progress.pct(5.0),
        ConvertPhase::Preparing => progress.pct(10.0),
        ConvertPhase::Moving => progress.pct(10.0 + 70.0 * ratio),
        ConvertPhase::Swapping => progress.pct(80.0),
        ConvertPhase::Verifying => progress.pct(85.0),
        ConvertPhase::Cleaning => progress.pct(95.0),
        ConvertPhase::RollingBack => progress,
    }
}

/// Entry point of the case sensitivity operations for the daemon. Cheap to clone.
#[derive(Clone)]
pub struct CaseConversions {
    jobs: JobRegistry,
    cs: Arc<dyn CaseSensitiveFs>,
    options: ConvertOptions,
}

impl std::fmt::Debug for CaseConversions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaseConversions")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl CaseConversions {
    /// Conversions started on `jobs` (events go to its bus) using `cs`.
    pub fn new(jobs: JobRegistry, cs: Arc<dyn CaseSensitiveFs>) -> Self {
        Self {
            jobs,
            cs,
            options: ConvertOptions::default(),
        }
    }

    /// Replaces the sampling options.
    pub fn with_options(mut self, options: ConvertOptions) -> Self {
        self.options = options;
        self
    }

    /// The job registry conversions run on.
    pub fn jobs(&self) -> &JobRegistry {
        &self.jobs
    }

    /// The platform implementation in use.
    pub fn case_sensitive_fs(&self) -> &Arc<dyn CaseSensitiveFs> {
        &self.cs
    }

    fn bus(&self) -> &EventBus {
        self.jobs.events()
    }

    /// Starts converting `dir` (`forge fs case-sensitive convert <dir> [--revert]`) and
    /// returns the job id at once; the job's result is a [`ConvertReport`]. A repeated
    /// `request_id` returns the job already started for it.
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn convert(&self, dir: PathBuf, enable: bool, request_id: Option<RequestId>) -> JobId {
        let mut spec = JobSpec::new(format!(
            "fs.case_convert:{}{}",
            posix_string(&dir),
            if enable { "" } else { " --revert" }
        ));
        if let Some(request_id) = request_id {
            spec = spec.request_id(request_id);
        }
        let this = self.clone();
        self.jobs
            .spawn(spec, move |ctx| async move {
                this.run(&ctx, dir, enable, None).await
            })
            .id()
    }

    /// Runs one conversion inside an existing job (`ctx`): progress and cancellation go
    /// through it. Used by [`CaseConversions::convert`] and by the per-site job of
    /// `forge_sites` (which passes the site id for the event). Publishes
    /// `fs.case_sensitivity_*` at the end.
    pub async fn run(
        &self,
        ctx: &JobContext,
        dir: PathBuf,
        enable: bool,
        site_id: Option<SiteId>,
    ) -> Result<ConvertReport> {
        let cs = Arc::clone(&self.cs);
        let options = self.options.clone();
        let token = ctx.cancellation_token();
        let progress_ctx = ctx.clone();
        let target = dir.clone();
        // No timeout: the duration grows with the tree and the work cannot be abandoned
        // halfway; it is cancellable instead (and restores the tree when cancelled).
        let joined = tokio::task::spawn_blocking(move || {
            let _busy = BusyGuard::acquire(&target)?;
            let mut report = |p: ConvertProgress| progress_ctx.progress(job_progress(p));
            convert_tree(cs.as_ref(), &target, enable, &options, &mut report, &|| {
                token.is_cancelled()
            })
        })
        .await;
        let result = joined.unwrap_or_else(|join| {
            Err(ForgeError::Internal(format!(
                "case sensitivity conversion task failed: {join}"
            )))
        });
        self.publish_outcome(&dir, site_id, enable, &result);
        result
    }

    fn publish_outcome(
        &self,
        dir: &Path,
        site_id: Option<SiteId>,
        enable: bool,
        result: &Result<ConvertReport>,
    ) {
        let dir_text = posix_string(dir);
        match result {
            Ok(_) if enable => {
                self.bus().publish(
                    EventKind::FsCaseSensitivityApplied,
                    case_event_payload(&dir_text, site_id, None),
                );
            }
            Ok(_) => {
                let (_, text) = messages::disabled();
                self.bus().publish(
                    EventKind::FsCaseSensitivityUnavailable,
                    case_event_payload(&dir_text, site_id, Some((CaseReasonCode::Disabled, text))),
                );
            }
            Err(ForgeError::UnsupportedPlatform(detail)) => {
                let (_, text) = messages::unsupported(detail);
                self.bus().publish(
                    EventKind::FsCaseSensitivityUnavailable,
                    case_event_payload(
                        &dir_text,
                        site_id,
                        Some((CaseReasonCode::Unsupported, &text)),
                    ),
                );
            }
            // Other failures are reported by `job.failed|cancelled`.
            Err(_) => {}
        }
    }

    /// [`super::status`] of `dir` (`forge fs case-sensitive status <dir>`).
    pub async fn status(&self, dir: PathBuf) -> Result<CaseStatus> {
        let cs = Arc::clone(&self.cs);
        spawn_blocking_timeout("case sensitivity status", STATUS_TIMEOUT, move || {
            status(cs.as_ref(), &dir)
        })
        .await
    }

    /// Turns the flag of the single folder `dir` on or off (`forge fs case-sensitive
    /// enable|disable <dir>`): not recursive, existing subfolders keep theirs (use
    /// [`CaseConversions::convert`] for a whole tree). Returns the new status.
    pub async fn set_flag(&self, dir: PathBuf, enable: bool) -> Result<CaseStatus> {
        let cs = Arc::clone(&self.cs);
        let result = spawn_blocking_timeout("setting case sensitivity", STATUS_TIMEOUT, {
            let dir = dir.clone();
            move || {
                cs.set(&dir, enable)?;
                status(cs.as_ref(), &dir)
            }
        })
        .await;
        if let Err(ForgeError::UnsupportedPlatform(detail)) = &result {
            let (_, text) = messages::unsupported(detail);
            self.bus().publish(
                EventKind::FsCaseSensitivityUnavailable,
                case_event_payload(
                    &posix_string(&dir),
                    None,
                    Some((CaseReasonCode::Unsupported, &text)),
                ),
            );
        }
        result
    }

    /// [`super::tree_flags`] of `dir` with the given bounds.
    pub async fn tree_flags(
        &self,
        dir: PathBuf,
        max_depth: usize,
        max_dirs: u64,
    ) -> Result<TreeFlags> {
        let cs = Arc::clone(&self.cs);
        spawn_blocking_timeout("case sensitivity walk", STATUS_TIMEOUT, move || {
            tree_flags(cs.as_ref(), &dir, max_depth, max_dirs)
        })
        .await
    }
}

/// Starts the conversion of `dir` on `jobs` and returns its id (shorthand for
/// [`CaseConversions::convert`] without a request id).
///
/// # Panics
/// Outside a tokio runtime.
pub fn convert_dir_case_sensitive(
    jobs: &JobRegistry,
    cs: Arc<dyn CaseSensitiveFs>,
    dir: PathBuf,
    enable: bool,
) -> JobId {
    CaseConversions::new(jobs.clone(), cs).convert(dir, enable, None)
}

#[cfg(test)]
mod tests {
    use super::super::fake::FakeCs;
    use super::*;
    use crate::jobs::JobState;

    #[test]
    fn payload_shape() {
        let id = SiteId::new();
        let v = case_event_payload("C:/www/loja", Some(id), None);
        assert_eq!(v["v"], 1);
        assert_eq!(v["dir"], "C:/www/loja");
        assert_eq!(v["site_id"], serde_json::to_value(id).unwrap());
        assert!(v["reason"].is_null() && v["reason_code"].is_null());
        let v = case_event_payload("C:/x", None, Some((CaseReasonCode::Partial, "texto")));
        assert_eq!(v["reason_code"], "partial");
        assert_eq!(v["reason"], "texto");
        assert!(v["site_id"].is_null());
    }

    #[test]
    fn progress_bands() {
        let p = job_progress(ConvertProgress {
            phase: ConvertPhase::Moving,
            done: 50,
            total: 100,
        });
        assert_eq!(p.pct, Some(45.0));
        assert!(p.message.unwrap().contains("50/100"));
        let p = job_progress(ConvertProgress {
            phase: ConvertPhase::RollingBack,
            done: 0,
            total: 0,
        });
        assert_eq!(p.pct, None);
    }

    #[test]
    fn busy_guard_is_exclusive_per_folder() {
        let base = tempfile::tempdir().unwrap();
        let a = BusyGuard::acquire(base.path()).unwrap();
        let err = BusyGuard::acquire(base.path()).err().unwrap();
        assert_eq!(err.code(), "conflict");
        drop(a);
        BusyGuard::acquire(base.path()).unwrap();
    }

    #[tokio::test]
    async fn unsupported_volume_fails_the_job_and_emits_unavailable() {
        let base = tempfile::tempdir().unwrap();
        let bus = EventBus::new();
        let mut events = bus.subscribe();
        let jobs = JobRegistry::new(bus.clone());
        let cs: Arc<dyn CaseSensitiveFs> = Arc::new(FakeCs {
            unsupported: Some("FAT32".into()),
            ..FakeCs::default()
        });
        let conv = CaseConversions::new(jobs.clone(), cs);
        let id = conv.convert(base.path().to_path_buf(), true, None);
        let handle_info = loop {
            if let Some(info) = jobs.get(id) {
                if info.state.is_terminal() {
                    break info;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(handle_info.state, JobState::Failed);
        assert_eq!(handle_info.error.unwrap().code, "unsupported_platform");
        let mut kinds = Vec::new();
        while let Some(ev) = events.try_recv() {
            if ev.kind == EventKind::FsCaseSensitivityUnavailable {
                assert_eq!(ev.payload["reason_code"], "unsupported");
            }
            kinds.push(ev.kind);
        }
        assert!(
            kinds.contains(&EventKind::FsCaseSensitivityUnavailable),
            "{kinds:?}"
        );
        // the folder is untouched
        assert!(base.path().is_dir());
    }

    #[tokio::test]
    async fn status_and_set_flag_run_off_the_runtime() {
        let base = tempfile::tempdir().unwrap();
        let jobs = JobRegistry::new(EventBus::new());
        let conv = CaseConversions::new(jobs, Arc::new(FakeCs::default()));
        let st = conv.status(base.path().to_path_buf()).await.unwrap();
        assert_eq!(st.enabled, Some(false));
        let st = conv
            .set_flag(base.path().to_path_buf(), true)
            .await
            .unwrap();
        assert_eq!(st.enabled, Some(true));
        let flags = conv
            .tree_flags(base.path().to_path_buf(), 1, 100)
            .await
            .unwrap();
        assert_eq!((flags.directories, flags.sensitive), (1, 1));
    }

    #[tokio::test]
    async fn elevation_needed_for_the_new_folder_leaves_nothing_behind() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("www");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("index.php"), "<?php").unwrap();
        let (tmp, _) = super::super::convert::sibling_paths(&dir).unwrap();
        let jobs = JobRegistry::new(EventBus::new());
        let cs = Arc::new(FakeCs {
            deny: Some(tmp.clone()),
            ..FakeCs::default()
        });
        let conv = CaseConversions::new(jobs.clone(), cs);
        let id = conv.convert(dir.clone(), true, None);
        let info = loop {
            match jobs.get(id) {
                Some(info) if info.state.is_terminal() => break info,
                _ => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        };
        assert_eq!(info.error.unwrap().code, "needs_elevation");
        assert!(!tmp.exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("index.php")).unwrap(),
            "<?php"
        );
    }
}
