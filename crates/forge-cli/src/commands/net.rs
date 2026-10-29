//! `forge dns ...`, `forge tls ...`, `forge lan ...` (docs/09 §net / tls) and `forge mail ...`.
//!
//! Actions that go through the elevated helper (NRPT, LocalMachine trust, firewall) announce
//! the UAC prompt, or fail with exit 5 under `--no-elevate` before any request is sent.

use forge_ipc::api::methods::{
    net_dns_disable, net_dns_enable, net_lan_expose, net_lan_unexpose, tls_ca_install,
    tls_ca_rotate, tls_ca_uninstall, tls_issue, tls_renew,
};
use forge_ipc::api::{
    Ack, CaInstallParams, CaRotateParams, CaStatusDto, CaUninstallParams, CertFormat,
    CertificateDto, CertificateListDto, DnsMode, DnsPolicyReason, DnsPolicyState, DnsProbeDto,
    DnsStatusDto, DnsTestParams, LanAccessDto, LanSiteParams, MailAttachmentParams,
    MailClearParams, MailDeleteParams, MailListParams, MailMarkReadParams, MailShowParams,
    NoParams, SiteRef, TlsExportParams, TlsHostParams, TlsRenewParams, TrustScopeDto,
};

use crate::cli::{DnsCmd, LanCmd, MailCmd, ScopeArg, TlsCaCmd, TlsCmd};
use crate::context::Context;
use crate::error::{CliError, CliResult};
use crate::messages::Msg;
use crate::output::{DIM, GREEN, Output, RED, YELLOW, or_dash, paint};
use crate::render;

// --- dns ----------------------------------------------------------------------------

fn dns_reason(out: &Output, reason: DnsPolicyReason) -> &'static str {
    out.t(match reason {
        DnsPolicyReason::GroupPolicyTablePresent => Msg::DnsReasonGroupPolicy,
        DnsPolicyReason::ResolverProbeFailed => Msg::DnsReasonProbeFailed,
        DnsPolicyReason::ResolverNotOnPort53 => Msg::DnsReasonNotOn53,
        DnsPolicyReason::NrptUnsupported => Msg::DnsReasonNrptUnsupported,
        DnsPolicyReason::NrptPointsElsewhere => Msg::DnsReasonPointsElsewhere,
    })
}

/// Effective mode, reason and probe of `tld` (`dns status`, `dns test`).
fn mode_rows(
    out: &Output,
    tld: &str,
    mode: DnsMode,
    reason: Option<DnsPolicyReason>,
    probe: Option<&DnsProbeDto>,
) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let mode = match mode {
        DnsMode::Nrpt => paint(GREEN, out.t(Msg::DnsModeNrpt)),
        DnsMode::Hosts => paint(YELLOW, out.t(Msg::DnsModeHosts)),
        DnsMode::None => paint(RED, out.t(Msg::DnsModeNone)),
    };
    rows.push((out.tf(Msg::HDnsMode, &[&tld]), mode));
    if let Some(reason) = reason {
        rows.push((
            out.tf(Msg::HDnsReason, &[&tld]),
            dns_reason(out, reason).to_owned(),
        ));
    }
    if let Some(probe) = probe {
        let text = if probe.ok {
            let text = out.tf(
                Msg::DnsProbeOk,
                &[
                    &probe.host,
                    &probe.resolved.join(", "),
                    &probe.attempts,
                    &probe.elapsed_ms,
                ],
            );
            paint(GREEN, text)
        } else {
            let text = out.tf(
                Msg::DnsProbeFailed,
                &[
                    &probe.host,
                    &probe.error.as_deref().unwrap_or("-"),
                    &probe.attempts,
                    &probe.elapsed_ms,
                ],
            );
            paint(RED, text)
        };
        rows.push((out.tf(Msg::HDnsProbe, &[&tld]), text));
    }
    rows
}

fn dns_status_lines(out: &Output, status: &DnsStatusDto) {
    let policy = match status.policy {
        DnsPolicyState::Applied => paint(GREEN, out.t(Msg::DnsApplied)),
        DnsPolicyState::HostsFallback => paint(YELLOW, out.t(Msg::DnsHostsFallback)),
        DnsPolicyState::NotApplied => paint(DIM, out.t(Msg::DnsNotApplied)),
    };
    let mut rows: Vec<(String, String)> = vec![
        (
            out.t(Msg::HEnabled).to_owned(),
            render::yes_no(out, status.enabled),
        ),
        (
            out.t(Msg::HResolver).to_owned(),
            or_dash(status.listen.as_deref()),
        ),
        (
            out.t(Msg::HResolverRunning).to_owned(),
            render::yes_no(out, status.server_running),
        ),
        ("TLDs".to_owned(), status.tlds.join(", ")),
        (out.t(Msg::HPolicy).to_owned(), policy),
    ];
    // Effective mode, reason and verification of each TLD (ADR-0033).
    for tld in &status.tld_policies {
        rows.extend(mode_rows(
            out,
            &tld.tld,
            tld.mode,
            tld.reason,
            tld.probe.as_ref(),
        ));
    }
    let pairs: Vec<(&str, String)> = rows.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    out.pairs(&pairs);
}

pub async fn dns(ctx: &Context, command: DnsCmd) -> CliResult<()> {
    match command {
        DnsCmd::Status => {
            let status = ctx
                .call::<forge_ipc::api::methods::net_dns_status>(NoParams {})
                .await?;
            ctx.out.emit(&status, |out| dns_status_lines(out, &status));
        }
        DnsCmd::Enable => {
            ctx.elevation("forge dns enable")?;
            let status: DnsStatusDto = ctx.job::<net_dns_enable, DnsStatusDto>(NoParams {}).await?;
            ctx.out.emit(&status, |out| dns_status_lines(out, &status));
        }
        DnsCmd::Disable => {
            ctx.elevation("forge dns disable")?;
            let status: DnsStatusDto = ctx
                .job::<net_dns_disable, DnsStatusDto>(NoParams {})
                .await?;
            ctx.out.emit(&status, |out| dns_status_lines(out, &status));
        }
        DnsCmd::Test { host } => {
            let test = ctx
                .call::<forge_ipc::api::methods::net_dns_test>(DnsTestParams { host })
                .await?;
            ctx.out.emit(&test, |out| {
                let verdict = if test.ok {
                    paint(GREEN, out.t(Msg::DnsResolves))
                } else {
                    paint(RED, out.t(Msg::DnsDoesNotResolve))
                };
                out.line(format!("{}  {}", test.host, verdict));
                let mut rows = vec![
                    (out.t(Msg::HResolved).to_owned(), test.resolved.join(", ")),
                    (out.t(Msg::HExpected).to_owned(), test.expected.join(", ")),
                    (out.t(Msg::HVia).to_owned(), test.via.clone()),
                ];
                let tld = test
                    .probe
                    .as_ref()
                    .and_then(|p| p.host.split_once('.'))
                    .map(|(_, tld)| format!(".{tld}"))
                    .unwrap_or_default();
                rows.extend(mode_rows(
                    out,
                    &tld,
                    test.mode,
                    test.reason,
                    test.probe.as_ref(),
                ));
                let pairs: Vec<(&str, String)> =
                    rows.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
                out.pairs(&pairs);
            });
            if !test.ok {
                return Err(CliError::exit_silently(1));
            }
        }
    }
    Ok(())
}

// --- tls ----------------------------------------------------------------------------

fn ca_lines(out: &Output, ca: &CaStatusDto) {
    if !ca.exists {
        out.line(paint(YELLOW, out.t(Msg::CaMissing)));
        return;
    }
    out.pairs(&[
        (out.t(Msg::HSubject), or_dash(ca.subject.as_deref())),
        ("SHA-256", or_dash(ca.thumbprint_sha256.as_deref())),
        (
            out.t(Msg::HExpires),
            ca.not_after.map_or_else(|| "-".into(), render::time_cell),
        ),
        (
            "CurrentUser\\Root",
            render::yes_no(out, ca.trusted_current_user),
        ),
        (
            "LocalMachine\\Root",
            render::yes_no(out, ca.trusted_local_machine),
        ),
        ("Name constraints", ca.name_constraints.join(", ")),
    ]);
}

fn certificates_table(out: &Output, certificates: &[CertificateDto]) {
    let rows = certificates
        .iter()
        .map(|c| {
            vec![
                c.host.clone(),
                c.sans.join(", "),
                render::time_cell(c.not_after),
            ]
        })
        .collect();
    out.table(
        &["HOST", "SAN", out.t(Msg::HExpires)],
        rows,
        Msg::NoCertificates,
    );
}

/// Writes `bytes` to `path` and tells the user.
fn write_output(ctx: &Context, path: &std::path::Path, bytes: &[u8]) -> CliResult<()> {
    std::fs::write(path, bytes).map_err(|e| CliError::io(format!("{}: {e}", path.display())))?;
    ctx.out
        .success(ctx.out.tf(Msg::FileWritten, &[&path.display()]));
    Ok(())
}

pub async fn tls(ctx: &Context, command: TlsCmd) -> CliResult<()> {
    match command {
        TlsCmd::Ca(ca) => tls_ca(ctx, ca).await,
        TlsCmd::Issue { host } => {
            let cert: CertificateDto = ctx
                .job::<tls_issue, CertificateDto>(TlsHostParams { host })
                .await?;
            ctx.out.emit(&cert, |out| {
                certificates_table(out, std::slice::from_ref(&cert))
            });
            Ok(())
        }
        TlsCmd::Renew { host, all } => {
            let list: CertificateListDto = ctx
                .job::<tls_renew, CertificateListDto>(TlsRenewParams { host, all })
                .await?;
            ctx.out
                .emit(&list, |out| certificates_table(out, &list.certificates));
            Ok(())
        }
        TlsCmd::Export {
            host,
            pfx,
            password,
            output,
        } => {
            let format = if pfx {
                CertFormat::Pfx
            } else {
                CertFormat::Pem
            };
            let export = ctx
                .call::<forge_ipc::api::methods::tls_export>(TlsExportParams {
                    host,
                    format,
                    password,
                })
                .await?;
            let bytes = decode_base64(&export.data_base64)?;
            match output {
                Some(path) if ctx.out.human() => write_output(ctx, &path, &bytes),
                Some(path) => {
                    std::fs::write(&path, &bytes)
                        .map_err(|e| CliError::io(format!("{}: {e}", path.display())))?;
                    ctx.out.json_value(&serde_json::json!({
                        "filename": export.filename,
                        "path": path.display().to_string(),
                        "bytes": bytes.len(),
                    }));
                    Ok(())
                }
                None => {
                    ctx.out.emit(&export, |out| {
                        if pfx {
                            out.line(out.tf(Msg::BinaryNeedsOutput, &[&export.filename]));
                        } else {
                            out.raw(&String::from_utf8_lossy(&bytes));
                        }
                    });
                    Ok(())
                }
            }
        }
    }
}

fn decode_base64(text: &str) -> CliResult<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|e| {
            CliError::new(
                "internal",
                format!("base64 inválido na resposta do daemon: {e}"),
            )
        })
}

async fn tls_ca(ctx: &Context, command: TlsCaCmd) -> CliResult<()> {
    match command {
        TlsCaCmd::Status => {
            let ca = ctx
                .call::<forge_ipc::api::methods::tls_ca_status>(NoParams {})
                .await?;
            ctx.out.emit(&ca, |out| ca_lines(out, &ca));
        }
        TlsCaCmd::Install { scope } => {
            let scope = match scope {
                ScopeArg::User => TrustScopeDto::CurrentUser,
                ScopeArg::Machine => {
                    ctx.elevation("forge tls ca install --scope machine")?;
                    TrustScopeDto::LocalMachine
                }
            };
            let ca: CaStatusDto = ctx
                .job::<tls_ca_install, CaStatusDto>(CaInstallParams { scope })
                .await?;
            ctx.out.emit(&ca, |out| ca_lines(out, &ca));
        }
        TlsCaCmd::Uninstall { scope } => {
            let scope = match scope {
                ScopeArg::User => TrustScopeDto::CurrentUser,
                ScopeArg::Machine => {
                    ctx.elevation("forge tls ca uninstall --scope machine")?;
                    TrustScopeDto::LocalMachine
                }
            };
            let ca: CaStatusDto = ctx
                .job::<tls_ca_uninstall, CaStatusDto>(CaUninstallParams { scope })
                .await?;
            ctx.out.emit(&ca, |out| ca_lines(out, &ca));
        }
        TlsCaCmd::Export { output } => {
            let pem = ctx
                .call::<forge_ipc::api::methods::tls_ca_export>(NoParams {})
                .await?;
            match output {
                Some(path) if ctx.out.human() => write_output(ctx, &path, pem.pem.as_bytes())?,
                Some(path) => {
                    std::fs::write(&path, pem.pem.as_bytes())
                        .map_err(|e| CliError::io(format!("{}: {e}", path.display())))?;
                    ctx.out.json_value(&serde_json::json!({
                        "filename": pem.filename,
                        "path": path.display().to_string(),
                    }));
                }
                None => ctx.out.emit(&pem, |out| out.raw(&pem.pem)),
            }
        }
        TlsCaCmd::Rotate => {
            ctx.confirm(ctx.out.t(Msg::ConfirmRotateCa))?;
            let ca: CaStatusDto = ctx
                .job::<tls_ca_rotate, CaStatusDto>(CaRotateParams { confirm: true })
                .await?;
            ctx.out.emit(&ca, |out| ca_lines(out, &ca));
        }
    }
    Ok(())
}

// --- lan ----------------------------------------------------------------------------

pub async fn lan(ctx: &Context, command: LanCmd) -> CliResult<()> {
    match command {
        LanCmd::Expose { site } => {
            ctx.elevation("forge lan expose")?;
            let access: LanAccessDto = ctx
                .job::<net_lan_expose, LanAccessDto>(LanSiteParams {
                    site: SiteRef::from(site),
                })
                .await?;
            ctx.out.emit(&access, |out| {
                for url in &access.urls {
                    out.line(url);
                }
            });
        }
        LanCmd::Unexpose { site } => {
            ctx.elevation("forge lan unexpose")?;
            let ack: Ack = ctx
                .job::<net_lan_unexpose, Ack>(LanSiteParams {
                    site: SiteRef::from(site),
                })
                .await?;
            ctx.out
                .emit(&ack, |out| out.success(out.t(Msg::LanUnexposed)));
        }
        LanCmd::Qr { site } => {
            let access = ctx
                .call::<forge_ipc::api::methods::net_lan_qr>(LanSiteParams {
                    site: SiteRef::from(site),
                })
                .await?;
            ctx.out.emit(&access, |out| {
                for url in &access.urls {
                    out.line(url);
                }
                if let Some(svg) = &access.qr_svg {
                    out.raw(svg);
                }
            });
        }
    }
    Ok(())
}

// --- mail ---------------------------------------------------------------------------

pub async fn mail(ctx: &Context, command: MailCmd) -> CliResult<()> {
    match command {
        MailCmd::Status => {
            let status = ctx
                .call::<forge_ipc::api::methods::mail_status>(NoParams {})
                .await?;
            ctx.out.emit(&status, |out| {
                out.pairs(&[
                    (out.t(Msg::HState), render::yes_no(out, status.running)),
                    ("SMTP", or_dash(status.smtp_port)),
                    ("UI", or_dash(status.ui_url.as_deref())),
                    (out.t(Msg::HMessages), status.messages.to_string()),
                    (out.t(Msg::HUnread), status.unread.to_string()),
                ]);
            });
        }
        MailCmd::Open => {
            let ack = ctx
                .call::<forge_ipc::api::methods::mail_open>(NoParams {})
                .await?;
            ctx.out.emit(&ack, |_| {});
        }
        MailCmd::List {
            unread,
            search,
            limit,
            offset,
        } => {
            let list = ctx
                .call::<forge_ipc::api::methods::mail_list>(MailListParams {
                    unread,
                    limit,
                    query: search,
                    offset,
                })
                .await?;
            ctx.out.emit(&list, |out| {
                let rows = list
                    .messages
                    .iter()
                    .map(|m| {
                        vec![
                            if m.read {
                                " ".into()
                            } else {
                                paint(GREEN, "*")
                            },
                            m.id.clone(),
                            m.from.clone(),
                            m.subject.clone(),
                            render::time_cell(m.received_at),
                        ]
                    })
                    .collect();
                out.table(
                    &[
                        "",
                        "ID",
                        out.t(Msg::HFrom),
                        out.t(Msg::HSubject),
                        out.t(Msg::HReceived),
                    ],
                    rows,
                    Msg::NoMail,
                );
                let shown = u32::try_from(list.messages.len()).unwrap_or(u32::MAX);
                let first = offset.unwrap_or(0);
                if shown > 0 && first.saturating_add(shown) < list.total {
                    out.line(paint(
                        DIM,
                        out.tf(
                            Msg::MailPage,
                            &[&(first + 1), &(first + shown), &list.total],
                        ),
                    ));
                }
            });
        }
        MailCmd::Show {
            id,
            attachment: Some(part_id),
            output,
            ..
        } => {
            let content = ctx
                .call::<forge_ipc::api::methods::mail_attachment>(MailAttachmentParams {
                    id,
                    part_id,
                })
                .await?;
            let bytes = decode_base64(&content.content_base64)?;
            match output {
                Some(path) if ctx.out.human() => write_output(ctx, &path, &bytes)?,
                Some(path) => {
                    std::fs::write(&path, &bytes)
                        .map_err(|e| CliError::io(format!("{}: {e}", path.display())))?;
                    ctx.out.json_value(&serde_json::json!({
                        "filename": content.file_name,
                        "path": path.display().to_string(),
                        "bytes": bytes.len(),
                    }));
                }
                // Never written under the sender's file name: it is untrusted input.
                None => ctx
                    .out
                    .emit(&content, |out| match std::str::from_utf8(&bytes) {
                        Ok(text) if content.content_type.starts_with("text/") => out.raw(text),
                        _ => out.line(out.tf(Msg::BinaryNeedsOutput, &[&content.file_name])),
                    }),
            }
        }
        MailCmd::Show {
            id, keep_unread, ..
        } => {
            let message = ctx
                .call::<forge_ipc::api::methods::mail_show>(MailShowParams {
                    id,
                    mark_read: !keep_unread,
                })
                .await?;
            ctx.out.emit(&message, |out| {
                let mut pairs = vec![
                    (out.t(Msg::HFrom), message.summary.from.clone()),
                    (out.t(Msg::HTo), message.summary.to.join(", ")),
                ];
                if !message.cc.is_empty() {
                    pairs.push(("Cc", message.cc.join(", ")));
                }
                pairs.push((out.t(Msg::HSubject), message.summary.subject.clone()));
                if !message.attachments.is_empty() {
                    let names: Vec<String> = message
                        .attachments
                        .iter()
                        .map(|a| format!("{} [{}]", a.file_name, a.part_id))
                        .collect();
                    pairs.push((out.t(Msg::HAttachments), names.join(", ")));
                }
                out.pairs(&pairs);
                out.line("");
                out.raw(message.text.as_deref().unwrap_or(""));
            });
        }
        MailCmd::Read { ids, unread } => {
            let count = ids.len();
            let ack = ctx
                .call::<forge_ipc::api::methods::mail_mark_read>(MailMarkReadParams {
                    ids,
                    read: !unread,
                })
                .await?;
            ctx.out.emit(&ack, |out| {
                let msg = if unread {
                    Msg::MailMarkedUnread
                } else {
                    Msg::MailMarkedRead
                };
                out.success(out.tf(msg, &[&count]));
            });
        }
        MailCmd::Delete { ids } => {
            let count = ids.len();
            ctx.confirm(&ctx.out.tf(Msg::ConfirmDeleteMail, &[&count]))?;
            let ack = ctx
                .call::<forge_ipc::api::methods::mail_delete>(MailDeleteParams {
                    ids,
                    confirm: true,
                })
                .await?;
            ctx.out
                .emit(&ack, |out| out.success(out.tf(Msg::MailDeleted, &[&count])));
        }
        MailCmd::Clear => {
            ctx.confirm(ctx.out.t(Msg::ConfirmClearMail))?;
            let ack = ctx
                .call::<forge_ipc::api::methods::mail_clear>(MailClearParams { confirm: true })
                .await?;
            ctx.out
                .emit(&ack, |out| out.success(out.t(Msg::MailCleared)));
        }
        MailCmd::Env => {
            let env = ctx
                .call::<forge_ipc::api::methods::mail_env>(NoParams {})
                .await?;
            ctx.out.emit(&env, |out| {
                for (key, value) in &env.vars {
                    out.line(format!("{key}={value}"));
                }
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_with_and_without_padding() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("aGk=\n").unwrap(), b"hi");
        assert_eq!(decode_base64("").unwrap(), b"");
        assert!(decode_base64("a*b").is_err());
    }
}
