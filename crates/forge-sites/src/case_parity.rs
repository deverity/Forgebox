//! Case sensitivity per site (docs/15 §3.1, §3.2, §6): the `Aa`/`aA` badge state, the
//! boot re-verification, `forge site case-sensitive <site> on|off` and
//! `forge site lint-case <site> [--fix]` as jobs.
//!
//! The state of a site is the state of its **project folder** (the folder holding
//! `composer.json`/`package.json`/`.git`, found from the document root by
//! [`lint_case::project_dir_for_root`]): case-sensitive when that folder and every
//! subfolder checked (two levels, at most [`SITE_CHECK_MAX_DIRS`]) carry the flag.
//! It is stored in `sites.case_sensitive` and re-checked at boot by
//! [`CaseParity::reverify`], which publishes one `fs.case_sensitivity_applied` or
//! `fs.case_sensitivity_unavailable` (with `reason_code`) per site.
//!
//! Turning a site on or off converts its project folder
//! ([`forge_core::fs::case::CaseConversions`]) and then updates the column. The daemon
//! must stop the site's processes before, and restart the parked watcher when the
//! converted folder **is** a parked directory (its handle follows the renamed folder).

use std::path::PathBuf;
use std::sync::Arc;

use forge_core::error::{ForgeError, Result};
use forge_core::event::EventKind;
use forge_core::fs::case::{
    CaseConversions, CaseReasonCode, STATUS_TIMEOUT, case_event_payload, messages, tree_flags,
};
use forge_core::ids::{JobId, RequestId, SiteId};
use forge_core::jobs::{JobContext, JobProgress, JobSpec};
use forge_core::paths::posix_string;
use forge_core::platform::{CaseSensitiveFs, CaseSensitivitySupport};
use forge_core::runtime::spawn_blocking_timeout;
use serde::{Deserialize, Serialize};

use crate::lint_case::{self, FixOptions, LintOptions, LintReport};
use crate::messages as messages_sites;
use crate::service::SiteService;
use crate::site::Site;

/// Depth of the boot check below the project folder.
pub const SITE_CHECK_DEPTH: usize = 2;
/// Folders looked at by the boot check of one site.
pub const SITE_CHECK_MAX_DIRS: u64 = 2_000;

/// Case sensitivity of one site's project folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteCaseCheck {
    pub site_id: SiteId,
    pub host: String,
    /// Project folder (`/`); empty for sites without a folder (proxies).
    pub dir: String,
    /// Badge `Aa` (true) or `aA` (false).
    pub enabled: bool,
    pub reason_code: Option<CaseReasonCode>,
    /// pt-BR text of `reason_code`.
    pub reason: Option<String>,
    /// `sites.case_sensitive` was updated by this check.
    pub changed: bool,
}

/// Case sensitivity jobs and checks of sites. Cheap to clone.
#[derive(Clone, Debug)]
pub struct CaseParity {
    sites: SiteService,
    conversions: CaseConversions,
}

/// Outcome of the blocking part of a check.
struct Observed {
    dir: PathBuf,
    enabled: bool,
    reason: Option<(CaseReasonCode, String)>,
}

fn observe(cs: &dyn CaseSensitiveFs, root: &std::path::Path) -> Observed {
    let dir = lint_case::project_dir_for_root(root);
    if !dir.is_dir() {
        let (_, text) = messages::missing(&posix_string(&dir));
        return Observed {
            dir,
            enabled: false,
            reason: Some((CaseReasonCode::Missing, text)),
        };
    }
    if let CaseSensitivitySupport::Unsupported(detail) = cs.support(&dir) {
        let (_, text) = messages::unsupported(&detail);
        return Observed {
            dir,
            enabled: false,
            reason: Some((CaseReasonCode::Unsupported, text)),
        };
    }
    match tree_flags(cs, &dir, SITE_CHECK_DEPTH, SITE_CHECK_MAX_DIRS) {
        Ok(f) if f.sensitive == f.directories => Observed {
            dir,
            enabled: true,
            reason: None,
        },
        Ok(f) if f.sensitive == 0 => Observed {
            dir,
            enabled: false,
            reason: Some((
                CaseReasonCode::Insensitive,
                messages::insensitive().1.to_owned(),
            )),
        },
        Ok(f) => Observed {
            dir,
            enabled: false,
            reason: Some((
                CaseReasonCode::Partial,
                messages::partial(f.sensitive, f.directories).1,
            )),
        },
        Err(err) => Observed {
            dir,
            enabled: false,
            reason: Some((CaseReasonCode::Error, err.to_string())),
        },
    }
}

impl CaseParity {
    pub fn new(sites: SiteService, conversions: CaseConversions) -> Self {
        Self { sites, conversions }
    }

    fn cs(&self) -> Arc<dyn CaseSensitiveFs> {
        Arc::clone(self.conversions.case_sensitive_fs())
    }

    /// Checks one site without touching the store or publishing anything.
    pub async fn check_site(&self, site: &Site) -> SiteCaseCheck {
        let mut check = SiteCaseCheck {
            site_id: site.id,
            host: site.host.clone(),
            dir: String::new(),
            enabled: site.case_sensitive,
            reason_code: None,
            reason: None,
            changed: false,
        };
        let Some(root) = site.root.clone() else {
            return check;
        };
        let cs = self.cs();
        let observed = spawn_blocking_timeout("site case sensitivity check", STATUS_TIMEOUT, {
            let root = PathBuf::from(root);
            move || Ok(observe(cs.as_ref(), &root))
        })
        .await;
        match observed {
            Ok(o) => {
                check.dir = posix_string(&o.dir);
                check.enabled = o.enabled;
                if let Some((code, text)) = o.reason {
                    check.reason_code = Some(code);
                    check.reason = Some(text);
                }
            }
            // Slow or stuck volume: keep the stored state, say why.
            Err(err) => {
                check.reason_code = Some(CaseReasonCode::Error);
                check.reason = Some(err.to_string());
            }
        }
        check
    }

    /// Boot re-verification (docs/15 §3.1): checks every site with a folder, updates
    /// `sites.case_sensitive` where it changed (emitting `site.updated`) and publishes
    /// `fs.case_sensitivity_applied|unavailable` per site.
    pub async fn reverify(&self) -> Result<Vec<SiteCaseCheck>> {
        let mut out = Vec::new();
        for site in self.sites.list().await? {
            if site.root.is_none() {
                continue;
            }
            let mut check = self.check_site(&site).await;
            let readable = check.reason_code != Some(CaseReasonCode::Error);
            if readable && check.enabled != site.case_sensitive {
                match self.sites.set_case_sensitive(site.id, check.enabled).await {
                    Ok(_) => check.changed = true,
                    Err(err) => {
                        tracing::warn!(host = %site.host, %err, "could not store the case sensitivity of a site");
                    }
                }
            }
            self.publish(&check);
            out.push(check);
        }
        Ok(out)
    }

    fn publish(&self, check: &SiteCaseCheck) {
        let (kind, reason) = if check.enabled {
            (EventKind::FsCaseSensitivityApplied, None)
        } else {
            (
                EventKind::FsCaseSensitivityUnavailable,
                check
                    .reason_code
                    .map(|c| (c, check.reason.as_deref().unwrap_or(""))),
            )
        };
        self.conversions.jobs().events().publish(
            kind,
            case_event_payload(&check.dir, Some(check.site_id), reason),
        );
    }

    async fn site(&self, id: SiteId) -> Result<Site> {
        self.sites
            .get(id)
            .await?
            .ok_or_else(|| ForgeError::NotFound(format!("site {id}")))
    }

    async fn project_dir(site: &Site) -> Result<PathBuf> {
        let root = site
            .root
            .clone()
            .ok_or_else(|| ForgeError::InvalidInput(format!("site {} has no folder", site.host)))?;
        spawn_blocking_timeout("finding the project folder", STATUS_TIMEOUT, move || {
            Ok(lint_case::project_dir_for_root(std::path::Path::new(&root)))
        })
        .await
    }

    /// `forge site case-sensitive <site> on|off`: converts the project folder of the site
    /// and stores the new state. Returns the job id at once; the job's result is the
    /// updated [`Site`].
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn set_site_case(
        &self,
        site_id: SiteId,
        enabled: bool,
        request_id: Option<RequestId>,
    ) -> JobId {
        let mut spec = JobSpec::new(format!(
            "fs_parity.site_case:{site_id} {}",
            if enabled { "on" } else { "off" }
        ));
        if let Some(r) = request_id {
            spec = spec.request_id(r);
        }
        let this = self.clone();
        self.conversions
            .jobs()
            .spawn(spec, move |ctx| async move {
                let site = this.site(site_id).await?;
                let dir = Self::project_dir(&site).await?;
                this.conversions
                    .run(&ctx, dir, enabled, Some(site.id))
                    .await?;
                this.sites.set_case_sensitive(site.id, enabled).await
            })
            .id()
    }

    /// `forge site lint-case <site> [--fix]`: scans the site's project (and fixes with
    /// `fix`) in a job whose result is the [`LintReport`].
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn lint_site(
        &self,
        site_id: SiteId,
        fix: Option<FixOptions>,
        request_id: Option<RequestId>,
    ) -> JobId {
        let mut spec = JobSpec::new(format!("fs_parity.lint_case:{site_id}"));
        if let Some(r) = request_id {
            spec = spec.request_id(r);
        }
        let this = self.clone();
        self.conversions
            .jobs()
            .spawn(spec, move |ctx| async move {
                let site = this.site(site_id).await?;
                let dir = Self::project_dir(&site).await?;
                let mut opts = LintOptions::new(dir);
                if let Some(root) = &site.root {
                    opts.doc_roots.push(PathBuf::from(root));
                }
                run_lint(&ctx, opts, fix).await
            })
            .id()
    }
}

/// Runs a scan (or a fix) of `opts` in `spawn_blocking`, with progress and cancellation
/// through `ctx`. No timeout: the duration grows with the project; it is cancellable.
pub async fn run_lint(
    ctx: &JobContext,
    opts: LintOptions,
    fix: Option<FixOptions>,
) -> Result<LintReport> {
    let progress_ctx = ctx.clone();
    let token = ctx.cancellation_token();
    tokio::task::spawn_blocking(move || {
        let mut progress = |files: u32| {
            progress_ctx
                .progress(JobProgress::new().message(messages_sites::lint_case_progress(files).1));
        };
        let cancel = || token.is_cancelled();
        match fix {
            Some(options) => lint_case::fix(&opts, &options, &mut progress, &cancel),
            None => lint_case::scan(&opts, &mut progress, &cancel),
        }
    })
    .await
    .unwrap_or_else(|join| {
        Err(ForgeError::Internal(format!(
            "case mismatch scan task failed: {join}"
        )))
    })
}
