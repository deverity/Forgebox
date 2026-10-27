//! Reversible conversion of an existing folder tree to case-sensitive (or back), by
//! "moving into a new flagged folder" (docs/15 §3.1, ADR-0010).
//!
//! The NTFS flag belongs to each directory and is inherited only when a directory is
//! **created** inside a flagged one; moving (renaming) a directory keeps the flag it had.
//! So moving the top-level entries of `www/` into a flagged folder would leave every
//! project inside insensitive. The conversion therefore recreates the directory tree
//! inside `<dir>.cs-tmp` (each new directory inherits the flag, which is checked and set
//! when it did not) and renames every **file** and every **link** (symbolic link,
//! junction or other reparse point: moved as a whole, never followed) into it. Renames on
//! the same volume are metadata only: no file content is copied.
//!
//! Steps, with the state left on disk if the process dies in between:
//!
//! | step | on disk afterwards | [`recover`] does |
//! |---|---|---|
//! | 1. scan: counts, bytes, flags, collisions, sample hashes | `dir` | nothing |
//! | 2. create `<dir>.cs-tmp`, set the flag | `dir`, empty tmp | moves tmp back, removes it |
//! | 3. recreate directories, rename files/links | `dir` (thinning), tmp (filling) | same |
//! | 4. swap: `dir` → `<dir>.cs-old`, tmp → `dir` | `.cs-old` skeleton, `dir` | removes the empty skeleton |
//! | 5. verify counts, bytes, flags and sample hashes | same | same |
//! | 6. remove the empty `.cs-old` skeleton | `dir` | nothing |
//!
//! A failure in steps 2–5 (a file held open without `FILE_SHARE_DELETE`, a folder that
//! is some process's current directory, cancellation, a verification mismatch) undoes
//! everything done so far by moving the entries back, so the tree is exactly as before.
//! Nothing is ever deleted except **empty** directories this module created or emptied;
//! directory removal is `remove_dir` (fails on non-empty), never a recursive delete.
//!
//! Reverting (`enable = false`) is the same operation towards an insensitive folder. It
//! is refused up front when a directory holds names that differ only by case (they
//! would collide), and every rename refuses to replace an existing entry.
//!
//! What is preserved: every file and link with its content, attributes, timestamps, ACL
//! and identity (a rename keeps all of them). Directories are recreated: their
//! attributes (read-only, hidden, system...) and timestamps are copied
//! ([`CaseSensitiveFs::apply_dir_meta`]); **explicit ACL entries of directories are
//! not** (new directories inherit the ACL of their parent, which is the common case for
//! projects). Content behind junctions/symbolic links is not converted.
//!
//! Everything here is blocking: async code goes through [`super::CaseConversions`].

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::fold_case;
use super::messages;
use crate::error::{ForgeError, IoResultExt, Result};
use crate::paths::posix_string;
use crate::platform::{CaseSensitiveFs, CaseSensitivitySupport, DirMeta, is_reparse_point};

/// Suffix of the folder the tree is moved into (`www` → `www.cs-tmp`).
pub const TMP_SUFFIX: &str = ".cs-tmp";
/// Suffix of the emptied original after the swap (`www` → `www.cs-old`).
pub const OLD_SUFFIX: &str = ".cs-old";

/// Tuning of [`convert_tree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertOptions {
    /// Files hashed (SHA-256) before the move and compared after the swap.
    pub sample_files: usize,
    /// Files larger than this are not sampled (hashing a video says nothing more than
    /// the size check, and takes long).
    pub sample_max_bytes: u64,
}

impl Default for ConvertOptions {
    /// 20 samples of at most 64 MiB.
    fn default() -> Self {
        Self {
            sample_files: 20,
            sample_max_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Step of a conversion, reported through the progress callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConvertPhase {
    Recovering,
    Scanning,
    Preparing,
    Moving,
    Swapping,
    Verifying,
    Cleaning,
    RollingBack,
}

/// Progress of a conversion: `done` of `total` units of the current phase (entries for
/// scanning/moving/verifying; `total` is 0 when unknown).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConvertProgress {
    pub phase: ConvertPhase,
    pub done: u64,
    pub total: u64,
}

/// Counts of a tree. `directories` includes the root.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeStats {
    pub directories: u64,
    /// Regular files.
    pub files: u64,
    /// Symbolic links, junctions and other reparse points (moved, never followed).
    pub links: u64,
    /// Total size of the regular files.
    pub bytes: u64,
}

impl TreeStats {
    /// Entries a conversion moves or recreates: every file, link and subdirectory.
    pub fn entries(&self) -> u64 {
        self.files + self.links + self.directories.saturating_sub(1)
    }
}

/// Result of a successful [`convert_tree`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvertReport {
    /// Converted folder (`/` separators).
    pub dir: String,
    /// `true`: the tree is now case-sensitive; `false`: insensitive (revert).
    pub enabled: bool,
    #[serde(flatten)]
    pub stats: TreeStats,
    /// Files whose SHA-256 was compared before and after.
    pub sampled: u32,
    /// Every directory already had the requested state: nothing was moved.
    pub already: bool,
    /// Leftovers of an interrupted conversion were restored first.
    pub recovered: bool,
    /// `<dir>.cs-old` kept because something wrote into it during the conversion (it
    /// held files, not only empty directories). `None` normally.
    pub leftover: Option<String>,
    pub duration_ms: u64,
}

/// What [`recover`] found and did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum RecoverOutcome {
    /// No leftovers.
    Nothing,
    /// An interrupted conversion was undone: the original tree is back in place.
    Restored,
    /// The conversion had completed; only the empty `.cs-old` skeleton was left and it
    /// was removed (or kept in `leftover` because it is not empty).
    Completed { leftover: Option<String> },
}

/// `<dir>.cs-tmp` and `<dir>.cs-old` next to `dir`. `InvalidInput` for a volume root.
pub fn sibling_paths(dir: &Path) -> Result<(PathBuf, PathBuf)> {
    let name = dir.file_name().ok_or_else(|| {
        ForgeError::InvalidInput(format!(
            "{} has no parent folder: a volume root cannot be converted",
            dir.display()
        ))
    })?;
    let with = |suffix: &str| {
        let mut n: OsString = name.to_os_string();
        n.push(suffix);
        dir.with_file_name(n)
    };
    Ok((with(TMP_SUFFIX), with(OLD_SUFFIX)))
}

/// Whether `path` exists without following a final link.
fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// `dir` must be an existing real directory (not a link: converting through a junction
/// would move the content of its target).
fn check_dir(dir: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(dir).at("inspecting", dir)?;
    if is_reparse_point(&meta) {
        return Err(ForgeError::InvalidInput(format!(
            "{} is a link (junction or symbolic link); convert its target instead",
            dir.display()
        )));
    }
    if !meta.is_dir() {
        return Err(ForgeError::InvalidInput(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    Ok(())
}

/// Entries of `dir` sorted by name (deterministic order, reproducible failures).
fn sorted_entries(dir: &Path) -> Result<Vec<(OsString, fs::Metadata)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).at("listing", dir)? {
        let entry = entry.at("listing", dir)?;
        // On Windows `DirEntry::metadata` comes from the directory listing itself and
        // describes the entry, not a link target.
        let meta = entry.metadata().at("inspecting", &entry.path())?;
        out.push((entry.file_name(), meta));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Kind of a directory entry for the conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Dir,
    File,
    Link,
}

fn kind_of(meta: &fs::Metadata) -> EntryKind {
    if is_reparse_point(meta) {
        EntryKind::Link
    } else if meta.is_dir() {
        EntryKind::Dir
    } else {
        EntryKind::File
    }
}

/// Small deterministic PRNG (xorshift64*) for reservoir sampling: the same tree always
/// yields the same samples, without a `rand` dependency in the core.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-enough value in `0..n` (`n > 0`).
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A sampled file: path relative to the root, size and SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sample {
    rel: PathBuf,
    size: u64,
    sha256: [u8; 32],
}

fn sha256_file(path: &Path) -> Result<[u8; 32]> {
    let mut file = fs::File::open(path).at("reading", path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).at("reading", path)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

/// What [`scan`] learned about a tree.
#[derive(Debug, Default)]
struct Scan {
    stats: TreeStats,
    /// Directories (root included) whose flag already equals the requested state.
    in_state: u64,
    /// `dir/A.txt` + `dir/a.txt` style groups (`/`-joined, relative), only collected
    /// when `want_collisions`.
    collisions: Vec<String>,
    samples: Vec<Sample>,
}

/// Walks the tree at `root` (iteratively, never following links): counts, flag states,
/// name collisions and a reservoir of sample files, hashed at the end.
fn scan(
    cs: &dyn CaseSensitiveFs,
    root: &Path,
    enable: bool,
    want_collisions: bool,
    opts: &ConvertOptions,
    progress: &mut dyn FnMut(ConvertProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<Scan> {
    let mut out = Scan::default();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut seen_files: u64 = 0;
    let mut reservoir: Vec<(PathBuf, u64)> = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        check_cancel(cancel)?;
        let dir = root.join(&rel);
        out.stats.directories += 1;
        if matches!(cs.get(&dir), Ok(state) if state == enable) {
            out.in_state += 1;
        }
        let entries = sorted_entries(&dir)?;
        if want_collisions {
            let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for (name, _) in &entries {
                let name = name.to_string_lossy().into_owned();
                groups.entry(fold_case(&name)).or_default().push(name);
            }
            for names in groups.into_values().filter(|g| g.len() > 1) {
                let prefix = posix_string(&rel);
                let joined = names
                    .iter()
                    .map(|n| {
                        if prefix.is_empty() {
                            n.clone()
                        } else {
                            format!("{prefix}/{n}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" + ");
                out.collisions.push(joined);
            }
        }
        for (name, meta) in entries {
            match kind_of(&meta) {
                EntryKind::Dir => stack.push(rel.join(&name)),
                EntryKind::Link => out.stats.links += 1,
                EntryKind::File => {
                    out.stats.files += 1;
                    out.stats.bytes += meta.len();
                    if opts.sample_files > 0 && meta.len() <= opts.sample_max_bytes {
                        seen_files += 1;
                        let item = (rel.join(&name), meta.len());
                        if reservoir.len() < opts.sample_files {
                            reservoir.push(item);
                        } else {
                            let j = rng.below(seen_files);
                            if let Ok(j) = usize::try_from(j) {
                                if j < opts.sample_files {
                                    reservoir[j] = item;
                                }
                            }
                        }
                    }
                }
            }
        }
        progress(ConvertProgress {
            phase: ConvertPhase::Scanning,
            done: out.stats.entries(),
            total: 0,
        });
    }
    reservoir.sort();
    for (rel, size) in reservoir {
        check_cancel(cancel)?;
        let sha256 = sha256_file(&root.join(&rel))?;
        out.samples.push(Sample { rel, size, sha256 });
    }
    Ok(out)
}

fn check_cancel(cancel: &dyn Fn() -> bool) -> Result<()> {
    if cancel() {
        Err(ForgeError::Cancelled(
            "case sensitivity conversion".to_owned(),
        ))
    } else {
        Ok(())
    }
}

/// Makes sure the (empty) directory `dir` has the flag `enable`.
fn ensure_flag(cs: &dyn CaseSensitiveFs, dir: &Path, enable: bool) -> Result<()> {
    if cs.get(dir)? == enable {
        return Ok(());
    }
    cs.set(dir, enable)?;
    if cs.get(dir)? == enable {
        Ok(())
    } else {
        Err(ForgeError::Internal(format!(
            "{}: case sensitivity set to {enable} but not reported back",
            dir.display()
        )))
    }
}

/// `Conflict` with the "in use" text for sharing violations and access denied (what a
/// rename of a file held open without `FILE_SHARE_DELETE` returns), `Io` otherwise.
fn move_error(path: &Path, err: io::Error) -> ForgeError {
    // ERROR_ACCESS_DENIED (5), ERROR_SHARING_VIOLATION (32), ERROR_LOCK_VIOLATION (33).
    let in_use = matches!(err.raw_os_error(), Some(5 | 32 | 33))
        || err.kind() == io::ErrorKind::PermissionDenied;
    if in_use {
        ForgeError::Conflict(messages::in_use(
            &path.display().to_string(),
            &err.to_string(),
        ))
    } else {
        ForgeError::io_at("moving", path, err)
    }
}

/// Moves the content of `src_root` into `dst_root` (which exists), recreating
/// directories and renaming files and links.
struct Mover<'a> {
    cs: &'a dyn CaseSensitiveFs,
    src_root: &'a Path,
    dst_root: &'a Path,
    /// Flag every created directory must have (conversion); `None` when restoring.
    flag: Option<bool>,
    /// Restoring: an entry whose destination exists is skipped (left in the source)
    /// instead of failing, so as much as possible goes back.
    lenient: bool,
    phase: ConvertPhase,
    done: u64,
    total: u64,
    /// Entries left behind in lenient mode.
    skipped: Vec<PathBuf>,
}

enum Step {
    Enter(PathBuf),
    Exit(PathBuf, Option<DirMeta>),
}

impl Mover<'_> {
    fn tick(&mut self, progress: &mut dyn FnMut(ConvertProgress)) {
        self.done += 1;
        if self.done.is_multiple_of(64) || self.done == self.total {
            progress(ConvertProgress {
                phase: self.phase,
                done: self.done,
                total: self.total,
            });
        }
    }

    fn run(
        &mut self,
        progress: &mut dyn FnMut(ConvertProgress),
        cancel: &dyn Fn() -> bool,
    ) -> Result<()> {
        let mut stack = vec![Step::Enter(PathBuf::new())];
        while let Some(step) = stack.pop() {
            match step {
                Step::Enter(rel) => {
                    let src = self.src_root.join(&rel);
                    let dst = self.dst_root.join(&rel);
                    // Captured before the entries leave (that changes the timestamps).
                    let meta = match self.flag {
                        Some(_) => self.cs.capture_dir_meta(&src),
                        None => None,
                    };
                    if !rel.as_os_str().is_empty() {
                        match fs::create_dir(&dst) {
                            Ok(()) => {
                                if let Some(flag) = self.flag {
                                    ensure_flag(self.cs, &dst, flag)?;
                                }
                            }
                            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && self.lenient => {}
                            Err(e) => return Err(ForgeError::io_at("creating", &dst, e)),
                        }
                        self.tick(progress);
                    }
                    stack.push(Step::Exit(rel.clone(), meta));
                    let mut subdirs = Vec::new();
                    for (name, meta) in sorted_entries(&src)? {
                        if !self.lenient {
                            check_cancel(cancel)?;
                        }
                        if kind_of(&meta) == EntryKind::Dir {
                            subdirs.push(rel.join(&name));
                            continue;
                        }
                        let from = src.join(&name);
                        let to = dst.join(&name);
                        if exists(&to) {
                            if self.lenient {
                                self.skipped.push(from);
                                continue;
                            }
                            return Err(ForgeError::Conflict(format!(
                                "{} already exists; refusing to replace it",
                                to.display()
                            )));
                        }
                        match fs::rename(&from, &to) {
                            Ok(()) => {}
                            Err(e) if self.lenient => {
                                tracing::warn!(from = %from.display(), err = %e, "could not move entry back");
                                self.skipped.push(from);
                                continue;
                            }
                            Err(e) => return Err(move_error(&from, e)),
                        }
                        self.tick(progress);
                    }
                    for dir in subdirs.into_iter().rev() {
                        stack.push(Step::Enter(dir));
                    }
                }
                Step::Exit(rel, meta) => {
                    if let Some(meta) = meta {
                        let dst = self.dst_root.join(&rel);
                        if let Err(err) = self.cs.apply_dir_meta(&dst, &meta) {
                            tracing::warn!(dir = %dst.display(), %err, "could not copy directory attributes");
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Removes the tree at `root` if it holds only directories (bottom-up `remove_dir`;
/// files and links are never deleted). Returns `false` when something other than an
/// empty directory was found (the tree is then left as is from that point up).
fn remove_empty_tree(root: &Path) -> Result<bool> {
    if !exists(root) {
        return Ok(true);
    }
    let mut order = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for (name, meta) in sorted_entries(&dir)? {
            if kind_of(&meta) != EntryKind::Dir {
                return Ok(false);
            }
            stack.push(dir.join(name));
        }
        order.push(dir);
    }
    for dir in order.into_iter().rev() {
        if let Err(err) = fs::remove_dir(&dir) {
            // A read-only directory cannot be removed until the attribute is cleared.
            let cleared = fs::metadata(&dir)
                .map(|m| {
                    let mut perms = m.permissions();
                    if perms.readonly() {
                        #[allow(clippy::permissions_set_readonly_false)]
                        perms.set_readonly(false);
                        fs::set_permissions(&dir, perms).is_ok()
                    } else {
                        false
                    }
                })
                .unwrap_or(false);
            if !cleared {
                return Err(ForgeError::io_at("removing", &dir, err));
            }
            fs::remove_dir(&dir).at("removing", &dir)?;
        }
    }
    Ok(true)
}

/// Moves everything from `tmp` back into `dir` (which must exist) and removes the
/// emptied `tmp`. Errors if something could not go back (it stays in `tmp`).
fn restore_into(
    cs: &dyn CaseSensitiveFs,
    tmp: &Path,
    dir: &Path,
    progress: &mut dyn FnMut(ConvertProgress),
) -> Result<()> {
    if !exists(tmp) {
        return Ok(());
    }
    let mut mover = Mover {
        cs,
        src_root: tmp,
        dst_root: dir,
        flag: None,
        lenient: true,
        phase: ConvertPhase::RollingBack,
        done: 0,
        total: 0,
        skipped: Vec::new(),
    };
    mover.run(progress, &|| false)?;
    if !mover.skipped.is_empty() {
        let first = mover.skipped[0].display().to_string();
        return Err(ForgeError::Conflict(format!(
            "{} entr(ies) could not be moved back from {} (first: {first}); run the \
             conversion again to recover",
            mover.skipped.len(),
            tmp.display()
        )));
    }
    if !remove_empty_tree(tmp)? {
        return Err(ForgeError::Conflict(format!(
            "{} still holds files after the rollback; run the conversion again to recover",
            tmp.display()
        )));
    }
    Ok(())
}

/// Puts back the state before a conversion of `dir` (absolute) that was interrupted
/// (crash, power loss) — see the table in the module docs. Idempotent.
pub fn recover(
    cs: &dyn CaseSensitiveFs,
    dir: &Path,
    progress: &mut dyn FnMut(ConvertProgress),
) -> Result<RecoverOutcome> {
    let (tmp, old) = sibling_paths(dir)?;
    let (has_dir, has_tmp, has_old) = (exists(dir), exists(&tmp), exists(&old));
    if has_tmp || has_old {
        progress(ConvertProgress {
            phase: ConvertPhase::Recovering,
            done: 0,
            total: 0,
        });
    }
    match (has_dir, has_tmp, has_old) {
        (_, false, false) => Ok(RecoverOutcome::Nothing),
        // Interrupted while moving (or rolling back): everything goes back into dir.
        (true, true, false) => {
            restore_into(cs, &tmp, dir, progress)?;
            Ok(RecoverOutcome::Restored)
        }
        // Interrupted between the two renames of the swap.
        (false, true, true) => {
            fs::rename(&old, dir).map_err(|e| move_error(&old, e))?;
            restore_into(cs, &tmp, dir, progress)?;
            Ok(RecoverOutcome::Restored)
        }
        // Swap done: only the empty skeleton is left.
        (true, false, true) => {
            let leftover = if remove_empty_tree(&old)? {
                None
            } else {
                Some(posix_string(&old))
            };
            Ok(RecoverOutcome::Completed { leftover })
        }
        // Only the skeleton: the swap's first rename happened and the second was undone.
        (false, false, true) => {
            fs::rename(&old, dir).map_err(|e| move_error(&old, e))?;
            Ok(RecoverOutcome::Restored)
        }
        (false, true, false) | (true, true, true) => Err(ForgeError::Conflict(format!(
            "unexpected leftovers of a case sensitivity conversion next to {} ({}{}); \
             check them by hand",
            dir.display(),
            if has_tmp { TMP_SUFFIX } else { "" },
            if has_old {
                format!(" {OLD_SUFFIX}")
            } else {
                String::new()
            }
        ))),
    }
}

/// Converts the tree at `dir` to case-sensitive (`enable`) or back to insensitive,
/// reporting progress and polling `cancel` between entries. See the module docs for the
/// steps, the guarantees and what is preserved. Blocking.
///
/// Errors: `NotFound`/`InvalidInput` for a bad `dir`; `UnsupportedPlatform` when the
/// volume or OS cannot use the flag; `NeedsElevation` when the flag cannot be set on the
/// new folder (route through `forgebox-helper`); `Conflict` when an entry is in use or
/// when reverting would merge names that differ only by case; `VerificationFailed` when
/// the converted tree does not match the scan; `Cancelled`. In every error case after
/// the scan the original tree is restored before returning.
pub fn convert_tree(
    cs: &dyn CaseSensitiveFs,
    dir: &Path,
    enable: bool,
    opts: &ConvertOptions,
    progress: &mut dyn FnMut(ConvertProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<ConvertReport> {
    let started = Instant::now();
    let dir = std::path::absolute(dir).at("resolving", dir)?;
    let (tmp, old) = sibling_paths(&dir)?;

    let recovered = !matches!(recover(cs, &dir, progress)?, RecoverOutcome::Nothing);
    check_dir(&dir)?;
    if let CaseSensitivitySupport::Unsupported(reason) = cs.support(&dir) {
        return Err(ForgeError::UnsupportedPlatform(reason));
    }

    let before = scan(cs, &dir, enable, !enable, opts, progress, cancel)?;
    let report =
        |stats: TreeStats, sampled: usize, already: bool, leftover: Option<String>| ConvertReport {
            dir: posix_string(&dir),
            enabled: enable,
            stats,
            sampled: u32::try_from(sampled).unwrap_or(u32::MAX),
            already,
            recovered,
            leftover,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        };
    if !before.collisions.is_empty() {
        let examples = before
            .collisions
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        return Err(ForgeError::Conflict(messages::case_collisions(
            &examples,
            before.collisions.len(),
        )));
    }
    if before.in_state == before.stats.directories {
        tracing::info!(dir = %dir.display(), enable, "case sensitivity already in place");
        return Ok(report(before.stats, 0, true, None));
    }

    progress(ConvertProgress {
        phase: ConvertPhase::Preparing,
        done: 0,
        total: 0,
    });
    fs::create_dir(&tmp).at("creating", &tmp)?;
    if let Err(err) = ensure_flag(cs, &tmp, enable) {
        let _ = fs::remove_dir(&tmp);
        return Err(err);
    }

    // Steps 3 (move) and 4 (swap): any failure restores the original tree.
    let mut mover = Mover {
        cs,
        src_root: &dir,
        dst_root: &tmp,
        flag: Some(enable),
        lenient: false,
        phase: ConvertPhase::Moving,
        done: 0,
        total: before.stats.entries(),
        skipped: Vec::new(),
    };
    if let Err(err) = mover.run(progress, cancel) {
        tracing::warn!(dir = %dir.display(), %err, "case conversion failed while moving; restoring");
        return Err(with_rollback(err, restore_into(cs, &tmp, &dir, progress)));
    }
    if let Some(meta) = cs.capture_dir_meta(&dir) {
        if let Err(err) = cs.apply_dir_meta(&tmp, &meta) {
            tracing::warn!(dir = %tmp.display(), %err, "could not copy directory attributes");
        }
    }
    progress(ConvertProgress {
        phase: ConvertPhase::Swapping,
        done: 0,
        total: 0,
    });
    if let Err(e) = fs::rename(&dir, &old) {
        let err = move_error(&dir, e);
        return Err(with_rollback(err, restore_into(cs, &tmp, &dir, progress)));
    }
    if let Err(e) = fs::rename(&tmp, &dir) {
        let err = move_error(&tmp, e);
        let undo = fs::rename(&old, &dir)
            .map_err(|e| move_error(&old, e))
            .and_then(|()| restore_into(cs, &tmp, &dir, progress));
        return Err(with_rollback(err, undo));
    }

    // Step 5: the new tree must hold exactly what the scan saw.
    progress(ConvertProgress {
        phase: ConvertPhase::Verifying,
        done: 0,
        total: before.stats.entries(),
    });
    if let Err(err) = verify(cs, &dir, enable, &before) {
        tracing::error!(dir = %dir.display(), %err, "case conversion verification failed; undoing");
        let undo = fs::rename(&dir, &tmp)
            .map_err(|e| move_error(&dir, e))
            .and_then(|()| fs::rename(&old, &dir).map_err(|e| move_error(&old, e)))
            .and_then(|()| restore_into(cs, &tmp, &dir, progress));
        return Err(with_rollback(err, undo));
    }

    // Step 6.
    progress(ConvertProgress {
        phase: ConvertPhase::Cleaning,
        done: 0,
        total: 0,
    });
    let leftover = match remove_empty_tree(&old) {
        Ok(true) => None,
        Ok(false) => {
            tracing::warn!(old = %old.display(), "files appeared in the old tree during the conversion; kept");
            Some(posix_string(&old))
        }
        Err(err) => {
            tracing::warn!(old = %old.display(), %err, "could not remove the old tree");
            Some(posix_string(&old))
        }
    };
    let out = report(before.stats, before.samples.len(), false, leftover);
    tracing::info!(
        dir = %dir.display(), enable, files = out.stats.files, dirs = out.stats.directories,
        bytes = out.stats.bytes, ms = out.duration_ms, "case sensitivity conversion done"
    );
    Ok(out)
}

/// Adds the outcome of the rollback to the original error.
fn with_rollback(err: ForgeError, rollback: Result<()>) -> ForgeError {
    match rollback {
        Ok(()) => err,
        Err(rb) => {
            tracing::error!(%err, rollback = %rb, "case conversion rollback incomplete");
            ForgeError::Internal(format!("{err}; rollback incomplete: {rb}"))
        }
    }
}

/// Compares the converted tree with the scan taken before the move.
fn verify(cs: &dyn CaseSensitiveFs, dir: &Path, enable: bool, before: &Scan) -> Result<()> {
    let no_samples = ConvertOptions {
        sample_files: 0,
        ..ConvertOptions::default()
    };
    let after = scan(cs, dir, enable, false, &no_samples, &mut |_| {}, &|| false)?;
    if after.stats != before.stats {
        return Err(ForgeError::VerificationFailed(format!(
            "{}: tree changed during the conversion (before {:?}, after {:?})",
            dir.display(),
            before.stats,
            after.stats
        )));
    }
    if after.in_state != after.stats.directories {
        return Err(ForgeError::VerificationFailed(format!(
            "{}: {} of {} directories do not have case sensitivity {enable}",
            dir.display(),
            after.stats.directories - after.in_state,
            after.stats.directories
        )));
    }
    for sample in &before.samples {
        let path = dir.join(&sample.rel);
        let size = fs::metadata(&path).at("inspecting", &path)?.len();
        if size != sample.size || sha256_file(&path)? != sample.sha256 {
            return Err(ForgeError::VerificationFailed(format!(
                "{}: content differs after the conversion",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Counts of the flag over a bounded walk of a tree (status, boot re-verification).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeFlags {
    /// Directories looked at (root included).
    pub directories: u64,
    /// Of those, how many are case-sensitive.
    pub sensitive: u64,
    /// The walk stopped at `max_dirs` or `max_depth` before seeing everything.
    pub truncated: bool,
}

/// Walks at most `max_depth` levels below `dir` (0 = only `dir`) and `max_dirs`
/// directories, counting case-sensitive ones. Links are not followed. Blocking.
pub fn tree_flags(
    cs: &dyn CaseSensitiveFs,
    dir: &Path,
    max_depth: usize,
    max_dirs: u64,
) -> Result<TreeFlags> {
    check_dir(dir)?;
    let mut out = TreeFlags::default();
    let mut queue = std::collections::VecDeque::from([(dir.to_path_buf(), 0usize)]);
    while let Some((current, depth)) = queue.pop_front() {
        if out.directories >= max_dirs {
            out.truncated = true;
            break;
        }
        out.directories += 1;
        if cs.get(&current)? {
            out.sensitive += 1;
        }
        let entries = match sorted_entries(&current) {
            Ok(entries) => entries,
            // A subfolder we may not list (ACL) does not make the whole check fail.
            Err(err) if depth > 0 => {
                tracing::debug!(dir = %current.display(), %err, "skipping unreadable folder");
                continue;
            }
            Err(err) => return Err(err),
        };
        for (name, meta) in entries {
            if kind_of(&meta) == EntryKind::Dir {
                if depth >= max_depth {
                    out.truncated = true;
                } else {
                    queue.push_back((current.join(name), depth + 1));
                }
            }
        }
    }
    Ok(out)
}
