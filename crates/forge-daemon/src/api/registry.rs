//! `registry.*`: manifest registries (docs/05 §7). Third-party registries are added
//! unverified-but-signed only (no `trusted_unsigned` from the API).

use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_ipc::api::methods::{registry_add, registry_list, registry_remove, registry_update};
use forge_ipc::api::{
    Ack, JobDto, NoParams, RegistryAddParams, RegistryDto, RegistryListDto, RegistryRemoveParams,
};
use forge_ipc::server::{Ctx, Router};
use forge_registry::{RegistryConfig, RegistrySource};

use super::convert;
use super::route;
use crate::boot::OFFICIAL_REGISTRY;
use crate::daemon::Daemon;

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, registry_list, list);
    route!(router, daemon, registry_add, add);
    route!(router, daemon, registry_remove, remove);
    route!(router, daemon, registry_update, update);
}

fn snapshot(daemon: &Daemon) -> RegistryListDto {
    RegistryListDto {
        registries: daemon
            .registry
            .list()
            .iter()
            .map(convert::registry)
            .collect(),
    }
}

async fn list(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<RegistryListDto> {
    Ok(snapshot(&daemon))
}

/// A registry name from its source (`https://x.dev/forgebox/index.yaml` → `x-dev`).
pub fn name_from(source: &RegistrySource) -> String {
    let location = source.location();
    let base = url::Url::parse(&location)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| {
            std::path::Path::new(&location)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "local".into())
        });
    let mut name: String = base
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    name = name.trim_matches('-').to_owned();
    if name.is_empty() {
        "local".into()
    } else {
        name
    }
}

async fn add(daemon: Daemon, params: RegistryAddParams, _ctx: Ctx) -> Result<RegistryDto> {
    let source = RegistrySource::parse(&params.url)?;
    let name = params.name.clone().unwrap_or_else(|| name_from(&source));
    if name == OFFICIAL_REGISTRY {
        return Err(ForgeError::Conflict(format!(
            "{name} is reserved for the official registry"
        )));
    }
    daemon
        .registry
        .add(RegistryConfig::new(name.clone(), source))
        .await?;
    let _ = daemon.registry.refresh(&name);
    daemon
        .registry
        .list()
        .iter()
        .find(|r| r.name == name)
        .map(convert::registry)
        .ok_or_else(|| ForgeError::Internal(format!("registry {name} vanished")))
}

async fn remove(daemon: Daemon, params: RegistryRemoveParams, _ctx: Ctx) -> Result<Ack> {
    if params.name == OFFICIAL_REGISTRY {
        return Err(ForgeError::Conflict(
            "the official registry cannot be removed".into(),
        ));
    }
    daemon.registry.remove(&params.name).await?;
    Ok(Ack::ok())
}

async fn update(daemon: Daemon, _params: NoParams, ctx: Ctx) -> Result<JobDto> {
    let d = daemon.clone();
    daemon.job(&ctx, "registry.update", move |_job| async move {
        let mut errors = Vec::new();
        for info in d.registry.list().into_iter().filter(|r| r.enabled) {
            let id = match d.registry.refresh(&info.name) {
                Ok(id) => id,
                Err(err) => {
                    errors.push(format!("{}: {err}", info.name));
                    continue;
                }
            };
            if let Some(handle) = d.jobs.handle(id) {
                match handle.wait(Duration::from_secs(600)).await {
                    Ok(job) if job.state == forge_core::jobs::JobState::Done => {}
                    Ok(job) => errors.push(format!(
                        "{}: {}",
                        info.name,
                        job.error
                            .map(|e| e.message)
                            .unwrap_or_else(|| "failed".into())
                    )),
                    Err(err) => errors.push(format!("{}: {err}", info.name)),
                }
            }
        }
        if !errors.is_empty() && d.registry.list().iter().all(|r| r.tools == 0) {
            return Err(ForgeError::Conflict(errors.join("; ")));
        }
        Ok(snapshot(&d))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_the_host_or_folder() {
        let http = RegistrySource::parse("https://Reg.Example.dev/forgebox/index.yaml").unwrap();
        assert_eq!(name_from(&http), "reg-example-dev");
        let dir = tempfile::tempdir().unwrap();
        let local = RegistrySource::Dir(dir.path().join("My Registry"));
        assert_eq!(name_from(&local), "my-registry");
    }
}
