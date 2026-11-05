//! Advanced settings of an instance (`services.update`, docs/07 §5.3, M2-E2-T10): extra
//! arguments, extra environment and the restart policy. They live in `instances.settings`
//! under daemon-owned `forgebox.*` keys (no migration, never a quick setting) and
//! [`crate::services::instance_spec`] layers them over the manifest's `service.daemon`.
//! Everything here is pure: validation and (de)serialization.

use std::collections::BTreeMap;

use forge_core::error::{ForgeError, Result};
use forge_core::store::{InstanceRow, LAUNCH_ARGS_KEY, LAUNCH_ENV_KEY, RESTART_POLICY_KEY};
use forge_ipc::api::{RestartOn, RestartPolicyDto, RestartPolicyPatchDto};
use forge_manifest::schema::RestartOn as ManifestRestartOn;
use forge_manifest::schema::tool::RestartPolicy as ManifestRestartPolicy;
use forge_manifest::schema::tool::ToolManifest;
use serde::{Deserialize, Serialize};

/// Most extra arguments / variables an instance may carry.
pub const MAX_ITEMS: usize = 64;
/// Longest argument or value, in bytes.
pub const MAX_LEN: usize = 8 * 1024;
/// Longest restart window the policy accepts (one day).
pub const MAX_WINDOW_SECS: u32 = 86_400;
/// Most restarts a window may allow.
pub const MAX_RESTARTS: u32 = 1_000;

/// The stored restart policy (`forgebox.restart_policy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredRestart {
    pub on: StoredOn,
    pub max: u32,
    pub window_secs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StoredOn {
    Never,
    Failure,
    Always,
}

impl StoredOn {
    fn of_manifest(on: ManifestRestartOn) -> Self {
        match on {
            ManifestRestartOn::Never => Self::Never,
            ManifestRestartOn::Failure => Self::Failure,
            ManifestRestartOn::Always => Self::Always,
        }
    }

    fn of_dto(on: RestartOn) -> Self {
        match on {
            RestartOn::Never => Self::Never,
            RestartOn::Failure => Self::Failure,
            RestartOn::Always => Self::Always,
        }
    }

    fn dto(self) -> RestartOn {
        match self {
            Self::Never => RestartOn::Never,
            Self::Failure => RestartOn::Failure,
            Self::Always => RestartOn::Always,
        }
    }

    pub fn manifest(self) -> ManifestRestartOn {
        match self {
            Self::Never => ManifestRestartOn::Never,
            Self::Failure => ManifestRestartOn::Failure,
            Self::Always => ManifestRestartOn::Always,
        }
    }
}

impl StoredRestart {
    /// The manifest's own policy (the "padrão").
    pub fn of_manifest(policy: &ManifestRestartPolicy) -> Self {
        Self {
            on: StoredOn::of_manifest(policy.on),
            max: policy.max,
            window_secs: u32::try_from(policy.window.as_std().as_secs()).unwrap_or(u32::MAX),
        }
    }

    pub fn dto(self) -> RestartPolicyDto {
        RestartPolicyDto {
            on: self.on.dto(),
            max: self.max,
            window_secs: self.window_secs,
        }
    }
}

/// What `instances.settings` adds to the manifest's launch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Launch {
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub restart: Option<StoredRestart>,
}

/// The advanced settings of `row`. A value that does not parse (edited by hand) counts as
/// absent and is logged: the instance still starts with the manifest's launch.
pub fn launch_of(row: &InstanceRow) -> Launch {
    fn read<T: serde::de::DeserializeOwned>(row: &InstanceRow, key: &str) -> Option<T> {
        let text = row.settings.get(key)?;
        match serde_json::from_str(text) {
            Ok(value) => Some(value),
            Err(err) => {
                tracing::warn!(instance = %row.id, key, %err, "ignoring an unreadable setting");
                None
            }
        }
    }
    Launch {
        args: read(row, LAUNCH_ARGS_KEY).unwrap_or_default(),
        env: read(row, LAUNCH_ENV_KEY).unwrap_or_default(),
        restart: read(row, RESTART_POLICY_KEY),
    }
}

/// The manifest's restart policy (the default of every instance of the tool).
pub fn default_restart(manifest: &ToolManifest) -> StoredRestart {
    manifest.spec.service.daemon.as_ref().map_or_else(
        || StoredRestart::of_manifest(&ManifestRestartPolicy::default()),
        |d| StoredRestart::of_manifest(&d.restart_policy),
    )
}

/// The policy in effect for `row`: its own, else the manifest's.
pub fn current_restart(manifest: &ToolManifest, row: &InstanceRow) -> StoredRestart {
    launch_of(row)
        .restart
        .unwrap_or_else(|| default_restart(manifest))
}

/// The policy in effect: the instance's, else the manifest's.
pub fn effective_restart(manifest: &ManifestRestartPolicy, launch: &Launch) -> StoredRestart {
    launch
        .restart
        .unwrap_or_else(|| StoredRestart::of_manifest(manifest))
}

/// Extra arguments: bounded, without NUL or line breaks.
pub fn validate_args(args: &[String]) -> Result<()> {
    if args.len() > MAX_ITEMS {
        return Err(ForgeError::InvalidInput(format!(
            "at most {MAX_ITEMS} extra arguments (got {})",
            args.len()
        )));
    }
    for arg in args {
        if arg.is_empty() || arg.len() > MAX_LEN || arg.contains(['\0', '\r', '\n']) {
            return Err(ForgeError::InvalidInput(format!(
                "invalid argument {arg:?}: it must be 1 to {MAX_LEN} bytes, without line breaks"
            )));
        }
    }
    Ok(())
}

/// Extra environment: `[A-Za-z_][A-Za-z0-9_]*` names (never `PATH`, which the manifest's
/// `path` entries own) and bounded values without NUL.
pub fn validate_env(env: &BTreeMap<String, String>) -> Result<()> {
    if env.len() > MAX_ITEMS {
        return Err(ForgeError::InvalidInput(format!(
            "at most {MAX_ITEMS} extra variables (got {})",
            env.len()
        )));
    }
    for (name, value) in env {
        let mut chars = name.chars();
        let valid = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(ForgeError::InvalidInput(format!(
                "invalid variable name {name:?} ([A-Za-z_][A-Za-z0-9_]*)"
            )));
        }
        if name.eq_ignore_ascii_case("PATH") {
            return Err(ForgeError::InvalidInput(
                "PATH cannot be set here: the tool's path entries own it".into(),
            ));
        }
        if value.len() > MAX_LEN || value.contains('\0') {
            return Err(ForgeError::InvalidInput(format!(
                "invalid value for {name}: at most {MAX_LEN} bytes, without NUL"
            )));
        }
    }
    Ok(())
}

/// The policy after `patch` over `current` (absent fields keep their value), validated.
pub fn merge_restart(
    current: StoredRestart,
    patch: &RestartPolicyPatchDto,
) -> Result<StoredRestart> {
    let merged = StoredRestart {
        on: patch.on.map_or(current.on, StoredOn::of_dto),
        max: patch.max.unwrap_or(current.max),
        window_secs: patch.window_secs.unwrap_or(current.window_secs),
    };
    if merged.max > MAX_RESTARTS {
        return Err(ForgeError::InvalidInput(format!(
            "restart max {} is above {MAX_RESTARTS}",
            merged.max
        )));
    }
    if merged.window_secs == 0 || merged.window_secs > MAX_WINDOW_SECS {
        return Err(ForgeError::InvalidInput(format!(
            "restart window must be 1 to {MAX_WINDOW_SECS} seconds (got {})",
            merged.window_secs
        )));
    }
    if merged.on == StoredOn::Failure && merged.max == 0 {
        return Err(ForgeError::InvalidInput(
            "restart on failure needs max >= 1 (use `never` to not restart)".into(),
        ));
    }
    Ok(merged)
}

/// JSON text of a stored value.
pub fn to_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|e| ForgeError::Internal(format!("advanced settings: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::ids::{ToolId, VersionId};

    fn policy() -> StoredRestart {
        StoredRestart {
            on: StoredOn::Failure,
            max: 3,
            window_secs: 60,
        }
    }

    #[test]
    fn args_and_env_are_validated() {
        validate_args(&["--a=1".into(), "-v".into()]).unwrap();
        assert!(validate_args(&["".into()]).is_err());
        assert!(validate_args(&["a\nb".into()]).is_err());
        let mut env = BTreeMap::new();
        env.insert("FB_X".to_owned(), "1".to_owned());
        validate_env(&env).unwrap();
        for bad in ["", "1A", "A=B", "A B", "path", "PATH"] {
            let mut env = BTreeMap::new();
            env.insert(bad.to_owned(), "v".to_owned());
            assert_eq!(
                validate_env(&env).unwrap_err().code(),
                "invalid_input",
                "{bad}"
            );
        }
    }

    #[test]
    fn restart_patch_keeps_absent_fields_and_checks_the_result() {
        let patch = RestartPolicyPatchDto {
            on: Some(RestartOn::Always),
            ..RestartPolicyPatchDto::default()
        };
        let merged = merge_restart(policy(), &patch).unwrap();
        assert_eq!(
            (merged.on, merged.max, merged.window_secs),
            (StoredOn::Always, 3, 60)
        );
        let bad = RestartPolicyPatchDto {
            window_secs: Some(0),
            ..RestartPolicyPatchDto::default()
        };
        assert!(merge_restart(policy(), &bad).is_err());
        let bad = RestartPolicyPatchDto {
            max: Some(0),
            ..RestartPolicyPatchDto::default()
        };
        assert!(merge_restart(policy(), &bad).is_err(), "failure with max 0");
    }

    #[test]
    fn launch_round_trips_and_ignores_garbage() {
        let mut row = InstanceRow::new(ToolId::new("mysql").unwrap(), VersionId::new(), "default");
        assert_eq!(launch_of(&row), Launch::default());
        row.settings
            .insert(LAUNCH_ARGS_KEY.into(), to_json(&vec!["--x"]).unwrap());
        row.settings
            .insert(RESTART_POLICY_KEY.into(), to_json(&policy()).unwrap());
        row.settings
            .insert(LAUNCH_ENV_KEY.into(), "not json".into());
        let launch = launch_of(&row);
        assert_eq!(launch.args, ["--x"]);
        assert!(launch.env.is_empty());
        assert_eq!(launch.restart, Some(policy()));
    }
}
