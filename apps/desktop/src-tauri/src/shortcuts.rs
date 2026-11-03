//! Global (system-wide) shortcuts: `Ctrl+Alt+T` and `` Ctrl+` `` open a terminal even when the
//! window is not focused (docs/07 §8, M2-E1-T14).
//!
//! The webview decides *whether* they exist: it asks for registration only when the daemon
//! reports `terminal.open` as implemented (`hello.not_implemented`), and for none otherwise.
//! Registration is best effort: a chord another program already owns is reported back in
//! [`SyncReport::failed`] and never aborts anything. A press is forwarded to the webview as
//! the [`EVENT`] event; the webview runs the `terminal.open` action (so the job and error
//! semantics stay in one place). The in-app shortcuts live in the webview, not here.

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use forge_ipc::ApiError;

/// Event the webview listens to; the payload is the chord as the webview wrote it.
pub const EVENT: &str = "forgebox://global-shortcut";

/// More chords than this are refused (only the terminal ones exist).
pub const MAX_CHORDS: usize = 4;

/// A chord that could not be registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failure {
    pub accelerator: String,
    pub error: String,
}

/// What a [`sync`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    pub registered: Vec<String>,
    pub failed: Vec<Failure>,
}

/// The system-wide registration, behind a trait so the policy is testable without a desktop.
pub trait Backend {
    fn register(&self, chord: &str, accelerator: &str) -> Result<(), String>;
    fn unregister_all(&self) -> Result<(), String>;
}

/// Converts the webview's chord (`Ctrl+Alt+T`, `` Ctrl+` ``) to the plugin's accelerator
/// syntax. Only chords with Ctrl or Alt are accepted: a bare key must never be global.
pub fn to_accelerator(chord: &str) -> Result<String, String> {
    let parts: Vec<&str> = chord.split('+').collect();
    let Some((key, modifiers)) = parts.split_last() else {
        return Err("empty chord".to_owned());
    };
    let mut out: Vec<String> = Vec::new();
    for modifier in modifiers {
        out.push(
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => "Control",
                "alt" => "Alt",
                "shift" => "Shift",
                other => return Err(format!("unknown modifier `{other}`")),
            }
            .to_owned(),
        );
    }
    if !out.iter().any(|m| m == "Control" || m == "Alt") {
        return Err("a global shortcut needs Ctrl or Alt".to_owned());
    }
    let key = match *key {
        "`" => "Backquote".to_owned(),
        "," => "Comma".to_owned(),
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphabetic()) => {
            format!("Key{}", k.to_ascii_uppercase())
        }
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_digit()) => format!("Digit{k}"),
        other => return Err(format!("unsupported key `{other}`")),
    };
    out.push(key);
    Ok(out.join("+"))
}

/// Replaces the registered chords: everything is unregistered first; with `enabled == false`
/// nothing is registered. Failures are collected, never raised.
pub fn sync(backend: &dyn Backend, enabled: bool, chords: &[String]) -> SyncReport {
    let mut report = SyncReport::default();
    if let Err(error) = backend.unregister_all() {
        tracing::warn!(%error, "unregistering the global shortcuts failed");
    }
    if !enabled {
        return report;
    }
    for (index, chord) in chords.iter().enumerate() {
        let result = if index >= MAX_CHORDS {
            Err("too many global shortcuts".to_owned())
        } else {
            to_accelerator(chord).and_then(|acc| backend.register(chord, &acc))
        };
        match result {
            Ok(()) => report.registered.push(chord.clone()),
            Err(error) => {
                tracing::warn!(chord = %chord, %error, "global shortcut not registered");
                report.failed.push(Failure {
                    accelerator: chord.clone(),
                    error,
                });
            }
        }
    }
    report
}

struct TauriBackend(AppHandle);

impl Backend for TauriBackend {
    fn register(&self, chord: &str, accelerator: &str) -> Result<(), String> {
        let chord = chord.to_owned();
        self.0
            .global_shortcut()
            .on_shortcut(accelerator, move |app, _shortcut, event| {
                if event.state() == ShortcutState::Pressed {
                    let _ = app.emit(EVENT, &chord);
                }
            })
            .map_err(|e| e.to_string())
    }

    fn unregister_all(&self) -> Result<(), String> {
        self.0
            .global_shortcut()
            .unregister_all()
            .map_err(|e| e.to_string())
    }
}

/// Registers the global chords the webview asks for (none when `enabled` is false).
/// Never fails because of a chord: see [`SyncReport::failed`].
#[tauri::command]
pub async fn shortcuts_sync(
    app: AppHandle,
    enabled: bool,
    chords: Vec<String>,
) -> Result<SyncReport, ApiError> {
    // The registration talks to the hotkey thread synchronously: off the async workers.
    tokio::task::spawn_blocking(move || sync(&TauriBackend(app), enabled, &chords))
        .await
        .map_err(|e| ApiError::new("internal", format!("global shortcuts: {e}")))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    struct Fake {
        taken: Vec<&'static str>,
        registered: RefCell<Vec<String>>,
        cleared: RefCell<u32>,
    }

    impl Backend for Fake {
        fn register(&self, chord: &str, accelerator: &str) -> Result<(), String> {
            if self.taken.contains(&chord) {
                return Err("already registered by another program".to_owned());
            }
            self.registered.borrow_mut().push(accelerator.to_owned());
            Ok(())
        }
        fn unregister_all(&self) -> Result<(), String> {
            *self.cleared.borrow_mut() += 1;
            Ok(())
        }
    }

    fn chords() -> Vec<String> {
        vec!["Ctrl+Alt+T".to_owned(), "Ctrl+`".to_owned()]
    }

    #[test]
    fn accelerators_use_the_plugin_syntax() {
        assert_eq!(to_accelerator("Ctrl+Alt+T").unwrap(), "Control+Alt+KeyT");
        assert_eq!(to_accelerator("Ctrl+`").unwrap(), "Control+Backquote");
        assert_eq!(
            to_accelerator("Ctrl+Shift+1").unwrap(),
            "Control+Shift+Digit1"
        );
        assert!(to_accelerator("T").is_err(), "bare key");
        assert!(to_accelerator("Shift+T").is_err(), "shift only");
        assert!(to_accelerator("Ctrl+F13x").is_err());
    }

    #[test]
    fn nothing_is_registered_while_disabled() {
        // terminal.open not implemented: the webview asks with enabled = false.
        let fake = Fake::default();
        let report = sync(&fake, false, &chords());
        assert_eq!(report, SyncReport::default());
        assert!(fake.registered.borrow().is_empty());
        assert_eq!(*fake.cleared.borrow(), 1, "stale registrations are dropped");
    }

    #[test]
    fn enabled_registers_every_chord() {
        let fake = Fake::default();
        let report = sync(&fake, true, &chords());
        assert_eq!(report.registered, chords());
        assert!(report.failed.is_empty());
        assert_eq!(
            *fake.registered.borrow(),
            ["Control+Alt+KeyT", "Control+Backquote"]
        );
    }

    #[test]
    fn a_registration_error_is_reported_and_not_fatal() {
        let fake = Fake {
            taken: vec!["Ctrl+Alt+T"],
            ..Fake::default()
        };
        let report = sync(&fake, true, &chords());
        assert_eq!(report.registered, ["Ctrl+`"]);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].accelerator, "Ctrl+Alt+T");
        assert!(report.failed[0].error.contains("another program"));
    }

    #[test]
    fn invalid_and_excess_chords_fail_individually() {
        let fake = Fake::default();
        let many: Vec<String> = ["Ctrl+A", "Ctrl+B", "Ctrl+C", "Ctrl+D", "Ctrl+E", "X"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let report = sync(&fake, true, &many);
        assert_eq!(report.registered.len(), 4);
        assert_eq!(report.failed.len(), 2);
    }
}
