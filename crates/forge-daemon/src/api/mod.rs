//! The API of the daemon: one handler per method of `forge_ipc::api::METHODS`,
//! delegating to the domain components (docs/09, ADR-0015). Handlers stay thin: they
//! resolve references, call the component and convert domain types into DTOs
//! ([`convert`]). Long operations answer with a `JobDto` at once ([`Daemon::job`]).
//!
//! Methods whose backend belongs to a later roadmap task answer `not_implemented` with
//! the task in the message ([`not_implemented`]); none of them panics.

pub mod convert;
mod daemon_api;
mod fs_parity;
mod logs;
mod net;
pub mod not_implemented;
mod php;
mod registry;
mod services;
mod shell;
mod sites;
mod system;
mod tls;
mod tools;

use forge_ipc::server::Router;

use crate::daemon::Daemon;

/// Registers `$method` with the async fn `$handler(Daemon, Params, Ctx) -> Result<R>`.
macro_rules! route {
    ($router:expr, $daemon:expr, $method:ty, $handler:path) => {{
        let daemon = $daemon.clone();
        $router.handle::<$method, _, _>(move |params, ctx| {
            let daemon = daemon.clone();
            async move { $handler(daemon, params, ctx).await }
        });
    }};
}
pub(crate) use route;

/// Router with every handler of the daemon (the server adds its built-ins: `system.ping`,
/// `events.*`, `jobs.*`, `fs.*`, `*.unsubscribe`).
pub fn router(daemon: &Daemon) -> Router {
    let mut router = Router::new();
    daemon_api::register(&mut router, daemon);
    tools::register(&mut router, daemon);
    services::register(&mut router, daemon);
    sites::register(&mut router, daemon);
    php::register(&mut router, daemon);
    net::register(&mut router, daemon);
    tls::register(&mut router, daemon);
    registry::register(&mut router, daemon);
    system::register(&mut router, daemon);
    logs::register(&mut router, daemon);
    shell::register(&mut router, daemon);
    fs_parity::register(&mut router, daemon);
    not_implemented::register(&mut router);
    router
}

/// Built-in methods of `forge_ipc::Server` (registered by the server itself).
pub const SERVER_BUILTINS: &[&str] = &[
    "system.hello",
    "system.ping",
    "daemon.status",
    "events.subscribe",
    "events.unsubscribe",
    "events.list",
    "jobs.list",
    "jobs.get",
    "jobs.cancel",
    "fs.read",
    "fs.write",
    "fs.tail",
    "fs.list",
    "logs.unsubscribe",
    "metrics.unsubscribe",
];
