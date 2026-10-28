//! `forge remote ...` (docs/09 §stats / remote, docs/13). `add|list|use|remove` manage the
//! connection profiles of this machine and never reach a daemon; `pair|listen|clients|revoke`
//! run on the remote side and go through the API (remote mode is M6-E3: the daemon answers
//! `not_implemented` until then).

use forge_ipc::api::methods::remote_listen;
use forge_ipc::api::{Ack, NoParams, RemoteListenParams, RemoteRevokeParams, RemoteTlsMode};
use forge_ipc::base::ids::ClientId;
use serde::Serialize;

use crate::cli::RemoteCmd;
use crate::context::Context;
use crate::error::{CliError, CliResult};
use crate::messages::Msg;
use crate::output::{GREEN, paint};
use crate::remotes::{LOCAL, Remotes};

/// One row of `forge remote list --json`.
#[derive(Debug, Serialize)]
struct ProfileDto {
    name: String,
    url: String,
    current: bool,
}

pub async fn run(ctx: &Context, command: RemoteCmd) -> CliResult<()> {
    match command {
        RemoteCmd::Add { name, url, code } => {
            let mut remotes = Remotes::load()?;
            remotes.add(&name, &url)?;
            remotes.save()?;
            ctx.out.emit(
                &ProfileDto {
                    name: name.clone(),
                    url: url.clone(),
                    current: false,
                },
                |out| {
                    out.success(out.tf(Msg::RemoteSaved, &[&name]));
                    if code.is_some() {
                        // The code is single use and pairing is remote mode (M6-E3).
                        out.warn(out.t(Msg::RemotePairingLater));
                    }
                },
            );
            Ok(())
        }
        RemoteCmd::List => {
            let remotes = Remotes::load()?;
            let mut profiles = vec![ProfileDto {
                name: LOCAL.into(),
                url: ctx.root.to_posix_string(),
                current: remotes.current.is_none(),
            }];
            profiles.extend(remotes.profiles.iter().map(|(name, p)| ProfileDto {
                name: name.clone(),
                url: p.url.clone(),
                current: remotes.current.as_deref() == Some(name.as_str()),
            }));
            ctx.out.emit(&profiles, |out| {
                let rows = profiles
                    .iter()
                    .map(|p| {
                        vec![
                            if p.current {
                                paint(GREEN, "*")
                            } else {
                                " ".into()
                            },
                            p.name.clone(),
                            p.url.clone(),
                        ]
                    })
                    .collect();
                out.table(&["", out.t(Msg::HName), "URL"], rows, Msg::NoProfiles);
            });
            Ok(())
        }
        RemoteCmd::Use { name } => {
            let mut remotes = Remotes::load()?;
            remotes.select(&name)?;
            remotes.save()?;
            ctx.out
                .emit(&serde_json::json!({ "current": name }), |out| {
                    out.success(out.tf(Msg::RemoteSelected, &[&name]));
                });
            Ok(())
        }
        RemoteCmd::Remove { name } => {
            let mut remotes = Remotes::load()?;
            remotes.remove(&name)?;
            remotes.save()?;
            ctx.out
                .emit(&serde_json::json!({ "removed": name }), |out| {
                    out.success(out.tf(Msg::RemoteRemoved, &[&name]));
                });
            Ok(())
        }
        RemoteCmd::Pair => {
            let code = ctx
                .call::<forge_ipc::api::methods::remote_pair>(NoParams {})
                .await?;
            ctx.out.emit(&code, |out| {
                out.pairs(&[
                    (out.t(Msg::HCode), code.code.clone()),
                    (out.t(Msg::HCaFingerprint), code.ca_fingerprint.clone()),
                    (
                        out.t(Msg::HExpires),
                        crate::render::time_cell(code.expires_at),
                    ),
                ]);
            });
            Ok(())
        }
        RemoteCmd::Listen { addr } => {
            let ack: Ack = ctx
                .job::<remote_listen, Ack>(RemoteListenParams {
                    addr,
                    tls: RemoteTlsMode::Auto,
                })
                .await?;
            ctx.out
                .emit(&ack, |out| out.success(out.t(Msg::RemoteListening)));
            Ok(())
        }
        RemoteCmd::Clients => {
            let list = ctx
                .call::<forge_ipc::api::methods::remote_clients>(NoParams {})
                .await?;
            ctx.out.emit(&list, |out| {
                let rows = list
                    .clients
                    .iter()
                    .map(|c| {
                        vec![
                            c.id.to_string(),
                            c.name.clone(),
                            c.role.to_string(),
                            c.last_seen_at
                                .map_or_else(|| "-".into(), crate::render::time_cell),
                        ]
                    })
                    .collect();
                out.table(
                    &[
                        "ID",
                        out.t(Msg::HName),
                        out.t(Msg::HRole),
                        out.t(Msg::HLastSeen),
                    ],
                    rows,
                    Msg::NoClients,
                );
            });
            Ok(())
        }
        RemoteCmd::Revoke { id } => {
            let client_id = id
                .parse::<ClientId>()
                .map_err(|_| CliError::usage(ctx.out.tf(Msg::InvalidClientId, &[&id])))?;
            ctx.confirm(&ctx.out.tf(Msg::ConfirmRevoke, &[&id]))?;
            let ack = ctx
                .call::<forge_ipc::api::methods::remote_revoke>(RemoteRevokeParams { client_id })
                .await?;
            ctx.out
                .emit(&ack, |out| out.success(out.t(Msg::ClientRevoked)));
            Ok(())
        }
    }
}
