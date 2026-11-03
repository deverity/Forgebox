//! `mail.*`: the captured e-mail of the `spec.mail` instances (docs/09 §mail). Thin
//! handlers over [`crate::mail`]; without an installed tool that declares `spec.mail`
//! every method answers `not_found` with `forge tool install mailpit`.

use forge_core::error::Result;
use forge_ipc::api::methods::{
    mail_attachment, mail_clear, mail_delete, mail_env, mail_list, mail_mark_read, mail_open,
    mail_show, mail_status,
};
use forge_ipc::api::{
    Ack, EnvVarsDto, MailAttachmentContentDto, MailAttachmentParams, MailClearParams,
    MailDeleteParams, MailListDto, MailListParams, MailMarkReadParams, MailMessageDto,
    MailShowParams, MailStatusDto, NoParams, Role,
};
use forge_ipc::server::{Ctx, Router};

use super::route;
use crate::daemon::Daemon;
use crate::mail;

/// Budget of the shell call of `mail.open`.
const OPEN_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
/// Answer deadline of the calls that make one request (the adapter's own budget is 2.3 s).
const SINGLE: std::time::Duration = std::time::Duration::from_millis(2500);
/// Answer deadline of `show` and `attachment` (a few requests).
const MULTI: std::time::Duration = std::time::Duration::from_secs(9);

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, mail_status, status);
    route!(router, daemon, mail_open, open);
    route!(router, daemon, mail_list, list);
    route!(router, daemon, mail_show, show);
    route!(router, daemon, mail_mark_read, mark_read);
    route!(router, daemon, mail_delete, delete);
    route!(router, daemon, mail_attachment, attachment);
    route!(router, daemon, mail_clear, clear);
    route!(router, daemon, mail_env, env);
    // After the routes: `handle` resets the deadline of its method.
    router
        .timeout::<mail_status>(SINGLE)
        .timeout::<mail_list>(SINGLE)
        .timeout::<mail_mark_read>(SINGLE)
        .timeout::<mail_delete>(SINGLE)
        .timeout::<mail_clear>(SINGLE)
        .timeout::<mail_show>(MULTI)
        .timeout::<mail_attachment>(MULTI)
        .timeout::<mail_open>(MULTI);
}

async fn status(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<MailStatusDto> {
    mail::status(&daemon).await
}

/// `mail.open` (local only): the Mailpit UI in the default browser of the daemon's machine.
async fn open(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<Ack> {
    let url = mail::ui_url(&daemon).await?;
    let opener = std::sync::Arc::clone(daemon.platform.shell_opener());
    forge_core::runtime::spawn_blocking_timeout("opening the browser", OPEN_BUDGET, move || {
        opener.open_url(&url)
    })
    .await?;
    Ok(Ack::ok())
}

async fn list(daemon: Daemon, params: MailListParams, _ctx: Ctx) -> Result<MailListDto> {
    mail::list(&daemon, params).await
}

/// `mail.show`: a `viewer` reads without changing the mailbox.
async fn show(daemon: Daemon, params: MailShowParams, ctx: Ctx) -> Result<MailMessageDto> {
    let may_change = ctx.session().role.allows(Role::Operator);
    mail::show(&daemon, &params.id, params.mark_read, may_change).await
}

async fn mark_read(daemon: Daemon, params: MailMarkReadParams, _ctx: Ctx) -> Result<Ack> {
    mail::mark_read(&daemon, params.ids, params.read).await?;
    Ok(Ack::ok())
}

async fn delete(daemon: Daemon, params: MailDeleteParams, _ctx: Ctx) -> Result<Ack> {
    mail::delete(&daemon, params.ids, params.confirm).await?;
    Ok(Ack::ok())
}

async fn attachment(
    daemon: Daemon,
    params: MailAttachmentParams,
    _ctx: Ctx,
) -> Result<MailAttachmentContentDto> {
    mail::attachment(&daemon, &params.id, &params.part_id).await
}

async fn clear(daemon: Daemon, params: MailClearParams, _ctx: Ctx) -> Result<Ack> {
    mail::clear(&daemon, params.confirm).await?;
    Ok(Ack::ok())
}

async fn env(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<EnvVarsDto> {
    Ok(EnvVarsDto {
        vars: mail::env(&daemon).await?,
    })
}
