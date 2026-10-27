//! `forge site ...`, `park`, `link`, `proxy`, `up`, `down`, `ps`, `run`, `share`, `new`
//! (docs/09 §site, docs/06) and the case-sensitivity commands of a site (docs/15 §8).

use std::path::Path;

use forge_ipc::api::methods::{
    fs_parity_lint_case, fs_parity_site_case, sites_add, sites_alias, sites_down, sites_new,
    sites_php, sites_proxy_add, sites_remove, sites_run, sites_secure, sites_server, sites_share,
    sites_up,
};
use forge_ipc::api::{
    Ack, CaseFixChoice, CaseLintReportDto, LintCaseParams, ListAction, NoParams, ParkParams,
    ParkedDirDto, ProcessAction, ProjectParams, ProjectUpDto, ProxyAddParams, ShareDto,
    SiteAddParams, SiteAliasParams, SiteCaseParams, SiteDto, SiteNewParams, SiteOpenParams,
    SiteOpenTarget, SiteParams, SitePhpParams, SiteProcessDto, SitePsParams, SiteRef,
    SiteRemoveParams, SiteRunParams, SiteSecureParams, SiteServerParams, SiteShareParams,
};

use crate::cli::{
    AliasAction, DirArg, LinkArgs, NewArgs, OnOff, OptDirArg, ParkArgs, PsArgs, RunAction, RunArgs,
    ShareArgs, SiteCmd, StrategyArg,
};
use crate::context::Context;
use crate::error::{CliError, CliResult};
use crate::messages::Msg;
use crate::output::{DIM, RED, YELLOW, or_dash, paint};
use crate::render;

pub async fn run(ctx: &Context, command: SiteCmd) -> CliResult<()> {
    match command {
        SiteCmd::List => {
            let list = ctx
                .call::<forge_ipc::api::methods::sites_list>(NoParams {})
                .await?;
            ctx.out
                .emit(&list, |out| render::site_table(out, &list.sites));
            Ok(())
        }
        SiteCmd::Add {
            dir,
            host,
            php,
            server,
            secure,
            driver,
        } => {
            let site = add(
                ctx,
                SiteAddParams {
                    dir: crate::parse::absolute(&dir)?,
                    host,
                    php,
                    server,
                    secure: secure.then_some(true),
                    driver,
                },
            )
            .await?;
            created(ctx, &site);
            Ok(())
        }
        SiteCmd::Remove { site } => remove(ctx, SiteRef::from(site)).await,
        SiteCmd::Php { site, version } => {
            let site: SiteDto = ctx
                .job::<sites_php, SiteDto>(SitePhpParams {
                    site: SiteRef::from(site),
                    php: version,
                })
                .await?;
            ctx.out.emit(&site, |out| {
                out.success(out.tf(Msg::SiteUpdated, &[&site.host]))
            });
            Ok(())
        }
        SiteCmd::Secure { site } => secure(ctx, site, true).await,
        SiteCmd::Unsecure { site } => secure(ctx, site, false).await,
        SiteCmd::Server { site, server } => {
            let site: SiteDto = ctx
                .job::<sites_server, SiteDto>(SiteServerParams {
                    site: SiteRef::from(site),
                    server,
                })
                .await?;
            ctx.out.emit(&site, |out| {
                out.success(out.tf(Msg::SiteUpdated, &[&site.host]))
            });
            Ok(())
        }
        SiteCmd::Alias {
            site,
            action,
            alias,
        } => {
            let site: SiteDto = ctx
                .job::<sites_alias, SiteDto>(SiteAliasParams {
                    site: SiteRef::from(site),
                    action: match action {
                        AliasAction::Add => ListAction::Add,
                        AliasAction::Remove => ListAction::Remove,
                    },
                    alias,
                })
                .await?;
            ctx.out.emit(&site, |out| {
                out.success(out.tf(Msg::SiteUpdated, &[&site.host]))
            });
            Ok(())
        }
        SiteCmd::Open {
            site,
            editor,
            terminal,
            explorer,
        } => {
            let with = if editor {
                SiteOpenTarget::Editor
            } else if terminal {
                SiteOpenTarget::Terminal
            } else if explorer {
                SiteOpenTarget::Explorer
            } else {
                SiteOpenTarget::Browser
            };
            let ack = ctx
                .call::<forge_ipc::api::methods::sites_open>(SiteOpenParams {
                    site: SiteRef::from(site),
                    with,
                })
                .await?;
            ctx.out.emit(&ack, |_| {});
            Ok(())
        }
        SiteCmd::Info { site } => {
            let info = ctx
                .call::<forge_ipc::api::methods::sites_info>(SiteParams {
                    site: SiteRef::from(site),
                })
                .await?;
            ctx.out.emit(&info, |out| {
                render::site_detail(out, &info.site);
                if !info.checks.is_empty() {
                    out.line("");
                    render::checks(out, &info.checks);
                }
            });
            Ok(())
        }
        SiteCmd::CaseSensitive { site, state } => {
            let site: SiteDto = ctx
                .job::<fs_parity_site_case, SiteDto>(SiteCaseParams {
                    site: SiteRef::from(site),
                    enabled: matches!(state, OnOff::On),
                })
                .await?;
            ctx.out.emit(&site, |out| {
                out.success(out.tf(
                    Msg::SiteCaseSet,
                    &[&site.host, &render::yes_no(out, site.case_sensitive)],
                ));
            });
            Ok(())
        }
        SiteCmd::LintCase {
            site,
            fix,
            dry_run,
            strategy,
        } => lint_case(ctx, site, fix, dry_run, strategy).await,
    }
}

async fn add(ctx: &Context, params: SiteAddParams) -> CliResult<SiteDto> {
    ctx.job::<sites_add, SiteDto>(params).await
}

fn created(ctx: &Context, site: &SiteDto) {
    ctx.out.emit(site, |out| {
        if out.quiet {
            // Scripts get the one thing they need: the host.
            out.line(&site.host);
            return;
        }
        out.success(out.tf(Msg::SiteCreated, &[&site.host]));
        render::site_detail(out, site);
    });
}

async fn remove(ctx: &Context, site: SiteRef) -> CliResult<()> {
    ctx.confirm(&ctx.out.tf(Msg::ConfirmRemoveSite, &[&site]))?;
    let ack: Ack = ctx
        .job::<sites_remove, Ack>(SiteRemoveParams {
            site: site.clone(),
            confirm: true,
        })
        .await?;
    ctx.out
        .emit(&ack, |out| out.success(out.tf(Msg::SiteRemoved, &[&site])));
    Ok(())
}

async fn secure(ctx: &Context, site: String, secure: bool) -> CliResult<()> {
    let site: SiteDto = ctx
        .job::<sites_secure, SiteDto>(SiteSecureParams {
            site: SiteRef::from(site),
            secure,
        })
        .await?;
    ctx.out.emit(&site, |out| {
        out.success(out.tf(
            if secure {
                Msg::SiteSecured
            } else {
                Msg::SiteUnsecured
            },
            &[&site.host],
        ));
    });
    Ok(())
}

// --- parked folders -----------------------------------------------------------------

fn parked_table(out: &crate::output::Output, dirs: &[ParkedDirDto]) {
    let rows = dirs
        .iter()
        .map(|d| vec![d.dir.clone(), d.tld.clone(), d.sites.to_string()])
        .collect();
    out.table(&[out.t(Msg::HPath), "TLD", "SITES"], rows, Msg::NoParked);
}

pub async fn park(ctx: &Context, args: ParkArgs) -> CliResult<()> {
    let dir = ctx
        .call::<forge_ipc::api::methods::sites_park>(ParkParams {
            dir: crate::parse::absolute(&args.dir)?,
            tld: args.tld,
        })
        .await?;
    ctx.out.emit(&dir, |out| {
        out.success(out.tf(Msg::Parked, &[&dir.dir, &dir.sites]));
    });
    Ok(())
}

pub async fn unpark(ctx: &Context, args: DirArg) -> CliResult<()> {
    let ack = ctx
        .call::<forge_ipc::api::methods::sites_unpark>(ParkParams {
            dir: crate::parse::absolute(&args.dir)?,
            tld: None,
        })
        .await?;
    ctx.out.emit(&ack, |out| out.success(out.t(Msg::Unparked)));
    Ok(())
}

pub async fn parked(ctx: &Context) -> CliResult<()> {
    let list = ctx
        .call::<forge_ipc::api::methods::sites_parked>(NoParams {})
        .await?;
    ctx.out.emit(&list, |out| parked_table(out, &list.dirs));
    Ok(())
}

// --- link / proxy / projects --------------------------------------------------------

pub async fn link(ctx: &Context, args: LinkArgs) -> CliResult<()> {
    let dir = ctx.dir_or_cwd(args.dir.as_deref())?;
    let site = add(
        ctx,
        SiteAddParams {
            dir,
            host: args.host,
            php: None,
            server: None,
            secure: None,
            driver: None,
        },
    )
    .await?;
    created(ctx, &site);
    Ok(())
}

/// Normal form for comparing folder paths: `/`, lowercase, no trailing slash.
fn path_key(path: &str) -> String {
    path.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

/// The site whose root is `dir`.
fn site_of_dir<'a>(sites: &'a [SiteDto], dir: &str) -> Option<&'a SiteDto> {
    let wanted = path_key(dir);
    let long = path_key(&forge_ipc::base::platform::long_path(Path::new(dir)).to_string_lossy());
    sites.iter().find(|s| {
        s.root
            .as_deref()
            .map(path_key)
            .is_some_and(|r| r == wanted || r == long)
    })
}

pub async fn unlink(ctx: &Context, args: OptDirArg) -> CliResult<()> {
    let dir = ctx.dir_or_cwd(args.dir.as_deref())?;
    let list = ctx
        .call::<forge_ipc::api::methods::sites_list>(NoParams {})
        .await?;
    let site = site_of_dir(&list.sites, &dir)
        .ok_or_else(|| CliError::new("not_found", ctx.out.tf(Msg::NoSiteForDir, &[&dir])))?;
    remove(ctx, SiteRef::from(site.id.to_string())).await
}

pub async fn proxy(ctx: &Context, command: crate::cli::ProxyCmd) -> CliResult<()> {
    match command {
        crate::cli::ProxyCmd::Add {
            host,
            upstream,
            ws,
            secure,
        } => {
            let site: SiteDto = ctx
                .job::<sites_proxy_add, SiteDto>(ProxyAddParams {
                    host,
                    upstream,
                    websocket: ws,
                    secure: secure.then_some(true),
                })
                .await?;
            created(ctx, &site);
            Ok(())
        }
        crate::cli::ProxyCmd::Remove { host } => remove(ctx, SiteRef::from(host)).await,
    }
}

pub async fn up(ctx: &Context, args: OptDirArg) -> CliResult<()> {
    let dir = ctx.dir_or_cwd(args.dir.as_deref())?;
    let up: ProjectUpDto = ctx
        .job::<sites_up, ProjectUpDto>(ProjectParams { dir })
        .await?;
    ctx.out.emit(&up, |out| {
        render::site_table(out, &up.sites);
        if !up.processes.is_empty() {
            out.line("");
            process_table(out, &up.processes);
        }
    });
    Ok(())
}

pub async fn down(ctx: &Context, args: OptDirArg) -> CliResult<()> {
    let dir = ctx.dir_or_cwd(args.dir.as_deref())?;
    let ack: Ack = ctx.job::<sites_down, Ack>(ProjectParams { dir }).await?;
    ctx.out
        .emit(&ack, |out| out.success(out.t(Msg::ProjectDown)));
    Ok(())
}

fn process_table(out: &crate::output::Output, processes: &[SiteProcessDto]) {
    let rows = processes
        .iter()
        .map(|p| {
            vec![
                p.host.clone(),
                p.name.clone(),
                render::state_cell(out, p.state),
                or_dash(p.pid),
                or_dash(p.port),
                paint(DIM, &p.command),
            ]
        })
        .collect();
    out.table(
        &[
            "SITE",
            out.t(Msg::HProcess),
            out.t(Msg::HState),
            "PID",
            out.t(Msg::HPort),
            out.t(Msg::HCommand),
        ],
        rows,
        Msg::NoProcesses,
    );
}

pub async fn ps(ctx: &Context, args: PsArgs) -> CliResult<()> {
    let list = ctx
        .call::<forge_ipc::api::methods::sites_ps>(SitePsParams {
            site: args.site.map(SiteRef::from),
        })
        .await?;
    ctx.out
        .emit(&list, |out| process_table(out, &list.processes));
    Ok(())
}

pub async fn run_process(ctx: &Context, args: RunArgs) -> CliResult<()> {
    let action = match args.action {
        RunAction::Logs => {
            return super::logs::run(
                ctx,
                super::logs::LogsRequest {
                    target: format!("app:{}/{}", args.site, args.process),
                    follow: args.follow,
                    since: None,
                    grep: None,
                    lines: None,
                },
            )
            .await;
        }
        RunAction::Start => ProcessAction::Start,
        RunAction::Stop => ProcessAction::Stop,
        RunAction::Restart => ProcessAction::Restart,
    };
    let process: SiteProcessDto = ctx
        .job::<sites_run, SiteProcessDto>(SiteRunParams {
            site: SiteRef::from(args.site),
            process: args.process,
            action,
        })
        .await?;
    ctx.out.emit(&process, |out| {
        process_table(out, std::slice::from_ref(&process))
    });
    Ok(())
}

pub async fn share(ctx: &Context, args: ShareArgs) -> CliResult<()> {
    let stopped = args.stop;
    let share: ShareDto = ctx
        .job::<sites_share, ShareDto>(SiteShareParams {
            site: SiteRef::from(args.site),
            provider: args.provider,
            stop: args.stop,
        })
        .await?;
    ctx.out.emit(&share, |out| match (&share.url, stopped) {
        (_, true) => out.success(out.t(Msg::ShareStopped)),
        (Some(url), false) => out.success(out.tf(Msg::SharedAt, &[&url])),
        (None, false) => out.success(out.t(Msg::ShareStarting)),
    });
    Ok(())
}

pub async fn new_project(ctx: &Context, args: NewArgs) -> CliResult<()> {
    let dir = match &args.dir {
        Some(dir) => Some(crate::parse::absolute(dir)?),
        None => None,
    };
    let site: SiteDto = ctx
        .job::<sites_new, SiteDto>(SiteNewParams {
            stack: args.stack,
            name: args.name,
            php: args.php,
            db: args.db,
            dir,
        })
        .await?;
    created(ctx, &site);
    Ok(())
}

// --- Case Mismatch Finder -----------------------------------------------------------

async fn lint_case(
    ctx: &Context,
    site: String,
    fix: bool,
    dry_run: bool,
    strategy: StrategyArg,
) -> CliResult<()> {
    let report: CaseLintReportDto = ctx
        .job::<fs_parity_lint_case, CaseLintReportDto>(LintCaseParams {
            site: SiteRef::from(site),
            fix,
            dry_run,
            strategy: match strategy {
                StrategyArg::Auto => CaseFixChoice::Auto,
                StrategyArg::Rename => CaseFixChoice::Rename,
                StrategyArg::Edit => CaseFixChoice::Edit,
            },
            overrides: Default::default(),
            only: None,
        })
        .await?;
    ctx.out.emit(&report, |out| {
        for issue in &report.issues {
            let target = issue
                .expected
                .clone()
                .or_else(|| issue.target.clone())
                .unwrap_or_default();
            let mut line = format!(
                "{}:{}:{}  [{}] {} -> {}",
                issue.file,
                issue.line,
                issue.column,
                paint(YELLOW, format!("{:?}", issue.category).to_lowercase()),
                issue.reference,
                target
            );
            if issue.fixed {
                line.push_str(&format!(
                    "  {}",
                    paint(crate::output::GREEN, out.t(Msg::LintFixed))
                ));
            } else if let Some(error) = issue.fix.as_ref().and_then(|f| f.error.as_ref()) {
                line.push_str(&format!("  {}", paint(RED, error)));
            }
            out.line(line);
        }
        out.line(out.tf(
            Msg::LintSummary,
            &[
                &report.issues.len(),
                &report.files_scanned,
                &report.references_checked,
            ],
        ));
    });
    let pending = report.issues.iter().filter(|i| !i.fixed).count();
    if pending > 0 {
        return Err(CliError::exit_silently(1));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites_are_found_by_folder_in_any_spelling() {
        let mut site = crate::commands::tests_support::sample_site("loja.test");
        site.root = Some("C:/Forgebox/www/Loja".into());
        let sites = [site];
        assert!(site_of_dir(&sites, r"C:\Forgebox\www\loja\").is_some());
        assert!(site_of_dir(&sites, r"C:\Forgebox\www\outra").is_none());
    }
}
