//! `forge php ...` (docs/09 §php).

use std::io::IsTerminal;

use forge_ipc::api::methods::{php_ext_disable, php_ext_enable, php_xdebug};
use forge_ipc::api::{
    NoParams, PhpExtListDto, PhpExtParams, PhpPoolStatusParams, PhpVersionParams, PhpXdebugParams,
    PoolDto, XdebugMode,
};

use crate::cli::{PhpCmd, PhpExtCmd, PhpPoolCmd, XdebugArg};
use crate::context::Context;
use crate::error::CliResult;
use crate::messages::Msg;
use crate::output::{GREEN, or_dash, paint};
use crate::render;

pub async fn run(ctx: &Context, command: PhpCmd) -> CliResult<()> {
    match command {
        PhpCmd::List => list(ctx).await,
        PhpCmd::Pool(PhpPoolCmd::Status { version }) => pool_status(ctx, version).await,
        PhpCmd::Ext(ext) => extension(ctx, ext).await,
        PhpCmd::Xdebug {
            version,
            mode,
            port,
        } => {
            let mode = match mode {
                XdebugArg::On => XdebugMode::On,
                XdebugArg::Off => XdebugMode::Off,
                XdebugArg::Trigger => XdebugMode::Trigger,
            };
            let pool: PoolDto = ctx
                .job::<php_xdebug, PoolDto>(PhpXdebugParams {
                    version,
                    mode,
                    port,
                })
                .await?;
            ctx.out.emit(&pool, |out| {
                out.success(out.tf(Msg::XdebugSet, &[&pool.series, &pool.xdebug]));
            });
            Ok(())
        }
        PhpCmd::Ini { version, open } => ini(ctx, version, open).await,
    }
}

async fn list(ctx: &Context) -> CliResult<()> {
    let list = ctx
        .call::<forge_ipc::api::methods::php_list>(NoParams {})
        .await?;
    ctx.out.emit(&list, |out| {
        let rows = list
            .versions
            .iter()
            .map(|v| {
                vec![
                    v.version.clone(),
                    v.series.clone(),
                    v.thread_safety.clone(),
                    or_dash(v.compiler.as_deref()),
                    v.arch.clone(),
                    if v.is_current_cli {
                        paint(GREEN, "CLI")
                    } else {
                        String::new()
                    },
                    v.pool
                        .as_ref()
                        .map_or_else(|| "-".into(), |p| render::state_cell(out, p.state)),
                    v.sites.to_string(),
                    format!("{:?}", v.xdebug).to_lowercase(),
                ]
            })
            .collect();
        out.table(
            &[
                out.t(Msg::HVersion),
                out.t(Msg::HSeries),
                "TS",
                out.t(Msg::HCompiler),
                "ARCH",
                "",
                "POOL",
                "SITES",
                "XDEBUG",
            ],
            rows,
            Msg::NoPhp,
        );
    });
    Ok(())
}

async fn pool_status(ctx: &Context, version: Option<String>) -> CliResult<()> {
    let pools = ctx
        .call::<forge_ipc::api::methods::php_pool_status>(PhpPoolStatusParams { version })
        .await?;
    ctx.out.emit(&pools, |out| {
        let rows = pools
            .pools
            .iter()
            .map(|p| {
                vec![
                    format!("php@{}", p.series),
                    p.version.clone(),
                    render::state_cell(out, p.state),
                    format!("{}/{}", p.busy_workers, p.workers),
                    format!("{}-{}", p.min_workers, p.max_workers),
                    p.sites.to_string(),
                    or_dash(p.requests_total),
                    or_dash(p.rss_bytes.map(crate::output::format_bytes)),
                ]
            })
            .collect();
        out.table(
            &[
                "POOL",
                out.t(Msg::HVersion),
                out.t(Msg::HState),
                out.t(Msg::HWorkers),
                "MIN-MAX",
                "SITES",
                out.t(Msg::HRequests),
                "RAM",
            ],
            rows,
            Msg::NoPools,
        );
    });
    Ok(())
}

fn extensions_table(out: &crate::output::Output, list: &PhpExtListDto) {
    let rows = list
        .extensions
        .iter()
        .map(|e| {
            vec![
                e.name.clone(),
                if e.enabled {
                    paint(GREEN, out.t(Msg::On))
                } else {
                    out.t(Msg::Off).to_owned()
                },
                if e.zend { "zend".into() } else { String::new() },
                if e.bundled {
                    out.t(Msg::Bundled).to_owned()
                } else {
                    String::new()
                },
            ]
        })
        .collect();
    out.table(
        &[out.t(Msg::HName), out.t(Msg::HState), "", ""],
        rows,
        Msg::NoExtensions,
    );
}

async fn extension(ctx: &Context, command: PhpExtCmd) -> CliResult<()> {
    match command {
        PhpExtCmd::List { version } => {
            let list = ctx
                .call::<forge_ipc::api::methods::php_ext_list>(PhpVersionParams { version })
                .await?;
            ctx.out.emit(&list, |out| extensions_table(out, &list));
        }
        PhpExtCmd::Enable { version, extension } => {
            let list: PhpExtListDto = ctx
                .job::<php_ext_enable, PhpExtListDto>(PhpExtParams { version, extension })
                .await?;
            ctx.out.emit(&list, |out| extensions_table(out, &list));
        }
        PhpExtCmd::Disable { version, extension } => {
            let list: PhpExtListDto = ctx
                .job::<php_ext_disable, PhpExtListDto>(PhpExtParams { version, extension })
                .await?;
            ctx.out.emit(&list, |out| extensions_table(out, &list));
        }
    }
    Ok(())
}

async fn ini(ctx: &Context, version: String, open: bool) -> CliResult<()> {
    let files = if open {
        ctx.call::<forge_ipc::api::methods::php_ini_open>(PhpVersionParams { version })
            .await?
    } else {
        ctx.call::<forge_ipc::api::methods::php_ini>(PhpVersionParams { version })
            .await?
    };
    ctx.out.emit(&files, |out| render::files(out, &files.files));
    if open && std::io::stdout().is_terminal() {
        let paths: Vec<std::path::PathBuf> = files
            .files
            .iter()
            .filter(|f| f.role == "php.ini" || f.role == "main")
            .map(|f| std::path::PathBuf::from(&f.path))
            .collect();
        let refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
        if !refs.is_empty() {
            crate::open::open_in_editor(&refs)?;
        }
    } else if open {
        // Not a terminal (scripts, tests, agents): the paths above are the answer.
        ctx.out.note(ctx.out.t(Msg::NotOpenedNoTerminal));
    }
    Ok(())
}
