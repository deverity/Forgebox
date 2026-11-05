//! `forge db ...` (docs/09 §db).

use forge_ipc::api::methods::{db_export, db_import};
use forge_ipc::api::{
    Ack, DatabaseDto, DbCreateParams, DbDropParams, DbExportDto, DbExportParams, DbImportParams,
    DbListParams, DbOpenParams, DbShellParams, DbUrlParams, InstanceRef,
};

use crate::cli::DbCmd;
use crate::context::Context;
use crate::error::CliResult;
use crate::messages::Msg;
use crate::output::{Output, format_bytes, or_dash};
use crate::parse;

fn instance(value: Option<String>) -> Option<InstanceRef> {
    value.map(InstanceRef::from)
}

fn database_table(out: &Output, databases: &[DatabaseDto]) {
    let rows = databases
        .iter()
        .map(|d| {
            vec![
                d.name.clone(),
                d.instance.clone(),
                or_dash(d.charset.as_deref()),
                or_dash(d.tables),
                or_dash(d.size_bytes.map(format_bytes)),
            ]
        })
        .collect();
    out.table(
        &[
            out.t(Msg::HName),
            out.t(Msg::HInstance),
            "CHARSET",
            out.t(Msg::HTables),
            out.t(Msg::HSize),
        ],
        rows,
        Msg::NoDatabases,
    );
}

pub async fn run(ctx: &Context, command: DbCmd) -> CliResult<()> {
    match command {
        DbCmd::List { instance: target } => {
            let list = ctx
                .call::<forge_ipc::api::methods::db_list>(DbListParams {
                    instance: instance(target),
                })
                .await?;
            ctx.out
                .emit(&list, |out| database_table(out, &list.databases));
        }
        DbCmd::Create { name, on, charset } => {
            let db = ctx
                .call::<forge_ipc::api::methods::db_create>(DbCreateParams {
                    name,
                    instance: instance(on),
                    charset,
                })
                .await?;
            ctx.out.emit(&db, |out| {
                out.success(out.tf(Msg::DatabaseCreated, &[&db.name]))
            });
        }
        DbCmd::Drop { name, on } => {
            ctx.confirm(&ctx.out.tf(Msg::ConfirmDropDatabase, &[&name]))?;
            let ack = ctx
                .call::<forge_ipc::api::methods::db_drop>(DbDropParams {
                    name: name.clone(),
                    instance: instance(on),
                    confirm: true,
                })
                .await?;
            ctx.out.emit(&ack, |out| {
                out.success(out.tf(Msg::DatabaseDropped, &[&name]))
            });
        }
        DbCmd::Export { name, on, output } => {
            let output = output.map(|p| parse::absolute(&p)).transpose()?;
            let export: DbExportDto = ctx
                .job::<db_export, DbExportDto>(DbExportParams {
                    name,
                    instance: instance(on),
                    output,
                })
                .await?;
            ctx.out.emit(&export, |out| {
                out.success(out.tf(
                    Msg::DatabaseExported,
                    &[&export.path, &format_bytes(export.size_bytes)],
                ));
            });
        }
        DbCmd::Import { name, file, on } => {
            let ack: Ack = ctx
                .job::<db_import, Ack>(DbImportParams {
                    name: name.clone(),
                    instance: instance(on),
                    file: parse::absolute(&file)?,
                })
                .await?;
            ctx.out.emit(&ack, |out| {
                out.success(out.tf(Msg::DatabaseImported, &[&name]))
            });
        }
        DbCmd::Shell { instance, database } => {
            let command = ctx
                .call::<forge_ipc::api::methods::db_shell>(DbShellParams {
                    instance: InstanceRef::from(instance),
                    database,
                })
                .await?;
            // `--json` prints the command (for scripts); otherwise the client runs here,
            // attached to this console, and `forge` exits with its code.
            if ctx.out.json {
                ctx.out.json_value(&command);
            } else {
                super::shell::run_attached(ctx, &command).await?;
            }
        }
        DbCmd::Url { instance, database } => {
            let url = ctx
                .call::<forge_ipc::api::methods::db_url>(DbUrlParams {
                    instance: InstanceRef::from(instance),
                    database,
                })
                .await?;
            ctx.out.emit(&url, |out| {
                out.line(&url.url);
                for (key, value) in &url.env.vars {
                    out.line(format!("{key}={value}"));
                }
            });
        }
        DbCmd::Open {
            instance,
            with,
            database,
        } => {
            let ack = ctx
                .call::<forge_ipc::api::methods::db_open>(DbOpenParams {
                    instance: InstanceRef::from(instance),
                    with,
                    database,
                })
                .await?;
            ctx.out.emit(&ack, |_| {});
        }
    }
    Ok(())
}
