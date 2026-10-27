//! `forgebox.yaml` (`kind: Project`, docs/04 §4): the loader behind `forge up`.
//!
//! [`ProjectPlan::load`] parses and validates the file with `forge-manifest` and resolves
//! it against the project folder into a plan the daemon can execute: the site to
//! create/update, the services and runtimes to ensure, the processes to supervise and the
//! extra proxy sites. Nothing is executed here.
//!
//! Resolution rules:
//! * `site.host` defaults to the folder name (sanitized) plus the first configured TLD.
//! * `site.root` is relative to the project and must stay inside it; when omitted the
//!   driver's root is used.
//! * `site.driver` overrides detection; without it the folder is detected.
//! * `site.php` is a series (`8.3`) or an exact version (`8.3.36`).
//! * Hosts are validated like any other site ([`crate::site::normalize_host`]).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use forge_core::error::{ForgeError, Result};
use forge_manifest::Manifest;
use forge_manifest::schema::project::{
    ProcessSpec, ProjectRuntime, ProjectService, ProjectSite, Share, XdebugMode,
};

use crate::site::{
    SiteDraft, SiteKind, SitePhp, SiteRules, SiteXdebug, Warning, host_label_for_folder,
    normalize_host,
};

/// File name looked up in the project folder.
pub const PROJECT_FILE: &str = "forgebox.yaml";

/// Everything `forge up` has to do for a project.
#[derive(Debug, Clone)]
pub struct ProjectPlan {
    /// `metadata.name`.
    pub name: String,
    /// Project folder.
    pub dir: PathBuf,
    /// The main site.
    pub site: SiteDraft,
    /// `php.ini` overrides of the site.
    pub php_ini: BTreeMap<String, serde_json::Value>,
    /// Services to have installed and running (databases, cache, mail).
    pub services: Vec<ProjectService>,
    /// Runtimes for `forge shell` and the processes.
    pub runtimes: Vec<ProjectRuntime>,
    /// Processes supervised while the site is up.
    pub processes: BTreeMap<String, ProcessSpec>,
    /// Extra reverse-proxy sites (kind `proxy`).
    pub proxies: Vec<SiteDraft>,
    /// Tunnel provider.
    pub share: Option<Share>,
    /// Normalization remarks (uppercase hosts are rejected by validation, so usually empty).
    pub warnings: Vec<Warning>,
}

fn config_error(path: &Path, msg: impl std::fmt::Display) -> ForgeError {
    ForgeError::ConfigInvalid(format!("{}: {msg}", path.display()))
}

/// `8.3` is a series, `8.3.36` an exact version.
fn php_selection(value: &str) -> Result<SitePhp> {
    let parts: Vec<&str> = value.split('.').collect();
    let numeric = parts
        .iter()
        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    match (numeric, parts.len()) {
        (true, 2) => Ok(SitePhp {
            series: Some(value.to_owned()),
            ..SitePhp::default()
        }),
        (true, 3) => Ok(SitePhp {
            series: Some(format!("{}.{}", parts[0], parts[1])),
            exact: Some(value.to_owned()),
            pool_overrides: None,
        }),
        _ => Err(ForgeError::ConfigInvalid(format!(
            "spec.site.php must be a series (\"8.3\") or an exact version (\"8.3.36\"), got {value:?}"
        ))),
    }
}

fn xdebug(mode: XdebugMode) -> SiteXdebug {
    match mode {
        XdebugMode::Inherit => SiteXdebug::Inherit,
        XdebugMode::Off => SiteXdebug::Off,
        XdebugMode::Trigger => SiteXdebug::Trigger,
        XdebugMode::Always => SiteXdebug::Always,
    }
}

/// `root` relative to `dir`, rejecting absolute paths and `..`.
fn project_relative(dir: &Path, root: &str) -> Result<PathBuf> {
    let rel = Path::new(root);
    let escapes = rel.is_absolute()
        || rel.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        });
    if escapes {
        return Err(ForgeError::ConfigInvalid(format!(
            "spec.site.root must be inside the project: {root:?}"
        )));
    }
    Ok(dir.join(rel))
}

impl ProjectPlan {
    /// Loads `<dir>/forgebox.yaml`. `NotFound` when the file does not exist.
    /// Blocking (reads files).
    pub fn load(dir: &Path, rules: &SiteRules) -> Result<Self> {
        let path = dir.join(PROJECT_FILE);
        if !path.is_file() {
            return Err(ForgeError::NotFound(format!("{}", path.display())));
        }
        let manifest = forge_manifest::load_file(&path).map_err(|e| config_error(&path, e))?;
        let issues = forge_manifest::validate(&manifest);
        if !issues.is_empty() {
            let list: Vec<String> = issues.iter().map(ToString::to_string).collect();
            return Err(config_error(&path, list.join("; ")));
        }
        let Manifest::Project(project) = manifest else {
            return Err(config_error(&path, "expected `kind: Project`"));
        };
        Self::resolve(
            dir,
            rules,
            project.metadata.name,
            project.spec.site.unwrap_or_default(),
            project.spec.services,
            project.spec.runtimes,
            project.spec.processes,
            project.spec.proxies,
            project.spec.share,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve(
        dir: &Path,
        rules: &SiteRules,
        name: String,
        site: ProjectSite,
        services: Vec<ProjectService>,
        runtimes: Vec<ProjectRuntime>,
        processes: BTreeMap<String, ProcessSpec>,
        proxies: Vec<forge_manifest::schema::project::ProjectProxy>,
        share: Option<Share>,
    ) -> Result<Self> {
        let folder = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let host = match site.host {
            Some(host) => host,
            None => {
                let label = host_label_for_folder(&folder).ok_or_else(|| {
                    ForgeError::ConfigInvalid(format!(
                        "cannot derive a host from the folder name {folder:?}; set spec.site.host"
                    ))
                })?;
                let tld = rules.tlds.first().map_or("test", String::as_str);
                format!("{label}.{tld}")
            }
        };

        let mut draft = SiteDraft::from_dir(dir, &host, site.driver.as_deref())?;
        if let Some(root) = &site.root {
            draft.root = Some(project_relative(dir, root)?);
        }
        draft.aliases = site.aliases;
        draft.server = site.server.map(|t| t.as_str().to_owned());
        if let Some(php) = &site.php {
            draft.php = php_selection(php)?;
        }
        if let Some(mode) = site.xdebug {
            draft.xdebug = xdebug(mode);
        }
        if let Some(secure) = site.secure {
            draft.secure = secure;
        }
        // Validate (and normalize) the main site once, so errors surface at load time.
        let (_, mut warnings) = draft.clone().into_site(rules)?;

        let mut proxy_drafts = Vec::new();
        for proxy in proxies {
            let host = normalize_host(&proxy.host, rules)?;
            warnings.extend(host.warnings);
            let draft = SiteDraft {
                host: host.value,
                aliases: Vec::new(),
                kind: SiteKind::Proxy,
                root: None,
                driver: None,
                server: draft.server.clone(),
                php: SitePhp::default(),
                upstream: Some(proxy.upstream),
                secure: proxy.secure.unwrap_or(true),
                http_redirect: true,
                env: BTreeMap::new(),
                xdebug: SiteXdebug::Inherit,
                parked: false,
            };
            draft.clone().into_site(rules)?;
            proxy_drafts.push(draft);
        }
        Ok(Self {
            name,
            dir: dir.to_path_buf(),
            site: draft,
            php_ini: site.php_ini,
            services,
            runtimes,
            processes,
            proxies: proxy_drafts,
            share,
            warnings,
        })
    }
}
