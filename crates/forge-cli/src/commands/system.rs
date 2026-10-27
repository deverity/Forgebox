//! `forge registry|import|bundle|backup|config|self-update|completions|mcp` (docs/09
//! §registry / sistema).

use std::path::PathBuf;
use std::process::{Command, Stdio};

use clap::CommandFactory;
use forge_ipc::api::methods::{
    registry_update, system_backup_now, system_bundle_export, system_bundle_import,
    system_import_laragon, system_self_update,
};
use forge_ipc::api::{
    ArtifactDto, BackupFrequency, BackupNowParams, BackupReportDto, BackupScheduleDto,
    BundleExportParams, BundleImportParams, ConfigGetParams, ConfigSetParams, ImportLaragonParams,
    ImportReportDto, InstanceRef, NoParams, RegistryAddParams, RegistryListDto,
    RegistryRemoveParams, SelfUpdateDto, SelfUpdateParams, ToolInstallResultDto, ToolSpec,
};

use crate::cli::{
    BackupCmd, BundleCmd, Cli, CompletionsArgs, ConfigCmd, FrequencyArg, ImportCmd, McpCmd,
    RegistryCmd, SelfUpdateArgs,
};
use crate::context::Context;
use crate::error::{CliError, CliResult};
use crate::messages::Msg;
use crate::output::{DIM, GREEN, Output, format_bytes, or_dash, paint};
use crate::parse;

// --- registry -----------------------------------------------------------------------

fn registry_table(out: &Output, list: &RegistryListDto) {
    let rows = list
        .registries
        .iter()
        .map(|r| {
            vec![
                r.name.clone(),
                format!("{:?}", r.kind).to_lowercase(),
                r.url.clone(),
                r.tools.to_string(),
                if r.official {
                    paint(GREEN, out.t(Msg::Yes))
                } else {
                    out.t(Msg::No).to_owned()
                },
                r.last_refresh_at
                    .map_or_else(|| "-".into(), crate::render::time_cell),
            ]
        })
        .collect();
    out.table(
        &[
            out.t(Msg::HName),
            out.t(Msg::HType),
            "URL",
            out.t(Msg::HTools),
            out.t(Msg::HOfficial),
            out.t(Msg::HUpdated),
        ],
        rows,
        Msg::NoRegistries,
    );
}

pub async fn registry(ctx: &Context, command: RegistryCmd) -> CliResult<()> {
    match command {
        RegistryCmd::List => {
            let list = ctx
                .call::<forge_ipc::api::methods::registry_list>(NoParams {})
                .await?;
            ctx.out.emit(&list, |out| registry_table(out, &list));
        }
        RegistryCmd::Add { url, name } => {
            let added = ctx
                .call::<forge_ipc::api::methods::registry_add>(RegistryAddParams { url, name })
                .await?;
            ctx.out.emit(&added, |out| {
                out.success(out.tf(Msg::RegistryAdded, &[&added.name]))
            });
        }
        RegistryCmd::Remove { name } => {
            let ack = ctx
                .call::<forge_ipc::api::methods::registry_remove>(RegistryRemoveParams {
                    name: name.clone(),
                })
                .await?;
            ctx.out.emit(&ack, |out| {
                out.success(out.tf(Msg::RegistryRemoved, &[&name]))
            });
        }
        RegistryCmd::Update => {
            let list: RegistryListDto = ctx
                .job::<registry_update, RegistryListDto>(NoParams {})
                .await?;
            ctx.out.emit(&list, |out| registry_table(out, &list));
        }
    }
    Ok(())
}

// --- import / bundle / backup -------------------------------------------------------

pub async fn import(ctx: &Context, command: ImportCmd) -> CliResult<()> {
    let ImportCmd::Laragon {
        path,
        dry_run,
        copy_tools,
        move_data,
        convert_case,
    } = command;
    let report: ImportReportDto = ctx
        .job::<system_import_laragon, ImportReportDto>(ImportLaragonParams {
            path: parse::absolute(&path)?,
            dry_run,
            copy_tools,
            move_data,
            convert_case_sensitive: convert_case,
        })
        .await?;
    ctx.out.emit(&report, |out| {
        if report.dry_run {
            out.info(out.t(Msg::ImportDryRun));
        }
        let rows = report
            .items
            .iter()
            .map(|i| {
                vec![
                    i.kind.clone(),
                    i.name.clone(),
                    if i.imported {
                        paint(GREEN, out.t(Msg::Yes))
                    } else {
                        out.t(Msg::No).to_owned()
                    },
                    paint(DIM, i.note.clone().unwrap_or_default()),
                ]
            })
            .collect();
        out.table(
            &[
                out.t(Msg::HType),
                out.t(Msg::HName),
                out.t(Msg::HImported),
                out.t(Msg::HNote),
            ],
            rows,
            Msg::NothingImported,
        );
        let imported = report.items.iter().filter(|i| i.imported).count();
        out.success(out.tf(
            Msg::ImportTotals,
            &[&imported, &(report.items.len() - imported)],
        ));
    });
    Ok(())
}

fn artifacts(out: &Output, artifacts: &[ArtifactDto]) {
    for a in artifacts {
        out.success(out.tf(
            Msg::ArtifactWritten,
            &[&a.path, &format_bytes(a.size_bytes), &a.sha256],
        ));
    }
}

pub async fn bundle(ctx: &Context, command: BundleCmd) -> CliResult<()> {
    match command {
        BundleCmd::Export { specs, output } => {
            let specs = specs
                .iter()
                .map(|s| parse::tool_spec(s).map(|_| ToolSpec::from(s.as_str())))
                .collect::<CliResult<Vec<_>>>()?;
            let output = output.map(|p| parse::absolute(&p)).transpose()?;
            let artifact: ArtifactDto = ctx
                .job::<system_bundle_export, ArtifactDto>(BundleExportParams { specs, output })
                .await?;
            ctx.out.emit(&artifact, |out| {
                artifacts(out, std::slice::from_ref(&artifact))
            });
        }
        BundleCmd::Import { file } => {
            let result: ToolInstallResultDto = ctx
                .job::<system_bundle_import, ToolInstallResultDto>(BundleImportParams {
                    file: parse::absolute(&file)?,
                })
                .await?;
            ctx.out.emit(&result, |out| {
                for v in &result.installed {
                    out.success(out.tf(Msg::ToolInstalled, &[&v.tool, &v.canonical_name]));
                }
            });
        }
    }
    Ok(())
}

pub async fn backup(ctx: &Context, command: BackupCmd) -> CliResult<()> {
    match command {
        BackupCmd::Now { instance } => {
            let report: BackupReportDto = ctx
                .job::<system_backup_now, BackupReportDto>(BackupNowParams {
                    instance: instance.map(InstanceRef::from),
                })
                .await?;
            ctx.out
                .emit(&report, |out| artifacts(out, &report.artifacts));
        }
        BackupCmd::Schedule { frequency, at } => {
            let frequency = match frequency {
                FrequencyArg::Off => BackupFrequency::Off,
                FrequencyArg::Daily => BackupFrequency::Daily,
                FrequencyArg::Weekly => BackupFrequency::Weekly,
            };
            if let Some(at) = &at {
                check_clock(at)?;
            }
            let schedule = ctx
                .call::<forge_ipc::api::methods::system_backup_schedule>(BackupScheduleDto {
                    frequency,
                    at,
                })
                .await?;
            ctx.out.emit(&schedule, |out| {
                out.success(out.tf(
                    Msg::BackupScheduled,
                    &[
                        &format!("{:?}", schedule.frequency).to_lowercase(),
                        &or_dash(schedule.at.as_deref()),
                    ],
                ));
            });
        }
    }
    Ok(())
}

/// `HH:MM`, 24 hours.
fn check_clock(text: &str) -> CliResult<()> {
    let ok = text.split_once(':').is_some_and(|(h, m)| {
        h.len() == 2
            && m.len() == 2
            && h.parse::<u8>().is_ok_and(|h| h < 24)
            && m.parse::<u8>().is_ok_and(|m| m < 60)
    });
    if ok {
        Ok(())
    } else {
        Err(CliError::usage(format!(
            "horário inválido: {text:?} (use HH:MM, 24 horas)"
        )))
    }
}

// --- config / self-update -----------------------------------------------------------

pub async fn config(ctx: &Context, command: ConfigCmd) -> CliResult<()> {
    let value = match command {
        ConfigCmd::Get { key } => {
            ctx.call::<forge_ipc::api::methods::system_config_get>(ConfigGetParams { key })
                .await?
        }
        ConfigCmd::Set { key, value } => {
            ctx.call::<forge_ipc::api::methods::system_config_set>(ConfigSetParams {
                key,
                value: parse::config_value(&value),
            })
            .await?
        }
    };
    ctx.out.emit(&value, |out| match &value.value {
        serde_json::Value::String(text) => out.line(text),
        other => out.line(other.to_string()),
    });
    Ok(())
}

pub async fn self_update(ctx: &Context, args: SelfUpdateArgs) -> CliResult<()> {
    let update: SelfUpdateDto = ctx
        .job::<system_self_update, SelfUpdateDto>(SelfUpdateParams {
            check: args.check,
            channel: args.channel,
        })
        .await?;
    ctx.out.emit(&update, |out| {
        out.pairs(&[
            (out.t(Msg::HCurrent), update.current.clone()),
            (out.t(Msg::HLatest), or_dash(update.latest.as_deref())),
        ]);
        if update.updated {
            out.success(out.t(Msg::SelfUpdated));
            if update.restart_required {
                out.warn(out.t(Msg::RestartRequired));
            }
        }
    });
    Ok(())
}

// --- client-only --------------------------------------------------------------------

/// Writes the completion script of a shell to stdout.
pub fn completions(args: CompletionsArgs) -> CliResult<()> {
    let mut command = Cli::command();
    let mut script = Vec::new();
    clap_complete::generate(args.shell, &mut command, "forge", &mut script);
    use std::io::Write;
    let _ = std::io::stdout().write_all(&script);
    Ok(())
}

/// The MCP server is its own executable (`forge-mcp`, M5): `forge mcp serve` hands over
/// stdio to it.
pub async fn mcp(ctx: &Context, command: McpCmd) -> CliResult<()> {
    let McpCmd::Serve = command;
    let exe =
        mcp_exe().ok_or_else(|| CliError::not_implemented(ctx.out.t(Msg::McpNotAvailable)))?;
    let mut args: Vec<String> = Vec::new();
    if let Some(root) = &ctx.global.root {
        args.push("--root".into());
        args.push(root.display().to_string());
    }
    let status = tokio::task::spawn_blocking(move || {
        Command::new(exe)
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
    })
    .await
    .map_err(|e| CliError::new("internal", e.to_string()))?
    .map_err(CliError::from)?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(CliError::exit_silently(code)),
        None => Err(CliError::exit_silently(1)),
    }
}

fn mcp_exe() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "forge-mcp.exe"
    } else {
        "forge-mcp"
    };
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .filter(|p| p.is_file());
    sibling.or_else(|| {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_times_are_validated() {
        for ok in ["00:00", "02:00", "23:59"] {
            assert!(check_clock(ok).is_ok(), "{ok}");
        }
        for bad in ["2:00", "24:00", "12:60", "noon", "12-30", ""] {
            assert_eq!(check_clock(bad).unwrap_err().exit, 2, "{bad:?}");
        }
    }
}
