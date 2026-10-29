//! Quick settings of a manifest (docs/04 §1.3, ADR-0042), without I/O: what
//! `services.settings` shows, how `services.set` validates and stores a value, and what
//! each config file gets written.
//!
//! * **Stored text** (`instances.settings` for instances, `tool_series_settings` for a PHP
//!   series) is canonical: toggles `true`/`false`, numbers in decimal without trailing
//!   zeros, `multi` items in the order of `options` joined by `,`, `select` the option's
//!   own text. A key without a stored value takes the manifest's `default`.
//! * **Effective values** = stored, else default. They are written into the files (all
//!   of them, defaults included), so what `services.settings` reports is what the tool
//!   reads; a stored value the manifest no longer accepts falls back to the default.
//! * **Written values** follow the format: a toggle writes its `on`/`off` text (or the
//!   format's native boolean), a `select` whose option is a YAML number is a number.
//! * `ports.<name>` keys are reserved: they move a named port of an instance
//!   ([`Changes::ports`]).

use std::collections::BTreeMap;
use std::path::PathBuf;

use forge_core::error::{ForgeError, Result};
use forge_ipc::api::{QuickSettingDto, QuickSettingKind, SettingApply, SettingValueDto};
use forge_manifest::schema::common::Apply;
use forge_manifest::schema::tool::{ConfigFormat, QuickSetting, QuickSettingType, ToolManifest};
use forge_render::settings::{Assignment, SettingValue, SettingsFile, SettingsFormat};
use serde_json::Value;

use crate::vars::RuntimeVars;

/// Prefix of the reserved keys that move a named port (`ports.main=3307`).
pub const PORT_PREFIX: &str = "ports.";

/// Longest text value accepted (a config value, not a document).
const MAX_TEXT: usize = 1024;

/// A validated `services.set` request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// Per key: `Some(text)` = store this canonical text, `None` = back to the default
    /// (an empty value in the request).
    pub settings: BTreeMap<String, Option<String>>,
    /// `ports.<name>` → the new port.
    pub ports: BTreeMap<String, u16>,
}

impl Changes {
    /// The settings as they will be stored after this change.
    pub fn applied_to(&self, stored: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut out = stored.clone();
        for (key, value) in &self.settings {
            match value {
                Some(v) => {
                    out.insert(key.clone(), v.clone());
                }
                None => {
                    out.remove(key);
                }
            }
        }
        out
    }
}

fn bad(key: &str, why: impl std::fmt::Display) -> ForgeError {
    ForgeError::InvalidInput(format!("{key}: {why}"))
}

/// Text of an option or default as YAML wrote it (`1G`, `-1`, `2`).
fn json_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Canonical decimal text of a number (`120`, `0.5`).
fn number_text(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn option_texts(setting: &QuickSetting) -> Vec<String> {
    setting.options.iter().filter_map(json_text).collect()
}

/// The option whose text matches `raw` (exact first, then ignoring ASCII case).
fn find_option(setting: &QuickSetting, raw: &str) -> Option<String> {
    let options = option_texts(setting);
    options
        .iter()
        .find(|o| *o == raw)
        .or_else(|| options.iter().find(|o| o.eq_ignore_ascii_case(raw)))
        .cloned()
}

fn toggle_text(setting: &QuickSetting, raw: &str) -> Option<bool> {
    let lower = raw.to_ascii_lowercase();
    match lower.as_str() {
        "true" | "on" | "1" | "yes" => return Some(true),
        "false" | "off" | "0" | "no" => return Some(false),
        _ => {}
    }
    if setting
        .on
        .as_deref()
        .is_some_and(|on| on.eq_ignore_ascii_case(raw))
    {
        Some(true)
    } else if setting
        .off
        .as_deref()
        .is_some_and(|off| off.eq_ignore_ascii_case(raw))
    {
        Some(false)
    } else {
        None
    }
}

fn plain_text(key: &str, raw: &str) -> Result<String> {
    if raw.chars().any(char::is_control) {
        return Err(bad(
            key,
            "line breaks and control characters are not allowed",
        ));
    }
    if raw.len() > MAX_TEXT {
        return Err(bad(key, format!("longer than {MAX_TEXT} bytes")));
    }
    Ok(raw.to_owned())
}

/// The canonical stored text of `raw` (already trimmed, not empty) for `setting`, or
/// `invalid_input` naming the key.
pub fn canonical(setting: &QuickSetting, raw: &str) -> Result<String> {
    let key = setting.key.as_str();
    match setting.kind {
        QuickSettingType::Toggle => toggle_text(setting, raw)
            .map(|b| b.to_string())
            .ok_or_else(|| bad(key, format!("{raw:?} is not true/false"))),
        QuickSettingType::Select => find_option(setting, raw).ok_or_else(|| {
            bad(
                key,
                format!("{raw:?} is not one of {}", option_texts(setting).join(", ")),
            )
        }),
        QuickSettingType::Multi => {
            let mut chosen = Vec::new();
            for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let found = find_option(setting, item).ok_or_else(|| {
                    bad(
                        key,
                        format!(
                            "{item:?} is not one of {}",
                            option_texts(setting).join(", ")
                        ),
                    )
                })?;
                if !chosen.contains(&found) {
                    chosen.push(found);
                }
            }
            let order = option_texts(setting);
            chosen.sort_by_key(|c| order.iter().position(|o| o == c));
            Ok(chosen.join(","))
        }
        QuickSettingType::Number | QuickSettingType::Slider => {
            let n: f64 = raw
                .parse()
                .ok()
                .filter(|n: &f64| n.is_finite())
                .ok_or_else(|| bad(key, format!("{raw:?} is not a number")))?;
            if let Some(min) = setting.min.filter(|min| n < *min) {
                return Err(bad(key, format!("{raw} is below the minimum {min}")));
            }
            if let Some(max) = setting.max.filter(|max| n > *max) {
                return Err(bad(key, format!("{raw} is above the maximum {max}")));
            }
            if let Some(step) = setting.step.filter(|s| *s > 0.0) {
                let base = setting.min.unwrap_or(0.0);
                let steps = (n - base) / step;
                if (steps - steps.round()).abs() > 1e-9 * steps.abs().max(1.0) {
                    return Err(bad(
                        key,
                        format!("{raw} is not a multiple of the step {step}"),
                    ));
                }
            }
            Ok(number_text(n))
        }
        QuickSettingType::Port => raw
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .map(|p| p.to_string())
            .ok_or_else(|| bad(key, format!("{raw:?} is not a port (1-65535)"))),
        QuickSettingType::Text => plain_text(key, raw),
        QuickSettingType::Path => plain_text(key, &raw.replace('\\', "/")),
    }
}

/// The manifest's `default` as stored text (`None` when there is none or it is unusable).
pub fn default_text(setting: &QuickSetting) -> Option<String> {
    let default = setting.default.as_ref()?;
    let raw = match (setting.kind, default) {
        (QuickSettingType::Multi, Value::Array(items)) => items
            .iter()
            .filter_map(json_text)
            .collect::<Vec<_>>()
            .join(","),
        (QuickSettingType::Multi, _) => return None,
        (_, value) => json_text(value)?,
    };
    if raw.is_empty() && setting.kind == QuickSettingType::Multi {
        return Some(String::new());
    }
    canonical(setting, &raw).ok()
}

/// Stored text of `setting` when valid for the manifest as it is now.
fn stored_text(setting: &QuickSetting, stored: &BTreeMap<String, String>) -> Option<String> {
    let value = stored.get(&setting.key)?;
    match canonical(setting, value.trim()) {
        Ok(text) => Some(text),
        Err(err) => {
            tracing::warn!(key = %setting.key, value = %value, %err, "stored quick setting no longer valid; using the default");
            None
        }
    }
}

/// The effective text of every quick setting that has one (stored, else default), by key.
pub fn effective<'m>(
    manifest: &'m ToolManifest,
    stored: &BTreeMap<String, String>,
) -> Vec<(&'m QuickSetting, String)> {
    manifest
        .spec
        .config
        .quick_settings
        .iter()
        .filter_map(|s| {
            stored_text(s, stored)
                .or_else(|| default_text(s))
                .map(|text| (s, text))
        })
        .collect()
}

/// What a file gets for `text` (the canonical text of `setting`).
pub fn written(setting: &QuickSetting, text: &str) -> SettingValue {
    match setting.kind {
        QuickSettingType::Toggle => {
            let on = text == "true";
            match (on, &setting.on, &setting.off) {
                (true, Some(v), _) | (false, _, Some(v)) => SettingValue::Text(v.clone()),
                _ => SettingValue::Bool(on),
            }
        }
        QuickSettingType::Select => {
            let numeric = setting
                .options
                .iter()
                .any(|o| o.is_number() && json_text(o).as_deref() == Some(text));
            if numeric {
                SettingValue::Number(text.to_owned())
            } else {
                SettingValue::Text(text.to_owned())
            }
        }
        QuickSettingType::Number | QuickSettingType::Slider | QuickSettingType::Port => {
            SettingValue::Number(text.to_owned())
        }
        QuickSettingType::Text | QuickSettingType::Path => SettingValue::Text(text.to_owned()),
        QuickSettingType::Multi => SettingValue::List(
            text.split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
    }
}

/// One-line text of a written value (templates, `{{ value }}`).
pub fn written_text(value: &SettingValue) -> String {
    match value {
        SettingValue::Text(t) | SettingValue::Number(t) => t.clone(),
        SettingValue::Bool(b) => b.to_string(),
        SettingValue::List(items) => items.join(","),
    }
}

/// `key → written text` of the effective values, for the render context
/// (`instance.settings.<key>`, `php.settings.<key>`): templates that render a whole file
/// emit the same values the writers enforce.
pub fn context(
    manifest: &ToolManifest,
    stored: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    effective(manifest, stored)
        .into_iter()
        .map(|(s, text)| (s.key.clone(), written_text(&written(s, &text))))
        .collect()
}

fn typed(setting: &QuickSetting, text: &str) -> SettingValueDto {
    match setting.kind {
        QuickSettingType::Toggle => SettingValueDto::Bool(text == "true"),
        QuickSettingType::Number | QuickSettingType::Slider | QuickSettingType::Port => {
            match text.parse::<i64>() {
                Ok(n) => SettingValueDto::Int(n),
                Err(_) => SettingValueDto::Float(text.parse().unwrap_or(0.0)),
            }
        }
        QuickSettingType::Multi => SettingValueDto::List(
            text.split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
        QuickSettingType::Select | QuickSettingType::Text | QuickSettingType::Path => {
            SettingValueDto::Text(text.to_owned())
        }
    }
}

fn kind_dto(kind: QuickSettingType) -> QuickSettingKind {
    match kind {
        QuickSettingType::Toggle => QuickSettingKind::Toggle,
        QuickSettingType::Select => QuickSettingKind::Select,
        QuickSettingType::Number => QuickSettingKind::Number,
        QuickSettingType::Text => QuickSettingKind::Text,
        QuickSettingType::Path => QuickSettingKind::Path,
        QuickSettingType::Port => QuickSettingKind::Port,
        QuickSettingType::Slider => QuickSettingKind::Slider,
        QuickSettingType::Multi => QuickSettingKind::Multi,
    }
}

pub fn apply_dto(apply: Apply) -> SettingApply {
    match apply {
        Apply::Reload => SettingApply::Reload,
        Apply::Restart => SettingApply::Restart,
        Apply::None => SettingApply::None,
    }
}

/// The quick settings of `manifest` with their current values (manifest order).
pub fn describe(
    manifest: &ToolManifest,
    stored: &BTreeMap<String, String>,
) -> Vec<QuickSettingDto> {
    manifest
        .spec
        .config
        .quick_settings
        .iter()
        .map(|s| {
            let stored = stored_text(s, stored);
            let default = default_text(s);
            let value = stored.as_ref().or(default.as_ref());
            QuickSettingDto {
                key: s.key.clone(),
                label: s.label.clone(),
                kind: kind_dto(s.kind),
                options: option_texts(s),
                min: s.min,
                max: s.max,
                step: s.step,
                value: value.map(|v| typed(s, v)),
                default: default.as_ref().map(|v| typed(s, v)),
                is_default: stored.is_none(),
                apply: apply_dto(s.apply),
                file: s.file.clone(),
                also_set: s.also_set.keys().cloned().collect(),
                reserve: s.reserve,
            }
        })
        .collect()
}

/// Validates every pair of a `services.set` request before anything is written:
/// unknown keys, values outside `options`/`min`/`max`/`step`, ports outside 1–65535 and
/// `ports.<name>` that `port_names` does not declare are `invalid_input` naming the key.
/// An empty value resets the key to the manifest's default.
pub fn validate(
    manifest: &ToolManifest,
    input: &BTreeMap<String, String>,
    port_names: &[String],
) -> Result<Changes> {
    if input.is_empty() {
        return Err(ForgeError::InvalidInput(
            "nothing to set: pass key=value pairs (services.settings lists the keys)".into(),
        ));
    }
    let mut changes = Changes::default();
    for (key, raw) in input {
        let raw = raw.trim();
        if let Some(name) = key.strip_prefix(PORT_PREFIX) {
            if !port_names.iter().any(|n| n == name) {
                let known = if port_names.is_empty() {
                    "none: its ports come from a range".to_owned()
                } else {
                    port_names
                        .iter()
                        .map(|n| format!("{PORT_PREFIX}{n}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                return Err(bad(key, format!("no such port (named ports: {known})")));
            }
            let port = raw
                .parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .ok_or_else(|| bad(key, format!("{raw:?} is not a port (1-65535)")))?;
            changes.ports.insert(name.to_owned(), port);
            continue;
        }
        let setting = manifest
            .spec
            .config
            .quick_settings
            .iter()
            .find(|s| s.key == *key)
            .ok_or_else(|| {
                let keys: Vec<&str> = manifest
                    .spec
                    .config
                    .quick_settings
                    .iter()
                    .map(|s| s.key.as_str())
                    .collect();
                bad(
                    key,
                    format!(
                        "not a quick setting of {} (keys: {})",
                        manifest.metadata.id,
                        if keys.is_empty() {
                            "none".to_owned()
                        } else {
                            keys.join(", ")
                        }
                    ),
                )
            })?;
        let value = if raw.is_empty() {
            None
        } else {
            Some(canonical(setting, raw)?)
        };
        changes.settings.insert(key.clone(), value);
    }
    Ok(changes)
}

/// Keys whose **effective** value differs between `before` and `after` (stored maps).
pub fn changed_keys(
    manifest: &ToolManifest,
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<String> {
    let map = |stored| -> BTreeMap<String, String> {
        effective(manifest, stored)
            .into_iter()
            .map(|(s, t)| (s.key.clone(), t))
            .collect()
    };
    let (old, new) = (map(before), map(after));
    let keys: std::collections::BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    keys.into_iter()
        .filter(|k| old.get(*k) != new.get(*k))
        .cloned()
        .collect()
}

/// What applying `keys` takes: `restart` when any needs it, else `reload` when any needs
/// it, else `none`.
pub fn apply_mode(manifest: &ToolManifest, keys: &[String]) -> Apply {
    let modes = manifest
        .spec
        .config
        .quick_settings
        .iter()
        .filter(|s| keys.contains(&s.key))
        .map(|s| s.apply);
    let mut out = Apply::None;
    for mode in modes {
        match mode {
            Apply::Restart => return Apply::Restart,
            Apply::Reload => out = Apply::Reload,
            Apply::None => {}
        }
    }
    out
}

fn format_of(format: ConfigFormat) -> Option<SettingsFormat> {
    match format {
        ConfigFormat::Ini => Some(SettingsFormat::Ini),
        ConfigFormat::ConfDirective => Some(SettingsFormat::ConfDirective),
        ConfigFormat::Yaml => Some(SettingsFormat::Yaml),
        ConfigFormat::Json => Some(SettingsFormat::Json),
        ConfigFormat::Args => Some(SettingsFormat::Args),
        ConfigFormat::Text => None,
    }
}

/// The files to write and what goes into each: every `config.files` entry that some
/// quick setting with a value writes, with its path (and overlay) substituted by `vars`,
/// and its assignments in manifest order (each setting, then its `also_set` targets with
/// `{{ value }}` = the written text).
pub fn plans(
    manifest: &ToolManifest,
    stored: &BTreeMap<String, String>,
    vars: &RuntimeVars,
) -> Result<Vec<SettingsFile>> {
    let values = effective(manifest, stored);
    let mut out: Vec<SettingsFile> = Vec::new();
    for file in &manifest.spec.config.files {
        let mine: Vec<&(&QuickSetting, String)> = values
            .iter()
            .filter(|(s, _)| s.file.as_deref() == Some(file.id.as_str()))
            .collect();
        if mine.is_empty() {
            continue;
        }
        let format = format_of(file.format).ok_or_else(|| {
            ForgeError::ConfigInvalid(format!(
                "{}: config file `{}` is `text`, which has no quick settings",
                manifest.metadata.id, file.id
            ))
        })?;
        let mut assignments = Vec::new();
        for (setting, text) in mine {
            let value = written(setting, text);
            let shown = written_text(&value);
            assignments.push(Assignment::new(setting.target(), value.clone()));
            for (target, template) in &setting.also_set {
                let also = if template.trim().replace(' ', "") == "{{value}}" {
                    value.clone()
                } else {
                    SettingValue::Text(vars.with_value(shown.clone()).substitute(template)?)
                };
                assignments.push(Assignment::new(target.clone(), also));
            }
        }
        out.push(SettingsFile {
            id: file.id.clone(),
            path: PathBuf::from(vars.substitute(&file.path)?),
            overlay: file
                .overlay
                .as_deref()
                .map(|o| vars.substitute(o).map(PathBuf::from))
                .transpose()?,
            format,
            assignments,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn manifest(name: &str) -> ToolManifest {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../registry/tools/{name}.yaml")),
        )
        .unwrap();
        match forge_manifest::load_str(&text).unwrap() {
            forge_manifest::Manifest::Tool(t) => t,
            _ => unreachable!(),
        }
    }

    fn input(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn php_vars() -> RuntimeVars {
        let root = forge_core::paths::ForgeRoot::new(r"C:\fb").unwrap();
        RuntimeVars::new(forge_provider::layout::TemplateVars {
            tool: "php".into(),
            version: "8.4.27".into(),
            series: "8.4".into(),
            canonical_name: "8.4.27-nts-vs17-x64".into(),
            variant: BTreeMap::new(),
            paths: forge_provider::layout::PathVars::new(
                &root,
                std::path::Path::new("C:/fb/bin/php/8.4.27-nts-vs17-x64"),
            ),
        })
    }

    #[test]
    fn php_describes_its_five_settings_with_typed_defaults() {
        let php = manifest("php");
        let list = describe(&php, &BTreeMap::new());
        let keys: Vec<&str> = list.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "memory_limit",
                "upload_max_filesize",
                "max_execution_time",
                "display_errors",
                "opcache"
            ]
        );
        assert_eq!(list[0].kind, QuickSettingKind::Select);
        assert_eq!(list[0].options, ["128M", "256M", "512M", "1G", "-1"]);
        assert_eq!(list[0].value, Some(SettingValueDto::Text("512M".into())));
        assert!(list[0].is_default);
        assert_eq!(list[1].also_set, ["post_max_size"]);
        assert_eq!(list[2].value, Some(SettingValueDto::Int(120)));
        assert_eq!(list[3].value, Some(SettingValueDto::Bool(true)));
        assert_eq!(list[4].value, None, "opcache has no default");
        assert_eq!(list[4].apply, SettingApply::Reload);
        let stored = BTreeMap::from([("memory_limit".to_owned(), "1G".to_owned())]);
        let list = describe(&php, &stored);
        assert_eq!(list[0].value, Some(SettingValueDto::Text("1G".into())));
        assert_eq!(list[0].default, Some(SettingValueDto::Text("512M".into())));
        assert!(!list[0].is_default);
    }

    #[test]
    fn values_are_validated_and_canonical() {
        let php = manifest("php");
        let ok = validate(
            &php,
            &input(&[
                ("memory_limit", "1g"),
                ("display_errors", "off"),
                ("max_execution_time", "300.0"),
                ("opcache", ""),
            ]),
            &[],
        )
        .unwrap();
        assert_eq!(
            ok.settings,
            BTreeMap::from([
                ("display_errors".to_owned(), Some("false".to_owned())),
                ("max_execution_time".to_owned(), Some("300".to_owned())),
                ("memory_limit".to_owned(), Some("1G".to_owned())),
                ("opcache".to_owned(), None),
            ])
        );
        for (pairs, key) in [
            (vec![("memory_limit", "3G")], "memory_limit"),
            (vec![("max_execution_time", "-1")], "max_execution_time"),
            (vec![("max_execution_time", "abc")], "max_execution_time"),
            (vec![("display_errors", "maybe")], "display_errors"),
            (vec![("upload_max_filesize", "1\nG")], "upload_max_filesize"),
            (vec![("nope", "1")], "nope"),
            (vec![("ports.main", "3306")], "ports.main"),
            (
                vec![("memory_limit", "1G"), ("max_execution_time", "x")],
                "max_execution_time",
            ),
        ] {
            let err = validate(&php, &input(&pairs), &[]).unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{pairs:?}");
            assert!(err.to_string().contains(key), "{pairs:?}: {err}");
        }
        assert!(validate(&php, &BTreeMap::new(), &[]).is_err());
        let mysql = manifest("mysql");
        let ports = ["main".to_owned()];
        let ok = validate(&mysql, &input(&[("ports.main", "3307")]), &ports).unwrap();
        assert_eq!(ok.ports["main"], 3307);
        for bad_port in ["0", "65536", "x"] {
            let err = validate(&mysql, &input(&[("ports.main", bad_port)]), &ports).unwrap_err();
            assert!(err.to_string().contains("ports.main"), "{err}");
        }
        let err = validate(&mysql, &input(&[("ports.http", "1")]), &ports).unwrap_err();
        assert!(
            err.to_string().contains("ports.main"),
            "lists the names: {err}"
        );
    }

    #[test]
    fn number_bounds_steps_and_multi_order() {
        let mut php = manifest("php");
        let qs = &mut php.spec.config.quick_settings;
        let mut slider = qs[2].clone();
        slider.key = "level".into();
        slider.kind = QuickSettingType::Slider;
        slider.min = Some(0.0);
        slider.max = Some(1.0);
        slider.step = Some(0.25);
        assert_eq!(canonical(&slider, "0.5").unwrap(), "0.5");
        assert_eq!(canonical(&slider, "1").unwrap(), "1");
        assert!(canonical(&slider, "0.3").is_err());
        assert!(canonical(&slider, "1.25").is_err());
        let mut multi = qs[0].clone();
        multi.key = "mode".into();
        multi.kind = QuickSettingType::Multi;
        multi.options = vec!["debug".into(), "develop".into(), "trace".into()];
        multi.default = Some(serde_json::json!(["develop", "debug"]));
        assert_eq!(
            canonical(&multi, "trace, debug,debug").unwrap(),
            "debug,trace"
        );
        assert_eq!(default_text(&multi).unwrap(), "debug,develop");
        assert!(canonical(&multi, "debug,nope").is_err());
        assert_eq!(
            written(&multi, "debug,trace"),
            SettingValue::List(vec!["debug".into(), "trace".into()])
        );
        let mut port = qs[2].clone();
        port.kind = QuickSettingType::Port;
        port.min = None;
        assert_eq!(canonical(&port, "9003").unwrap(), "9003");
        assert!(canonical(&port, "70000").is_err());
        let mut path = qs[2].clone();
        path.kind = QuickSettingType::Path;
        path.min = None;
        assert_eq!(canonical(&path, r"C:\x\y").unwrap(), "C:/x/y");
    }

    #[test]
    fn php_plan_goes_to_the_overlay_with_also_set() {
        let php = manifest("php");
        let stored = BTreeMap::from([
            ("memory_limit".to_owned(), "1G".to_owned()),
            ("upload_max_filesize".to_owned(), "256M".to_owned()),
            ("display_errors".to_owned(), "false".to_owned()),
        ]);
        let plans = plans(&php, &stored, &php_vars()).unwrap();
        assert_eq!(plans.len(), 1);
        let plan = &plans[0];
        assert_eq!(plan.path, PathBuf::from("C:/fb/etc/php/8.4/php.ini"));
        assert_eq!(
            plan.overlay.as_deref(),
            Some(std::path::Path::new(
                "C:/fb/etc/php/8.4/conf.d/90-forgebox-settings.ini"
            ))
        );
        let body = forge_render::settings::apply(plan.format, "", &plan.assignments).unwrap();
        assert_eq!(
            body,
            "memory_limit = 1G\nupload_max_filesize = 256M\npost_max_size = 256M\nmax_execution_time = 120\ndisplay_errors = Off\n"
        );
        let ctx = context(&php, &stored);
        assert_eq!(ctx["display_errors"], "Off");
        assert_eq!(ctx["memory_limit"], "1G");
        assert!(!ctx.contains_key("opcache"));
    }

    #[test]
    fn changed_keys_and_apply_mode() {
        let nginx = manifest("nginx");
        let before = BTreeMap::new();
        // Storing the default changes nothing effective.
        let same = BTreeMap::from([("keepalive_timeout".to_owned(), "65".to_owned())]);
        assert!(changed_keys(&nginx, &before, &same).is_empty());
        let after = BTreeMap::from([
            ("keepalive_timeout".to_owned(), "30".to_owned()),
            ("client_max_body_size".to_owned(), "1G".to_owned()),
        ]);
        let keys = changed_keys(&nginx, &before, &after);
        assert_eq!(keys, ["client_max_body_size", "keepalive_timeout"]);
        assert_eq!(apply_mode(&nginx, &keys), Apply::Reload);
        let restart = BTreeMap::from([("worker_processes".to_owned(), "2".to_owned())]);
        let keys = changed_keys(&nginx, &before, &restart);
        assert_eq!(apply_mode(&nginx, &keys), Apply::Restart);
        assert_eq!(apply_mode(&nginx, &[]), Apply::None);
    }

    #[test]
    fn mysql_select_numbers_and_sections() {
        let mysql = manifest("mysql");
        let stored = BTreeMap::from([
            ("innodb_flush_log_at_trx_commit".to_owned(), "1".to_owned()),
            ("max_allowed_packet".to_owned(), "1G".to_owned()),
        ]);
        let root = forge_core::paths::ForgeRoot::new(r"C:\fb").unwrap();
        let mut vars = RuntimeVars::new(forge_provider::layout::TemplateVars {
            tool: "mysql".into(),
            version: "8.4.6".into(),
            series: "8.4".into(),
            canonical_name: "8.4.6".into(),
            variant: BTreeMap::new(),
            paths: forge_provider::layout::PathVars::new(
                &root,
                std::path::Path::new("C:/fb/bin/mysql/8.4.6"),
            ),
        });
        vars.instance = Some(crate::vars::InstanceVars {
            name: "default".into(),
            ..Default::default()
        });
        let plans = plans(&mysql, &stored, &vars).unwrap();
        assert_eq!(
            plans[0].path,
            PathBuf::from("C:/fb/etc/mysql/default/my.ini")
        );
        assert_eq!(plans[0].overlay, None);
        let targets: Vec<(&str, &SettingValue)> = plans[0]
            .assignments
            .iter()
            .map(|a| (a.target.as_str(), &a.value))
            .collect();
        assert_eq!(
            targets,
            [
                (
                    "mysqld/max_connections",
                    &SettingValue::Number("200".into())
                ),
                (
                    "mysqld/innodb_buffer_pool_size",
                    &SettingValue::Text("256M".into())
                ),
                (
                    "mysqld/max_allowed_packet",
                    &SettingValue::Text("1G".into())
                ),
                (
                    "mysqldump/max_allowed_packet",
                    &SettingValue::Text("1G".into())
                ),
                (
                    "mysqld/innodb_flush_log_at_trx_commit",
                    &SettingValue::Number("1".into())
                ),
            ]
        );
    }
}
