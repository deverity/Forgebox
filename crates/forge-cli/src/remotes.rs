//! Connection profiles (`forge remote add|list|use|remove`, `--context`). They live on the
//! client's machine, never in the daemon (docs/09): `%LOCALAPPDATA%\Forgebox\remotes.toml`
//! (`FORGE_CONFIG_DIR` overrides the folder).
//!
//! The pairing code (`--code`) is single use and is never stored. Connecting to a remote
//! daemon is remote mode (M6-E3): until then a profile can be saved and selected, and
//! commands that would use it say so instead of silently talking to the local daemon.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{CliError, CliResult};

/// The implicit profile: the daemon of this machine.
pub const LOCAL: &str = "local";

/// One saved connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// `wss://10.0.0.5:7788` or `ssh://user@host`.
    pub url: String,
}

/// The saved profiles and the selected one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remotes {
    /// Profile used when `--context` is absent (`None` = local).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

/// Folder of the client's files.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FORGE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir).join("Forgebox");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home).join(".config").join("forgebox");
    }
    PathBuf::from(".forgebox")
}

fn file() -> PathBuf {
    config_dir().join("remotes.toml")
}

impl Remotes {
    /// Reads the profiles; a missing file is an empty set.
    pub fn load() -> CliResult<Self> {
        let path = file();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text))
                .map_err(|e| CliError::new("config_invalid", format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(CliError::io(format!("{}: {e}", path.display()))),
        }
    }

    /// Writes the profiles (temporary file + rename).
    pub fn save(&self) -> CliResult<()> {
        let path = file();
        let dir = path.parent().unwrap_or(std::path::Path::new("."));
        std::fs::create_dir_all(dir)
            .map_err(|e| CliError::io(format!("{}: {e}", dir.display())))?;
        let text = toml::to_string_pretty(self)
            .map_err(|e| CliError::new("internal", format!("remotes.toml: {e}")))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| CliError::io(format!("{}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &path).map_err(|e| CliError::io(format!("{}: {e}", path.display())))
    }

    /// The profile in effect: `--context`, else the selected one; `None` = local.
    pub fn effective<'a>(&'a self, context: Option<&'a str>) -> Option<&'a str> {
        context
            .or(self.current.as_deref())
            .filter(|name| *name != LOCAL)
    }

    /// Adds or replaces a profile.
    pub fn add(&mut self, name: &str, url: &str) -> CliResult<()> {
        validate_name(name)?;
        validate_url(url)?;
        self.profiles.insert(
            name.to_owned(),
            Profile {
                url: url.to_owned(),
            },
        );
        Ok(())
    }

    /// Selects a profile (`local` goes back to this machine).
    pub fn select(&mut self, name: &str) -> CliResult<()> {
        if name == LOCAL {
            self.current = None;
            return Ok(());
        }
        if !self.profiles.contains_key(name) {
            return Err(CliError::new(
                "not_found",
                format!("perfil {name:?} não existe (veja `forge remote list`)"),
            ));
        }
        self.current = Some(name.to_owned());
        Ok(())
    }

    /// Removes a profile; the selection falls back to local.
    pub fn remove(&mut self, name: &str) -> CliResult<()> {
        if self.profiles.remove(name).is_none() {
            return Err(CliError::new(
                "not_found",
                format!("perfil {name:?} não existe (veja `forge remote list`)"),
            ));
        }
        if self.current.as_deref() == Some(name) {
            self.current = None;
        }
        Ok(())
    }
}

fn validate_name(name: &str) -> CliResult<()> {
    let ok = !name.is_empty()
        && name.len() <= 40
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if !ok || name == LOCAL {
        return Err(CliError::usage(format!(
            "nome de perfil inválido: {name:?} (letras minúsculas, números, - e _; `{LOCAL}` é reservado)"
        )));
    }
    Ok(())
}

fn validate_url(url: &str) -> CliResult<()> {
    let rest = ["wss://", "ws://", "ssh://"]
        .iter()
        .find_map(|scheme| url.strip_prefix(scheme));
    match rest {
        Some(rest) if !rest.is_empty() && !rest.contains(char::is_whitespace) => Ok(()),
        _ => Err(CliError::usage(format!(
            "endereço remoto inválido: {url:?} (use wss://host:porta ou ssh://usuario@host)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_select_remove() {
        let mut remotes = Remotes::default();
        remotes.add("hml", "wss://10.0.0.5:7788").unwrap();
        remotes.add("vps", "ssh://dev@host").unwrap();
        assert!(remotes.select("nope").is_err());
        remotes.select("hml").unwrap();
        assert_eq!(remotes.effective(None), Some("hml"));
        assert_eq!(remotes.effective(Some("vps")), Some("vps"));
        assert_eq!(remotes.effective(Some("local")), None);
        remotes.select("local").unwrap();
        assert_eq!(remotes.effective(None), None);
        remotes.select("vps").unwrap();
        remotes.remove("vps").unwrap();
        assert_eq!(remotes.current, None);
        assert_eq!(remotes.remove("vps").unwrap_err().exit, 3);
    }

    #[test]
    fn names_and_urls_are_validated() {
        let mut remotes = Remotes::default();
        for bad in ["", "Local", "local", "a b", "x/y"] {
            assert!(remotes.add(bad, "wss://h:1").is_err(), "{bad:?}");
        }
        for bad in ["http://h", "wss://", "h:1", "wss://a b"] {
            assert!(remotes.add("ok", bad).is_err(), "{bad:?}");
        }
        assert!(remotes.profiles.is_empty());
    }

    #[test]
    fn toml_round_trip() {
        let mut remotes = Remotes::default();
        remotes.add("hml", "wss://10.0.0.5:7788").unwrap();
        remotes.select("hml").unwrap();
        let text = toml::to_string_pretty(&remotes).unwrap();
        assert_eq!(toml::from_str::<Remotes>(&text).unwrap(), remotes);
    }
}
