//! Minimal reader for the `.ini` files of Laragon (`usr/laragon.ini`).
//!
//! Sections in `[brackets]`, `key=value` lines, `;` and `#` comments. Keys are matched
//! case-insensitively (Laragon is Delphi: `Version` and `version` are the same key);
//! section names too. Lines Laragon pads with spaces, repeated sections and values with
//! `=` inside are accepted. Never fails: whatever is not understood is ignored.

use std::collections::BTreeMap;

/// Parsed file: `section (lowercase) → key (lowercase) → value (trimmed)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ini {
    sections: BTreeMap<String, BTreeMap<String, String>>,
}

impl Ini {
    /// Parses `text` (a UTF-8 BOM is skipped). Later duplicates of a key win.
    pub fn parse(text: &str) -> Self {
        let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut current = String::new();
        for line in text.trim_start_matches('\u{feff}').lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                current = name.trim().to_ascii_lowercase();
                sections.entry(current.clone()).or_default();
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                sections
                    .entry(current.clone())
                    .or_default()
                    .insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        Self { sections }
    }

    /// Value of `key` in `section`, when present and not empty.
    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections
            .get(&section.to_ascii_lowercase())?
            .get(&key.to_ascii_lowercase())
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// Names of the sections, lowercase.
    pub fn sections(&self) -> impl Iterator<Item = &str> {
        self.sections.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_shape_laragon_writes() {
        let ini = Ini::parse(
            "\u{feff}[preferences]\r\nDataDir=C:\\laragon\\data\r\nHostnameFormat={name}.test\r\n \r\n\
             ; comment\r\n[PHP]\r\nVersion=php-8.2-Win32-vs16-x64\r\nQuickSettings=a, b\r\n\
             [mysql]\r\nVersion = mysql-8.0.46-winx64 \r\nEmpty=\r\n",
        );
        assert_eq!(ini.get("preferences", "datadir"), Some(r"C:\laragon\data"));
        assert_eq!(
            ini.get("Preferences", "HostnameFormat"),
            Some("{name}.test")
        );
        assert_eq!(ini.get("php", "version"), Some("php-8.2-Win32-vs16-x64"));
        assert_eq!(ini.get("mysql", "version"), Some("mysql-8.0.46-winx64"));
        assert_eq!(ini.get("mysql", "empty"), None);
        assert_eq!(ini.get("nginx", "version"), None);
        assert_eq!(ini.sections().count(), 3);
    }

    #[test]
    fn garbage_is_ignored() {
        let ini = Ini::parse("no section here\nkey=value\n[x\n=\n");
        assert_eq!(ini.get("", "key"), Some("value"));
        assert_eq!(ini.get("x", "key"), None);
    }
}
