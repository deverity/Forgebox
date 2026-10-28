//! Rust-side i18n for the native surfaces (tray menu, notifications).
//!
//! The `tray` and `notifications` namespaces are shared with the frontend: the same JSON files
//! under `src/i18n/<locale>/` are embedded with `include_str!`, with `{var}` interpolation (same
//! syntax as the i18next config). Everything else is localized by the UI from codes and details;
//! API messages are never translated (docs/07 §9).

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use serde_json::Value;

/// Shipped locales. The default is pt-BR (CLAUDE.md, docs/07 §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    PtBr,
    EnUs,
}

impl Language {
    pub const DEFAULT: Language = Language::PtBr;

    /// Parses a BCP 47 tag such as `pt-BR`, `en-US` or `en`; unknown tags yield `None`.
    pub fn parse(tag: &str) -> Option<Language> {
        let lower = tag.to_ascii_lowercase();
        if lower.starts_with("pt") {
            Some(Language::PtBr)
        } else if lower.starts_with("en") {
            Some(Language::EnUs)
        } else {
            None
        }
    }

    pub fn tag(self) -> &'static str {
        match self {
            Language::PtBr => "pt-BR",
            Language::EnUs => "en-US",
        }
    }
}

const TRAY_PT: &str = include_str!("../../src/i18n/pt-BR/tray.json");
const TRAY_EN: &str = include_str!("../../src/i18n/en-US/tray.json");
const NOTIFICATIONS_PT: &str = include_str!("../../src/i18n/pt-BR/notifications.json");
const NOTIFICATIONS_EN: &str = include_str!("../../src/i18n/en-US/notifications.json");

/// Namespaces Rust may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Namespace {
    Tray,
    Notifications,
}

fn source(ns: Namespace, lang: Language) -> &'static str {
    match (ns, lang) {
        (Namespace::Tray, Language::PtBr) => TRAY_PT,
        (Namespace::Tray, Language::EnUs) => TRAY_EN,
        (Namespace::Notifications, Language::PtBr) => NOTIFICATIONS_PT,
        (Namespace::Notifications, Language::EnUs) => NOTIFICATIONS_EN,
    }
}

type Table = HashMap<String, String>;

fn flatten(prefix: &str, value: &Value, out: &mut Table) {
    match value {
        Value::String(s) => {
            out.insert(prefix.to_owned(), s.clone());
        }
        Value::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&key, v, out);
            }
        }
        _ => {}
    }
}

fn table(ns: Namespace, lang: Language) -> &'static Table {
    static TABLES: OnceLock<HashMap<(Namespace, Language), Table>> = OnceLock::new();
    let all = TABLES.get_or_init(|| {
        let mut all = HashMap::new();
        for ns in [Namespace::Tray, Namespace::Notifications] {
            for lang in [Language::PtBr, Language::EnUs] {
                // The files are embedded at compile time and validated by the parity tests.
                let json: Value =
                    serde_json::from_str(source(ns, lang)).expect("embedded i18n JSON");
                let mut t = Table::new();
                flatten("", &json, &mut t);
                all.insert((ns, lang), t);
            }
        }
        all
    });
    &all[&(ns, lang)]
}

/// Replaces every `{name}` with its value; unknown placeholders are left untouched.
pub fn interpolate(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for (name, value) in vars {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// Translates `key` in `ns` for `lang`, falling back to the default language and then to the key.
pub fn translate(lang: Language, ns: Namespace, key: &str, vars: &[(&str, &str)]) -> String {
    let text = table(ns, lang)
        .get(key)
        .or_else(|| table(ns, Language::DEFAULT).get(key))
        .map_or(key, String::as_str);
    interpolate(text, vars)
}

/// Process-wide current language, set by the frontend through `set_language`.
///
/// A `std::sync::RwLock` is fine here: it is never held across an `.await` (CLAUDE.md).
fn current() -> &'static RwLock<Language> {
    static CURRENT: OnceLock<RwLock<Language>> = OnceLock::new();
    CURRENT.get_or_init(|| RwLock::new(Language::DEFAULT))
}

pub fn current_language() -> Language {
    *current()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn set_current_language(lang: Language) {
    *current()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = lang;
}

/// Translates with the current language (tray and notification builders call this).
pub fn t(ns: Namespace, key: &str, vars: &[(&str, &str)]) -> String {
    translate(current_language(), ns, key, vars)
}

/// Keeps Rust in sync with the UI language. Rejects tags that are not shipped locales.
#[tauri::command]
pub fn set_language(language: String) -> Result<(), String> {
    let lang =
        Language::parse(&language).ok_or_else(|| format!("unsupported language: {language}"))?;
    set_current_language(lang);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keys the Rust side uses today. Every one must resolve in both locales.
    const USED_KEYS: &[(Namespace, &str)] = &[
        (Namespace::Tray, "open"),
        (Namespace::Tray, "start_all"),
        (Namespace::Tray, "stop_all"),
        (Namespace::Tray, "services_running"),
        (Namespace::Tray, "quit"),
        (Namespace::Notifications, "service_crashed"),
        (Namespace::Notifications, "service_restarted"),
        (Namespace::Notifications, "update_available"),
    ];

    #[test]
    fn used_keys_resolve_in_both_locales() {
        for lang in [Language::PtBr, Language::EnUs] {
            for (ns, key) in USED_KEYS {
                assert!(
                    table(*ns, lang).contains_key(*key),
                    "{lang:?} {ns:?} missing {key}"
                );
            }
        }
    }

    #[test]
    fn locales_have_identical_keys_and_placeholders() {
        fn placeholders(s: &str) -> Vec<String> {
            let mut v: Vec<String> = s
                .split('{')
                .skip(1)
                .filter_map(|p| p.split_once('}').map(|(n, _)| n.to_owned()))
                .collect();
            v.sort();
            v
        }
        for ns in [Namespace::Tray, Namespace::Notifications] {
            let (pt, en) = (table(ns, Language::PtBr), table(ns, Language::EnUs));
            let mut a: Vec<_> = pt.keys().collect();
            let mut b: Vec<_> = en.keys().collect();
            a.sort();
            b.sort();
            assert_eq!(a, b, "{ns:?}");
            for (k, v) in pt {
                assert_eq!(placeholders(v), placeholders(&en[k]), "{ns:?} {k}");
            }
        }
    }

    #[test]
    fn interpolates_variables() {
        let pt = translate(
            Language::PtBr,
            Namespace::Tray,
            "services_running",
            &[("running", "3"), ("total", "5")],
        );
        assert_eq!(pt, "3 de 5 serviços em execução");
        let en = translate(
            Language::EnUs,
            Namespace::Notifications,
            "service_crashed",
            &[("service", "nginx")],
        );
        assert_eq!(en, "nginx stopped unexpectedly");
    }

    #[test]
    fn unknown_key_falls_back_to_the_key() {
        assert_eq!(
            translate(Language::EnUs, Namespace::Tray, "nope", &[]),
            "nope"
        );
    }

    #[test]
    fn set_language_switches_and_rejects_unknown_tags() {
        assert!(set_language("fr-FR".into()).is_err());
        set_language("en-US".into()).unwrap();
        assert_eq!(current_language(), Language::EnUs);
        assert_eq!(t(Namespace::Tray, "quit", &[]), "Quit");
        set_language("pt-BR".into()).unwrap();
        assert_eq!(t(Namespace::Tray, "quit", &[]), "Sair");
    }

    #[test]
    fn parses_tags() {
        assert_eq!(Language::parse("pt-BR"), Some(Language::PtBr));
        assert_eq!(Language::parse("en"), Some(Language::EnUs));
        assert_eq!(Language::parse("de"), None);
        assert_eq!(Language::PtBr.tag(), "pt-BR");
    }
}
