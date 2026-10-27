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
    sites_add, sites_alias, sites_down, sites_info, sites_list, sites_park, sites_parked,
    sites_php, sites_proxy_add, sites_ps, sites_remove, sites_run, sites_secure, sites_server,
    sites_unpark, sites_up,
};
use forge_ipc::api::{
    Ack, CheckDto, CheckStatus, JobDto, ListAction, NoParams, ParkParams, ParkedDirDto,
    ParkedListDto, ProcessAction, ProjectParams, ProjectUpDto, ProxyAddParams, SiteAddParams,
    SiteAliasParams, SiteDto, SiteInfoDto, SiteKey, SiteListDto, SiteParams, SitePhpParams,
    SiteProcessDto, SiteProcessListDto, SitePsParams, SiteRef, SiteRemoveParams, SiteRunParams,
    SiteSecureParams, SiteServerParams,
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
}

/// The site a reference designates.
pub async fn find(daemon: &Daemon, site: &SiteRef) -> Result<Site> {
    let found = match site.key() {
        SiteKey::Id(id) => daemon.sites.get(id).await?,
        SiteKey::Host(host) => daemon.sites.get_by_host(&host).await?,
    };
    found.ok_or_else(|| ForgeError::NotFound(format!("site {site}")))
}

async fn dto(daemon: &Daemon, site: &Site) -> SiteDto {
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

async fn reconcile_quietly(daemon: &Daemon, reason: &str) {
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

async fn php(daemon: Daemon, params: SitePhpParams, ctx: Ctx) -> Result<JobDto> {
    let selection = php_selection(&params.php)?;
    update_site(daemon, ctx, &params.site, "sites.php", move |site| {
        if site.kind != SiteKind::Php {
            return Err(ForgeError::InvalidInput(format!(
                "{} is not a PHP site",
                site.host
            )));
        }
        site.php.series = selection.series;
        site.php.exact = selection.exact;
        Ok(())
    })
    .await
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

async fn info(daemon: Daemon, params: SiteParams, _ctx: Ctx) -> Result<SiteInfoDto> {
    let site = find(&daemon, &params.site).await?;
    let mut checks = Vec::new();
    let root_ok = match &site.root {
        Some(root) => {
            let root = PathBuf::from(root);
            tokio::task::spawn_blocking(move || root.is_dir())
                .await
                .unwrap_or(false)
        }
        None => true,
    };
    checks.push(CheckDto {
        id: "site.root".into(),
        status: if root_ok {
            CheckStatus::Ok
        } else {
            CheckStatus::Fail
        },
        message: site.root.clone().unwrap_or_else(|| "-".into()),
        fix: None,
    });
    let dto = dto(&daemon, &site).await;
    if let Some(php) = &dto.php {
        let running = match &php.upstream {
            Some(up) => daemon.pools.statuses().await.iter().any(|s| {
                &s.upstream.name == up || php.resolved.as_deref() == Some(s.version.as_str())
            }),
            None => false,
        };
        checks.push(CheckDto {
            id: "site.php_pool".into(),
            status: if running {
                CheckStatus::Ok
            } else {
                CheckStatus::Warn
            },
            message: format!(
                "PHP {} ({})",
                php.resolved
                    .clone()
                    .unwrap_or_else(|| "not installed".into()),
                php.upstream.clone().unwrap_or_default()
            ),
            fix: (!running).then(|| "forge tool install php".into()),
        });
    }
    if site.secure {
        let crt = daemon.root.certs_sites().join(format!("{}.crt", site.host));
        let exists = tokio::task::spawn_blocking(move || crt.is_file())
            .await
            .unwrap_or(false);
        checks.push(CheckDto {
            id: "site.certificate".into(),
            status: if exists {
                CheckStatus::Ok
            } else {
                CheckStatus::Warn
            },
            message: if exists {
                "certificate present"
            } else {
                "no certificate yet"
            }
            .into(),
            fix: (!exists).then(|| format!("forge tls issue {}", site.host)),
        });
    }
    Ok(SiteInfoDto { site: dto, checks })
}

fn parked_path(daemon: &Daemon, dir: &str) -> PathBuf {
    let path = Path::new(dir);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        daemon.root.path().join(path)
    }
}

async fn save_config(daemon: &Daemon, config: forge_core::config::ForgeConfig) -> Result<()> {
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

async fn reconcile_parked(daemon: &Daemon) {
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
