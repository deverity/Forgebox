//! One module per group of commands (docs/02). Every command is a thin call of the
//! daemon API plus a human rendering; nothing here knows a tool by id.

mod daemon;
mod db;
mod fs;
mod jobs;
mod logs;
mod net;
mod php;
mod remote;
mod service;
mod shell;
mod site;
mod system;
mod tool;

use crate::cli::Command;
use crate::context::Context;
use crate::error::CliResult;

/// Runs a parsed command.
pub async fn run(ctx: &Context, command: Command) -> CliResult<()> {
    match command {
        Command::Daemon(cmd) => daemon::run(ctx, cmd).await,
        Command::Doctor => daemon::doctor(ctx).await,
        Command::Search(args) => tool::search(ctx, args).await,
        Command::Tool(cmd) => tool::run(ctx, cmd).await,
        Command::Use(args) => tool::use_version(ctx, args).await,
        Command::Which(args) => tool::which(ctx, args).await,
        Command::Start(args) => service::control(ctx, service::Control::Start, args).await,
        Command::Stop(args) => service::control(ctx, service::Control::Stop, args).await,
        Command::Restart(args) => service::control(ctx, service::Control::Restart, args).await,
        Command::Reload(args) => service::reload(ctx, args).await,
        Command::Status(args) => service::status(ctx, args).await,
        Command::Service(cmd) => service::service(ctx, cmd).await,
        Command::Logs(args) => {
            logs::run(
                ctx,
                logs::LogsRequest {
                    target: args.target,
                    follow: args.follow,
                    since: args.since,
                    grep: args.grep,
                    lines: args.lines,
                },
            )
            .await
        }
        Command::Php(cmd) => php::run(ctx, cmd).await,
        Command::Site(cmd) => site::run(ctx, cmd).await,
        Command::Park(args) => site::park(ctx, args).await,
        Command::Unpark(args) => site::unpark(ctx, args).await,
        Command::Parked => site::parked(ctx).await,
        Command::Link(args) => site::link(ctx, args).await,
        Command::Unlink(args) => site::unlink(ctx, args).await,
        Command::Proxy(cmd) => site::proxy(ctx, cmd).await,
        Command::Up(args) => site::up(ctx, args).await,
        Command::Down(args) => site::down(ctx, args).await,
        Command::Ps(args) => site::ps(ctx, args).await,
        Command::Run(args) => site::run_process(ctx, args).await,
        Command::Share(args) => site::share(ctx, args).await,
        Command::New(args) => site::new_project(ctx, args).await,
        Command::Db(cmd) => db::run(ctx, cmd).await,
        Command::Dns(cmd) => net::dns(ctx, cmd).await,
        Command::Tls(cmd) => net::tls(ctx, cmd).await,
        Command::Lan(cmd) => net::lan(ctx, cmd).await,
        Command::Mail(cmd) => net::mail(ctx, cmd).await,
        Command::Shell(args) => shell::shell(ctx, args).await,
        Command::Env(args) => shell::env(ctx, args).await,
        Command::Exec(args) => shell::exec(ctx, args).await,
        Command::Path(cmd) => shell::path(ctx, cmd).await,
        Command::Fs(cmd) => fs::run(ctx, cmd).await,
        Command::Jobs(args) => jobs::jobs(ctx, args).await,
        Command::Stats(args) => jobs::stats(ctx, args).await,
        Command::Top => jobs::top(ctx).await,
        Command::Remote(cmd) => remote::run(ctx, cmd).await,
        Command::Registry(cmd) => system::registry(ctx, cmd).await,
        Command::Import(cmd) => system::import(ctx, cmd).await,
        Command::Bundle(cmd) => system::bundle(ctx, cmd).await,
        Command::Backup(cmd) => system::backup(ctx, cmd).await,
        Command::Config(cmd) => system::config(ctx, cmd).await,
        Command::SelfUpdate(args) => system::self_update(ctx, args).await,
        Command::Completions(args) => system::completions(args),
        Command::Mcp(cmd) => system::mcp(ctx, cmd).await,
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    use forge_ipc::api::{SiteDto, SiteKind, SiteXdebug};

    /// A site with the given host and defaults everywhere else.
    pub fn sample_site(host: &str) -> SiteDto {
        let now: forge_ipc::base::Timestamp = "2026-10-27T12:00:00Z".parse().unwrap();
        SiteDto {
            id: forge_ipc::base::ids::SiteId::new(),
            slug: host.split('.').next().unwrap_or(host).into(),
            host: host.into(),
            aliases: vec![],
            kind: SiteKind::Static,
            root: None,
            driver: None,
            server: None,
            php: None,
            upstream: None,
            websocket: false,
            secure: false,
            http_redirect: false,
            env: Default::default(),
            xdebug: SiteXdebug::Inherit,
            parked: false,
            enabled: true,
            case_sensitive: true,
            url: format!("http://{host}"),
            processes: vec![],
            created_at: now,
            updated_at: now,
        }
    }
}
