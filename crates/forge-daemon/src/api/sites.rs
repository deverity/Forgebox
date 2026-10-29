//! `sites.*`: sites, proxies, parked folders, projects (`forgebox.yaml`) and processes.
//! Changes go through `forge_sites::SiteService` (events `site.*`); the scheduler's
//! reaction re-renders the web servers and starts the pools they need. Jobs that change
//! a site also reconcile before answering, so the result reflects the running server.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use forge_core::error::{ForgeError, Result};
use forge_core::ids::SiteId;
use forge_core::store::{Site, SiteKind, SitePhp};
use forge_ipc::api::methods::{
    sites_add, sites_alias, sites_down, sites_driver, sites_info, sites_list, sites_open,
    sites_park, sites_parked, sites_php, sites_proxy_add, sites_ps, sites_remove, sites_run,
    sites_secure, sites_server, sites_unpark, sites_up,
};
use forge_ipc::api::{
    Ack, CheckDto, CheckStatus, JobDto, ListAction, NoParams, ParkParams, ParkedDirDto,
    ParkedListDto, ProcessAction, ProjectParams, ProjectUpDto, ProxyAddParams, SiteAddParams,
    SiteAliasParams, SiteDriverParams, SiteDto, SiteInfoDto, SiteKey, SiteListDto, SiteOpenParams,
    SiteOpenTarget, SiteParams, SitePhpParams, SiteProcessDto, SiteProcessListDto, SitePsParams,
    SiteRef, SiteRemoveParams, SiteRunParams, SiteSecureParams, SiteServerParams,
};
use forge_ipc::server::{Ctx, Router};
use forge_sites::apps::AppSite;
use forge_sites::{ProjectPlan, ProxyTarget, SiteDraft, SiteRules};

use super::convert;
use super::route;
use crate::daemon::Daemon;

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, sites_list, list);
    route!(router, daemon, sites_add, add);
    route!(router, daemon, sites_remove, remove);
    route!(router, daemon, sites_php, php);
    route!(router, daemon, sites_driver, driver);
    route!(router, daemon, sites_secure, secure);
    route!(router, daemon, sites_server, server);
    route!(router, daemon, sites_alias, alias);
    route!(router, daemon, sites_info, info);
    route!(router, daemon, sites_park, park);
    route!(router, daemon, sites_unpark, unpark);
    route!(router, daemon, sites_parked, parked);
    route!(router, daemon, sites_proxy_add, proxy_add);
    route!(router, daemon, sites_up, up);
    route!(router, daemon, sites_down, down);
    route!(router, daemon, sites_ps, ps);
    route!(router, daemon, sites_run, run);
    route!(router, daemon, sites_open, open);
    // The shell may load the browser before it answers; more than the 1 s default.
    router.timeout::<sites_open>(OPEN_BUDGET);
    // `sites.info` runs the DNS, certificate, pool and folder checks side by side.
    router.timeout::<sites_info>(INFO_BUDGET);
}

/// Budget of `sites.info`: each check has its own shorter one, they run in parallel.
const INFO_BUDGET: std::time::Duration = std::time::Duration::from_secs(8);
/// Budget of one check of `sites.info`.
const CHECK_BUDGET: std::time::Duration = std::time::Duration::from_secs(4);
/// `site.certificate` warns below this many days left.
const CERT_WARN_DAYS: i64 = 30;

/// Budget of the shell call of `sites.open`.
const OPEN_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// `sites.open` (local only): the site in the default browser, or its folder in the file
/// manager, of the daemon's machine, through the platform's [`ShellOpener`]
/// (`ShellExecuteExW`, in `spawn_blocking`). The editor and the terminal belong to
/// M2-E3-T9 and answer `not_implemented` naming it.
///
/// [`ShellOpener`]: forge_core::platform::ShellOpener
async fn open(daemon: Daemon, params: SiteOpenParams, _ctx: Ctx) -> Result<Ack> {
    let site = find(&daemon, &params.site).await?;
    let opener = std::sync::Arc::clone(daemon.platform.shell_opener());
    match params.with {
        SiteOpenTarget::Browser => {
            let url = convert::site_url(&site);
            forge_core::runtime::spawn_blocking_timeout(
                "opening the browser",
                OPEN_BUDGET,
                move || opener.open_url(&url),
            )
            .await?;
        }
        SiteOpenTarget::Explorer => {
            let root = site
                .root
                .as_deref()
                .filter(|r| !r.is_empty())
                .ok_or_else(|| {
                    ForgeError::InvalidInput(format!("{} has no folder (it is a proxy)", site.host))
                })?;
            let root = PathBuf::from(root);
            forge_core::runtime::spawn_blocking_timeout(
                "opening the file manager",
                OPEN_BUDGET,
                move || opener.open_path(&root),
            )
            .await?;
        }
        SiteOpenTarget::Editor | SiteOpenTarget::Terminal => {
            let what = if params.with == SiteOpenTarget::Editor {
                "opening a site in the editor"
            } else {
                "opening a site in the terminal"
            };
            return Err(ForgeError::NotImplementedYet {
                method: "sites.open".into(),
                task: super::not_implemented::SITES_OPEN_EDITOR_TERMINAL_TASK.into(),
                what: what.into(),
            });
        }
    }
    Ok(Ack::ok())
}

/// The site a reference designates.
pub async fn find(daemon: &Daemon, site: &SiteRef) -> Result<Site> {
    let found = match site.key() {
        SiteKey::Id(id) => daemon.sites.get(id).await?,
        SiteKey::Host(host) => daemon.sites.get_by_host(&host).await?,
    };
    found.ok_or_else(|| ForgeError::NotFound(format!("site {site}")))
}

pub(super) async fn dto(daemon: &Daemon, site: &Site) -> SiteDto {
    let plan = daemon
        .web
        .php_plan(std::slice::from_ref(site))
        .await
        .unwrap_or_default();
    let config = daemon.config();
    let mut out = convert::site(site, plan.get(&site.id), config.default_server.as_str());
    if site.kind == SiteKind::App {
        if let Ok(status) = daemon.apps.status(&site.host).await {
            out.processes = processes(site.id, &status);
        }
    }
    out
}

fn processes(site_id: SiteId, status: &forge_sites::apps::SiteAppsStatus) -> Vec<SiteProcessDto> {
    status
        .processes
        .iter()
        .map(|p| SiteProcessDto {
            site_id,
            host: status.host.clone(),
            name: p.name.clone(),
            command: p.command.clone(),
            state: convert::service_state(&p.instance.state),
            pid: p.instance.pid,
            port: p.port,
            autostart: p.autostart == "true",
            // The id the instance's `service.*` events carry (with `site_id`).
            instance_id: Some(p.instance.instance_id),
        })
        .collect()
}

async fn list(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<SiteListDto> {
    let sites = daemon.sites.list().await?;
    let plan = daemon.web.php_plan(&sites).await.unwrap_or_default();
    let config = daemon.config();
    Ok(SiteListDto {
        sites: sites
            .iter()
            .map(|s| convert::site(s, plan.get(&s.id), config.default_server.as_str()))
            .collect(),
    })
}

/// `8.3` → series; `8.3.36` → exact (and its series).
pub fn php_selection(value: &str) -> Result<SitePhp> {
    let value = value.trim();
    let parts: Vec<&str> = value.split('.').collect();
    if parts.is_empty()
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(ForgeError::InvalidInput(format!(
            "PHP must be a series (8.3) or an exact version (8.3.36), got {value:?}"
        )));
    }
    Ok(match parts.len() {
        1 | 2 => SitePhp {
            series: Some(value.to_owned()),
            ..SitePhp::default()
        },
        _ => SitePhp {
            series: Some(format!("{}.{}", parts[0], parts[1])),
            exact: Some(value.to_owned()),
            pool_overrides: None,
        },
    })
}

pub(super) async fn reconcile_quietly(daemon: &Daemon, reason: &str) {
    if let Err(err) = daemon.web.reconcile(reason).await {
        tracing::warn!(%err, reason, "reconcile failed");
    }
}

fn default_host(dir: &Path, tld: &str) -> Result<String> {
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| ForgeError::InvalidInput(format!("{} has no folder name", dir.display())))?;
    let label = forge_sites::site::host_label_for_folder(name).ok_or_else(|| {
        ForgeError::InvalidInput(format!("{name:?} cannot become a host name; pass --host"))
    })?;
    Ok(format!("{label}.{}", tld.trim_start_matches('.')))
}

async fn add(daemon: Daemon, params: SiteAddParams, ctx: Ctx) -> Result<JobDto> {
    let dir = PathBuf::from(&params.dir);
    if !dir.is_absolute() {
        return Err(ForgeError::InvalidInput(format!(
            "{} must be an absolute folder on the daemon's machine",
            params.dir
        )));
    }
    let config = daemon.config();
    let host = match &params.host {
        Some(host) => host.trim().to_ascii_lowercase(),
        None => default_host(&dir, &config.tld)?,
    };
    let php = params.php.as_deref().map(php_selection).transpose()?;
    let d = daemon.clone();
    daemon.job(&ctx, format!("sites.add {host}"), move |_job| async move {
        let driver = params.driver.clone();
        let dir2 = dir.clone();
        let host2 = host.clone();
        let mut draft = tokio::task::spawn_blocking(move || {
            if !dir2.is_dir() {
                return Err(ForgeError::NotFound(format!("folder {}", dir2.display())));
            }
            // Stored with long names: they end up in server configs (docs/03 §3.1).
            let dir2 = forge_core::platform::long_path(&dir2);
            SiteDraft::from_dir(&dir2, &host2, driver.as_deref())
        })
        .await
        .map_err(|e| ForgeError::Internal(format!("driver detection: {e}")))??;
        if let Some(php) = php {
            draft.php = php;
        }
        if let Some(server) = &params.server {
            draft.server = Some(server.to_string());
        }
        if let Some(secure) = params.secure {
            draft.secure = secure;
            draft.http_redirect = secure;
        }
        let created = d.sites.create(draft).await?;
        reconcile_quietly(&d, "site added").await;
        Ok(dto(&d, &created.site).await)
    })
}

async fn remove(daemon: Daemon, params: SiteRemoveParams, ctx: Ctx) -> Result<JobDto> {
    if !params.confirm {
        return Err(ForgeError::InvalidInput(
            "removing a site needs confirm: true (project files are never deleted)".into(),
        ));
    }
    let site = find(&daemon, &params.site).await?;
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("sites.remove {}", site.host),
        move |_job| async move {
            if site.kind == SiteKind::App {
                let _ = d.apps.forget(site.id, &site.host).await;
            }
            d.sites.remove(site.id).await?;
            reconcile_quietly(&d, "site removed").await;
            Ok(Ack::ok())
        },
    )
}

async fn update_site<F>(
    daemon: Daemon,
    ctx: Ctx,
    site: &SiteRef,
    name: &str,
    change: F,
) -> Result<JobDto>
where
    F: FnOnce(&mut Site) -> Result<()> + Send + 'static,
{
    let mut current = find(&daemon, site).await?;
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("{name} {}", current.host),
        move |_job| async move {
            change(&mut current)?;
            let updated = d.sites.update(current).await?;
            reconcile_quietly(&d, "site updated").await;
            Ok(dto(&d, &updated).await)
        },
    )
}

/// `sites.php`: switches the PHP of a site, blue/green (docs/06 §2.2). The version or
/// series must be installed (else `not_found` with `version_not_installed` details and
/// the site untouched). In the job: the target pool is started first (failure: the job
/// fails and the row is never written), then the row is updated and the web servers
/// re-rendered and reloaded; a render/reload failure restores the row. `site.php_changed {from, to}` goes out after the swap, before
/// `job.done`.
async fn php(daemon: Daemon, params: SitePhpParams, ctx: Ctx) -> Result<JobDto> {
    let selection = php_selection(&params.php)?;
    let previous = find(&daemon, &params.site).await?;
    if previous.kind != SiteKind::Php {
        return Err(ForgeError::InvalidInput(format!(
            "{} is not a PHP site",
            previous.host
        )));
    }
    let requested = selection
        .exact
        .clone()
        .or_else(|| selection.series.clone())
        .unwrap_or_else(|| params.php.trim().to_owned());
    daemon.web.require_php(&requested).await?;
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("sites.php {}", previous.host),
        move |_job| async move {
            let from = d.web.resolved_php(&previous).await;
            let mut next = previous.clone();
            next.php.series = selection.series;
            next.php.exact = selection.exact;
            // Blue/green step 1 (docs/06 §2.2): the target pool must be up before the
            // row is written. On failure nothing was stored, so no `site.updated`, no
            // concurrent reconcile and the old pool is never drained.
            d.web.ensure_pool_for(&next).await?;
            let updated = d.sites.update(next).await?;
            apply_or_restore(&d, &previous, &updated, "site php changed", true).await?;
            let to = d.web.resolved_php(&updated).await.unwrap_or(requested);
            if from.as_deref() != Some(to.as_str()) {
                d.sites.publish_php_changed(&updated, from.as_deref(), &to);
            }
            Ok(dto(&d, &updated).await)
        },
    )
}

/// Re-renders for `updated` and answers for that site only
/// ([`crate::web::WebManager::reconcile_for`]); on failure the stored row goes back to
/// `previous` (and the servers are rendered again from it) and the error is returned.
async fn apply_or_restore(
    daemon: &Daemon,
    previous: &Site,
    updated: &Site,
    reason: &str,
    check_pool: bool,
) -> Result<()> {
    let Err(err) = daemon.web.reconcile_for(reason, updated, check_pool).await else {
        return Ok(());
    };
    tracing::warn!(site = %updated.host, %err, "change failed; restoring the site");
    if let Err(restore) = daemon.sites.update(previous.clone()).await {
        tracing::error!(site = %updated.host, %restore, "could not restore the site");
    }
    reconcile_quietly(daemon, "site restored").await;
    Err(err)
}

/// `sites.driver`: overrides the framework driver of a site, or detects it again
/// (`driver: None`/`auto`); the document root follows the driver (`laravel` serves
/// `public/`). Re-renders the web server; a failure restores the row.
async fn driver(daemon: Daemon, params: SiteDriverParams, ctx: Ctx) -> Result<JobDto> {
    let previous = find(&daemon, &params.site).await?;
    if !matches!(previous.kind, SiteKind::Php | SiteKind::Static) {
        return Err(ForgeError::InvalidInput(format!(
            "{} is a proxy or app site: it has no framework driver",
            previous.host
        )));
    }
    // ParkedSync re-detects parked sites at every boot and folder change, so an
    // override would be silently reverted (ADR-0041 §9: no marker, no migration).
    if previous.parked {
        return Err(ForgeError::Conflict(format!(
            "{} is a parked site: its driver follows detection; pin it with `forge site add` (explicit site) to choose a driver",
            previous.host
        )));
    }
    let wanted = params
        .driver
        .as_deref()
        .map(|d| d.trim().to_ascii_lowercase())
        .filter(|d| !d.is_empty() && d != "auto");
    if let Some(id) = &wanted {
        if forge_sites::drivers::by_id(id).is_none() {
            let known: Vec<&str> = forge_sites::drivers::all().iter().map(|d| d.id()).collect();
            return Err(ForgeError::InvalidInput(format!(
                "unknown driver {id:?}; known: {}",
                known.join(", ")
            )));
        }
    }
    let root = previous
        .root
        .clone()
        .filter(|r| !r.is_empty())
        .ok_or_else(|| ForgeError::InvalidInput(format!("{} has no folder", previous.host)))?;
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("sites.driver {}", previous.host),
        move |_job| async move {
            let found = forge_core::runtime::spawn_blocking_timeout(
                "detecting the driver",
                std::time::Duration::from_secs(10),
                move || forge_sites::drivers::redetect(Path::new(&root), wanted.as_deref()),
            )
            .await?;
            let mut next = previous.clone();
            next.driver = Some(found.driver.id().to_owned());
            next.kind = found.driver.kind();
            next.root = Some(forge_core::paths::posix_string(&found.root));
            let updated = d.sites.update(next).await?;
            apply_or_restore(&d, &previous, &updated, "site driver changed", false).await?;
            Ok(dto(&d, &updated).await)
        },
    )
}

async fn secure(daemon: Daemon, params: SiteSecureParams, ctx: Ctx) -> Result<JobDto> {
    update_site(daemon, ctx, &params.site, "sites.secure", move |site| {
        site.secure = params.secure;
        site.http_redirect = params.secure;
        Ok(())
    })
    .await
}

async fn server(daemon: Daemon, params: SiteServerParams, ctx: Ctx) -> Result<JobDto> {
    if !crate::web::serves_sites(params.server.as_str()) {
        return Err(ForgeError::InvalidInput(format!(
            "{} is not a web server Forgebox can configure",
            params.server
        )));
    }
    update_site(daemon, ctx, &params.site, "sites.server", move |site| {
        site.server = Some(params.server.to_string());
        Ok(())
    })
    .await
}

async fn alias(daemon: Daemon, params: SiteAliasParams, ctx: Ctx) -> Result<JobDto> {
    let alias = params.alias.trim().to_ascii_lowercase();
    update_site(daemon, ctx, &params.site, "sites.alias", move |site| {
        match params.action {
            ListAction::Add => {
                if !site.aliases.contains(&alias) {
                    site.aliases.push(alias);
                }
            }
            ListAction::Remove => site.aliases.retain(|a| a != &alias),
        }
        Ok(())
    })
    .await
}

/// `sites.info`: the site and its diagnosis, from the checks `site.dns` (the
/// `net.dns_test` path), `site.root_access`, `site.php_pool` (php sites) and
/// `site.certificate`. They run side by side, each with its own budget.
async fn info(daemon: Daemon, params: SiteParams, _ctx: Ctx) -> Result<SiteInfoDto> {
    let site = find(&daemon, &params.site).await?;
    let dto = dto(&daemon, &site).await;
    let (dns, root, certificate) = tokio::join!(
        check_dns(&daemon, &site),
        check_root_access(&daemon, &site),
        check_certificate(&daemon, &site),
    );
    let mut checks = vec![dns, root];
    if let Some(php) = &dto.php {
        checks.push(check_php_pool(&daemon, php).await);
    }
    checks.push(certificate);
    Ok(SiteInfoDto { site: dto, checks })
}

fn check(id: &str, status: CheckStatus, message: impl Into<String>) -> CheckDto {
    CheckDto {
        id: id.into(),
        status,
        message: message.into(),
        fix: None,
        fix_method: None,
        fix_params: None,
    }
}

/// `site.dns`: does the host resolve to this machine, as the OS resolves it.
async fn check_dns(daemon: &Daemon, site: &Site) -> CheckDto {
    let probe = super::net::dns_test_for(daemon, &site.host);
    match tokio::time::timeout(CHECK_BUDGET, probe).await {
        Ok(Ok(test)) if test.ok => check(
            "site.dns",
            CheckStatus::Ok,
            format!(
                "{} resolves to {} (via {})",
                test.host,
                test.resolved.join(", "),
                test.via
            ),
        ),
        Ok(Ok(test)) => CheckDto {
            fix: Some("forge dns enable".into()),
            ..check(
                "site.dns",
                CheckStatus::Fail,
                format!(
                    "{} resolves to [{}], expected [{}] (via {})",
                    test.host,
                    test.resolved.join(", "),
                    test.expected.join(", "),
                    test.via
                ),
            )
        },
        Ok(Err(err)) => check(
            "site.dns",
            CheckStatus::Warn,
            format!("could not be checked: {err}"),
        ),
        Err(_) => check(
            "site.dns",
            CheckStatus::Warn,
            "could not be checked in time",
        ),
    }
}

/// `site.root_access`: the folder exists, the daemon can read it, and whether its NTFS
/// case-sensitivity flag is on (docs/15).
async fn check_root_access(daemon: &Daemon, site: &Site) -> CheckDto {
    let Some(root) = site.root.clone().filter(|r| !r.is_empty()) else {
        return check("site.root_access", CheckStatus::Ok, "no folder (proxy)");
    };
    let case_fs = daemon.platform.shared_case_sensitive_fs();
    let wanted = daemon.config().case_sensitive;
    let path = PathBuf::from(&root);
    let probed = forge_core::runtime::spawn_blocking_timeout(
        "checking the site folder",
        CHECK_BUDGET,
        move || {
            if !path.is_dir() {
                return Ok(Err("the folder does not exist".to_owned()));
            }
            if let Err(err) = std::fs::read_dir(&path) {
                return Ok(Err(format!("the folder cannot be read: {err}")));
            }
            Ok(Ok(forge_core::fs::case::status(&*case_fs, &path).ok()))
        },
    )
    .await;
    match probed {
        Ok(Err(problem)) => check(
            "site.root_access",
            CheckStatus::Fail,
            format!("{root}: {problem}"),
        ),
        Ok(Ok(case)) => {
            let enabled = case.as_ref().and_then(|c| c.enabled);
            let state = match enabled {
                Some(true) => "case-sensitive",
                Some(false) => "case-insensitive",
                None => "case sensitivity unavailable",
            };
            let message = format!("{root}: readable, {state}");
            if wanted && enabled == Some(false) {
                CheckDto {
                    fix: Some(format!("forge site case-sensitive {} on", site.host)),
                    ..check("site.root_access", CheckStatus::Warn, message)
                }
            } else {
                check("site.root_access", CheckStatus::Ok, message)
            }
        }
        Err(err) => check(
            "site.root_access",
            CheckStatus::Warn,
            format!("could not be checked: {err}"),
        ),
    }
}

/// `site.php_pool`: the PHP version the site resolves to and whether its pool runs.
async fn check_php_pool(daemon: &Daemon, php: &forge_ipc::api::SitePhpDto) -> CheckDto {
    let Some(resolved) = &php.resolved else {
        let wanted = php.exact.clone().or_else(|| php.series.clone());
        return CheckDto {
            fix: Some(format!(
                "forge tool install php{}",
                wanted.map(|w| format!("@{w}")).unwrap_or_default()
            )),
            ..check(
                "site.php_pool",
                CheckStatus::Fail,
                "no installed PHP serves this site",
            )
        };
    };
    let running = match &php.upstream {
        Some(up) => daemon
            .pools
            .statuses()
            .await
            .iter()
            .any(|s| &s.upstream.name == up || s.version == *resolved),
        None => false,
    };
    let message = format!(
        "PHP {resolved} ({}){}",
        php.upstream.clone().unwrap_or_default(),
        if running { "" } else { ": pool not running" }
    );
    if running {
        check("site.php_pool", CheckStatus::Ok, message)
    } else {
        CheckDto {
            fix: Some("forge up".into()),
            ..check("site.php_pool", CheckStatus::Warn, message)
        }
    }
}

/// `site.certificate`: expiry of the site's leaf certificate (warns below 30 days).
async fn check_certificate(daemon: &Daemon, site: &Site) -> CheckDto {
    if !site.secure {
        return check(
            "site.certificate",
            CheckStatus::Ok,
            "served over HTTP: no certificate needed",
        );
    }
    let status = match tokio::time::timeout(CHECK_BUDGET, daemon.tls.status()).await {
        Ok(Ok(status)) => status,
        Ok(Err(err)) => {
            return check(
                "site.certificate",
                CheckStatus::Warn,
                format!("could not be checked: {err}"),
            );
        }
        Err(_) => {
            return check(
                "site.certificate",
                CheckStatus::Warn,
                "could not be checked in time",
            );
        }
    };
    let host = &site.host;
    let Some(leaf) = status.leaves.iter().find(|l| &l.info.host == host) else {
        return CheckDto {
            fix: Some(format!("forge tls issue {host}")),
            fix_method: Some("tls.issue".into()),
            fix_params: Some(serde_json::json!({ "host": host })),
            ..check("site.certificate", CheckStatus::Warn, "no certificate yet")
        };
    };
    let days = days_left(leaf.info.not_after);
    let not_after = leaf.info.not_after;
    let renew = |status, message: String| CheckDto {
        fix: Some(format!("forge tls renew {host}")),
        fix_method: Some("tls.renew".into()),
        fix_params: Some(serde_json::json!({ "host": host })),
        ..check("site.certificate", status, message)
    };
    if days < 0 {
        renew(
            CheckStatus::Fail,
            format!("expired on {not_after} ({} days ago)", -days),
        )
    } else if !leaf.signed_by_ca {
        renew(
            CheckStatus::Warn,
            "signed by another CA than the current one".into(),
        )
    } else if days < CERT_WARN_DAYS {
        renew(
            CheckStatus::Warn,
            format!("expires on {not_after} ({days} days left)"),
        )
    } else {
        check(
            "site.certificate",
            CheckStatus::Ok,
            format!("expires on {not_after} ({days} days left)"),
        )
    }
}

/// Whole days from now to `not_after` (negative once expired, rounded down).
pub(super) fn days_left(not_after: jiff::Timestamp) -> i64 {
    not_after
        .duration_since(jiff::Timestamp::now())
        .as_secs()
        .div_euclid(86_400)
}

fn parked_path(daemon: &Daemon, dir: &str) -> PathBuf {
    let path = Path::new(dir);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        daemon.root.path().join(path)
    }
}

pub(super) async fn save_config(
    daemon: &Daemon,
    config: forge_core::config::ForgeConfig,
) -> Result<()> {
    let path = daemon.root.forgebox_toml();
    let saved = config.clone();
    forge_core::runtime::spawn_blocking_timeout(
        "saving forgebox.toml",
        std::time::Duration::from_secs(5),
        move || saved.save(&path),
    )
    .await?;
    daemon.set_config(config);
    Ok(())
}

pub(super) async fn reconcile_parked(daemon: &Daemon) {
    let config = daemon.config();
    let dirs: Vec<forge_sites::ParkedDir> = config
        .parked_dirs
        .iter()
        .map(|d| forge_sites::ParkedDir::new(parked_path(daemon, d)))
        .collect();
    let mut parked = forge_sites::ParkedConfig::new(dirs);
    parked.default_tld = config.tld.trim_start_matches('.').to_owned();
    let sync = forge_sites::ParkedSync::new(daemon.sites.clone(), parked);
    if let Err(err) = sync.reconcile().await {
        tracing::warn!(%err, "parked reconcile failed");
    }
}

async fn park(daemon: Daemon, params: ParkParams, _ctx: Ctx) -> Result<ParkedDirDto> {
    let dir = parked_path(&daemon, &params.dir);
    let exists = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || dir.is_dir())
            .await
            .unwrap_or(false)
    };
    if !exists {
        return Err(ForgeError::NotFound(format!("folder {}", dir.display())));
    }
    let mut config = (*daemon.config()).clone();
    let text = forge_core::paths::posix_string(&dir);
    if !config
        .parked_dirs
        .iter()
        .any(|d| forge_core::paths::posix_string(&parked_path(&daemon, d)) == text)
    {
        config.parked_dirs.push(text.clone());
        save_config(&daemon, config).await?;
    }
    reconcile_parked(&daemon).await;
    let tld = daemon.config().tld.clone();
    let count = daemon
        .sites
        .list()
        .await?
        .iter()
        .filter(|s| {
            s.parked
                && s.root
                    .as_deref()
                    .is_some_and(|r| r.replace('\\', "/").starts_with(&text))
        })
        .count();
    Ok(ParkedDirDto {
        dir: text,
        tld,
        sites: u32::try_from(count).unwrap_or(u32::MAX),
    })
}

async fn unpark(daemon: Daemon, params: ParkParams, _ctx: Ctx) -> Result<Ack> {
    let text = forge_core::paths::posix_string(&parked_path(&daemon, &params.dir));
    let mut config = (*daemon.config()).clone();
    let before = config.parked_dirs.len();
    config
        .parked_dirs
        .retain(|d| forge_core::paths::posix_string(&parked_path(&daemon, d)) != text);
    if config.parked_dirs.len() == before {
        return Err(ForgeError::NotFound(format!("{text} is not parked")));
    }
    save_config(&daemon, config).await?;
    reconcile_parked(&daemon).await;
    Ok(Ack::ok())
}

async fn parked(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<ParkedListDto> {
    let config = daemon.config();
    let sites = daemon.sites.list().await?;
    let dirs = config
        .parked_dirs
        .iter()
        .map(|d| {
            let text = forge_core::paths::posix_string(&parked_path(&daemon, d));
            let count = sites
                .iter()
                .filter(|s| {
                    s.parked
                        && s.root
                            .as_deref()
                            .is_some_and(|r| r.replace('\\', "/").starts_with(&text))
                })
                .count();
            ParkedDirDto {
                dir: text,
                tld: config.tld.clone(),
                sites: u32::try_from(count).unwrap_or(u32::MAX),
            }
        })
        .collect();
    Ok(ParkedListDto { dirs })
}

async fn proxy_add(daemon: Daemon, params: ProxyAddParams, ctx: Ctx) -> Result<JobDto> {
    let target = ProxyTarget::parse(&params.upstream)?;
    let host = params.host.trim().to_ascii_lowercase();
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("sites.proxy_add {host}"),
        move |_job| async move {
            let secure = params.secure.unwrap_or(true);
            let draft = SiteDraft {
                host: host.clone(),
                aliases: Vec::new(),
                kind: SiteKind::Proxy,
                root: None,
                driver: None,
                server: None,
                php: SitePhp::default(),
                upstream: Some(target.url()),
                secure,
                http_redirect: secure,
                env: BTreeMap::new(),
                xdebug: forge_core::store::SiteXdebug::Inherit,
                parked: false,
                processes: BTreeMap::new(),
            };
            let created = d.sites.create(draft).await?;
            reconcile_quietly(&d, "proxy added").await;
            Ok(dto(&d, &created.site).await)
        },
    )
}

async fn load_plan(daemon: &Daemon, dir: &str) -> Result<ProjectPlan> {
    let dir = PathBuf::from(dir);
    let rules = SiteRules::new(daemon.config().tlds());
    tokio::task::spawn_blocking(move || ProjectPlan::load(&dir, &rules))
        .await
        .map_err(|e| ForgeError::Internal(format!("reading forgebox.yaml: {e}")))?
}

async fn upsert(daemon: &Daemon, draft: SiteDraft) -> Result<Site> {
    match daemon
        .sites
        .get_by_host(&draft.host.to_ascii_lowercase())
        .await?
    {
        Some(existing) => {
            let (mut site, _) = draft.into_site(daemon.sites.rules())?;
            site.id = existing.id;
            site.slug = existing.slug;
            site.created_at = existing.created_at;
            daemon.sites.update(site).await
        }
        None => Ok(daemon.sites.create(draft).await?.site),
    }
}

async fn up(daemon: Daemon, params: ProjectParams, ctx: Ctx) -> Result<JobDto> {
    let plan = load_plan(&daemon, &params.dir).await?;
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("sites.up {}", plan.name),
        move |_job| async move {
            let mut site_draft = plan.site.clone();
            site_draft.processes = plan.processes.clone();
            let site = upsert(&d, site_draft).await?;
            let mut sites = vec![site.clone()];
            for proxy in plan.proxies.clone() {
                sites.push(upsert(&d, proxy).await?);
            }
            let mut process_list = Vec::new();
            if !plan.processes.is_empty() {
                let app = AppSite::from_site(&site, plan.dir.clone(), plan.runtimes.clone())?;
                let status = d.apps.up(&app).await?;
                process_list = processes(site.id, &status);
            }
            reconcile_quietly(&d, "project up").await;
            let mut dtos = Vec::new();
            for s in &sites {
                dtos.push(dto(&d, s).await);
            }
            Ok(ProjectUpDto {
                sites: dtos,
                processes: process_list,
            })
        },
    )
}

async fn down(daemon: Daemon, params: ProjectParams, ctx: Ctx) -> Result<JobDto> {
    let plan = load_plan(&daemon, &params.dir).await?;
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("sites.down {}", plan.name),
        move |_job| async move {
            let host = plan.site.host.to_ascii_lowercase();
            d.apps.down(&host).await?;
            Ok(Ack::with_message(format!("{host}: processes stopped")))
        },
    )
}

async fn ps(daemon: Daemon, params: SitePsParams, _ctx: Ctx) -> Result<SiteProcessListDto> {
    let hosts: Vec<(SiteId, String)> = match &params.site {
        Some(site) => {
            let site = find(&daemon, site).await?;
            vec![(site.id, site.host)]
        }
        None => {
            let sites = daemon.sites.list().await?;
            let known = daemon.apps.hosts().await.unwrap_or_default();
            sites
                .into_iter()
                .filter(|s| known.contains(&s.host))
                .map(|s| (s.id, s.host))
                .collect()
        }
    };
    let mut out = Vec::new();
    for (id, host) in hosts {
        if let Ok(status) = daemon.apps.status(&host).await {
            out.extend(processes(id, &status));
        }
    }
    Ok(SiteProcessListDto { processes: out })
}

async fn run(daemon: Daemon, params: SiteRunParams, ctx: Ctx) -> Result<JobDto> {
    let site = find(&daemon, &params.site).await?;
    let d = daemon.clone();
    daemon.job(
        &ctx,
        format!("sites.run {} {}", site.host, params.process),
        move |_job| async move {
            let process = Some(params.process.as_str());
            let status = match params.action {
                ProcessAction::Start => d.apps.start_process(&site.host, process).await?,
                ProcessAction::Stop => d.apps.stop_process(&site.host, process).await?,
                ProcessAction::Restart => d.apps.restart_process(&site.host, process).await?,
            };
            processes(site.id, &status)
                .into_iter()
                .find(|p| p.name == params.process)
                .ok_or_else(|| {
                    ForgeError::NotFound(format!("process {} of {}", params.process, site.host))
                })
        },
    )
}
