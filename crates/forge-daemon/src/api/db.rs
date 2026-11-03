//! `db.*`: databases of the database instances (docs/09 §db). Thin handlers over
//! [`crate::databases`]; `db.export|import` belong to M2-E3-T6 and `db.open` to M3-E1
//! (`not_implemented`).

use std::time::Duration;

use forge_core::error::Result;
use forge_ipc::api::methods::{db_create, db_drop, db_list, db_shell, db_url};
use forge_ipc::api::{
    Ack, DatabaseDto, DatabaseListDto, DbCreateParams, DbDropParams, DbListParams, DbShellParams,
    DbUrlDto, DbUrlParams, ShellCommandDto,
};
use forge_ipc::server::{Ctx, Router};

use super::route;
use crate::daemon::Daemon;
use crate::databases;

/// Answer deadline of the methods that run the database client (5 s probe budget plus
/// slack); the default of 1 s is for calls that never leave the daemon.
const CLIENT_BUDGET: Duration = Duration::from_secs(8);

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, db_list, list);
    route!(router, daemon, db_create, create);
    route!(router, daemon, db_drop, drop_database);
    route!(router, daemon, db_url, url);
    route!(router, daemon, db_shell, shell);
    // After the routes: `handle` resets the deadline of its method.
    router
        .timeout::<db_list>(CLIENT_BUDGET)
        .timeout::<db_create>(CLIENT_BUDGET)
        .timeout::<db_drop>(CLIENT_BUDGET);
}

async fn list(daemon: Daemon, params: DbListParams, _ctx: Ctx) -> Result<DatabaseListDto> {
    databases::list(&daemon, params).await
}

async fn create(daemon: Daemon, params: DbCreateParams, _ctx: Ctx) -> Result<DatabaseDto> {
    databases::create(&daemon, params).await
}

async fn drop_database(daemon: Daemon, params: DbDropParams, _ctx: Ctx) -> Result<Ack> {
    databases::drop_database(&daemon, params).await
}

async fn url(daemon: Daemon, params: DbUrlParams, _ctx: Ctx) -> Result<DbUrlDto> {
    databases::url(&daemon, params).await
}

async fn shell(daemon: Daemon, params: DbShellParams, _ctx: Ctx) -> Result<ShellCommandDto> {
    databases::shell(&daemon, params).await
}
