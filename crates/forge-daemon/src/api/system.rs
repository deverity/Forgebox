//! `system.*` (`system.hello`/`system.ping` are server built-ins): preferences
//! (`forgebox.toml` by dotted key). Import, bundles, backups and self-update are later
//! tasks (see `not_implemented`).

use forge_core::config::ForgeConfig;
use forge_core::error::{ForgeError, Result};
use forge_ipc::api::methods::{system_config_get, system_config_set};
use forge_ipc::api::{ConfigGetParams, ConfigSetParams, ConfigValueDto};
use forge_ipc::server::{Ctx, Router};
use serde_json::Value;

use super::route;
use crate::daemon::Daemon;

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, system_config_get, get);
    route!(router, daemon, system_config_set, set);
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
