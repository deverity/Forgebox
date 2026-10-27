//! The second step of the Laragon import: registers what an [`ImportPlan`] holds.
//!
//! Everything goes through the same code in a dry run and in the real import; the only
//! difference is that `dry_run` stops before each write (copy, move, database row, site,
//! preference). Existing rows are never changed: running the import twice reports the
//! second time "already registered" for everything and creates nothing.
//!
//! Order: preferences, tool versions (referenced where they are, or copied into
//! `bin/<tool>/`), database instances, parked folders (with the optional case-sensitivity
//! conversion first), sites. The store is only touched through its writer/reader
//! (`StoreWriter::execute`, `StoreReader::query`), the file copy runs in `spawn_blocking`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use forge_core::config::ForgeConfig;
use forge_core::error::{ForgeError, Result};
use forge_core::ids::{ToolId, VersionId};
use forge_core::paths::{ForgeRoot, posix_string};
use forge_core::platform::is_reparse_point;
use forge_core::store::{
    EXTERNAL_DATA_DIR_KEY, InstanceRepo, InstanceRow, NewToolVersion, StoreReader, StoreWriter,
    ToolVersionRepo, ToolVersionRow, VersionSource, VersionState,
};
use futures::future::BoxFuture;

use super::laragon::{LaragonImporter, norm};
use super::messages as msg;
use super::plan::{ImportOptions, ImportPlan, ImportReport, PlannedTool, kind};
use crate::drivers;
use crate::service::SiteService;
use crate::site::{SiteDraft, SitePhp, normalize_alias, normalize_host};

/// Converts a folder to case-sensitive (`CaseConversions::run` in the daemon). The
/// future owns everything it needs.
pub type CaseConvertFn = Arc<dyn Fn(PathBuf) -> BoxFuture<'static, Result<()>> + Send + Sync>;

/// What `apply` needs from its surroundings.
#[derive(Clone)]
pub struct ApplyEnv {
    pub root: ForgeRoot,
    pub writer: StoreWriter,
    pub reader: StoreReader,
    pub sites: SiteService,
    /// Current preferences; the changed copy comes back in [`ApplyOutcome::config`].
    pub config: ForgeConfig,
    /// Conversion to case-sensitive for `--convert-case`; `None` where unavailable.
    pub convert_case: Option<CaseConvertFn>,
}

/// Result of [`LaragonImporter::apply`].
#[derive(Debug, Clone)]
pub struct ApplyOutcome {
    pub report: ImportReport,
    /// Preferences after the import. The caller saves them when [`Self::config_changed`]
    /// (the importer does not own `forgebox.toml`) and re-runs the parked-folder sync.
    pub config: ForgeConfig,
    pub config_changed: bool,
    pub tool_versions: usize,
    pub instances: usize,
    pub sites: usize,
}

/// Tool versions of the plan that are (or, in a dry run, would be) in the store, by
/// `(tool, Laragon folder)`; the id is `None` for a version a dry run would register.
type Registered = BTreeMap<(ToolId, String), Option<VersionId>>;

fn same_path(a: &str, b: &str) -> bool {
    norm(a) == norm(b)
}

fn label(tool: &PlannedTool) -> String {
    format!("{} {} ({})", tool.tool_id, tool.version, tool.folder)
}

fn tld_of(host: &str) -> &str {
    host.rsplit('.').next().unwrap_or("")
}

impl LaragonImporter {
    /// Registers `plan` (see the module docs). Errors are reserved for failures of the
    /// store; anything that cannot be imported is a line of the report.
    pub async fn apply(
        plan: &ImportPlan,
        options: &ImportOptions,
        env: &ApplyEnv,
    ) -> Result<ApplyOutcome> {
        let mut report = ImportReport {
            dry_run: options.dry_run,
            items: Vec::new(),
        };
        let mut config = env.config.clone();
        let mut outcome = Counts::default();

        prefs(plan, &config, &mut report);
        let registered = tools(plan, options, env, &mut report, &mut outcome).await?;
        databases(plan, options, env, &registered, &mut report, &mut outcome).await?;
        parked(plan, options, env, &mut config, &mut report).await;
        sites(plan, options, env, &mut report, &mut outcome).await?;
        for s in &plan.skipped {
            report.push(s.kind, s.name.clone(), false, Some(s.reason.clone()));
        }

        let config_changed = config != env.config;
        Ok(ApplyOutcome {
            report,
            config,
            config_changed,
            tool_versions: outcome.tool_versions,
            instances: outcome.instances,
            sites: outcome.sites,
        })
    }
}

#[derive(Default)]
struct Counts {
    tool_versions: usize,
    instances: usize,
    sites: usize,
}

// --------------------------------------------------------------------- prefs

/// The TLD is reported, not changed: Forgebox's DNS, certificates and `SiteRules` are
/// built from it at boot, so switching it is the user's explicit `forge config set tld`.
fn prefs(plan: &ImportPlan, config: &ForgeConfig, report: &mut ImportReport) {
    let Some(tld) = &plan.prefs.tld else { return };
    let current = config.tld.trim_start_matches('.').to_ascii_lowercase();
    if *tld == current {
        report.push(kind::PREF, "tld", true, Some(msg::pref_tld_same(tld).1));
    } else {
        report.push(
            kind::PREF,
            "tld",
            false,
            Some(msg::pref_tld_differs(tld, &current).1),
        );
    }
}

// --------------------------------------------------------------------- tools

/// Copies `src` to `dst` through `dst.tmp` (`*.tmp` folders are never scanned as tools).
/// Links inside are skipped, not followed.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    let tmp = dst.with_file_name(format!(
        "{}.tmp",
        dst.file_name().unwrap_or_default().to_string_lossy()
    ));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    copy_dir(src, &tmp)?;
    std::fs::rename(&tmp, dst)
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let meta = std::fs::symlink_metadata(entry.path())?;
        if is_reparse_point(&meta) {
            continue;
        }
        let target = dst.join(entry.file_name());
        if meta.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if meta.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

async fn tools(
    plan: &ImportPlan,
    options: &ImportOptions,
    env: &ApplyEnv,
    report: &mut ImportReport,
    counts: &mut Counts,
) -> Result<Registered> {
    let existing: Vec<ToolVersionRow> = env
        .reader
        .query(|conn| ToolVersionRepo::new(conn).list(None))
        .await?;
    let mut registered = Registered::new();
    for tool in &plan.tools {
        let name = label(tool);
        let (target_name, target_path) = if options.copy_tools {
            let folder = tool
                .canonical_name
                .clone()
                .unwrap_or_else(|| tool.folder.clone());
            let dest = env.root.bin().join(tool.tool_id.as_str()).join(&folder);
            (folder, dest)
        } else {
            (tool.folder.clone(), PathBuf::from(&tool.path))
        };
        let target_text = posix_string(&target_path);
        let key = (tool.tool_id.clone(), tool.folder.clone());

        // Already in the store: by Forgebox folder name, by the final path or (copy mode)
        // by the Laragon path it was referenced from.
        let found = existing.iter().find(|v| {
            v.tool_id == tool.tool_id
                && (v.canonical_name == target_name
                    || same_path(&v.path, &target_text)
                    || same_path(&v.path, &tool.path))
        });
        if let Some(found) = found {
            registered.insert(key, Some(found.id));
            report.push(
                kind::TOOL_VERSION,
                name,
                false,
                Some(msg::already_registered().1),
            );
            continue;
        }

        let note = if options.copy_tools {
            if tokio::fs::try_exists(&target_path).await.unwrap_or(false) {
                report.push(
                    kind::TOOL_VERSION,
                    name,
                    false,
                    Some(msg::copy_dest_exists(&target_text).1),
                );
                continue;
            }
            if !options.dry_run {
                let (src, dst) = (PathBuf::from(&tool.path), target_path.clone());
                let copied = tokio::task::spawn_blocking(move || copy_tree(&src, &dst))
                    .await
                    .map_err(|e| ForgeError::Internal(format!("copying a tool: {e}")))?;
                if let Err(e) = copied {
                    report.push(
                        kind::TOOL_VERSION,
                        name,
                        false,
                        Some(msg::copy_failed(&e.to_string()).1),
                    );
                    continue;
                }
            }
            msg::copied_to(&target_text).1
        } else {
            msg::referenced().1
        };

        let id = if options.dry_run {
            None
        } else {
            let new = NewToolVersion {
                tool_id: tool.tool_id.clone(),
                tool_name: tool.tool_name.clone(),
                version: tool.version.clone(),
                canonical_name: target_name.clone(),
                variant: tool.variants.clone(),
                path: target_text,
                state: VersionState::Installed,
                source: VersionSource::Import,
            };
            let id = env
                .writer
                .execute(move |conn| ToolVersionRepo::new(conn).register_if_absent(&new))
                .await?;
            if id.is_none() {
                report.push(
                    kind::TOOL_VERSION,
                    name,
                    false,
                    Some(msg::already_registered().1),
                );
                continue;
            }
            id
        };
        counts.tool_versions += 1;
        registered.insert(key, id);
        report.push(kind::TOOL_VERSION, name, true, Some(note));
    }
    Ok(registered)
}

// ----------------------------------------------------------------- databases

async fn databases(
    plan: &ImportPlan,
    options: &ImportOptions,
    env: &ApplyEnv,
    registered: &Registered,
    report: &mut ImportReport,
    counts: &mut Counts,
) -> Result<()> {
    let existing: Vec<InstanceRow> = env
        .reader
        .query(|conn| InstanceRepo::new(conn).list())
        .await?;
    for db in &plan.databases {
        let name = format!("{} ({})", db.name, db.tool_id);
        if existing
            .iter()
            .any(|i| i.tool_id == db.tool_id && i.name == db.name)
        {
            report.push(
                kind::DATABASE_DIR,
                name,
                false,
                Some(msg::instance_exists().1),
            );
            continue;
        }
        let Some(version_id) = registered.get(&(db.tool_id.clone(), db.version_folder.clone()))
        else {
            report.push(
                kind::DATABASE_DIR,
                name,
                false,
                Some(msg::instance_tool_missing(&db.version_folder).1),
            );
            continue;
        };

        // Where the data ends up: referenced, or moved to `data/<tool>/<name>`.
        let source_dir = PathBuf::from(&db.data_dir);
        let mut data_dir = db.data_dir.clone();
        let mut external = true;
        let mut moved_from: Option<PathBuf> = None;
        let mut note = msg::data_referenced(&db.version).1;
        if options.move_data {
            let dest = env.root.data().join(db.tool_id.as_str()).join(&db.name);
            let dest_text = posix_string(&dest);
            if tokio::fs::try_exists(&dest).await.unwrap_or(false) {
                note = msg::data_move_dest_exists(&dest_text).1;
            } else if options.dry_run {
                (data_dir, external, note) =
                    (dest_text.clone(), false, msg::data_moved(&dest_text).1);
            } else {
                let (from, to) = (source_dir.clone(), dest.clone());
                let moved = tokio::task::spawn_blocking(move || {
                    if let Some(parent) = to.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    // Same volume: one atomic rename. Another volume (or a locked file)
                    // fails and the folder stays where it is.
                    std::fs::rename(&from, &to)
                })
                .await
                .map_err(|e| ForgeError::Internal(format!("moving a data folder: {e}")))?;
                match moved {
                    Ok(()) => {
                        (data_dir, external, note) =
                            (dest_text.clone(), false, msg::data_moved(&dest_text).1);
                        moved_from = Some(source_dir.clone());
                    }
                    Err(e) => note = msg::data_move_failed(&e.to_string()).1,
                }
            }
        }

        if options.dry_run {
            report.push(kind::DATABASE_DIR, name, true, Some(note));
            counts.instances += 1;
            continue;
        }
        let Some(version_id) = *version_id else {
            report.push(
                kind::DATABASE_DIR,
                name,
                false,
                Some(msg::instance_tool_missing(&db.version_folder).1),
            );
            continue;
        };
        let mut row = InstanceRow::new(db.tool_id.clone(), version_id, db.name.clone());
        row.data_dir = Some(data_dir.clone());
        if external {
            row.settings
                .insert(EXTERNAL_DATA_DIR_KEY.to_owned(), "true".to_owned());
        }
        let inserted = env
            .writer
            .execute(move |conn| InstanceRepo::new(conn).insert(&row))
            .await;
        match inserted {
            Ok(()) => {
                counts.instances += 1;
                report.push(kind::DATABASE_DIR, name, true, Some(note));
            }
            Err(err) => {
                // Put a moved folder back: no row, no move.
                if let Some(original) = moved_from {
                    let current = PathBuf::from(&data_dir);
                    let _ = tokio::task::spawn_blocking(move || std::fs::rename(current, original))
                        .await;
                }
                match err {
                    ForgeError::Conflict(_) | ForgeError::InvalidInput(_) => {
                        report.push(kind::DATABASE_DIR, name, false, Some(err.to_string()))
                    }
                    other => return Err(other),
                }
            }
        }
    }
    Ok(())
}

// -------------------------------------------------------------------- parked

async fn parked(
    plan: &ImportPlan,
    options: &ImportOptions,
    env: &ApplyEnv,
    config: &mut ForgeConfig,
    report: &mut ImportReport,
) {
    for dir in &plan.parked {
        if dir.offer_case_conversion {
            match (&env.convert_case, options.convert_case_sensitive) {
                (Some(convert), true) if !options.dry_run => {
                    match convert(PathBuf::from(&dir.dir)).await {
                        Ok(()) => report.push(
                            kind::CASE_CONVERSION,
                            dir.dir.clone(),
                            true,
                            Some(msg::case_converted().1),
                        ),
                        Err(e) => report.push(
                            kind::CASE_CONVERSION,
                            dir.dir.clone(),
                            false,
                            Some(msg::case_failed(&e.to_string()).1),
                        ),
                    }
                }
                (_, true) if options.dry_run => report.push(
                    kind::CASE_CONVERSION,
                    dir.dir.clone(),
                    true,
                    Some(msg::case_converted().1),
                ),
                _ => {}
            }
        }
        let present = config.parked_dirs.iter().any(|d| same_path(d, &dir.dir));
        if present {
            report.push(
                kind::PARKED_DIR,
                dir.dir.clone(),
                false,
                Some(msg::parked_present().1),
            );
        } else {
            if !options.dry_run {
                config.parked_dirs.push(dir.dir.clone());
            }
            let mut note = msg::parked_added().1;
            if dir.offer_case_conversion && !options.convert_case_sensitive {
                note = format!("{note}; {}", msg::parked_offer_conversion(&dir.dir).1);
            }
            report.push(kind::PARKED_DIR, dir.dir.clone(), true, Some(note));
        }
    }
}

// --------------------------------------------------------------------- sites

async fn sites(
    plan: &ImportPlan,
    options: &ImportOptions,
    env: &ApplyEnv,
    report: &mut ImportReport,
    counts: &mut Counts,
) -> Result<()> {
    let rules = env.sites.rules().clone();
    // Names of explicit sites; parked ones yield to an explicit site, so they do not
    // block it (`SiteService::create` replaces them).
    let mut taken: BTreeSet<String> = env
        .sites
        .list()
        .await?
        .iter()
        .filter(|s| !s.parked)
        .flat_map(|s| s.names().map(str::to_owned).collect::<Vec<_>>())
        .collect();
    for site in &plan.sites {
        let Some(primary) = site
            .hosts
            .iter()
            .find(|h| rules.tlds.iter().any(|t| t == tld_of(h)))
            .cloned()
        else {
            report.push(
                kind::SITE,
                site.hosts[0].clone(),
                false,
                Some(msg::site_tld(&site.hosts, &rules.tlds).1),
            );
            continue;
        };
        if normalize_host(&primary, &rules).is_err() || taken.contains(&primary) {
            report.push(kind::SITE, primary, false, Some(msg::site_exists().1));
            continue;
        }
        let mut notes = site.notes.clone();
        let mut aliases = Vec::new();
        for alias in site.hosts.iter().filter(|h| **h != primary) {
            if normalize_alias(alias).is_err() {
                notes.push(msg::alias_invalid(alias).1);
            } else if taken.contains(alias) {
                notes.push(msg::alias_taken(alias).1);
            } else {
                aliases.push(alias.clone());
            }
        }
        let driver = drivers::by_id(&site.driver)
            .or_else(|| drivers::by_id("generic-php"))
            .ok_or_else(|| ForgeError::Internal("generic-php driver missing".into()))?;
        let mut draft = SiteDraft::from_detected(&primary, driver, PathBuf::from(&site.root));
        draft.aliases = aliases.clone();
        draft.secure = site.secure;
        draft.http_redirect = site.secure;
        if let Some(series) = &site.php_series {
            draft.php = SitePhp {
                series: Some(series.clone()),
                ..SitePhp::default()
            };
        }

        let summary = |notes: &[String]| {
            let mut parts = vec![format!("driver {}", site.driver)];
            if let Some(series) = &site.php_series {
                parts.push(format!("PHP {series}"));
            }
            if !aliases.is_empty() {
                parts.push(format!("aliases: {}", aliases.join(", ")));
            }
            parts.extend(notes.iter().cloned());
            parts.join("; ")
        };
        if options.dry_run {
            report.push(kind::SITE, primary.clone(), true, Some(summary(&notes)));
        } else {
            match env.sites.create(draft).await {
                Ok(created) => {
                    notes.extend(created.warnings.iter().map(|w| w.message.clone()));
                    report.push(kind::SITE, primary.clone(), true, Some(summary(&notes)));
                }
                Err(err @ (ForgeError::Conflict(_) | ForgeError::InvalidInput(_))) => {
                    report.push(
                        kind::SITE,
                        primary,
                        false,
                        Some(msg::site_failed(&err.to_string()).1),
                    );
                    continue;
                }
                Err(other) => return Err(other),
            }
        }
        counts.sites += 1;
        taken.insert(primary);
        taken.extend(aliases);
    }
    Ok(())
}
