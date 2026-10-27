//! Archive extraction (docs/03 §5.1, docs/04 `install.archive`/`strip_root`, docs/11 §2):
//! zip, 7z, tar.gz, tar.xz and single files (`archive: file`).
//!
//! Pipeline of [`Extractor::extract`], all in `spawn_blocking`:
//! 1. a fresh staging directory `tmp/stage/<ulid>/` with the case sensitivity of the
//!    destination's parent (`bin/` stays case-insensitive, docs/15);
//! 2. every entry is checked before it is written (`..`, absolute paths, drive letters,
//!    reserved names, illegal characters, links/reparse points, devices, encrypted data);
//!    zip and 7z are scanned completely first, so a bad entry rejects the archive before
//!    anything is written. Rejections are `verification_failed`;
//! 3. a size budget ([`ExtractLimits`]) is enforced on the bytes actually written
//!    (zip bombs, lying headers) and on the declared sizes up front, plus an entry count
//!    limit;
//! 4. `strip_root` (`auto`: drop a single top-level folder when every entry is under it);
//! 5. the tree is moved into `dest` in one rename (or copied to a hidden sibling, checked
//!    and renamed, when staging is on another volume), so `dest` never holds a partial
//!    installation. The staging directory is removed on success and on failure.
//!
//! File modification times are preserved (zip: NTFS/extended timestamp fields, else the
//! DOS time taken as UTC; tar: header mtime; 7z: entry mtime); directory times are not.
//! No permission bits are applied: executables need none on Windows. Progress is reported
//! per entry (`job.progress`), and cancellation is checked between entries and buffers.

mod entry;
mod formats;
mod install;

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::jobs::{JobContext, JobProgress};
use forge_core::platform::{CaseSensitivitySupport, Platform};
pub use forge_manifest::schema::tool::{ArchiveFormat, StripRoot};
use serde::{Deserialize, Serialize};

use self::formats::TarCompression;
use self::install::StageGuard;
use crate::messages;

/// Copy buffer for entry data.
const BUFFER_SIZE: usize = 256 * 1024;
/// Minimum interval between progress reports.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Budgets that stop decompression bombs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractLimits {
    /// Total bytes written (sum of all files). Default 4 GiB.
    pub max_total_bytes: u64,
    /// Entries (files, directories). Default 200 000.
    pub max_entries: u64,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: 4 * 1024 * 1024 * 1024,
            max_entries: 200_000,
        }
    }
}

/// What to extract and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractRequest {
    /// The (already verified) downloaded file.
    pub archive: PathBuf,
    pub format: ArchiveFormat,
    /// Final directory, e.g. `bin/php/8.4.14-nts-vs17-x64`. Created by the final move;
    /// its parent is created if needed.
    pub dest: PathBuf,
    /// `tmp/stage/` of the root ([`forge_core::paths::ForgeRoot::tmp_stage`]); best on the
    /// same volume as `dest` so the final move is a rename.
    pub stage_root: PathBuf,
    pub strip_root: StripRoot,
    pub limits: ExtractLimits,
    /// Replace an existing `dest` (reinstall/repair). Default: `conflict` if it exists.
    pub replace_existing: bool,
    /// [`ArchiveFormat::File`] only: name of the file inside `dest` (default: the name of
    /// `archive`).
    pub file_name: Option<String>,
    /// Tests only: take the cross-volume path (copy + check + rename) even on one volume.
    #[doc(hidden)]
    pub force_copy: bool,
}

impl ExtractRequest {
    /// Request with `strip_root: auto`, default limits and no replacement.
    pub fn new(
        archive: impl Into<PathBuf>,
        format: ArchiveFormat,
        dest: impl Into<PathBuf>,
        stage_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            archive: archive.into(),
            format,
            dest: dest.into(),
            stage_root: stage_root.into(),
            strip_root: StripRoot::Auto,
            limits: ExtractLimits::default(),
            replace_existing: false,
            file_name: None,
            force_copy: false,
        }
    }

    pub fn strip_root(mut self, strip_root: StripRoot) -> Self {
        self.strip_root = strip_root;
        self
    }

    pub fn limits(mut self, limits: ExtractLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn replace_existing(mut self, replace: bool) -> Self {
        self.replace_existing = replace;
        self
    }

    pub fn file_name(mut self, name: impl Into<String>) -> Self {
        self.file_name = Some(name.into());
        self
    }
}

/// Result of a successful extraction (`job.done` payload of extraction steps).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractReport {
    pub dest: PathBuf,
    /// Files written.
    pub files: u64,
    /// Directory entries in the archive.
    pub dirs: u64,
    /// Bytes written.
    pub bytes: u64,
    /// Top-level folder removed by `strip_root`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stripped_root: Option<String>,
    /// The staging directory was on another volume: the tree was copied, not renamed.
    pub copied: bool,
}

/// Progress of an extraction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExtractProgress {
    pub entries_done: u64,
    /// Known for zip and 7z (indexed formats).
    pub entries_total: Option<u64>,
    pub bytes_done: u64,
    /// Declared uncompressed size (zip, 7z).
    pub bytes_total: Option<u64>,
    /// `0.0..=100.0`: by entries for indexed formats, by compressed bytes read for tar.
    pub pct: f32,
    /// Extraction finished, the tree is being moved into place.
    pub installing: bool,
}

/// Writes validated entries under the staging directory and keeps the budgets.
pub(crate) struct Sink<'a> {
    root: PathBuf,
    limits: ExtractLimits,
    entries: u64,
    files: u64,
    dirs: u64,
    bytes: u64,
    entries_total: Option<u64>,
    bytes_total: Option<u64>,
    stream: Option<(Arc<AtomicU64>, u64)>,
    buffer: Vec<u8>,
    last_report: Option<Instant>,
    progress: &'a mut dyn FnMut(ExtractProgress),
    cancelled: &'a dyn Fn() -> bool,
}

impl<'a> Sink<'a> {
    fn new(
        root: PathBuf,
        limits: ExtractLimits,
        progress: &'a mut dyn FnMut(ExtractProgress),
        cancelled: &'a dyn Fn() -> bool,
    ) -> Self {
        Self {
            root,
            limits,
            entries: 0,
            files: 0,
            dirs: 0,
            bytes: 0,
            entries_total: None,
            bytes_total: None,
            stream: None,
            buffer: vec![0; BUFFER_SIZE],
            last_report: None,
            progress,
            cancelled,
        }
    }

    fn over_budget(&self, what: String) -> ForgeError {
        ForgeError::VerificationFailed(format!(
            "archive exceeds the extraction limit of {} bytes ({what}); possible \
             decompression bomb",
            self.limits.max_total_bytes
        ))
    }

    /// Rejects an archive whose declared sizes already exceed the budget.
    pub(crate) fn check_declared(&self, declared: u64) -> Result<()> {
        if declared > self.limits.max_total_bytes {
            return Err(self.over_budget(format!("declares at least {declared} bytes")));
        }
        Ok(())
    }

    pub(crate) fn set_totals(&mut self, entries: Option<u64>, bytes: Option<u64>) {
        self.entries_total = entries;
        self.bytes_total = bytes;
        self.report(true);
    }

    /// For streamed formats: shared counter of compressed bytes read, and the total.
    pub(crate) fn set_stream_position(&mut self, read: Arc<AtomicU64>, total: u64) {
        self.stream = Some((read, total));
    }

    fn check_cancelled(&self) -> Result<()> {
        if (self.cancelled)() {
            return Err(ForgeError::Cancelled("extraction".into()));
        }
        Ok(())
    }

    fn snapshot(&self) -> ExtractProgress {
        let pct = match (self.entries_total, &self.stream) {
            (Some(total), _) if total > 0 => self.entries as f32 * 100.0 / total as f32,
            (_, Some((read, total))) if *total > 0 => {
                read.load(Ordering::Relaxed) as f32 * 100.0 / *total as f32
            }
            _ => 0.0,
        };
        ExtractProgress {
            entries_done: self.entries,
            entries_total: self.entries_total,
            bytes_done: self.bytes,
            bytes_total: self.bytes_total,
            pct: pct.clamp(0.0, 100.0),
            installing: false,
        }
    }

    fn report(&mut self, force: bool) {
        if !force
            && self
                .last_report
                .is_some_and(|t| t.elapsed() < PROGRESS_INTERVAL)
        {
            return;
        }
        self.last_report = Some(Instant::now());
        let snapshot = self.snapshot();
        (self.progress)(snapshot);
    }

    /// Counts one more entry (budget, cancellation, progress).
    pub(crate) fn begin_entry(&mut self) -> Result<()> {
        self.check_cancelled()?;
        self.entries += 1;
        if self.entries > self.limits.max_entries {
            return Err(ForgeError::VerificationFailed(format!(
                "archive has more than {} entries",
                self.limits.max_entries
            )));
        }
        self.report(false);
        Ok(())
    }

    pub(crate) fn dir(&mut self, rel: &Path) -> Result<()> {
        let path = self.root.join(rel);
        std::fs::create_dir_all(&path).at("creating", &path)?;
        self.dirs += 1;
        Ok(())
    }

    /// Writes entry `name` (validated as `rel`) from `data`. `declared` is the size the
    /// archive announces; the bytes really produced must match it.
    pub(crate) fn file(
        &mut self,
        name: &str,
        rel: &Path,
        data: &mut dyn Read,
        declared: Option<u64>,
        mtime: Option<SystemTime>,
    ) -> Result<()> {
        if let Some(size) = declared {
            let after = self.bytes.saturating_add(size);
            if after > self.limits.max_total_bytes {
                return Err(self.over_budget(format!("{name:?} would bring it to {after} bytes")));
            }
        }
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).at("creating", parent)?;
        }
        if path.is_dir() {
            return Err(entry::unsafe_entry(
                name,
                "file with the same name as a directory",
            ));
        }
        let mut out = File::create(&path).at("creating", &path)?;
        let mut written = 0u64;
        loop {
            self.check_cancelled()?;
            let n = match data.read(&mut self.buffer) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Err(ForgeError::VerificationFailed(format!(
                        "corrupt archive entry {name:?}: {e}"
                    )));
                }
            };
            written += n as u64;
            self.bytes += n as u64;
            if self.bytes > self.limits.max_total_bytes {
                return Err(self.over_budget(format!("while writing {name:?}")));
            }
            if declared.is_some_and(|d| written > d) {
                return Err(ForgeError::VerificationFailed(format!(
                    "archive entry {name:?} is larger than its declared size"
                )));
            }
            out.write_all(&self.buffer[..n]).at("writing", &path)?;
            self.report(false);
        }
        if let Some(size) = declared {
            if written != size {
                return Err(ForgeError::VerificationFailed(format!(
                    "archive entry {name:?} has {written} bytes, declared {size}"
                )));
            }
        }
        if let Some(mtime) = mtime {
            if let Err(e) = out.set_modified(mtime) {
                tracing::debug!(path = %path.display(), "could not set modification time: {e}");
            }
        }
        self.files += 1;
        Ok(())
    }
}

/// Extracts archives. Cheap to clone.
#[derive(Clone)]
pub struct Extractor {
    platform: Platform,
}

impl std::fmt::Debug for Extractor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Extractor")
            .field("platform", &self.platform)
            .finish()
    }
}

impl Extractor {
    /// Uses the case-sensitivity implementation of `platform` for the staging directory.
    pub fn new(platform: &Platform) -> Self {
        Self {
            platform: platform.clone(),
        }
    }

    /// Extracts in `spawn_blocking`.
    pub async fn extract(&self, req: ExtractRequest) -> Result<ExtractReport> {
        self.run(req, None).await
    }

    /// Like [`Extractor::extract`], as a step of a job: `job.progress` per entry
    /// ("Extraindo 120/2400"), `cancelled` when the job is cancelled.
    pub async fn extract_in_job(
        &self,
        ctx: &JobContext,
        req: ExtractRequest,
    ) -> Result<ExtractReport> {
        self.run(req, Some(ctx.clone())).await
    }

    async fn run(&self, req: ExtractRequest, ctx: Option<JobContext>) -> Result<ExtractReport> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut progress = |p: ExtractProgress| {
                let Some(ctx) = &ctx else { return };
                let message = if p.installing {
                    messages::installing_files()
                } else {
                    messages::extracting(p.entries_done, p.entries_total)
                };
                let mut update = JobProgress::new().pct(p.pct).message(message);
                if !p.installing {
                    update = update.bytes(p.bytes_done, p.bytes_total);
                }
                ctx.progress(update);
            };
            let cancelled = || ctx.as_ref().is_some_and(JobContext::is_cancelled);
            this.extract_blocking(&req, &mut progress, &cancelled)
        })
        .await
        .map_err(|e| ForgeError::Internal(format!("extraction task failed: {e}")))?
    }

    /// The extraction itself (blocking).
    pub fn extract_blocking(
        &self,
        req: &ExtractRequest,
        progress: &mut dyn FnMut(ExtractProgress),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<ExtractReport> {
        if req.format == ArchiveFormat::Msi {
            return Err(ForgeError::InvalidInput(
                "msi packages are not supported (docs/04 install.archive)".into(),
            ));
        }
        let meta = std::fs::metadata(&req.archive).at("inspecting", &req.archive)?;
        if !meta.is_file() {
            return Err(ForgeError::InvalidInput(format!(
                "{} is not a file",
                req.archive.display()
            )));
        }
        let dest_parent = req.dest.parent().filter(|p| !p.as_os_str().is_empty());
        let (Some(dest_parent), Some(_)) = (dest_parent, req.dest.file_name()) else {
            return Err(ForgeError::InvalidInput(format!(
                "extraction destination {} has no parent folder",
                req.dest.display()
            )));
        };
        if !req.replace_existing && std::fs::symlink_metadata(&req.dest).is_ok() {
            return Err(ForgeError::Conflict(format!(
                "{} already exists",
                req.dest.display()
            )));
        }

        std::fs::create_dir_all(&req.stage_root).at("creating", &req.stage_root)?;
        std::fs::create_dir_all(dest_parent).at("creating", dest_parent)?;
        let id = ulid::Ulid::generate().to_string();
        let stage = req.stage_root.join(&id);
        std::fs::create_dir(&stage).at("creating", &stage)?;
        let _guard = StageGuard(stage.clone());
        let cs = self.platform.case_sensitive_fs();
        if matches!(cs.support(&stage), CaseSensitivitySupport::Supported) {
            install::match_case_sensitivity(cs, &stage, dest_parent);
        }

        let mut sink = Sink::new(stage.clone(), req.limits, progress, cancelled);
        match req.format {
            ArchiveFormat::Zip => formats::extract_zip(&req.archive, &mut sink)?,
            ArchiveFormat::SevenZ => formats::extract_7z(&req.archive, &mut sink)?,
            ArchiveFormat::TarGz => {
                formats::extract_tar(&req.archive, TarCompression::Gzip, &mut sink)?
            }
            ArchiveFormat::TarXz => {
                formats::extract_tar(&req.archive, TarCompression::Xz, &mut sink)?
            }
            ArchiveFormat::File => copy_single_file(req, meta.len(), &mut sink)?,
            ArchiveFormat::Msi => unreachable!("rejected above"),
        }
        sink.check_cancelled()?;
        let mut done = sink.snapshot();
        done.pct = 100.0;
        (sink.progress)(done);
        let Sink {
            files,
            dirs,
            bytes,
            progress,
            ..
        } = sink;

        let (source, stripped_root) = if req.format == ArchiveFormat::File {
            (stage.clone(), None)
        } else {
            install::resolve_strip(&stage, req.strip_root)?
        };
        progress(ExtractProgress {
            installing: true,
            ..done
        });
        let copied = install::install(
            &source,
            &req.dest,
            req.replace_existing,
            &id,
            req.force_copy,
        )?;
        tracing::info!(
            archive = %req.archive.display(),
            dest = %req.dest.display(),
            files,
            bytes,
            copied,
            "archive extracted"
        );
        Ok(ExtractReport {
            dest: req.dest.clone(),
            files,
            dirs,
            bytes,
            stripped_root,
            copied,
        })
    }
}

/// `archive: file`: the download itself is the installed file.
fn copy_single_file(req: &ExtractRequest, size: u64, sink: &mut Sink<'_>) -> Result<()> {
    let name = match &req.file_name {
        Some(name) => name.clone(),
        None => req
            .archive
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    let rel = entry::sanitize(&name)?
        .filter(|p| p.components().count() == 1)
        .ok_or_else(|| {
            ForgeError::InvalidInput(format!("{name:?} is not a valid single file name"))
        })?;
    sink.set_totals(Some(1), Some(size));
    sink.begin_entry()?;
    let mtime = std::fs::metadata(&req.archive)
        .and_then(|m| m.modified())
        .ok();
    let mut file = File::open(&req.archive).at("opening", &req.archive)?;
    sink.file(&name, &rel, &mut file, Some(size), mtime)
}

/// Extracts with the platform of the running OS. See [`Extractor::extract`].
pub async fn extract(req: ExtractRequest) -> Result<ExtractReport> {
    Extractor::new(&forge_core::platform::current())
        .extract(req)
        .await
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    #[test]
    fn sink_budgets_apply_to_bytes_actually_produced() {
        let dir = tempfile::tempdir().unwrap();
        let mut progress = |_: ExtractProgress| {};
        let cancelled = || false;
        let limits = ExtractLimits {
            max_total_bytes: 1000,
            max_entries: 2,
        };
        let mut sink = Sink::new(dir.path().to_path_buf(), limits, &mut progress, &cancelled);
        // No declared size (or a lying one): the budget still holds while writing.
        let mut endless = std::io::repeat(0).take(5000);
        let err = sink
            .file("bomb", Path::new("bomb.bin"), &mut endless, None, None)
            .unwrap_err();
        assert!(err.to_string().contains("decompression bomb"), "{err}");
        drop(sink);

        let mut sink = Sink::new(dir.path().to_path_buf(), limits, &mut progress, &cancelled);
        // Declared sizes must match what the decoder really produces.
        let mut more = std::io::repeat(1).take(100);
        let err = sink
            .file("liar", Path::new("liar.bin"), &mut more, Some(10), None)
            .unwrap_err();
        assert!(
            err.to_string().contains("larger than its declared size"),
            "{err}"
        );
        let mut less = std::io::repeat(1).take(5);
        let err = sink
            .file("short", Path::new("short.bin"), &mut less, Some(10), None)
            .unwrap_err();
        assert!(
            err.to_string().contains("has 5 bytes, declared 10"),
            "{err}"
        );

        // Entry budget.
        sink.begin_entry().unwrap();
        sink.begin_entry().unwrap();
        let err = sink.begin_entry().unwrap_err();
        assert_eq!(err.code(), "verification_failed");
    }

    #[test]
    fn sink_reports_progress_and_honours_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let mut seen = Vec::new();
        let mut progress = |p: ExtractProgress| seen.push(p);
        let stop = std::cell::Cell::new(false);
        let cancelled = || stop.get();
        let mut sink = Sink::new(
            dir.path().to_path_buf(),
            ExtractLimits::default(),
            &mut progress,
            &cancelled,
        );
        sink.set_totals(Some(2), Some(6));
        sink.begin_entry().unwrap();
        sink.file("a", Path::new("a.txt"), &mut &b"abc"[..], Some(3), None)
            .unwrap();
        stop.set(true);
        assert_eq!(sink.begin_entry().unwrap_err().code(), "cancelled");
        drop(sink);
        let first = seen.first().unwrap();
        assert_eq!(first.entries_total, Some(2));
        assert_eq!(first.bytes_total, Some(6));
    }

    #[test]
    fn default_limits() {
        let limits = ExtractLimits::default();
        assert_eq!(limits.max_total_bytes, 4 << 30);
        assert_eq!(limits.max_entries, 200_000);
    }

    #[test]
    fn report_wire_format() {
        let report = ExtractReport {
            dest: PathBuf::from("bin/nginx/1.30.6"),
            files: 2,
            dirs: 1,
            bytes: 10,
            stripped_root: Some("nginx-1.30.6".into()),
            copied: false,
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["stripped_root"], "nginx-1.30.6");
        assert_eq!(
            serde_json::from_value::<ExtractReport>(json).unwrap(),
            report
        );
    }
}
