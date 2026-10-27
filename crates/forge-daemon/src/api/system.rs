//! `system.*` (`system.hello`/`system.ping` are server built-ins): preferences
//! (`forgebox.toml` by dotted key) and the Laragon import. Bundles, backups and
//! self-update are later tasks (see `not_implemented`).

use std::path::PathBuf;
use std::sync::Arc;

use forge_core::config::ForgeConfig;
use forge_core::error::{ForgeError, Result};
use forge_ipc::api::methods::{system_config_get, system_config_set, system_import_laragon};
use forge_ipc::api::{
    ConfigGetParams, ConfigSetParams, ConfigValueDto, ImportItemDto, ImportLaragonParams,
    ImportReportDto, JobDto,
};
use forge_ipc::server::{Ctx, Router};
use forge_sites::import::{ApplyEnv, CaseConvertFn};
use forge_sites::{ImportOptions, LaragonImporter};
use futures::FutureExt;
use serde_json::Value;

use super::route;
use super::sites::{reconcile_parked, reconcile_quietly, save_config};
use crate::catalog;
use crate::daemon::Daemon;

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, system_config_get, get);
    route!(router, daemon, system_config_set, set);
    route!(router, daemon, system_import_laragon, import_laragon);
}

/// `system.import_laragon`: scans the Laragon folder (read-only), registers what the
/// plan holds and wires the result into the running daemon (preferences saved, parked
/// folders reconciled, imported database instances adopted, web servers re-rendered).
/// With `dry_run` it stops after deciding.
async fn import_laragon(daemon: Daemon, params: ImportLaragonParams, ctx: Ctx) -> Result<JobDto> {
    let path = PathBuf::from(&params.path);
    if !path.is_absolute() {
        return Err(ForgeError::InvalidInput(format!(
            "{} must be an absolute folder on the daemon's machine",
            params.path
        )));
    }
    let d = daemon.clone();
    daemon.job(&ctx, "system.import_laragon", move |job| async move {
        let manifests: Vec<_> = catalog::tools(&d.registry)
            .into_iter()
            .map(|t| (*t.manifest).clone())
            .collect();
        let importer = LaragonImporter::new(manifests);
        let plan = importer.scan(&path).await?;

        let case = d.case.clone();
        let case_job = job.clone();
        let convert: CaseConvertFn = Arc::new(move |dir| {
            let (case, job) = (case.clone(), case_job.clone());
            async move { case.run(&job, dir, true, None).await.map(|_| ()) }.boxed()
        });
        let env = ApplyEnv {
            root: d.root.clone(),
            writer: d.writer.clone(),
            reader: d.reader.clone(),
            sites: d.sites.clone(),
            config: (*d.config()).clone(),
            convert_case: Some(convert),
        };
        let options = ImportOptions {
            dry_run: params.dry_run,
            copy_tools: params.copy_tools,
            move_data: params.move_data,
            convert_case_sensitive: params.convert_case_sensitive,
        };
        let outcome = LaragonImporter::apply(&plan, &options, &env).await?;

        if !params.dry_run {
            if outcome.config_changed {
                save_config(&d, outcome.config.clone()).await?;
                reconcile_parked(&d).await;
            }
            if outcome.instances > 0 {
                d.services.adopt_stored().await?;
            }
            if outcome.tool_versions > 0 {
                if let Err(err) = d.services.ensure_defaults().await {
                    tracing::warn!(%err, "default instances after the Laragon import");
                }
            }
            if outcome.tool_versions + outcome.sites > 0 || outcome.config_changed {
                reconcile_quietly(&d, "laragon import").await;
            }
        }
        Ok(ImportReportDto {
            dry_run: outcome.report.dry_run,
            items: outcome
                .report
                .items
                .into_iter()
                .map(|i| ImportItemDto {
                    kind: i.kind.to_owned(),
                    name: i.name,
                    imported: i.imported,
                    note: i.note,
                })
                .collect(),
        })
    })
}

/// `prefs.default_server` and `default_server` name the same key (the CLI documents
/// the `prefs.` prefix).
fn pointer(key: &str) -> Result<String> {
    let key = key.trim().strip_prefix("prefs.").unwrap_or(key.trim());
    if key.is_empty() || key.split('.').any(str::is_empty) {
        return Err(ForgeError::InvalidInput(format!(
            "invalid preference key {key:?}"
        )));
    }
    Ok(format!("/{}", key.replace('.', "/")))
}

fn value_of(config: &ForgeConfig, key: &str) -> Result<Value> {
    let json = serde_json::to_value(config)
        .map_err(|e| ForgeError::Internal(format!("preferences: {e}")))?;
    json.pointer(&pointer(key)?)
        .cloned()
        .ok_or_else(|| ForgeError::NotFound(format!("preference {key}")))
}

/// Sets `key` to `value` in `config` (validated as a whole).
pub fn with_value(config: &ForgeConfig, key: &str, value: Value) -> Result<ForgeConfig> {
    let mut json = serde_json::to_value(config)
        .map_err(|e| ForgeError::Internal(format!("preferences: {e}")))?;
    let target = json
        .pointer_mut(&pointer(key)?)
        .ok_or_else(|| ForgeError::NotFound(format!("preference {key}")))?;
    *target = value;
    let updated: ForgeConfig = serde_json::from_value(json)
        .map_err(|e| ForgeError::InvalidInput(format!("{key}: {e}")))?;
    updated.validate()?;
    Ok(updated)
}

async fn get(daemon: Daemon, params: ConfigGetParams, _ctx: Ctx) -> Result<ConfigValueDto> {
    let config = daemon.config();
    let value = value_of(&config, &params.key)?;
    let default = value_of(&ForgeConfig::default(), &params.key).ok();
    Ok(ConfigValueDto {
        key: params.key,
        is_default: default.as_ref() == Some(&value),
        value,
    })
}

async fn set(daemon: Daemon, params: ConfigSetParams, _ctx: Ctx) -> Result<ConfigValueDto> {
    let updated = with_value(&daemon.config(), &params.key, params.value)?;
    let path = daemon.root.forgebox_toml();
    let saved = updated.clone();
    forge_core::runtime::spawn_blocking_timeout(
        "saving forgebox.toml",
        std::time::Duration::from_secs(5),
        move || saved.save(&path),
    )
    .await?;
    daemon.set_config(updated.clone());
    let value = value_of(&updated, &params.key)?;
    let default = value_of(&ForgeConfig::default(), &params.key).ok();
    Ok(ConfigValueDto {
        key: params.key,
        is_default: default.as_ref() == Some(&value),
        value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_keys_read_and_write_the_toml_model() {
        let config = ForgeConfig::default();
        assert_eq!(value_of(&config, "prefs.tld").unwrap(), ".test");
        assert_eq!(value_of(&config, "dns.enabled").unwrap(), true);
        let updated = with_value(&config, "dns.enabled", Value::Bool(false)).unwrap();
        assert!(!updated.dns.enabled);
        assert_eq!(
            with_value(&config, "tld", Value::String(".COM x".into()))
                .unwrap_err()
                .code(),
            "config_invalid"
        );
        assert_eq!(value_of(&config, "nope").unwrap_err().code(), "not_found");
        assert_eq!(pointer("a..b").unwrap_err().code(), "invalid_input");
    }
}
