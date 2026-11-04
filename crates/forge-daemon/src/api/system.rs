//! `system.*` (`system.hello`/`system.ping` are server built-ins): preferences
//! (`forgebox.toml` by dotted key) and the Laragon import. Bundles, backups and
//! self-update are later tasks (see `not_implemented`).

use std::path::PathBuf;
use std::sync::Arc;

use forge_core::config::ForgeConfig;
use forge_core::error::{ForgeError, Result};
use forge_ipc::api::methods::{
    system_config_get, system_config_list, system_config_set, system_import_laragon,
};
use forge_ipc::api::{
    ConfigDocumentDto, ConfigGetParams, ConfigSetParams, ConfigValueDto, ImportItemDto,
    ImportLaragonParams, ImportReportDto, JobDto, NoParams,
};
use forge_ipc::server::{Ctx, Router};
use forge_sites::import::{ApplyEnv, CaseConvertFn};
use forge_sites::{ImportOptions, LaragonImporter};
use futures::FutureExt;
use serde_json::Value;

use super::route;
use super::sites::reconcile_quietly;
use crate::catalog;
use crate::daemon::Daemon;

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, system_config_get, get);
    route!(router, daemon, system_config_list, list);
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
                // The owner saves, reconciles the parked folders and publishes the change.
                crate::config_owner::replace(&d, outcome.config.clone()).await?;
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

/// Whether `pointer` names a property of the preferences' JSON Schema (`$ref`s followed):
/// an optional key that is unset (`metrics.enabled`, `editor.command`) is absent from the
/// serialized preferences but still a key.
fn schema_has(pointer: &str) -> bool {
    let schema = ForgeConfig::json_schema();
    let mut node = &schema;
    for part in pointer.trim_start_matches('/').split('/') {
        // Follow `{"$ref": "#/$defs/X"}` (possibly wrapped in `allOf`/`anyOf`).
        for _ in 0..4 {
            let reference = node
                .get("$ref")
                .or_else(|| {
                    ["allOf", "anyOf", "oneOf"]
                        .iter()
                        .find_map(|k| node.get(*k)?.as_array()?.iter().find_map(|v| v.get("$ref")))
                })
                .and_then(Value::as_str);
            match reference.and_then(|r| r.strip_prefix('#')) {
                Some(target) => match schema.pointer(target) {
                    Some(found) => node = found,
                    None => return false,
                },
                None => break,
            }
        }
        match node.get("properties").and_then(|p| p.get(part)) {
            Some(child) => node = child,
            None => return false,
        }
    }
    true
}

fn value_of(config: &ForgeConfig, key: &str) -> Result<Value> {
    let json = serde_json::to_value(config)
        .map_err(|e| ForgeError::Internal(format!("preferences: {e}")))?;
    let path = pointer(key)?;
    match json.pointer(&path) {
        Some(value) => Ok(value.clone()),
        // An optional key that is unset.
        None if schema_has(&path) => Ok(Value::Null),
        None => Err(ForgeError::NotFound(format!("preference {key}"))),
    }
}

/// Sets `key` to `value` in `config` (validated as a whole). An optional key that is unset
/// (absent from the serialized preferences, like `metrics.enabled`) can be set too, as long
/// as the schema has it.
pub fn with_value(config: &ForgeConfig, key: &str, value: Value) -> Result<ForgeConfig> {
    let not_found = || ForgeError::NotFound(format!("preference {key}"));
    let mut json = serde_json::to_value(config)
        .map_err(|e| ForgeError::Internal(format!("preferences: {e}")))?;
    let path = pointer(key)?;
    match json.pointer_mut(&path) {
        Some(target) => *target = value,
        None => {
            if !schema_has(&path) {
                return Err(not_found());
            }
            let (parent, leaf) = path.rsplit_once('/').ok_or_else(not_found)?;
            let parent = if parent.is_empty() {
                Some(&mut json)
            } else {
                json.pointer_mut(parent)
            };
            parent
                .and_then(Value::as_object_mut)
                .ok_or_else(not_found)?
                .insert(leaf.to_owned(), value);
        }
    }
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
        requires: Vec::new(),
    })
}

async fn set(daemon: Daemon, params: ConfigSetParams, _ctx: Ctx) -> Result<ConfigValueDto> {
    let (key, value) = (params.key.clone(), params.value);
    let change =
        crate::config_owner::update(&daemon, move |config| with_value(config, &key, value)).await?;
    let value = value_of(&daemon.config(), &params.key)?;
    let default = value_of(&ForgeConfig::default(), &params.key).ok();
    Ok(ConfigValueDto {
        key: params.key,
        is_default: default.as_ref() == Some(&value),
        value,
        requires: change.requires,
    })
}

/// `system.config_list`: values, defaults and the JSON Schema (ADR-0040).
async fn list(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<ConfigDocumentDto> {
    let config = daemon.config();
    Ok(ConfigDocumentDto {
        values: serde_json::to_value(&*config).unwrap_or(Value::Null),
        defaults: serde_json::to_value(ForgeConfig::default()).unwrap_or(Value::Null),
        schema: ForgeConfig::json_schema(),
        set_keys: crate::config_owner::changed_keys(&ForgeConfig::default(), &config),
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
        // Unset optional keys read as null and can be set; unknown ones cannot.
        assert_eq!(value_of(&config, "metrics.enabled").unwrap(), Value::Null);
        assert_eq!(value_of(&config, "editor.command").unwrap(), Value::Null);
        let on = with_value(&config, "metrics.enabled", Value::Bool(false)).unwrap();
        assert_eq!(on.metrics.enabled, Some(false));
        assert_eq!(value_of(&on, "metrics.enabled").unwrap(), false);
        let unset = with_value(&on, "metrics.enabled", Value::Null).unwrap();
        assert_eq!(unset.metrics.enabled, None);
        assert_eq!(
            value_of(&config, "metrics.nope").unwrap_err().code(),
            "not_found"
        );
        assert_eq!(
            with_value(&config, "metrics.enbled", Value::Bool(true))
                .unwrap_err()
                .code(),
            "not_found"
        );
        assert_eq!(
            with_value(&config, "nope", Value::Bool(true))
                .unwrap_err()
                .code(),
            "not_found"
        );
        assert_eq!(pointer("a..b").unwrap_err().code(), "invalid_input");
    }
}
