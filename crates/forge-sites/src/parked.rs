//! Parked directories (docs/06 §1.1): every sub-folder becomes the implicit site
//! `<folder>.test`.
//!
//! * [`scan`] lists the project folders of a parked directory (folders starting with `.`
//!   or `_` are ignored) with their detected driver.
//! * [`ParkedSync::reconcile`] makes the store match the disk: creates missing implicit
//!   sites, removes the ones whose folder is gone, refreshes root/driver when the project
//!   changed (for example after `composer create-project` filled the folder). An explicit
//!   site with the same host always wins: the implicit one is simply not created.
//! * [`ParkedWatcher`] runs a reconcile at start and after every burst of file system
//!   events, debounced (500 ms by default).
//!
//! The watch is **not recursive**: only the parked directory itself is observed, so
//! creating or deleting a project folder is seen at once, while a driver change inside a
//! project (for example `artisan` appearing) is picked up at the next reconcile.
//!
//! A parked directory that cannot be read (drive unplugged) is left alone: its sites are
//! not removed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::paths::posix_string;
use forge_core::store::sites::Site;
use notify::{EventKind as FsEventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::drivers;
use crate::service::SiteService;
use crate::site::{SiteDraft, host_label_for_folder};

/// Default debounce of file system events.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(500);
/// A burst of events never delays a reconcile by more than this.
const MAX_DEBOUNCE_WAIT: Duration = Duration::from_secs(5);
/// Budget of one reconcile (scan + store writes).
const RECONCILE_TIMEOUT: Duration = Duration::from_secs(60);

/// One parked directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedDir {
    pub path: PathBuf,
    /// TLD of the sites of this directory (without dot); `None` = the default TLD.
    pub tld: Option<String>,
}

impl ParkedDir {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            tld: None,
        }
    }
}

/// Configuration of the parked directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedConfig {
    pub dirs: Vec<ParkedDir>,
    /// TLD used by directories without their own (`test`).
    pub default_tld: String,
    /// Quiet time after the last file system event before reconciling.
    pub debounce: Duration,
}

impl ParkedConfig {
    pub fn new(dirs: Vec<ParkedDir>) -> Self {
        Self {
            dirs,
            default_tld: "test".to_owned(),
            debounce: DEFAULT_DEBOUNCE,
        }
    }

    fn tld_of(&self, dir: &ParkedDir) -> String {
        dir.tld
            .as_deref()
            .unwrap_or(&self.default_tld)
            .trim_start_matches('.')
            .to_ascii_lowercase()
    }
}

/// A project folder found in a parked directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedEntry {
    /// Folder name as on disk.
    pub folder: String,
    pub project_dir: PathBuf,
    /// `<label>.<tld>`.
    pub host: String,
    pub driver: &'static str,
    pub root: PathBuf,
}

/// Whether a folder name takes part in parking: hidden (`.git`) and `_`-prefixed folders
/// (`_old`, `_backup`) do not.
pub fn is_parkable(name: &str) -> bool {
    !(name.starts_with('.') || name.starts_with('_'))
}

/// Project folders of `dir`, sorted by name. Blocking.
pub fn scan(dir: &Path, tld: &str) -> Result<Vec<ParkedEntry>> {
    let read = std::fs::read_dir(dir).map_err(|source| ForgeError::Io {
        context: format!("reading parked directory {}", dir.display()),
        source,
    })?;
    let mut entries = Vec::new();
    for item in read.flatten() {
        let folder = item.file_name().to_string_lossy().into_owned();
        // `is_dir` follows junctions/symlinks to projects kept on other drives.
        if !is_parkable(&folder) || !item.path().is_dir() {
            continue;
        }
        let Some(label) = host_label_for_folder(&folder) else {
            tracing::warn!(folder, "parked folder name cannot become a host; ignored");
            continue;
        };
        let project_dir = item.path();
        let found = drivers::detect(&project_dir, None)?;
        entries.push(ParkedEntry {
            folder,
            host: format!("{label}.{tld}"),
            driver: found.driver.id(),
            root: found.root,
            project_dir,
        });
    }
    entries.sort_by(|a, b| a.folder.cmp(&b.folder));
    Ok(entries)
}

/// What a reconcile changed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub updated: Vec<String>,
    /// Hosts skipped because another site owns them (explicit wins).
    pub skipped: Vec<String>,
    /// Parked directories that could not be read and were left untouched.
    pub unreadable: Vec<PathBuf>,
}

/// Key of a project folder in the maps below: `<dir>/<folder>` in POSIX form.
fn project_key(dir: &Path, folder: &str) -> String {
    format!("{}/{folder}", posix_string(dir).trim_end_matches('/'))
}

/// Project key of an implicit site: `root` is the document root, which is the project
/// folder or a path below it; the key is the first path component below a parked dir.
fn site_project_key(site: &Site, dirs: &[ParkedDir]) -> Option<String> {
    let root = site.root.as_deref()?;
    dirs.iter().find_map(|d| {
        let base = posix_string(&d.path);
        let base = base.trim_end_matches('/');
        let rest = root.strip_prefix(base)?.strip_prefix('/')?;
        let folder = rest.split('/').next().filter(|f| !f.is_empty())?;
        Some(format!("{base}/{folder}"))
    })
}

/// Keeps the implicit sites of the store equal to the folders on disk.
#[derive(Debug, Clone)]
pub struct ParkedSync {
    sites: SiteService,
    config: ParkedConfig,
}

impl ParkedSync {
    pub fn new(sites: SiteService, config: ParkedConfig) -> Self {
        Self { sites, config }
    }

    /// One full pass: scan the disk, compare with the store, apply the difference.
    pub async fn reconcile(&self) -> Result<ReconcileReport> {
        let mut report = ReconcileReport::default();
        let dirs = self.config.dirs.clone();
        let config = self.config.clone();
        let scans = tokio::task::spawn_blocking(move || {
            dirs.iter()
                .map(|d| (d.clone(), scan(&d.path, &config.tld_of(d))))
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|e| ForgeError::Internal(format!("parked scan task: {e}")))?;

        let mut desired: BTreeMap<String, ParkedEntry> = BTreeMap::new();
        let mut readable: Vec<ParkedDir> = Vec::new();
        for (dir, result) in scans {
            match result {
                Ok(entries) => {
                    for entry in entries {
                        desired
                            .entry(project_key(&dir.path, &entry.folder))
                            .or_insert(entry);
                    }
                    readable.push(dir);
                }
                Err(err) => {
                    tracing::warn!(dir = %dir.path.display(), %err, "parked directory unreadable");
                    report.unreadable.push(dir.path);
                }
            }
        }

        let existing: BTreeMap<String, Site> = self
            .sites
            .list()
            .await?
            .into_iter()
            .filter(|s| s.parked)
            .filter_map(|s| site_project_key(&s, &self.config.dirs).map(|k| (k, s)))
            .collect();

        for (key, entry) in &desired {
            match existing.get(key) {
                Some(site) => {
                    let root = posix_string(&entry.root);
                    if site.root.as_deref() != Some(root.as_str())
                        || site.driver.as_deref() != Some(entry.driver)
                    {
                        let mut updated = site.clone();
                        let draft = SiteDraft::from_dir(&entry.project_dir, &entry.host, None)?;
                        updated.root = draft.root.as_deref().map(posix_string);
                        updated.driver = draft.driver;
                        updated.kind = draft.kind;
                        self.sites.update(updated).await?;
                        report.updated.push(entry.host.clone());
                    }
                }
                None => {
                    let mut draft = SiteDraft::from_dir(&entry.project_dir, &entry.host, None)?;
                    draft.parked = true;
                    match self.sites.create_parked_if_free(draft).await? {
                        Some(site) => report.added.push(site.host),
                        None => report.skipped.push(entry.host.clone()),
                    }
                }
            }
        }

        let readable_keys: BTreeSet<String> = readable
            .iter()
            .map(|d| posix_string(&d.path).trim_end_matches('/').to_owned())
            .collect();
        for (key, site) in &existing {
            let in_readable_dir = key
                .rsplit_once('/')
                .is_some_and(|(base, _)| readable_keys.contains(base));
            if in_readable_dir && !desired.contains_key(key) {
                self.sites.remove(site.id).await?;
                report.removed.push(site.host.clone());
            }
        }
        Ok(report)
    }
}

/// Watches the parked directories and keeps the store in sync. Dropping it (or
/// [`ParkedWatcher::stop`]) stops the watch.
pub struct ParkedWatcher {
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
    // Kept alive: dropping the watcher stops the notifications.
    _watcher: RecommendedWatcher,
}

impl std::fmt::Debug for ParkedWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParkedWatcher").finish_non_exhaustive()
    }
}

impl ParkedWatcher {
    /// Starts watching `sync`'s directories and runs the first reconcile in the
    /// background. Directories that do not exist are logged and skipped.
    pub fn start(sync: ParkedSync) -> Result<Self> {
        // Capacity 1 + try_send: a signal carries no data, so extra ones coalesce.
        let (tx, mut rx) = mpsc::channel::<()>(1);
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let relevant = match &res {
                Ok(event) => !matches!(event.kind, FsEventKind::Access(_)),
                // Errors (for example buffer overflow) mean events were lost: rescan.
                Err(_) => true,
            };
            if relevant {
                let _ = tx.try_send(());
            }
        })
        .map_err(|e| ForgeError::Internal(format!("creating the file watcher: {e}")))?;
        for dir in &sync.config.dirs {
            if let Err(err) = watcher.watch(&dir.path, RecursiveMode::NonRecursive) {
                tracing::warn!(dir = %dir.path.display(), %err, "cannot watch parked directory");
            }
        }

        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let debounce = sync.config.debounce;
        let task = tokio::spawn(async move {
            run_reconcile(&sync).await;
            loop {
                tokio::select! {
                    () = token.cancelled() => return,
                    got = rx.recv() => if got.is_none() { return },
                }
                // Debounce: wait for a quiet period, but never longer than the cap.
                let started = tokio::time::Instant::now();
                loop {
                    let left = MAX_DEBOUNCE_WAIT.saturating_sub(started.elapsed());
                    let wait = debounce.min(left);
                    tokio::select! {
                        () = token.cancelled() => return,
                        res = tokio::time::timeout(wait, rx.recv()) => match res {
                            Ok(Some(())) if !left.is_zero() => continue,
                            Ok(None) => return,
                            _ => break,
                        },
                    }
                }
                run_reconcile(&sync).await;
            }
        });
        Ok(Self {
            cancel,
            task: Some(task),
            _watcher: watcher,
        })
    }

    /// Stops the watch and waits for the background task (up to 5 s).
    pub async fn stop(mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
        }
    }
}

impl Drop for ParkedWatcher {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn run_reconcile(sync: &ParkedSync) {
    match tokio::time::timeout(RECONCILE_TIMEOUT, sync.reconcile()).await {
        Ok(Ok(report)) => {
            if !(report.added.is_empty() && report.removed.is_empty() && report.updated.is_empty())
            {
                tracing::info!(?report, "parked sites reconciled");
            }
        }
        Ok(Err(err)) => tracing::warn!(%err, "parked reconcile failed"),
        Err(_) => tracing::warn!("parked reconcile timed out"),
    }
}
