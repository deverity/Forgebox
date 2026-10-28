//! The GUI writes only inside its data dir (ADR-0034 §5, ADR-0035, docs/08 §2.2), in the
//! three modes: installed (`%LOCALAPPDATA%\Forgebox\gui`), portable (`<root>/state/gui`) and
//! `FORGE_GUI_DATA_DIR`. A sandbox holds every candidate location (a fake `%LOCALAPPDATA%`,
//! an install folder, a portable root, an override folder, a temp dir); the test runs what
//! `Boot::load` and a session do — resolve, create, read and update the preferences, save
//! the window placement — and then lists the sandbox: every new entry must be inside the
//! resolved dir. Nothing touches the real environment (explicit inputs only).

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::json;

use forge_ipc::base::paths::{DiscoverInputs, ForgeRoot};
use forgebox_desktop::data_dir::{GuiDataDir, GuiDataSource, GuiDirInputs, root_initialized};
use forgebox_desktop::prefs::PrefsStore;
use forgebox_desktop::window_state::{MonitorId, Placement, Rect, WindowStateFile};

/// Every file and folder under `dir`, relative.
fn tree(dir: &Path) -> BTreeSet<PathBuf> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeSet<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            out.insert(path.strip_prefix(base).unwrap().to_path_buf());
            if path.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(dir, dir, &mut out);
    out
}

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    /// `lad/` (a `%LOCALAPPDATA%` with a `root.toml` pointing at `root/`), `install/` (the
    /// folder of Forgebox.exe), `root/` (a Forgebox root with `www/`), `override/`, `tmp/`.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        for sub in ["lad/Forgebox", "install", "root/www", "override", "tmp"] {
            std::fs::create_dir_all(p.join(sub)).unwrap();
        }
        std::fs::write(
            p.join("lad/Forgebox/root.toml"),
            format!(
                "root = \"{}\"\n",
                p.join("root").display().to_string().replace('\\', "/")
            ),
        )
        .unwrap();
        std::fs::write(p.join("install/Forgebox.exe"), b"").unwrap();
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn inputs(&self) -> GuiDirInputs {
        let p = self.path();
        GuiDirInputs {
            override_dir: None,
            root: DiscoverInputs {
                flag: None,
                env_root: None,
                exe_dir: Some(p.join("install")),
                local_app_data: Some(p.join("lad")),
            },
            local_app_data: Some(p.join("lad")),
            temp_dir: p.join("tmp"),
        }
    }
}

/// A session: create the dir, update the prefs twice, save a placement.
fn run_session(dir: &GuiDataDir) {
    dir.ensure().unwrap();
    let prefs = PrefsStore::load(dir.prefs_file());
    let patch = json!({"theme": "dark", "language": "en-US"});
    prefs.update(patch.as_object().unwrap()).unwrap();
    let patch = json!({"close_to_tray": false});
    prefs.update(patch.as_object().unwrap()).unwrap();
    let mut state = WindowStateFile::load(&dir.window_state_file());
    state.record(Placement {
        monitor: MonitorId {
            name: r"\\.\DISPLAY1".to_owned(),
            bounds: Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
        },
        rect: Rect {
            x: 100,
            y: 80,
            width: 1200,
            height: 800,
        },
        scale: 1.0,
        maximized: false,
    });
    state.save(&dir.window_state_file()).unwrap();
    // The WebView2 profile goes in its own folder inside.
    std::fs::create_dir_all(dir.webview_dir()).unwrap();
}

/// Runs a session and asserts that every new entry of the sandbox is under the data dir.
fn assert_writes_stay_inside(sandbox: &Sandbox, inputs: &GuiDirInputs, expected: GuiDataSource) {
    let before = tree(sandbox.path());
    let dir = GuiDataDir::resolve(inputs);
    assert_eq!(dir.source(), expected, "{dir:?}");
    assert_eq!(tree(sandbox.path()), before, "resolving wrote something");
    run_session(&dir);

    let inside = dir
        .path()
        .strip_prefix(sandbox.path())
        .unwrap()
        .to_path_buf();
    let new: Vec<PathBuf> = tree(sandbox.path()).difference(&before).cloned().collect();
    assert!(!new.is_empty());
    for entry in &new {
        let ancestor_of_dir = inside.starts_with(entry);
        assert!(
            entry.starts_with(&inside) || ancestor_of_dir,
            "{} was written outside {} ({expected:?})",
            entry.display(),
            inside.display()
        );
    }
    for file in ["prefs.json", "window-state.json"] {
        assert!(dir.path().join(file).is_file(), "{file}");
    }
    assert!(!dir.path().join("prefs.json.tmp").exists());
}

#[test]
fn installed_mode_writes_only_under_local_app_data() {
    let sandbox = Sandbox::new();
    let inputs = sandbox.inputs();
    assert_writes_stay_inside(&sandbox, &inputs, GuiDataSource::Installed);
    assert!(sandbox.path().join("lad/Forgebox/gui/prefs.json").is_file());
    // The (non-portable) root of root.toml is not touched.
    assert!(!sandbox.path().join("root/state").exists());
}

#[test]
fn portable_mode_writes_only_under_the_root_state_gui() {
    let sandbox = Sandbox::new();
    std::fs::write(sandbox.path().join("install/portable.flag"), b"").unwrap();
    let inputs = sandbox.inputs();
    assert_writes_stay_inside(&sandbox, &inputs, GuiDataSource::Portable);
    assert!(
        sandbox
            .path()
            .join("install/state/gui/prefs.json")
            .is_file()
    );
    // Nothing under %LOCALAPPDATA% besides what was there.
    assert_eq!(
        tree(&sandbox.path().join("lad")),
        BTreeSet::from([
            PathBuf::from("Forgebox"),
            PathBuf::from("Forgebox/root.toml")
        ])
    );
    // The GUI's folder alone does not make the portable root "initialized" (the bridge
    // would otherwise start a daemon in a root that never went through onboarding).
    let root = ForgeRoot::new(sandbox.path().join("install")).unwrap();
    assert!(!root_initialized(&root));
}

#[test]
fn the_override_writes_only_inside_itself() {
    let sandbox = Sandbox::new();
    std::fs::write(sandbox.path().join("install/portable.flag"), b"").unwrap();
    let mut inputs = sandbox.inputs();
    inputs.override_dir = Some(OsString::from(sandbox.path().join("override/profile")));
    assert_writes_stay_inside(&sandbox, &inputs, GuiDataSource::Override);
    assert!(!sandbox.path().join("install/state").exists());
}

#[test]
fn a_second_session_reads_what_the_first_saved() {
    let sandbox = Sandbox::new();
    let dir = GuiDataDir::resolve(&sandbox.inputs());
    run_session(&dir);
    let prefs = PrefsStore::load(dir.prefs_file()).get();
    assert!(!prefs.close_to_tray);
    assert_eq!(prefs.other["language"], "en-US");
    let state = WindowStateFile::load(&dir.window_state_file());
    assert_eq!(state.placements.len(), 1);
    assert_eq!(state.placements[0].rect.width, 1200);
}
