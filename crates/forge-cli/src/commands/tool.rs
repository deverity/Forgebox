//! `forge search`, `forge tool ...`, `forge use`, `forge which`: the catalog and the
//! installed versions (docs/09 §tool).

use forge_ipc::api::methods::{
    tools_add_local, tools_check_updates, tools_discover, tools_install, tools_remove,
    tools_rollback, tools_update,
};
use forge_ipc::api::{Ack, ToolVersionDto};
use forge_ipc::api::{
    ToolAddLocalParams, ToolDiscoverDto, ToolDto, ToolInstallParams, ToolInstallResultDto,
    ToolListParams, ToolParams, ToolPolicyParams, ToolRemoveParams, ToolRollbackParams,
    ToolSearchParams, ToolSpec, ToolUpdateParams, ToolUpdatesDto, ToolUseParams, ToolWhichParams,
    UpdatePolicy,
};

use crate::cli::{PolicyArg, SearchArgs, ToolCmd, UseArgs, WhichArgs};
use crate::context::Context;
use crate::error::{CliError, CliResult};
use crate::messages::Msg;
use crate::output::{DIM, GREEN, YELLOW, or_dash, paint};
use crate::parse;
use crate::render;

pub fn policy_name(policy: UpdatePolicy) -> &'static str {
    match policy {
        UpdatePolicy::Manual => "manual",
        UpdatePolicy::Notify => "notify",
        UpdatePolicy::AutoPatch => "auto-patch",
        UpdatePolicy::AutoMinor => "auto-minor",
        UpdatePolicy::Auto => "auto",
    }
}

fn policy_from(arg: PolicyArg) -> UpdatePolicy {
    match arg {
        PolicyArg::Manual => UpdatePolicy::Manual,
        PolicyArg::Notify => UpdatePolicy::Notify,
        PolicyArg::AutoPatch => UpdatePolicy::AutoPatch,
        PolicyArg::AutoMinor => UpdatePolicy::AutoMinor,
        PolicyArg::Auto => UpdatePolicy::Auto,
    }
}

/// `php@8.3` after checking its syntax (exit 2 when it is invalid).
fn spec(text: &str) -> CliResult<ToolSpec> {
    parse::tool_spec(text)?;
    Ok(ToolSpec::from(text))
}

pub async fn search(ctx: &Context, args: SearchArgs) -> CliResult<()> {
    let found = ctx
        .call::<forge_ipc::api::methods::tools_search>(ToolSearchParams {
            text: args.text,
            category: args.category,
        })
        .await?;
    ctx.out.emit(&found, |out| {
        let rows = found
            .entries
            .iter()
            .map(|e| {
                let mut versions = e
                    .versions
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                if e.versions.len() > 3 {
                    versions.push_str(", ...");
                }
                vec![
                    e.id.to_string(),
                    or_dash(e.category.as_deref()),
                    versions,
                    if e.installed {
                        paint(GREEN, out.t(Msg::Yes))
                    } else {
                        out.t(Msg::No).to_owned()
                    },
                    e.description.clone().unwrap_or_default(),
                ]
            })
            .collect();
        out.table(
            &[
                "ID",
                out.t(Msg::HCategory),
                out.t(Msg::HVersions),
                out.t(Msg::HInstalled),
                out.t(Msg::HDescription),
            ],
            rows,
            Msg::NoResults,
        );
    });
    Ok(())
}

pub async fn run(ctx: &Context, command: ToolCmd) -> CliResult<()> {
    match command {
        ToolCmd::List { installed, updates } => list(ctx, installed, updates).await,
        ToolCmd::Info { tool } => info(ctx, tool).await,
        ToolCmd::Install { specs, variant } => install(ctx, specs, variant).await,
        ToolCmd::Remove {
            spec: text,
            keep_config,
        } => {
            let spec = spec(&text)?;
            ctx.confirm(&ctx.out.tf(Msg::ConfirmRemoveTool, &[&spec]))?;
            let ack: Ack = ctx
                .job::<tools_remove, Ack>(ToolRemoveParams {
                    spec: spec.clone(),
                    keep_config,
                    confirm: true,
                })
                .await?;
            ctx.out
                .emit(&ack, |out| out.success(out.tf(Msg::ToolRemoved, &[&spec])));
            Ok(())
        }
        ToolCmd::Update { tool, all, check } => update(ctx, tool, all, check).await,
        ToolCmd::Rollback { spec: text } => {
            let spec = spec(&text)?;
            let version: ToolVersionDto = ctx
                .job::<tools_rollback, ToolVersionDto>(ToolRollbackParams { spec })
                .await?;
            ctx.out.emit(&version, |out| {
                out.success(out.tf(Msg::ToolRolledBack, &[&version.tool, &version.version]));
            });
            Ok(())
        }
        ToolCmd::Policy { tool, policy } => {
            let dto: ToolDto = ctx
                .call::<forge_ipc::api::methods::tools_policy>(ToolPolicyParams {
                    tool,
                    policy: policy_from(policy),
                })
                .await?;
            ctx.out.emit(&dto, |out| {
                out.success(out.tf(Msg::ToolPolicySet, &[&dto.id, &policy_name(dto.policy)]));
            });
            Ok(())
        }
        ToolCmd::AddLocal { source } => {
            let source = parse::absolute(&source)?;
            let version: ToolVersionDto = ctx
                .job::<tools_add_local, ToolVersionDto>(ToolAddLocalParams { source })
                .await?;
            ctx.out.emit(&version, |out| {
                out.success(out.tf(Msg::ToolAdded, &[&version.tool, &version.version]));
            });
            Ok(())
        }
        ToolCmd::Discover => {
            let found: ToolDiscoverDto = ctx
                .job::<tools_discover, ToolDiscoverDto>(forge_ipc::api::NoParams {})
                .await?;
            ctx.out.emit(&found, |out| {
                for v in &found.registered {
                    out.success(out.tf(Msg::ToolAdded, &[&v.tool, &v.version]));
                }
                for path in &found.unknown {
                    out.warn(out.tf(Msg::ToolUnknownFolder, &[path]));
                }
                if found.registered.is_empty() && found.unknown.is_empty() {
                    out.line(out.t(Msg::NothingToDiscover));
                }
            });
            Ok(())
        }
    }
}

async fn list(ctx: &Context, installed: bool, updates: bool) -> CliResult<()> {
    let list = ctx
        .call::<forge_ipc::api::methods::tools_list>(ToolListParams { installed, updates })
        .await?;
    ctx.out.emit(&list, |out| {
        let rows = list
            .tools
            .iter()
            .map(|t| {
                vec![
                    t.id.to_string(),
                    or_dash(t.category.as_deref()),
                    if t.installed.is_empty() {
                        "-".into()
                    } else {
                        t.installed
                            .iter()
                            .map(|v| v.version.clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                    or_dash(t.current.as_deref()),
                    policy_name(t.policy).to_owned(),
                    t.update_available
                        .as_ref()
                        .map_or_else(|| "-".into(), |v| paint(YELLOW, format!("-> {v}"))),
                ]
            })
            .collect();
        out.table(
            &[
                out.t(Msg::HTool),
                out.t(Msg::HCategory),
                out.t(Msg::HInstalled),
                out.t(Msg::HCurrent),
                out.t(Msg::HPolicy),
                out.t(Msg::HUpdate),
            ],
            rows,
            Msg::NoTools,
        );
    });
    Ok(())
}

async fn info(ctx: &Context, tool: forge_ipc::base::ids::ToolId) -> CliResult<()> {
    let info = ctx
        .call::<forge_ipc::api::methods::tools_info>(ToolParams { tool })
        .await?;
    ctx.out.emit(&info, |out| {
        let t = &info.tool;
        out.pairs(&[
            (out.t(Msg::HTool), t.name.clone()),
            ("ID", t.id.to_string()),
            (out.t(Msg::HCategory), or_dash(t.category.as_deref())),
            (out.t(Msg::HPolicy), policy_name(t.policy).to_owned()),
            (out.t(Msg::HCurrent), or_dash(t.current.as_deref())),
            (out.t(Msg::HLatest), or_dash(t.latest.as_deref())),
        ]);
        if let Some(description) = &t.description {
            out.line(paint(DIM, description));
        }
        out.line("");
        out.line(out.t(Msg::HInstalled));
        let rows = t
            .installed
            .iter()
            .map(|v| {
                vec![
                    v.canonical_name.clone(),
                    if v.is_current {
                        paint(GREEN, "current")
                    } else {
                        String::new()
                    },
                    or_dash(v.size_bytes.map(crate::output::format_bytes)),
                ]
            })
            .collect();
        out.table(
            &[out.t(Msg::HVersion), "", out.t(Msg::HSize)],
            rows,
            Msg::NoVersionsInstalled,
        );
        if !info.available.is_empty() {
            out.line("");
            out.line(out.t(Msg::HAvailable));
            let rows = info
                .available
                .iter()
                .take(15)
                .map(|a| {
                    vec![
                        a.version.clone(),
                        a.variants.join(", "),
                        or_dash(a.channel.as_deref()),
                    ]
                })
                .collect();
            out.table(
                &[out.t(Msg::HVersion), out.t(Msg::HVariants), "Canal"],
                rows,
                Msg::NoResults,
            );
        }
        if !info.used_by.is_empty() {
            out.line("");
            out.line(out.t(Msg::HUsedBy));
            for usage in &info.used_by {
                out.line(format!("  {} ({})", usage.host, usage.version));
            }
        }
    });
    Ok(())
}

async fn install(ctx: &Context, specs: Vec<String>, variant: Option<String>) -> CliResult<()> {
    let specs = specs
        .iter()
        .map(|s| spec(s))
        .collect::<CliResult<Vec<_>>>()?;
    let result: ToolInstallResultDto = ctx
        .job::<tools_install, ToolInstallResultDto>(ToolInstallParams {
            specs,
            variant,
            yes: ctx.global.yes,
        })
        .await?;
    ctx.out.emit(&result, |out| {
        for v in &result.installed {
            out.success(out.tf(Msg::ToolInstalled, &[&v.tool, &v.canonical_name]));
        }
    });
    Ok(())
}

async fn update(
    ctx: &Context,
    tool: Option<forge_ipc::base::ids::ToolId>,
    all: bool,
    check: bool,
) -> CliResult<()> {
    if tool.is_none() && !all && !check {
        return Err(CliError::usage(ctx.out.t(Msg::UpdateNeedsTarget)));
    }
    let params = ToolUpdateParams { tool };
    let updates: ToolUpdatesDto = if check {
        ctx.job::<tools_check_updates, ToolUpdatesDto>(params)
            .await?
    } else {
        ctx.job::<tools_update, ToolUpdatesDto>(params).await?
    };
    ctx.out.emit(&updates, |out| {
        let rows = updates
            .updates
            .iter()
            .map(|u| {
                vec![
                    u.tool.to_string(),
                    u.from.clone(),
                    u.to.clone(),
                    out.t(if u.automatic { Msg::Yes } else { Msg::No })
                        .to_owned(),
                ]
            })
            .collect();
        out.table(
            &[
                out.t(Msg::HTool),
                out.t(Msg::HFrom),
                out.t(Msg::HTo),
                out.t(Msg::HAutomatic),
            ],
            rows,
            Msg::AllUpToDate,
        );
    });
    Ok(())
}

pub async fn use_version(ctx: &Context, args: UseArgs) -> CliResult<()> {
    let version = ctx
        .call::<forge_ipc::api::methods::tools_use>(ToolUseParams {
            tool: args.tool,
            version: args.version,
        })
        .await?;
    ctx.out.emit(&version, |out| {
        out.success(out.tf(Msg::ToolNowUses, &[&version.tool, &version.version]));
    });
    Ok(())
}

pub async fn which(ctx: &Context, args: WhichArgs) -> CliResult<()> {
    let project_dir = ctx.cwd().ok();
    let which = ctx
        .call::<forge_ipc::api::methods::tools_which>(ToolWhichParams {
            tool: args.tool,
            project_dir,
        })
        .await?;
    ctx.out.emit(&which, |out| {
        let show = |v: &Option<ToolVersionDto>| {
            v.as_ref().map_or_else(|| "-".into(), render::version_line)
        };
        out.pairs(&[
            (out.t(Msg::HGlobal), show(&which.global)),
            (out.t(Msg::HProject), show(&which.project)),
            (
                out.t(Msg::HProjectFile),
                or_dash(which.project_source.as_deref()),
            ),
            (
                out.t(Msg::HExecutable),
                or_dash(which.executable.as_deref()),
            ),
        ]);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_names_match_the_wire_names() {
        for (policy, name) in [
            (UpdatePolicy::Manual, "manual"),
            (UpdatePolicy::Notify, "notify"),
            (UpdatePolicy::AutoPatch, "auto-patch"),
            (UpdatePolicy::AutoMinor, "auto-minor"),
            (UpdatePolicy::Auto, "auto"),
        ] {
            assert_eq!(policy_name(policy), name);
            assert_eq!(serde_json::to_value(policy).unwrap(), name);
        }
    }

    #[test]
    fn specs_are_checked_before_calling() {
        assert!(spec("php@8.3").is_ok());
        assert_eq!(spec("PHP@8.3").unwrap_err().exit, 2);
    }
}
