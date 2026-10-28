//! GUI-only preferences ([ADR-0034] §5): `prefs.json` in the GUI data dir
//! ([`crate::data_dir`]), never the daemon's `forgebox.toml`.
//!
//! This module knows the keys the window shell needs ([`GuiPrefs`]: theme, materials,
//! close to tray) and keeps every other top-level key untouched, so later features
//! (language, shortcuts, muted notifications, onboarding progress...) store theirs in the
//! same file through the same two commands (`gui_prefs_get`, `gui_prefs_set`) without a
//! schema change here.
//!
//! Writes are atomic (temporary file + rename) and serialized by the store's lock, which
//! is only taken inside blocking code (`spawn_blocking`), never across an `.await`.
//!
//! [ADR-0034]: ../../../../docs/adr/0034-desktop-shell-architecture.md

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tauri::{AppHandle, State};

use forge_ipc::ApiError;

use crate::window::WindowShell;

/// Light/dark choice of the user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePref {
    /// Follow Windows ("Choose your default app mode").
    #[default]
    System,
    Light,
    Dark,
}

/// The user's switch for window materials (Mica/Acrylic).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VibrancyPref {
    /// The policy of `window::choose_effect` decides.
    #[default]
    Auto,
    /// Always opaque.
    Off,
}

/// Preferences of `Forgebox.exe`. Unknown keys are kept in [`GuiPrefs::other`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiPrefs {
    pub theme: ThemePref,
    pub vibrancy: VibrancyPref,
    /// Opt into Acrylic on Windows 10, which is opaque by default (ADR-0035: Acrylic lags
    /// while dragging/resizing there).
    pub win10_acrylic: bool,
    /// Closing the window hides it in the tray (docs/07 §4), when a tray exists.
    pub close_to_tray: bool,
    /// Keys owned by other features, preserved as they are.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

impl Default for GuiPrefs {
    fn default() -> Self {
        Self {
            theme: ThemePref::System,
            vibrancy: VibrancyPref::Auto,
            win10_acrylic: false,
            close_to_tray: true,
            other: Map::new(),
        }
    }
}

/// Why a preferences update was refused or not saved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrefsError {
    /// The merged document is not valid ([`GuiPrefs`] rejects a value).
    #[error("invalid preference: {0}")]
    Invalid(String),
    /// The file could not be written.
    #[error("could not save {path}: {message}")]
    Io { path: String, message: String },
}

impl PrefsError {
    /// Stable code for the webview (`ApiError`-like).
    pub fn code(&self) -> &'static str {
        match self {
            PrefsError::Invalid(_) => "invalid_input",
            PrefsError::Io { .. } => "io",
        }
    }
}

/// Applies `patch` (top-level keys; `null` removes a key, i.e. resets it to its default)
/// to `current`. Pure.
pub fn merge(current: &GuiPrefs, patch: &Map<String, Value>) -> Result<GuiPrefs, PrefsError> {
    let mut doc = match serde_json::to_value(current) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    for (key, value) in patch {
        if value.is_null() {
            doc.remove(key);
        } else {
            doc.insert(key.clone(), value.clone());
        }
    }
    serde_json::from_value(Value::Object(doc)).map_err(|e| PrefsError::Invalid(e.to_string()))
}

/// Parses a `prefs.json` text; anything unreadable yields the defaults (the next save
/// rewrites the file).
pub fn parse(text: &str) -> GuiPrefs {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    match serde_json::from_str::<GuiPrefs>(text) {
        Ok(prefs) => prefs,
        Err(err) => {
            tracing::warn!(%err, "prefs.json is not valid; using the defaults");
            GuiPrefs::default()
        }
    }
}

/// The preferences file and its current content.
#[derive(Debug)]
pub struct PrefsStore {
    file: PathBuf,
    current: Mutex<GuiPrefs>,
}

impl PrefsStore {
    /// Reads `file` (missing or invalid → defaults). Blocking.
    pub fn load(file: PathBuf) -> Self {
        let current = match std::fs::read_to_string(&file) {
            Ok(text) => parse(&text),
            Err(_) => GuiPrefs::default(),
        };
        Self {
            file,
            current: Mutex::new(current),
        }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// A copy of the current preferences.
    pub fn get(&self) -> GuiPrefs {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Merges `patch`, saves the file and returns `(before, after)`. Blocking (call from
    /// `spawn_blocking`); concurrent updates are serialized.
    pub fn update(&self, patch: &Map<String, Value>) -> Result<(GuiPrefs, GuiPrefs), PrefsError> {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let next = merge(&current, patch)?;
        write_atomic(&self.file, &next)?;
        let before = std::mem::replace(&mut *current, next.clone());
        Ok((before, next))
    }
}

/// Writes `prefs` as pretty JSON (UTF-8, LF, no BOM) through `<file>.tmp` + rename.
fn write_atomic(file: &Path, prefs: &GuiPrefs) -> Result<(), PrefsError> {
    let io = |e: std::io::Error| PrefsError::Io {
        path: file.display().to_string(),
        message: e.to_string(),
    };
    let mut bytes = serde_json::to_vec_pretty(prefs).map_err(|e| PrefsError::Io {
        path: file.display().to_string(),
        message: e.to_string(),
    })?;
    bytes.push(b'\n');
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes).map_err(io)?;
    std::fs::rename(&tmp, file).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io(e)
    })
}

impl From<PrefsError> for ApiError {
    fn from(err: PrefsError) -> Self {
        ApiError::new(err.code(), err.to_string())
    }
}

/// The GUI preferences (every key, including those of other features).
#[tauri::command]
pub async fn gui_prefs_get(shell: State<'_, Arc<WindowShell>>) -> Result<GuiPrefs, ApiError> {
    Ok(shell.prefs().get())
}

/// Merges `patch` (top-level keys; `null` resets one) into the GUI preferences, saves them
/// and applies what the window shell owns (theme, materials). Answers the new preferences;
/// `invalid_input` for a value of the wrong type, `io` when the file cannot be written.
#[tauri::command]
pub async fn gui_prefs_set(
    app: AppHandle,
    shell: State<'_, Arc<WindowShell>>,
    patch: Map<String, Value>,
) -> Result<GuiPrefs, ApiError> {
    let store = shell.prefs().clone();
    let (before, after) = tokio::task::spawn_blocking(move || store.update(&patch))
        .await
        .map_err(|e| ApiError::new("internal", format!("prefs update: {e}")))??;
    crate::window::on_prefs_changed(&app, &before, &after);
    Ok(after)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn obj(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => panic!("not an object"),
        }
    }

    #[test]
    fn defaults_follow_the_docs() {
        let prefs = GuiPrefs::default();
        assert_eq!(prefs.theme, ThemePref::System);
        assert_eq!(prefs.vibrancy, VibrancyPref::Auto);
        assert!(
            !prefs.win10_acrylic,
            "Windows 10 is opaque unless the user opts in"
        );
        assert!(
            prefs.close_to_tray,
            "docs/07 §4: close hides to the tray by default"
        );
        assert_eq!(
            serde_json::to_value(&prefs).unwrap(),
            json!({"theme": "system", "vibrancy": "auto", "win10_acrylic": false, "close_to_tray": true})
        );
    }

    #[test]
    fn unknown_keys_survive_a_round_trip_and_a_merge() {
        let prefs =
            parse(r#"{"theme":"dark","language":"en-US","shortcuts":{"palette":"Ctrl+P"}}"#);
        assert_eq!(prefs.theme, ThemePref::Dark);
        assert_eq!(prefs.other["language"], "en-US");
        let merged = merge(&prefs, &obj(json!({"vibrancy": "off"}))).unwrap();
        assert_eq!(merged.vibrancy, VibrancyPref::Off);
        assert_eq!(merged.theme, ThemePref::Dark);
        assert_eq!(merged.other["shortcuts"]["palette"], "Ctrl+P");
    }

    #[test]
    fn null_resets_a_key_and_bad_values_are_refused() {
        let prefs = merge(
            &GuiPrefs::default(),
            &obj(json!({"theme": "light", "close_to_tray": false, "language": "pt-BR"})),
        )
        .unwrap();
        let reset = merge(
            &prefs,
            &obj(json!({"theme": null, "close_to_tray": null, "language": null})),
        )
        .unwrap();
        assert_eq!(reset, GuiPrefs::default());

        let err = merge(&prefs, &obj(json!({"theme": "sepia"}))).unwrap_err();
        assert_eq!(err.code(), "invalid_input");
        let err = merge(&prefs, &obj(json!({"close_to_tray": "yes"}))).unwrap_err();
        assert!(matches!(err, PrefsError::Invalid(_)), "{err}");
    }

    #[test]
    fn invalid_files_load_as_defaults() {
        assert_eq!(parse("not json"), GuiPrefs::default());
        assert_eq!(parse(r#"{"theme": 3}"#), GuiPrefs::default());
        assert_eq!(parse("\u{feff}{\"theme\":\"dark\"}").theme, ThemePref::Dark);
    }

    #[test]
    fn the_store_saves_atomically_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("gui").join("prefs.json");
        let store = PrefsStore::load(file.clone());
        assert_eq!(store.get(), GuiPrefs::default());
        assert!(!file.exists(), "loading never writes");

        let (before, after) = store
            .update(&obj(json!({"win10_acrylic": true, "language": "en-US"})))
            .unwrap();
        assert!(!before.win10_acrylic);
        assert!(after.win10_acrylic);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.ends_with("}\n") && !text.contains('\r'), "{text:?}");
        assert!(!file.with_extension("json.tmp").exists());

        let again = PrefsStore::load(file);
        assert_eq!(again.get(), after);

        // A refused patch changes nothing.
        assert!(again.update(&obj(json!({"vibrancy": 1}))).is_err());
        assert_eq!(again.get(), after);
    }
}
