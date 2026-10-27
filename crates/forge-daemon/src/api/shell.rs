//! `shell.*`: the user `PATH` entries of Forgebox (`forge path show|add|remove`) and the
//! environment of a project (`forge env`, `forge exec`). `shell.open` is later work.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::platform::normalize_path_entry;
use forge_ipc::api::methods::{shell_env, shell_path_add, shell_path_remove, shell_path_show};
use forge_ipc::api::{NoParams, ShellEnvDto, ShellEnvParams, ShellFormat, UserPathDto};
use forge_ipc::server::{Ctx, Router};
use forge_manifest::schema::tool::ToolManifest;

use super::route;
use crate::catalog;
use crate::daemon::Daemon;

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, shell_path_show, path_show);
    route!(router, daemon, shell_path_add, path_add);
    route!(router, daemon, shell_path_remove, path_remove);
    route!(router, daemon, shell_env, env);
}

/// Manifests of the installed tools.
async fn installed_manifests(daemon: &Daemon) -> Result<Vec<Arc<ToolManifest>>> {
    let installed: std::collections::BTreeSet<_> = catalog::all_installed(&daemon.reader)
        .await?
        .into_iter()
        .map(|r| r.tool_id)
        .collect();
    Ok(catalog::tools(&daemon.registry)
        .into_iter()
        .filter(|t| installed.contains(&t.manifest.metadata.id))
        .map(|t| t.manifest)
        .collect())
}

async fn snapshot(daemon: &Daemon) -> Result<UserPathDto> {
    let manifests = installed_manifests(daemon).await?;
    let config = daemon.config();
    let refs: Vec<&ToolManifest> = manifests.iter().map(AsRef::as_ref).collect();
    let wanted = forge_provider::layout::path::wanted_entries(&daemon.root, &refs, &config.path);
    let env = Arc::clone(daemon.platform.user_env());
    let entries = forge_core::runtime::spawn_blocking_timeout(
        "reading the user PATH",
        Duration::from_secs(5),
        move || env.path_entries(),
    )
    .await?;
    let bin = normalize_path_entry(&daemon.root.bin().to_string_lossy(), &|_| None);
    let forgebox_entries: Vec<String> = entries
        .iter()
        .filter(|e| normalize_path_entry(e, &|_| None).starts_with(&bin))
        .cloned()
        .collect();
    let present: Vec<String> = entries
        .iter()
        .map(|e| normalize_path_entry(e, &|_| None))
        .collect();
    let missing = wanted
        .into_iter()
        .filter(|w| !present.contains(&normalize_path_entry(w, &|_| None)))
        .collect();
    Ok(UserPathDto {
        entries,
        forgebox_entries,
        missing,
    })
}

async fn path_show(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<UserPathDto> {
    snapshot(&daemon).await
}

async fn path_add(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<UserPathDto> {
    if daemon.root.is_portable() {
        return Err(ForgeError::Conflict(
            "portable mode never changes the user PATH (docs/08 §2.2)".into(),
        ));
    }
    let manifests = installed_manifests(&daemon).await?;
    daemon
        .layout
        .ensure_path(manifests, daemon.config().path.clone())
        .await?;
    snapshot(&daemon).await
}

async fn path_remove(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<UserPathDto> {
    daemon.layout.remove_path_entries().await?;
    snapshot(&daemon).await
}

fn quote_ps(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// `PATH` entries for a project: each installed tool with a `current` link, at the
/// version the project selects (`install.version_files` up from `dir`), an override, or
/// `current`.
async fn env(daemon: Daemon, params: ShellEnvParams, _ctx: Ctx) -> Result<ShellEnvDto> {
    let manifests = installed_manifests(&daemon).await?;
    let mut path_prepend = Vec::new();
    let vars = BTreeMap::new();
    for manifest in manifests {
        let install = &manifest.spec.install;
        if !install.current_link {
            continue;
        }
        let id = manifest.metadata.id.clone();
        let rows = catalog::installed(&daemon.reader, &id, Some(&manifest)).await?;
        let mut wanted = params.versions.get(id.as_str()).cloned();
        if wanted.is_none() {
            if let Some(dir) = params.dir.clone() {
                let files = install.version_files.clone();
                wanted = tokio::task::spawn_blocking(move || {
                    let mut current = Some(PathBuf::from(dir));
                    while let Some(d) = current {
                        for name in &files {
                            if let Ok(text) = std::fs::read_to_string(d.join(name)) {
                                let v = text.trim().trim_start_matches('v').to_owned();
                                if !v.is_empty() {
                                    return Some(v);
                                }
                            }
                        }
                        current = d.parent().map(PathBuf::from);
                    }
                    None
                })
                .await
                .ok()
                .flatten();
            }
        }
        let base = match wanted
            .as_deref()
            .and_then(|w| catalog::pick(&manifest, &rows, w))
        {
            Some(row) => PathBuf::from(&row.path),
            None => daemon.root.tool_current_link(&id),
        };
        for sub in &install.path {
            let sub = sub.trim_matches('/');
            let dir = if sub.is_empty() || sub == "." {
                base.clone()
            } else {
                base.join(sub.replace('/', "\\"))
            };
            path_prepend.push(dir.to_string_lossy().replace('/', "\\"));
        }
    }
    let script = match params.format {
        ShellFormat::Json => None,
        ShellFormat::Powershell => Some(format!(
            "$env:PATH = {} + ';' + $env:PATH",
            quote_ps(&path_prepend.join(";"))
        )),
        ShellFormat::Cmd => Some(format!("set \"PATH={};%PATH%\"", path_prepend.join(";"))),
        ShellFormat::Bash => Some(format!(
            "export PATH=\"{}:$PATH\"",
            path_prepend
                .iter()
                .map(|p| p.replace('\\', "/"))
                .collect::<Vec<_>>()
                .join(":")
        )),
    };
    Ok(ShellEnvDto {
        path_prepend,
        vars,
        script,
    })
}
