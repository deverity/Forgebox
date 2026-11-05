//! `db.*`: databases of the database instances (docs/09 §db). Thin handlers over
//! [`crate::databases`]; `db.export|import` are jobs ([`crate::databases::transfer`],
//! M2-E3-T6); `db.open` belongs to M3-E1 (`not_implemented`).

use std::sync::Arc;
use std::time::Duration;

use forge_core::error::Result;
use forge_core::jobs::JobContext;
use forge_ipc::api::methods::{
    db_create, db_drop, db_export, db_import, db_list, db_shell, db_url,
};
use forge_ipc::api::{
    Ack, DatabaseDto, DatabaseListDto, DbCreateParams, DbDropParams, DbExportParams,
    DbImportParams, DbListParams, DbShellParams, DbUrlDto, DbUrlParams, JobDto, ShellCommandDto,
};
use forge_ipc::server::{Ctx, Router};

use super::route;
use crate::daemon::Daemon;
use crate::databases::{self, transfer};

/// Answer deadline of the methods that run the database client (5 s probe budget plus
/// slack); the default of 1 s is for calls that never leave the daemon.
const CLIENT_BUDGET: Duration = Duration::from_secs(8);

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, db_list, list);
    route!(router, daemon, db_create, create);
    route!(router, daemon, db_drop, drop_database);
    route!(router, daemon, db_url, url);
    route!(router, daemon, db_shell, shell);
    route!(router, daemon, db_export, export);
    route!(router, daemon, db_import, import);
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

/// Progress of a transfer into its job.
fn sink(job: &JobContext) -> transfer::ProgressSink {
    let job = job.clone();
    Arc::new(move |progress| job.progress(progress))
}

async fn export(daemon: Daemon, params: DbExportParams, ctx: Ctx) -> Result<JobDto> {
    let local = ctx.session().connection.local;
    let prepared = databases::prepare_export(&daemon, &params, local).await?;
    let name = format!("db.export {} {}", prepared.database, prepared.label);
    let d = daemon.clone();
    daemon.job(&ctx, name, move |job| async move {
        transfer::export(prepared, &d.job_tree, sink(&job), &job.cancellation_token()).await
    })
}

async fn import(daemon: Daemon, params: DbImportParams, ctx: Ctx) -> Result<JobDto> {
    let local = ctx.session().connection.local;
    let prepared = databases::prepare_import(&daemon, &params, local).await?;
    let name = format!("db.import {} {}", prepared.database, prepared.label);
    let d = daemon.clone();
    daemon.job(&ctx, name, move |job| async move {
        transfer::import(prepared, &d.job_tree, sink(&job), &job.cancellation_token()).await
    })
}
