//! `forge fs case-sensitive ...` (docs/15 §8): the NTFS case-sensitivity flag of folders.

use forge_ipc::api::methods::fs_parity_case_convert;
use forge_ipc::api::{CaseConvertDto, CaseConvertParams, CaseDirParams};

use crate::cli::{FsCaseCmd, FsCmd};
use crate::context::Context;
use crate::error::CliResult;
use crate::messages::Msg;
use crate::output::format_bytes;
use crate::render;

pub async fn run(ctx: &Context, command: FsCmd) -> CliResult<()> {
    let FsCmd::CaseSensitive(command) = command;
    match command {
        FsCaseCmd::Status { dir } => {
            let dir = ctx.dir_or_cwd(dir.as_deref())?;
            let status = ctx
                .call::<forge_ipc::api::methods::fs_parity_case_status>(CaseDirParams { dir })
                .await?;
            ctx.out
                .emit(&status, |out| render::case_status(out, &status));
        }
        FsCaseCmd::Enable { dir } => {
            let dir = crate::parse::absolute(&dir)?;
            let status = ctx
                .call::<forge_ipc::api::methods::fs_parity_case_enable>(CaseDirParams { dir })
                .await?;
            ctx.out
                .emit(&status, |out| render::case_status(out, &status));
        }
        FsCaseCmd::Disable { dir } => {
            let dir = crate::parse::absolute(&dir)?;
            let status = ctx
                .call::<forge_ipc::api::methods::fs_parity_case_disable>(CaseDirParams { dir })
                .await?;
            ctx.out
                .emit(&status, |out| render::case_status(out, &status));
        }
        FsCaseCmd::Convert { dir, revert } => {
            let dir = crate::parse::absolute(&dir)?;
            let report: CaseConvertDto = ctx
                .job::<fs_parity_case_convert, CaseConvertDto>(CaseConvertParams { dir, revert })
                .await?;
            ctx.out.emit(&report, |out| {
                if report.already {
                    out.success(out.tf(Msg::CaseAlready, &[&report.dir]));
                } else {
                    out.success(out.tf(
                        Msg::CaseConverted,
                        &[
                            &report.dir,
                            &report.files,
                            &report.directories,
                            &format_bytes(report.bytes),
                        ],
                    ));
                }
            });
        }
    }
    Ok(())
}
