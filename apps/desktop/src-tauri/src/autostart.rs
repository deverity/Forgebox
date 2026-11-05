//! Start with Windows, minimized (docs/07 §7, [ADR-0038]).
//!
//! `tauri-plugin-autostart` writes `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`; the
//! command line it stores carries [`AUTOSTART_ARGS`], which makes `Forgebox.exe` start in the
//! tray without a window (`window::LaunchArgs::minimized`). Everything goes through the
//! [`AutostartBackend`] trait so no test ever touches the real registry.
//!
//! In portable mode (docs/08 §2.2) autostart is refused with a localized reason: a portable
//! folder must leave no traces outside itself, and the Run value would point into a folder
//! the user may move or delete.
//!
//! The registry calls are blocking; the commands run them in `spawn_blocking`.
//!
//! [ADR-0038]: ../../../../docs/adr/0038-gui-autostart-updater.md

use std::sync::Arc;

use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Runtime, State};
use tauri_plugin_autostart::ManagerExt;

use forge_ipc::ApiError;

use crate::data_dir::GuiDataSource;
use crate::i18n::{self, Namespace};
use crate::window::WindowShell;

/// Arguments stored in the Run value. The window shell parses the same string.
pub const AUTOSTART_ARGS: &[&str] = &["--minimized"];

/// Reads and writes the Windows startup entry of `Forgebox.exe`.
pub trait AutostartBackend: Send + Sync {
    /// Creates (or replaces) the entry so Windows starts the app with `args`.
    fn enable(&self, args: &[&str]) -> Result<(), String>;
    /// Removes the entry (absent is not an error).
    fn disable(&self) -> Result<(), String>;
    /// Whether the entry exists.
    fn is_enabled(&self) -> Result<bool, String>;
}

/// The real backend: the Run key through `tauri-plugin-autostart`. The plugin fixes its
/// arguments at registration ([`plugin`]), so `enable` insists on the same ones.
pub struct PluginBackend<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> PluginBackend<R> {
    pub fn new(app: AppHandle<R>) -> Self {
        Self { app }
    }
}

impl<R: Runtime> AutostartBackend for PluginBackend<R> {
    fn enable(&self, args: &[&str]) -> Result<(), String> {
        if args != AUTOSTART_ARGS {
            return Err(format!("the plugin is registered with {AUTOSTART_ARGS:?}"));
        }
        self.app.autolaunch().enable().map_err(|e| e.to_string())
    }

    fn disable(&self) -> Result<(), String> {
        self.app.autolaunch().disable().map_err(|e| e.to_string())
    }

    fn is_enabled(&self) -> Result<bool, String> {
        self.app
            .autolaunch()
            .is_enabled()
            .map_err(|e| e.to_string())
    }
}

/// The plugin, registered with [`AUTOSTART_ARGS`].
pub fn plugin<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri_plugin_autostart::Builder::new()
        .args(AUTOSTART_ARGS.iter().copied())
        .build()
}

/// What the UI needs to draw the toggle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutostartStatus {
    /// Can the toggle be used? `false` in portable mode.
    pub available: bool,
    /// Is the Run entry present?
    pub enabled: bool,
    /// Localized explanation when `available` is false.
    pub reason: Option<String>,
}

/// Autostart policy over a backend.
pub struct Autostart {
    backend: Arc<dyn AutostartBackend>,
    portable: bool,
}

impl Autostart {
    pub fn new(backend: Arc<dyn AutostartBackend>, portable: bool) -> Self {
        Self { backend, portable }
    }

    fn portable_error(&self) -> ApiError {
        ApiError::new(
            "unsupported_platform",
            i18n::t(Namespace::Updater, "autostart.portable", &[]),
        )
        .with_reason("autostart_portable", json!({}))
    }

    /// Current state. Blocking.
    pub fn status(&self) -> Result<AutostartStatus, ApiError> {
        if self.portable {
            return Ok(AutostartStatus {
                available: false,
                enabled: false,
                reason: Some(i18n::t(Namespace::Updater, "autostart.portable", &[])),
            });
        }
        let enabled = self.backend.is_enabled().map_err(Self::failed)?;
        Ok(AutostartStatus {
            available: true,
            enabled,
            reason: None,
        })
    }

    /// Turns the entry on or off and answers the new state. Portable mode refuses `true`
    /// (and never touches the backend); turning off is always a no-op there. Blocking.
    pub fn set(&self, enabled: bool) -> Result<AutostartStatus, ApiError> {
        if self.portable {
            return if enabled {
                Err(self.portable_error())
            } else {
                self.status()
            };
        }
        if enabled {
            self.backend.enable(AUTOSTART_ARGS)
        } else {
            self.backend.disable()
        }
        .map_err(Self::failed)?;
        self.status()
    }

    fn failed(detail: String) -> ApiError {
        ApiError::new(
            "io",
            i18n::t(
                Namespace::Updater,
                "autostart.failed",
                &[("detail", &detail)],
            ),
        )
        .with_reason("autostart_failed", json!({ "detail": detail }))
    }
}

/// Is this GUI data dir a portable one?
pub fn is_portable(source: GuiDataSource) -> bool {
    source == GuiDataSource::Portable
}

fn service(app: &AppHandle, shell: &WindowShell) -> Autostart {
    Autostart::new(
        Arc::new(PluginBackend::new(app.clone())),
        is_portable(shell.data_dir().source()),
    )
}

/// Autostart state for onboarding and settings.
#[tauri::command]
pub async fn autostart_get(
    app: AppHandle,
    shell: State<'_, Arc<WindowShell>>,
) -> Result<AutostartStatus, ApiError> {
    let service = service(&app, &shell);
    tokio::task::spawn_blocking(move || service.status())
        .await
        .map_err(|e| ApiError::new("internal", format!("autostart: {e}")))?
}

/// Enables or disables autostart; `unsupported` (with a localized message) in portable mode.
#[tauri::command]
pub async fn autostart_set(
    app: AppHandle,
    shell: State<'_, Arc<WindowShell>>,
    enabled: bool,
) -> Result<AutostartStatus, ApiError> {
    let service = service(&app, &shell);
    tokio::task::spawn_blocking(move || service.set(enabled))
        .await
        .map_err(|e| ApiError::new("internal", format!("autostart: {e}")))?
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::i18n::{Language, set_current_language};

    /// In-memory Run key: records the arguments it was asked to write.
    #[derive(Default)]
    struct FakeRunKey {
        entry: Mutex<Option<Vec<String>>>,
        calls: Mutex<u32>,
        fail: bool,
    }

    impl AutostartBackend for FakeRunKey {
        fn enable(&self, args: &[&str]) -> Result<(), String> {
            *self.calls.lock().unwrap() += 1;
            if self.fail {
                return Err("access denied".to_owned());
            }
            *self.entry.lock().unwrap() = Some(args.iter().map(|a| (*a).to_owned()).collect());
            Ok(())
        }
        fn disable(&self) -> Result<(), String> {
            *self.calls.lock().unwrap() += 1;
            *self.entry.lock().unwrap() = None;
            Ok(())
        }
        fn is_enabled(&self) -> Result<bool, String> {
            *self.calls.lock().unwrap() += 1;
            Ok(self.entry.lock().unwrap().is_some())
        }
    }

    #[test]
    fn enable_writes_the_minimized_argument_and_disable_removes_it() {
        let key = Arc::new(FakeRunKey::default());
        let autostart = Autostart::new(key.clone(), false);
        assert_eq!(
            autostart.status().unwrap(),
            AutostartStatus {
                available: true,
                enabled: false,
                reason: None
            }
        );
        let status = autostart.set(true).unwrap();
        assert!(status.enabled);
        assert_eq!(
            key.entry.lock().unwrap().as_deref(),
            Some(&["--minimized".to_owned()][..])
        );
        assert!(!autostart.set(false).unwrap().enabled);
        assert!(key.entry.lock().unwrap().is_none());
    }

    #[test]
    fn portable_mode_refuses_with_a_localized_reason_and_never_touches_the_backend() {
        let key = Arc::new(FakeRunKey::default());
        let autostart = Autostart::new(key.clone(), true);

        set_current_language(Language::EnUs);
        let err = autostart.set(true).unwrap_err();
        assert_eq!(err.code, "unsupported_platform");
        assert_eq!(err.reason(), Some("autostart_portable"));
        assert!(err.message.contains("portable mode"), "{}", err.message);

        set_current_language(Language::PtBr);
        let err = autostart.set(true).unwrap_err();
        assert!(err.message.contains("modo portátil"), "{}", err.message);

        let status = autostart.status().unwrap();
        assert!(!status.available && !status.enabled);
        assert!(status.reason.unwrap().contains("modo portátil"));
        assert!(!autostart.set(false).unwrap().enabled);
        assert_eq!(*key.calls.lock().unwrap(), 0);
    }

    #[test]
    fn a_failing_backend_surfaces_a_localized_io_error() {
        let key = Arc::new(FakeRunKey {
            fail: true,
            ..FakeRunKey::default()
        });
        let err = Autostart::new(key, false).set(true).unwrap_err();
        assert_eq!(err.code, "io");
        assert!(err.message.contains("access denied"), "{}", err.message);
        assert_eq!(err.reason(), Some("autostart_failed"));
    }

    #[test]
    fn only_a_portable_data_dir_is_portable() {
        assert!(is_portable(GuiDataSource::Portable));
        assert!(!is_portable(GuiDataSource::Installed));
        assert!(!is_portable(GuiDataSource::Override));
    }

    #[test]
    fn the_stored_argument_is_the_one_the_window_shell_parses() {
        let args = crate::window::LaunchArgs::parse(AUTOSTART_ARGS.iter().copied());
        assert!(args.minimized);
    }
}
