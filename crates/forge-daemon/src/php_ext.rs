//! Extensions of a pool tool (PHP) and its debugger extension (Xdebug), docs/03 §2.3,
//! M2-E2-T9. Everything comes from the manifests; no tool id is written here.
//!
//! * **Choices** of the user per series live in `tool_series_settings` of the pool tool
//!   (`php@8.4`) as `ext_<name> = true|false` ([`EXTENSION_CHOICE_PREFIX`], a prefix the
//!   manifest lint reserves): only what differs from the manifest is stored, so turning an
//!   extension back to its default leaves no row. Bundled extensions start from
//!   `config.extensions.defaults_on` (and the gated `zend_extensions`); registry
//!   extensions (`kind: Extension` installed into the version, ADR-0043) start off.
//! * **The debugger** is the Extension of the pool tool whose `toggle` declares `modes`
//!   (Xdebug): `php.xdebug {mode}` stores the preset of that mode into the extension's own
//!   per-series settings (`tool_series_settings` of `xdebug@8.4`), turns its choice on
//!   (`off` = choice off, the `toggle.disable` line) and reserves its `type: port,
//!   reserve: true` setting for the owner `pool/<tool>@<series>#<extension>`, shown as
//!   `php@8.4/xdebug` ([`client_port_owner`]).
//! * The render model ([`debugger_model`]) carries the `toggle` line and every effective
//!   quick setting as `ini directive = value`, for `php/xdebug.ini.j2`.

use std::collections::{BTreeMap, BTreeSet};

use forge_core::error::{ForgeError, Result};
use forge_core::ids::{ToolId, VersionId};
use forge_core::model::port::{OwnerKind, ReservationOwner};
use forge_core::store::{StoreReader, ToolSeriesSettingsRepo, ToolVersionRow};
use forge_manifest::schema::ExtensionManifest;
use forge_manifest::schema::extension::{TARGET_ID_KEY, TARGET_TOOL_KEY};
use forge_manifest::schema::tool::{
    ExtensionsConfig, QuickSetting, QuickSettingType, ToolManifest,
};
use forge_registry::Registry;
use forge_render::{IniSetting, PhpModel, XdebugModel};

use crate::catalog::{self, ExtensionEntry};
use crate::settings;

pub use forge_manifest::schema::tool::EXTENSION_CHOICE_PREFIX;

/// Longest extension name accepted.
const MAX_NAME: usize = 64;

/// The stored key of the choice for extension `name` (`ext_intl`).
pub fn choice_key(name: &str) -> String {
    format!("{EXTENSION_CHOICE_PREFIX}{name}")
}

/// An extension name of a request (`Intl ` → `intl`), or `invalid_input`: names are
/// `[a-z0-9_]`, like the file names of the official builds (`php_pdo_mysql.dll`).
pub fn parse_name(raw: &str) -> Result<String> {
    let name = raw.trim().to_ascii_lowercase();
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(name)
    } else {
        Err(ForgeError::InvalidInput(format!(
            "extension {raw:?}: names use [a-z0-9_] (intl, pdo_mysql)"
        )))
    }
}

/// The user's choices stored for a series (`ext_<name>` → on/off); malformed values are
/// ignored (the default applies).
pub fn choices(stored: &BTreeMap<String, String>) -> BTreeMap<String, bool> {
    stored
        .iter()
        .filter_map(|(key, value)| {
            let name = key.strip_prefix(EXTENSION_CHOICE_PREFIX)?;
            let on = match value.as_str() {
                "true" => true,
                "false" => false,
                _ => return None,
            };
            Some((name.to_owned(), on))
        })
        .collect()
}

/// What to store for turning an extension `enable`d or not when its default is
/// `default_on`: `None` (no row: the default) or the explicit choice.
pub fn change_for(default_on: bool, enable: bool) -> Option<String> {
    (enable != default_on).then(|| enable.to_string())
}

/// The bundled extensions of `php` (`defaults_on` and the gated zend extensions, as the
/// manifest gave them) with the user's `choices`: chosen ones added, refused ones
/// removed, registry extensions (`registry`, rendered by their own overlay) left out.
pub fn apply_choices(
    choices: &BTreeMap<String, bool>,
    registry: &BTreeSet<String>,
    php: &mut PhpModel,
) {
    for (name, on) in choices {
        if *on
            && !registry.contains(name)
            && !php.extensions.contains(name)
            && !php.zend_extensions.contains(name)
        {
            php.extensions.push(name.clone());
        }
    }
    php.extensions
        .retain(|name| choices.get(name) != Some(&false) && !registry.contains(name));
    php.zend_extensions
        .retain(|name| choices.get(name) != Some(&false));
}

/// The debugger extension of pool tool `tool`: the first Extension (by id) that extends
/// it and declares `toggle.modes`.
pub fn debugger(registry: &Registry, tool: &ToolId) -> Option<ExtensionEntry> {
    catalog::extensions(registry).into_iter().find(|e| {
        e.manifest.spec.extends == *tool
            && e.manifest
                .spec
                .toggle
                .as_ref()
                .is_some_and(|t| !t.modes.is_empty())
    })
}

/// Whether `ext` is a debugger (`toggle.modes`).
pub fn is_debugger(ext: &ExtensionManifest) -> bool {
    ext.spec
        .toggle
        .as_ref()
        .is_some_and(|t| !t.modes.is_empty())
}

/// The name an extension goes by among the target's extensions: the file of its
/// `install.target` under the target's `config.extensions.pattern` (`php_xdebug.dll` →
/// `xdebug`), else its id.
pub fn ext_name(config: Option<&ExtensionsConfig>, ext: &ExtensionManifest) -> String {
    let file = ext
        .spec
        .install
        .target
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim();
    config
        .and_then(|c| c.name_of(file))
        .unwrap_or_else(|| ext.metadata.id.to_string())
}

/// Whether the extension is loaded with `zend_extension` (the directive of its `toggle`).
pub fn is_zend(ext: &ExtensionManifest) -> bool {
    ext.spec
        .toggle
        .as_ref()
        .is_some_and(|t| t.directive() == "zend_extension")
}

/// An extension installed into one version of the pool tool.
#[derive(Debug, Clone)]
pub struct Linked {
    pub entry: ExtensionEntry,
    /// Its `tool_versions` row (`path` = the installed file).
    pub row: ToolVersionRow,
    /// [`ext_name`].
    pub name: String,
}

/// The registry extensions installed into `target` (a version of `tool`), by name.
pub async fn linked(
    reader: &StoreReader,
    registry: &Registry,
    tool: &ToolManifest,
    target: VersionId,
) -> Result<Vec<Linked>> {
    let target_text = target.to_string();
    let tool_id = tool.metadata.id.as_str();
    let mut out: Vec<Linked> = catalog::all_installed(reader)
        .await?
        .into_iter()
        .filter(|r| {
            r.variant.get(TARGET_ID_KEY) == Some(&target_text)
                && r.variant.get(TARGET_TOOL_KEY).map(String::as_str) == Some(tool_id)
        })
        .filter_map(|row| {
            let entry = catalog::extension(registry, &row.tool_id)?;
            let name = ext_name(tool.spec.config.extensions.as_ref(), &entry.manifest);
            Some(Linked { entry, row, name })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Stored settings of `tool`@`series` (empty when nothing is stored).
pub async fn stored(
    reader: &StoreReader,
    tool: &ToolId,
    series: &str,
) -> Result<BTreeMap<String, String>> {
    let (tool, series) = (tool.clone(), series.to_owned());
    reader
        .query(move |conn| ToolSeriesSettingsRepo::new(conn).get(&tool, &series))
        .await
}

/// The quick setting of `ext` with `type: port` and `reserve: true` (Xdebug's client
/// port), if any.
pub fn reserved_port_setting(ext: &ExtensionManifest) -> Option<&QuickSetting> {
    ext.spec
        .config
        .quick_settings
        .iter()
        .find(|q| q.kind == QuickSettingType::Port && q.reserve)
}

/// The stored-text changes of the preset `mode` of `ext` (`toggle.modes.<mode>`), each
/// validated against its quick setting; `config_invalid` when the manifest has no such
/// mode or a value its setting refuses.
pub fn mode_changes(
    ext: &ExtensionManifest,
    mode: &str,
) -> Result<BTreeMap<String, Option<String>>> {
    let id = &ext.metadata.id;
    let preset = ext
        .spec
        .toggle
        .as_ref()
        .and_then(|t| t.modes.get(mode))
        .ok_or_else(|| {
            ForgeError::ConfigInvalid(format!("{id} declares no toggle.modes.{mode}"))
        })?;
    let mut out = BTreeMap::new();
    for (key, value) in preset {
        let setting = ext
            .spec
            .config
            .quick_settings
            .iter()
            .find(|q| &q.key == key)
            .ok_or_else(|| {
                ForgeError::ConfigInvalid(format!(
                    "{id}: toggle.modes.{mode}.{key} is not a quick setting"
                ))
            })?;
        let text = settings::value_text(setting, value).ok_or_else(|| {
            ForgeError::ConfigInvalid(format!(
                "{id}: toggle.modes.{mode}.{key} = {value} is not a valid value"
            ))
        })?;
        out.insert(key.clone(), Some(text));
    }
    Ok(out)
}

/// Canonical text of each effective quick setting of `ext` (stored, else default).
fn effective(
    ext: &ExtensionManifest,
    stored: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    settings::effective_of(&ext.spec.config.quick_settings, stored)
        .into_iter()
        .map(|(s, text)| (s.key.clone(), text))
        .collect()
}

/// The effective port of the reserved port setting of `ext` with `stored`.
pub fn effective_port(ext: &ExtensionManifest, stored: &BTreeMap<String, String>) -> Option<u16> {
    let setting = reserved_port_setting(ext)?;
    effective(ext, stored).get(&setting.key)?.parse().ok()
}

/// The mode an enabled debugger is in: the first preset (by name) whose values all equal
/// the effective ones; `None` when the values match no preset (set by hand).
pub fn mode_of(ext: &ExtensionManifest, stored: &BTreeMap<String, String>) -> Option<String> {
    let now = effective(ext, stored);
    let toggle = ext.spec.toggle.as_ref()?;
    toggle.modes.keys().find_map(|mode| {
        let changes = mode_changes(ext, mode).ok()?;
        changes
            .iter()
            .all(|(key, text)| now.get(key) == text.as_ref())
            .then(|| mode.clone())
    })
}

/// The render model of the debugger `ext` of a series: its `toggle` line for `enabled`
/// and every effective quick setting as `ini directive = written value`.
pub fn debugger_model(
    ext: &ExtensionManifest,
    enabled: bool,
    stored: &BTreeMap<String, String>,
) -> Option<XdebugModel> {
    let toggle = ext.spec.toggle.as_ref()?;
    let settings = settings::effective_of(&ext.spec.config.quick_settings, stored)
        .into_iter()
        .map(|(s, text)| IniSetting {
            key: s.target().to_owned(),
            value: settings::written_text(&settings::written(s, &text)),
        })
        .collect();
    Some(XdebugModel {
        enabled,
        line: if enabled {
            toggle.enable.clone()
        } else {
            toggle.disable.clone()
        },
        settings,
    })
}

/// Owner of the client port of extension `ext` for `tool`@`series`
/// (`pool/php@8.4#xdebug`, shown as `php@8.4/xdebug`). The id is the series label, not
/// a version id, so pool starts (which release their `pool/<version id>` reservations)
/// never drop it.
pub fn client_port_owner(tool: &ToolId, series: &str, ext: &ToolId) -> Result<ReservationOwner> {
    ReservationOwner::new(OwnerKind::Pool, format!("{tool}@{series}"), ext.as_str())
}

/// Whether `owner` is a client port ([`client_port_owner`]): the port a debugger
/// connects to, where the IDE is the listener, so a listener there is expected.
pub fn is_client_port(owner: &ReservationOwner) -> bool {
    owner.kind == OwnerKind::Pool && owner.id.contains('@')
}

/// Xdebug state of one version of the pool tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebuggerState {
    pub enabled: bool,
    /// Preset matched by the stored values ([`mode_of`]).
    pub mode: Option<String>,
}

/// The debugger state of every version in `rows` (installed versions of `tool`) that has
/// the debugger installed.
pub async fn debugger_states(
    reader: &StoreReader,
    registry: &Registry,
    tool: &ToolManifest,
    rows: &[ToolVersionRow],
) -> Result<BTreeMap<VersionId, DebuggerState>> {
    let Some(ext) = debugger(registry, &tool.metadata.id) else {
        return Ok(BTreeMap::new());
    };
    let ext_id = ext.manifest.metadata.id.clone();
    let tool_id = tool.metadata.id.clone();
    let installed = catalog::installed(reader, &ext_id, None).await?;
    if installed.is_empty() {
        return Ok(BTreeMap::new());
    }
    let (php_all, ext_all) = {
        let (tool_id, ext_id) = (tool_id.clone(), ext_id.clone());
        reader
            .query(move |conn| {
                let repo = ToolSeriesSettingsRepo::new(conn);
                Ok((repo.list_tool(&tool_id)?, repo.list_tool(&ext_id)?))
            })
            .await?
    };
    let name = ext_name(tool.spec.config.extensions.as_ref(), &ext.manifest);
    let key = choice_key(&name);
    let empty = BTreeMap::new();
    let mut out = BTreeMap::new();
    for row in rows {
        let target = row.id.to_string();
        let has = installed.iter().any(|r| {
            r.variant.get(TARGET_ID_KEY) == Some(&target)
                && r.variant.get(TARGET_TOOL_KEY).map(String::as_str) == Some(tool_id.as_str())
        });
        if !has {
            continue;
        }
        let series = catalog::series_of(tool, &row.version);
        let enabled = php_all
            .get(&series)
            .and_then(|s| s.get(&key))
            .is_some_and(|v| v == "true");
        let ext_stored = ext_all.get(&series).unwrap_or(&empty);
        out.insert(
            row.id,
            DebuggerState {
                enabled,
                mode: mode_of(&ext.manifest, ext_stored),
            },
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xdebug() -> ExtensionManifest {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../registry/tools/xdebug.yaml"),
        )
        .unwrap();
        match forge_manifest::load_str(&text).unwrap() {
            forge_manifest::Manifest::Extension(e) => e,
            _ => unreachable!(),
        }
    }

    fn php() -> ToolManifest {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/tools/php.yaml"),
        )
        .unwrap();
        match forge_manifest::load_str(&text).unwrap() {
            forge_manifest::Manifest::Tool(t) => t,
            _ => unreachable!(),
        }
    }

    fn map(items: &[(&str, &str)]) -> BTreeMap<String, String> {
        items
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn names_and_choices() {
        assert_eq!(parse_name(" Intl ").unwrap(), "intl");
        assert_eq!(parse_name("pdo_mysql").unwrap(), "pdo_mysql");
        for bad in ["", "a b", "../x", "x.dll", "ação"] {
            assert_eq!(
                parse_name(bad).unwrap_err().code(),
                "invalid_input",
                "{bad}"
            );
        }
        assert_eq!(choice_key("intl"), "ext_intl");
        let stored = map(&[
            ("ext_intl", "false"),
            ("ext_xsl", "true"),
            ("ext_bad", "maybe"),
            ("memory_limit", "1G"),
        ]);
        assert_eq!(
            choices(&stored),
            BTreeMap::from([("intl".to_owned(), false), ("xsl".to_owned(), true)])
        );
        // Only what differs from the default is stored.
        assert_eq!(change_for(true, true), None);
        assert_eq!(change_for(false, false), None);
        assert_eq!(change_for(true, false).as_deref(), Some("false"));
        assert_eq!(change_for(false, true).as_deref(), Some("true"));
    }

    #[test]
    fn choices_change_the_bundled_lists_but_never_registry_extensions() {
        let mut model = PhpModel {
            series: "8.4".into(),
            extensions: vec!["curl".into(), "intl".into(), "mbstring".into()],
            zend_extensions: vec!["opcache".into()],
            settings: BTreeMap::new(),
            xdebug: None,
        };
        let choices = BTreeMap::from([
            ("intl".to_owned(), false),
            ("xsl".to_owned(), true),
            ("opcache".to_owned(), false),
            ("xdebug".to_owned(), true),
        ]);
        let registry = BTreeSet::from(["xdebug".to_owned()]);
        apply_choices(&choices, &registry, &mut model);
        assert_eq!(model.extensions, ["curl", "mbstring", "xsl"]);
        assert!(model.zend_extensions.is_empty());
    }

    #[test]
    fn xdebug_is_the_debugger_with_data_driven_modes() {
        let ext = xdebug();
        assert!(is_debugger(&ext) && is_zend(&ext));
        let php = php();
        assert_eq!(
            ext_name(php.spec.config.extensions.as_ref(), &ext),
            "xdebug"
        );
        assert_eq!(ext_name(None, &ext), "xdebug");
        let port = reserved_port_setting(&ext).unwrap();
        assert_eq!(port.key, "client_port");
        assert_eq!(effective_port(&ext, &BTreeMap::new()), Some(9003));
        assert_eq!(
            effective_port(&ext, &map(&[("client_port", "9010")])),
            Some(9010)
        );

        let trigger = mode_changes(&ext, "trigger").unwrap();
        assert_eq!(
            trigger,
            BTreeMap::from([
                ("mode".to_owned(), Some("debug,develop".to_owned())),
                ("start_with_request".to_owned(), Some("trigger".to_owned())),
            ])
        );
        let on = mode_changes(&ext, "on").unwrap();
        assert_eq!(on["start_with_request"].as_deref(), Some("yes"));
        assert_eq!(
            mode_changes(&ext, "off").unwrap_err().code(),
            "config_invalid"
        );

        // The manifest defaults are the trigger preset; stored values decide otherwise.
        assert_eq!(mode_of(&ext, &BTreeMap::new()).as_deref(), Some("trigger"));
        let stored_on: BTreeMap<String, String> = on
            .iter()
            .map(|(k, v)| (k.clone(), v.clone().unwrap()))
            .collect();
        assert_eq!(mode_of(&ext, &stored_on).as_deref(), Some("on"));
        assert_eq!(mode_of(&ext, &map(&[("mode", "profile")])), None);

        let model = debugger_model(&ext, true, &map(&[("client_port", "9010")])).unwrap();
        assert!(model.enabled);
        assert_eq!(model.line, "zend_extension=php_xdebug.dll");
        let lines: Vec<(&str, &str)> = model
            .settings
            .iter()
            .map(|s| (s.key.as_str(), s.value.as_str()))
            .collect();
        assert_eq!(
            lines,
            [
                ("xdebug.mode", "debug,develop"),
                ("xdebug.start_with_request", "trigger"),
                ("xdebug.client_port", "9010"),
                ("xdebug.idekey", "FORGEBOX"),
            ]
        );
        let off = debugger_model(&ext, false, &BTreeMap::new()).unwrap();
        assert_eq!(off.line, ";zend_extension=php_xdebug.dll");
    }

    #[test]
    fn the_client_port_owner_is_named_by_the_series() {
        let owner = client_port_owner(
            &ToolId::new("php").unwrap(),
            "8.4",
            &ToolId::new("xdebug").unwrap(),
        )
        .unwrap();
        assert_eq!(owner.to_string(), "pool/php@8.4#xdebug");
        assert!(is_client_port(&owner));
        let worker =
            ReservationOwner::new(OwnerKind::Pool, VersionId::new().to_string(), "worker-1")
                .unwrap();
        assert!(!is_client_port(&worker));
    }
}
