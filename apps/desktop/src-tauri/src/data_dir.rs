//! Where `Forgebox.exe` keeps its own files ([ADR-0034] §5, [ADR-0035]): GUI preferences
//! (`prefs.json`), the window placement (`window-state.json`) and the WebView2 user data
//! folder (`webview/`). Never the daemon's `forgebox.toml`, which may be remote.
//!
//! Resolution, first match wins:
//!
//! 1. `FORGE_GUI_DATA_DIR` (tests, QA runs, side-by-side profiles);
//! 2. portable mode — the Forgebox root found like the CLI finds it (`FORGEBOX_ROOT`,
//!    `portable.flag` next to the executable, `root.toml`) has a `portable.flag` — →
//!    `<root>/state/gui`, so nothing is written outside the root (docs/08 §2.2);
//! 3. installed: `%LOCALAPPDATA%\Forgebox\gui`;
//! 4. no `%LOCALAPPDATA%` at all (service accounts): `%TEMP%\Forgebox\gui`.
//!
//! Every plugin or crate that would pick its own folder (Tauri's `app_config_dir`, the
//! default WebView2 profile under `%LOCALAPPDATA%\dev.forgebox.app`) is redirected here or
//! not used; `tests/data_dir.rs` and the real-app check assert it.
//!
//! In portable mode the GUI may create `<root>/state/gui` before the daemon ever ran, so
//! "the root is initialized" means `state/` holds something besides `gui/`
//! ([`root_initialized`]).
//!
//! Everything here is blocking file I/O: async callers use `spawn_blocking`.
//!
//! [ADR-0034]: ../../../../docs/adr/0034-desktop-shell-architecture.md
//! [ADR-0035]: ../../../../docs/adr/0035-window-chrome-materials.md

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;

use forge_ipc::base::paths::{DiscoverInputs, ForgeRoot, RootSource};

/// Environment variable that overrides the GUI data dir.
pub const GUI_DATA_DIR_ENV: &str = "FORGE_GUI_DATA_DIR";
/// Name of the GUI folder (under `%LOCALAPPDATA%\Forgebox` or `<root>/state`).
pub const GUI_DIR_NAME: &str = "gui";
/// WebView2 user data folder inside the GUI data dir.
pub const WEBVIEW_DIR_NAME: &str = "webview";
/// GUI preferences file.
pub const PREFS_FILE: &str = "prefs.json";
/// Window placement file.
pub const WINDOW_STATE_FILE: &str = "window-state.json";

/// Why the GUI data dir is where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuiDataSource {
    /// `FORGE_GUI_DATA_DIR`.
    Override,
    /// `<root>/state/gui` of a portable root.
    Portable,
    /// `%LOCALAPPDATA%\Forgebox\gui`.
    Installed,
    /// `%TEMP%\Forgebox\gui` (no `%LOCALAPPDATA%`).
    Fallback,
}

/// Inputs of [`GuiDataDir::resolve`], injectable for tests.
#[derive(Debug, Clone, Default)]
pub struct GuiDirInputs {
    /// Value of `FORGE_GUI_DATA_DIR`.
    pub override_dir: Option<OsString>,
    /// How the Forgebox root is discovered (same rules as `forge`).
    pub root: DiscoverInputs,
    /// `%LOCALAPPDATA%`.
    pub local_app_data: Option<PathBuf>,
    /// Last resort parent (`std::env::temp_dir()`).
    pub temp_dir: PathBuf,
}

impl GuiDirInputs {
    /// Reads the environment of the current process.
    pub fn from_env() -> Self {
        Self {
            override_dir: std::env::var_os(GUI_DATA_DIR_ENV),
            root: DiscoverInputs::from_env(None),
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
            temp_dir: std::env::temp_dir(),
        }
    }
}

/// The resolved GUI data dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuiDataDir {
    path: PathBuf,
    source: GuiDataSource,
}

impl GuiDataDir {
    /// Applies the rules of the module documentation. Reads the file system only to find a
    /// portable root (`portable.flag`, `root.toml`); never writes.
    pub fn resolve(inputs: &GuiDirInputs) -> Self {
        if let Some(dir) = inputs.override_dir.as_ref().filter(|d| !d.is_empty()) {
            let dir = PathBuf::from(dir);
            let path = std::path::absolute(&dir).unwrap_or(dir);
            return Self::at(path, GuiDataSource::Override);
        }
        if let Some(root) = portable_root(&inputs.root) {
            return Self::at(root.state().join(GUI_DIR_NAME), GuiDataSource::Portable);
        }
        if let Some(lad) = inputs.local_app_data.as_ref().filter(|p| p.is_absolute()) {
            return Self::at(
                lad.join("Forgebox").join(GUI_DIR_NAME),
                GuiDataSource::Installed,
            );
        }
        Self::at(
            inputs.temp_dir.join("Forgebox").join(GUI_DIR_NAME),
            GuiDataSource::Fallback,
        )
    }

    /// A data dir at `path` (tests, previews).
    pub fn at(path: PathBuf, source: GuiDataSource) -> Self {
        Self { path, source }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn source(&self) -> GuiDataSource {
        self.source
    }

    /// WebView2 user data folder (`WebviewWindowBuilder::data_directory`).
    pub fn webview_dir(&self) -> PathBuf {
        self.path.join(WEBVIEW_DIR_NAME)
    }

    pub fn prefs_file(&self) -> PathBuf {
        self.path.join(PREFS_FILE)
    }

    pub fn window_state_file(&self) -> PathBuf {
        self.path.join(WINDOW_STATE_FILE)
    }

    /// Creates the folder (and its parents). Idempotent.
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.path)
    }
}

/// The discovered root when it is a portable one.
fn portable_root(inputs: &DiscoverInputs) -> Option<ForgeRoot> {
    let (root, source) = ForgeRoot::discover_with(inputs).ok()?;
    (source == RootSource::Portable || root.is_portable()).then_some(root)
}

/// Whether the daemon (or `forge init`) has initialized `root`: its `state/` exists and
/// holds something besides the GUI's own `gui/` folder (portable mode, module docs).
/// Blocking.
pub fn root_initialized(root: &ForgeRoot) -> bool {
    match std::fs::read_dir(root.state()) {
        Ok(entries) => entries
            .flatten()
            .any(|entry| entry.file_name() != GUI_DIR_NAME),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(sandbox: &Path) -> GuiDirInputs {
        GuiDirInputs {
            override_dir: None,
            root: DiscoverInputs {
                flag: None,
                env_root: None,
                exe_dir: Some(sandbox.join("install")),
                local_app_data: Some(sandbox.join("lad")),
            },
            local_app_data: Some(sandbox.join("lad")),
            temp_dir: sandbox.join("tmp"),
        }
    }

    #[test]
    fn installed_mode_uses_local_app_data() {
        let sandbox = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(sandbox.path().join("install")).unwrap();
        let dir = GuiDataDir::resolve(&inputs(sandbox.path()));
        assert_eq!(dir.source(), GuiDataSource::Installed);
        assert_eq!(
            dir.path(),
            sandbox.path().join("lad").join("Forgebox").join("gui")
        );
        assert_eq!(dir.webview_dir(), dir.path().join("webview"));
        assert_eq!(dir.prefs_file(), dir.path().join("prefs.json"));
        assert_eq!(
            dir.window_state_file(),
            dir.path().join("window-state.json")
        );
    }

    #[test]
    fn portable_flag_next_to_the_exe_wins_over_local_app_data() {
        let sandbox = tempfile::tempdir().unwrap();
        let install = sandbox.path().join("install");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(install.join("portable.flag"), b"").unwrap();
        let dir = GuiDataDir::resolve(&inputs(sandbox.path()));
        assert_eq!(dir.source(), GuiDataSource::Portable);
        assert_eq!(dir.path(), install.join("state").join("gui"));
    }

    #[test]
    fn a_portable_root_named_by_forgebox_root_is_portable_too() {
        let sandbox = tempfile::tempdir().unwrap();
        let root = sandbox.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("portable.flag"), b"").unwrap();
        let mut inputs = inputs(sandbox.path());
        inputs.root.env_root = Some(root.clone().into_os_string());
        let dir = GuiDataDir::resolve(&inputs);
        assert_eq!(dir.source(), GuiDataSource::Portable);
        assert_eq!(dir.path(), root.join("state").join("gui"));

        // The same root without the flag is an installed root: the GUI stays per user.
        std::fs::remove_file(root.join("portable.flag")).unwrap();
        let dir = GuiDataDir::resolve(&inputs);
        assert_eq!(dir.source(), GuiDataSource::Installed);
    }

    #[test]
    fn the_override_wins_over_everything() {
        let sandbox = tempfile::tempdir().unwrap();
        let install = sandbox.path().join("install");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(install.join("portable.flag"), b"").unwrap();
        let mut inputs = inputs(sandbox.path());
        inputs.override_dir = Some(sandbox.path().join("custom").into_os_string());
        let dir = GuiDataDir::resolve(&inputs);
        assert_eq!(dir.source(), GuiDataSource::Override);
        assert_eq!(dir.path(), sandbox.path().join("custom"));

        // An empty value is "not set".
        inputs.override_dir = Some(OsString::new());
        assert_eq!(
            GuiDataDir::resolve(&inputs).source(),
            GuiDataSource::Portable
        );
    }

    #[test]
    fn a_relative_override_is_made_absolute() {
        let mut inputs = GuiDirInputs {
            override_dir: Some(OsString::from("gui-data")),
            ..GuiDirInputs::default()
        };
        inputs.temp_dir = std::env::temp_dir();
        let dir = GuiDataDir::resolve(&inputs);
        assert!(dir.path().is_absolute(), "{dir:?}");
        assert!(dir.path().ends_with("gui-data"));
    }

    #[test]
    fn without_local_app_data_the_temp_dir_is_used() {
        let sandbox = tempfile::tempdir().unwrap();
        let mut inputs = inputs(sandbox.path());
        inputs.local_app_data = None;
        inputs.root.local_app_data = None;
        let dir = GuiDataDir::resolve(&inputs);
        assert_eq!(dir.source(), GuiDataSource::Fallback);
        assert_eq!(
            dir.path(),
            sandbox.path().join("tmp").join("Forgebox").join("gui")
        );
    }

    #[test]
    fn resolving_never_writes() {
        let sandbox = tempfile::tempdir().unwrap();
        let install = sandbox.path().join("install");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(install.join("portable.flag"), b"").unwrap();
        let dir = GuiDataDir::resolve(&inputs(sandbox.path()));
        assert!(!dir.path().exists());
        assert!(!install.join("state").exists());
        dir.ensure().unwrap();
        assert!(dir.path().is_dir());
        dir.ensure().unwrap();
    }

    #[test]
    fn the_gui_folder_alone_does_not_initialize_a_root() {
        let sandbox = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(sandbox.path()).unwrap();
        assert!(!root_initialized(&root), "no state/");
        std::fs::create_dir_all(root.state().join(GUI_DIR_NAME)).unwrap();
        assert!(!root_initialized(&root), "state/gui only");
        std::fs::create_dir_all(root.state_locks()).unwrap();
        assert!(root_initialized(&root), "state/locks");

        let sandbox = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(sandbox.path()).unwrap();
        std::fs::create_dir_all(root.state()).unwrap();
        std::fs::write(root.db(), b"").unwrap();
        assert!(root_initialized(&root), "state/forgebox.db");
    }
}
