//! `php.*`: installed versions, pools, extensions and `php.ini` files. "PHP" is the tool
//! whose manifest declares `service.kind: pool` (no tool id is hardcoded).

use std::collections::BTreeSet;
use std::path::PathBuf;

use forge_core::error::{ForgeError, Result};
use forge_core::store::ToolVersionRow;
use forge_ipc::api::methods::{php_ext_list, php_ini, php_ini_open, php_list, php_pool_status};
use forge_ipc::api::{
    FileRefDto, NoParams, PhpExtDto, PhpExtListDto, PhpIniDto, PhpPoolStatusParams, PhpVersionDto,
    PhpVersionListDto, PhpVersionParams, PoolListDto,
};
use forge_ipc::server::{Ctx, Router};

use super::convert;
use super::route;
use crate::catalog::{self, ToolEntry};
use crate::daemon::Daemon;

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, php_list, list);
    route!(router, daemon, php_pool_status, pool_status);
    route!(router, daemon, php_ext_list, ext_list);
    route!(router, daemon, php_ini, ini);
    route!(router, daemon, php_ini_open, ini);
}

fn pool_tool(daemon: &Daemon) -> Result<ToolEntry> {
    catalog::pool_tools(&daemon.registry)
        .into_iter()
        .next()
        .ok_or_else(|| ForgeError::NotFound("no FastCGI runtime (PHP) in the catalog".into()))
}

async fn rows(daemon: &Daemon, tool: &ToolEntry) -> Result<Vec<ToolVersionRow>> {
    catalog::installed(
        &daemon.reader,
        &tool.manifest.metadata.id,
        Some(&tool.manifest),
    )
    .await
}

async fn pick(daemon: &Daemon, version: &str) -> Result<(ToolEntry, ToolVersionRow)> {
    let tool = pool_tool(daemon)?;
    let rows = rows(daemon, &tool).await?;
    let row = catalog::pick(&tool.manifest, &rows, version)
        .cloned()
        .ok_or_else(|| ForgeError::NotFound(format!("PHP {version} is not installed")))?;
    Ok((tool, row))
}

async fn list(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<PhpVersionListDto> {
    let Ok(tool) = pool_tool(&daemon) else {
        return Ok(PhpVersionListDto {
            versions: Vec::new(),
        });
    };
    let rows = rows(&daemon, &tool).await?;
    let current = daemon
        .layout
        .current_version(&tool.manifest.metadata.id)
        .await
        .ok()
        .flatten()
        .map(|c| c.canonical_name);
    let sites = daemon.sites.list().await?;
    let plan = daemon.web.php_plan(&sites).await.unwrap_or_default();
    let statuses = daemon.pools.statuses().await;
    let versions = rows
        .iter()
        .map(|row| {
            let count = plan.values().filter(|p| p.version.id == row.id).count();
            let count = u32::try_from(count).unwrap_or(u32::MAX);
            PhpVersionDto {
                version_id: row.id,
                version: row.version.clone(),
                series: catalog::series_of(&tool.manifest, &row.version),
                thread_safety: row.variant.get("ts").cloned().unwrap_or_default(),
                compiler: row.variant.get("compiler").cloned(),
                arch: row
                    .variant
                    .get("arch")
                    .cloned()
                    .unwrap_or_else(|| "x64".into()),
                is_current_cli: current.as_deref() == Some(row.canonical_name.as_str()),
                pool: statuses
                    .iter()
                    .find(|s| s.version_id == row.id)
                    .map(|s| convert::pool(s, count)),
                sites: count,
                xdebug: convert::xdebug_mode(false),
            }
        })
        .collect();
    Ok(PhpVersionListDto { versions })
}

async fn pool_status(
    daemon: Daemon,
    params: PhpPoolStatusParams,
    _ctx: Ctx,
) -> Result<PoolListDto> {
    let sites = daemon.sites.list().await?;
    let plan = daemon.web.php_plan(&sites).await.unwrap_or_default();
    let filter = match &params.version {
        Some(v) => Some(pick(&daemon, v).await?.1.id),
        None => None,
    };
    let pools = daemon
        .pools
        .statuses()
        .await
        .iter()
        .filter(|s| filter.is_none_or(|id| id == s.version_id))
        .map(|s| {
            let count = plan
                .values()
                .filter(|p| p.version.id == s.version_id)
                .count();
            convert::pool(s, u32::try_from(count).unwrap_or(u32::MAX))
        })
        .collect();
    Ok(PoolListDto { pools })
}

async fn ext_list(daemon: Daemon, params: PhpVersionParams, _ctx: Ctx) -> Result<PhpExtListDto> {
    let (tool, row) = pick(&daemon, &params.version).await?;
    let Some(config) = tool.manifest.spec.config.extensions.clone() else {
        return Ok(PhpExtListDto {
            version: row.version,
            extensions: Vec::new(),
        });
    };
    let dir = PathBuf::from(&row.path).join("ext");
    let pattern = config.pattern.clone();
    let names = tokio::task::spawn_blocking(move || {
        let (prefix, suffix) = pattern.split_once("{name}").unwrap_or(("php_", ".dll"));
        let mut names = BTreeSet::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let file = entry.file_name().to_string_lossy().into_owned();
                if let Some(name) = file
                    .strip_prefix(prefix)
                    .and_then(|r| r.strip_suffix(suffix))
                {
                    names.insert(name.to_owned());
                }
            }
        }
        names
    })
    .await
    .unwrap_or_default();
    let enabled: BTreeSet<String> = config.defaults_on.iter().cloned().collect();
    let scheme = tool.manifest.spec.versioning.scheme;
    let zend: BTreeSet<String> = forge_manifest::version::Version::parse(scheme, &row.version)
        .map(|v| config.zend_extensions_for(scheme, &v).into_iter().collect())
        .unwrap_or_default();
    let extensions = names
        .into_iter()
        .map(|name| PhpExtDto {
            enabled: enabled.contains(&name) || zend.contains(&name),
            zend: zend.contains(&name),
            bundled: true,
            name,
        })
        .collect();
    Ok(PhpExtListDto {
        version: row.version,
        extensions,
    })
}

/// `php.ini` (the user's), then the managed `conf.d/*.ini` overlays.
async fn ini(daemon: Daemon, params: PhpVersionParams, _ctx: Ctx) -> Result<PhpIniDto> {
    let (tool, row) = pick(&daemon, &params.version).await?;
    let series = catalog::series_of(&tool.manifest, &row.version);
    let dir = daemon
        .root
        .etc()
        .join(tool.manifest.metadata.id.as_str())
        .join(&series);
    let root = daemon.root.path().to_path_buf();
    let files = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let rel = |p: &std::path::Path| {
            p.strip_prefix(&root)
                .ok()
                .map(forge_core::paths::posix_string)
        };
        let ini = dir.join("php.ini");
        out.push(FileRefDto {
            role: "php.ini".into(),
            path: forge_core::paths::posix_string(&ini),
            fs_path: rel(&ini),
            managed: false,
        });
        if let Ok(entries) = std::fs::read_dir(dir.join("conf.d")) {
            let mut overlays: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "ini"))
                .collect();
            overlays.sort();
            for path in overlays {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.push(FileRefDto {
                    role: format!("conf.d/{name}"),
                    path: forge_core::paths::posix_string(&path),
                    fs_path: rel(&path),
                    managed: true,
                });
            }
        }
        out
    })
    .await
    .unwrap_or_default();
    Ok(PhpIniDto {
        version: row.version,
        files,
    })
}
