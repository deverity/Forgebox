//! First-run detection and root selection (M2-E2-T11, docs/08 §2, docs/07 onboarding).
//!
//! No domain logic lives here: the root is whatever [`ForgeRoot::discover`] says, the choice
//! is persisted through the `RootPointer` writer of `forge-core` (`root.toml`), and the
//! daemon itself creates the layout (`ForgeRoot::init`, case-sensitive directories) when the
//! bridge starts it. This module only
//!
//! * validates a folder the user picked ([`validate_choice`]) and refuses with a stable
//!   `details.reason` that the UI localizes (pt-BR/en-US, `errors.json`);
//! * switches the bridge to the new root ([`choose_root`]);
//! * keeps the client-only onboarding progress ([`OnboardingStore`]) in the GUI data dir,
//!   one entry per root (`root:<posix path, lowercased>`) or per remote context
//!   (`context:<name>`), so a closed app resumes where it stopped.
//!
//! Everything but [`choose_root`] and [`first_run`] is blocking file I/O: async callers use
//! `spawn_blocking`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use forge_ipc::ApiError;
use forge_ipc::base::paths::{
    DiscoverInputs, ForgeRoot, RootPointer, RootSource, plain_path, posix_string,
};

use crate::bridge::{Bridge, ConnectionStatus, ContextStateDto, LOCAL_CONTEXT};
use crate::data_dir::root_initialized;

/// File of the onboarding progress in the GUI data dir.
pub const ONBOARDING_FILE: &str = "onboarding.json";

/// Where the root of this run comes from (`FirstRunState.source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootOrigin {
    /// `FORGEBOX_ROOT` (or `--root`, which the GUI never passes): not changeable here.
    Env,
    /// `portable.flag` next to the executable: not changeable here.
    Portable,
    /// `%LOCALAPPDATA%\Forgebox\root.toml` (or `FORGE_CONFIG_DIR`).
    RootToml,
    /// The built-in default (`C:\Forgebox`).
    Default,
}

impl From<RootSource> for RootOrigin {
    fn from(source: RootSource) -> Self {
        match source {
            RootSource::Flag | RootSource::Env => RootOrigin::Env,
            RootSource::Portable => RootOrigin::Portable,
            RootSource::RootToml => RootOrigin::RootToml,
            RootSource::Default => RootOrigin::Default,
        }
    }
}

impl RootOrigin {
    /// Whether the user can pick another root (no override forces this one).
    pub fn changeable(self) -> bool {
        matches!(self, RootOrigin::RootToml | RootOrigin::Default)
    }
}

/// Why a folder cannot be the root. [`RootRejection::reason`] is the stable code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootRejection {
    /// Empty, or with control characters.
    PathInvalid,
    /// `FORGEBOX_ROOT` forces the root.
    OverriddenByEnv,
    /// `portable.flag` forces the root.
    OverriddenByPortable,
    /// Not an absolute path (relative, drive-relative, rooted without drive).
    PathNotAbsolute,
    /// A network path (UNC) or a device path.
    PathNotLocal,
    /// The path is an existing file.
    PathIsFile,
    /// The path is, or is inside, the `bin` folder of a Forgebox root.
    PathInsideBin,
    /// A folder with content that is not a Forgebox root.
    PathNotEmpty,
    /// The folder cannot be created or written.
    PathNotWritable(String),
}

impl RootRejection {
    /// Stable `details.reason`; one text per locale in `errors.json`.
    pub fn reason(&self) -> &'static str {
        match self {
            RootRejection::PathInvalid => "path_invalid",
            RootRejection::OverriddenByEnv => "root_overridden_env",
            RootRejection::OverriddenByPortable => "root_overridden_portable",
            RootRejection::PathNotAbsolute => "path_not_absolute",
            RootRejection::PathNotLocal => "path_not_local",
            RootRejection::PathIsFile => "path_is_file",
            RootRejection::PathInsideBin => "path_inside_bin",
            RootRejection::PathNotEmpty => "path_not_empty",
            RootRejection::PathNotWritable(_) => "path_not_writable",
        }
    }
}

impl From<RootRejection> for ApiError {
    fn from(rejection: RootRejection) -> Self {
        let message = match &rejection {
            RootRejection::PathInvalid => "the path is empty or has control characters".to_owned(),
            RootRejection::OverriddenByEnv => {
                "FORGEBOX_ROOT is set and decides the root; unset it to choose another".to_owned()
            }
            RootRejection::OverriddenByPortable => {
                "portable mode (portable.flag) decides the root".to_owned()
            }
            RootRejection::PathNotAbsolute => "the path must be absolute".to_owned(),
            RootRejection::PathNotLocal => {
                "the root must be on a local drive, not a network or device path".to_owned()
            }
            RootRejection::PathIsFile => "the path is a file, not a folder".to_owned(),
            RootRejection::PathInsideBin => {
                "the path is inside the bin folder of a Forgebox root".to_owned()
            }
            RootRejection::PathNotEmpty => {
                "the folder is not empty and is not a Forgebox root".to_owned()
            }
            RootRejection::PathNotWritable(why) => format!("the folder is not writable: {why}"),
        };
        ApiError::new("invalid_input", message).with_reason(rejection.reason(), json!({}))
    }
}

/// Whether `dir` is an existing Forgebox root: initialized by a daemon, or with its
/// `forgebox.toml` / `VERSION` file.
pub fn is_forgebox_root(dir: &Path) -> bool {
    let Ok(root) = ForgeRoot::new(dir) else {
        return false;
    };
    root_initialized(&root) || root.forgebox_toml().is_file() || root.version_file().is_file()
}

/// `path` lowercased with `/` separators and no trailing slash, for comparisons.
fn folded(path: &Path) -> String {
    posix_string(path)
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

/// Whether `path` is the `bin` of the `current` root or below it, or below any ancestor
/// called `bin` whose parent is a Forgebox root.
fn inside_a_bin(path: &Path, current: &ForgeRoot) -> bool {
    let me = folded(path);
    let bin = folded(&current.bin());
    if me == bin || me.starts_with(&format!("{bin}/")) {
        return true;
    }
    path.ancestors().any(|ancestor| {
        let is_bin = ancestor
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("bin"));
        is_bin && ancestor.parent().is_some_and(is_forgebox_root)
    })
}

/// Validates the folder the user picked. `inputs` is the process environment
/// ([`DiscoverInputs::from_env`]) or a test's. Blocking; creates the folder when it is
/// missing (and leaves it: it is going to be the root).
pub fn validate_choice(raw: &str, inputs: &DiscoverInputs) -> Result<ForgeRoot, RootRejection> {
    let (current, source) =
        ForgeRoot::discover_with(inputs).map_err(|_| RootRejection::PathInvalid)?;
    match source {
        RootSource::Env | RootSource::Flag => return Err(RootRejection::OverriddenByEnv),
        RootSource::Portable => return Err(RootRejection::OverriddenByPortable),
        RootSource::RootToml | RootSource::Default => {}
    }
    let raw = raw.trim();
    if raw.is_empty() || raw.chars().any(char::is_control) {
        return Err(RootRejection::PathInvalid);
    }
    let path = plain_path(Path::new(raw));
    let text = path.to_string_lossy();
    if text.starts_with(r"\\") || text.starts_with("//") {
        return Err(RootRejection::PathNotLocal);
    }
    if !path.is_absolute() {
        return Err(RootRejection::PathNotAbsolute);
    }
    let root = ForgeRoot::new(&path).map_err(|_| RootRejection::PathInvalid)?;
    let dir = root.path();
    if dir.is_file() {
        return Err(RootRejection::PathIsFile);
    }
    if inside_a_bin(dir, &current) {
        return Err(RootRejection::PathInsideBin);
    }
    if dir.is_dir() {
        let empty = std::fs::read_dir(dir)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false);
        if !empty && !is_forgebox_root(dir) {
            return Err(RootRejection::PathNotEmpty);
        }
    }
    probe_writable(dir)?;
    Ok(root)
}

fn probe_writable(dir: &Path) -> Result<(), RootRejection> {
    let fail = |e: std::io::Error| RootRejection::PathNotWritable(e.to_string());
    std::fs::create_dir_all(dir).map_err(fail)?;
    let probe = dir.join(format!(".forgebox-write-test-{}", std::process::id()));
    std::fs::write(&probe, b"").map_err(fail)?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Answer of `choose_root`.
#[derive(Debug, Clone, Serialize)]
pub struct ChooseRootAnswer {
    /// The new root (`/` separators).
    pub root: String,
    /// The pointer file that was written.
    pub pointer: String,
    /// The `local` context after the daemon of the new root was started.
    pub state: ContextStateDto,
}

/// Validates `raw`, writes the root pointer and reconnects the `local` context to the new
/// root, starting its daemon (which creates the layout). The previous root's daemon, if
/// any, keeps running until stopped.
pub async fn choose_root(
    bridge: &Bridge,
    raw: &str,
    inputs: DiscoverInputs,
) -> Result<ChooseRootAnswer, ApiError> {
    let raw = raw.to_owned();
    let (root, pointer) = tokio::task::spawn_blocking(move || {
        let root = validate_choice(&raw, &inputs).map_err(ApiError::from)?;
        let file = RootPointer::file_for(&inputs).ok_or_else(|| {
            ApiError::new(
                "invalid_input",
                "no folder for root.toml: neither FORGE_CONFIG_DIR nor LOCALAPPDATA is set",
            )
            .with_reason("pointer_unavailable", json!({}))
        })?;
        RootPointer::write_to(&file, &root).map_err(ApiError::from)?;
        Ok::<_, ApiError>((root, file))
    })
    .await
    .map_err(|e| ApiError::new("internal", format!("choose_root: {e}")))??;
    bridge.reset(LOCAL_CONTEXT).await?;
    let state = bridge.start(LOCAL_CONTEXT, true).await?;
    Ok(ChooseRootAnswer {
        root: root.to_posix_string(),
        pointer: posix_string(&pointer),
        state,
    })
}

/// What the first-run screen needs (`first_run_state`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FirstRunState {
    pub context: String,
    /// The context is this machine's daemon. A remote context has no root of its own here:
    /// `root` is `None` and the root step is never shown.
    pub local: bool,
    pub root: Option<String>,
    pub source: Option<RootOrigin>,
    pub root_initialized: bool,
    pub daemon_reachable: bool,
    pub onboarding_done: bool,
    /// Whether onboarding step 1 (choose the root) applies: local, not finished and not
    /// forced by an override.
    pub show_root_step: bool,
}

/// Computes [`FirstRunState`] for `context`.
pub async fn first_run(
    bridge: &Bridge,
    file: PathBuf,
    context: &str,
    inputs: DiscoverInputs,
) -> Result<FirstRunState, ApiError> {
    let daemon_reachable = bridge
        .state(context)
        .await
        .map(|s| s.status == ConnectionStatus::Connected)
        .unwrap_or(false);
    let context = context.to_owned();
    tokio::task::spawn_blocking(move || {
        let store = OnboardingStore::new(file);
        if context != LOCAL_CONTEXT {
            let onboarding_done = store.get(&context_key(&context)).done;
            return Ok(FirstRunState {
                context,
                local: false,
                root: None,
                source: None,
                root_initialized: true,
                daemon_reachable,
                onboarding_done,
                show_root_step: false,
            });
        }
        let (root, source) = ForgeRoot::discover_with(&inputs).map_err(ApiError::from)?;
        let origin = RootOrigin::from(source);
        let onboarding_done = store.get(&root_key(&root)).done;
        Ok(FirstRunState {
            context,
            local: true,
            root: Some(root.to_posix_string()),
            source: Some(origin),
            root_initialized: root_initialized(&root),
            daemon_reachable,
            onboarding_done,
            show_root_step: !onboarding_done && origin.changeable(),
        })
    })
    .await
    .map_err(|e| ApiError::new("internal", format!("first_run_state: {e}")))?
}

/// Key of the onboarding entry of a local root.
pub fn root_key(root: &ForgeRoot) -> String {
    format!("root:{}", folded(root.path()))
}

/// Key of the onboarding entry of a remote context.
pub fn context_key(context: &str) -> String {
    format!("context:{context}")
}

/// Key for `context` (the local one is keyed by its root).
pub fn key_for(context: &str, inputs: &DiscoverInputs) -> Result<String, ApiError> {
    if context == LOCAL_CONTEXT {
        let (root, _) = ForgeRoot::discover_with(inputs).map_err(ApiError::from)?;
        Ok(root_key(&root))
    } else {
        Ok(context_key(context))
    }
}

/// Progress of one onboarding.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OnboardingEntry {
    /// Id of the step the user is on (`""`: not started).
    pub step: String,
    /// Choices made so far (free-form, owned by the steps).
    pub choices: Map<String, Value>,
    pub done: bool,
}

/// A partial update: absent fields stay, `choices` is merged by key (`null` removes one).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OnboardingPatch {
    pub step: Option<String>,
    pub choices: Option<Map<String, Value>>,
    pub done: Option<bool>,
}

/// `onboarding.json`: `{"entries": {"<key>": OnboardingEntry}}`. Reads every time (the file
/// is tiny and a restart needs nothing else); writes are atomic and serialized. The lock is
/// only taken in blocking code.
#[derive(Debug)]
pub struct OnboardingStore {
    file: PathBuf,
    write_lock: Mutex<()>,
}

impl OnboardingStore {
    pub fn new(file: PathBuf) -> Self {
        Self {
            file,
            write_lock: Mutex::new(()),
        }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    fn read_all(&self) -> Map<String, Value> {
        let Ok(text) = std::fs::read_to_string(&self.file) else {
            return Map::new();
        };
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(mut doc)) => match doc.remove("entries") {
                Some(Value::Object(entries)) => entries,
                _ => Map::new(),
            },
            _ => Map::new(),
        }
    }

    /// The entry of `key` (defaults when missing or unreadable). Blocking.
    pub fn get(&self, key: &str) -> OnboardingEntry {
        self.read_all()
            .remove(key)
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    /// Applies `patch` to `key`, saves atomically and returns the new entry. Blocking.
    pub fn update(&self, key: &str, patch: OnboardingPatch) -> Result<OnboardingEntry, ApiError> {
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let internal =
            |e: serde_json::Error| ApiError::new("internal", format!("onboarding state: {e}"));
        let mut entries = self.read_all();
        let mut entry: OnboardingEntry = entries
            .remove(key)
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        if let Some(step) = patch.step {
            entry.step = step;
        }
        if let Some(choices) = patch.choices {
            for (k, v) in choices {
                if v.is_null() {
                    entry.choices.remove(&k);
                } else {
                    entry.choices.insert(k, v);
                }
            }
        }
        if let Some(done) = patch.done {
            entry.done = done;
        }
        entries.insert(
            key.to_owned(),
            serde_json::to_value(&entry).map_err(internal)?,
        );
        let mut bytes =
            serde_json::to_vec_pretty(&json!({ "entries": entries })).map_err(internal)?;
        bytes.push(b'\n');
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ApiError::new("io", format!("could not create {}: {e}", parent.display()))
            })?;
        }
        forge_ipc::base::paths::write_atomic(&self.file, &bytes).map_err(ApiError::from)?;
        Ok(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(sandbox: &Path) -> DiscoverInputs {
        DiscoverInputs {
            flag: None,
            env_root: None,
            exe_dir: Some(sandbox.join("install")),
            local_app_data: Some(sandbox.join("lad")),
            config_dir: Some(sandbox.join("cfg")),
        }
    }

    fn reason(result: Result<ForgeRoot, RootRejection>) -> &'static str {
        result.expect_err("must be refused").reason()
    }

    #[test]
    fn rejects_relative_file_non_empty_and_inside_bin() {
        let sandbox = tempfile::tempdir().unwrap();
        let inputs = inputs(sandbox.path());
        let p = sandbox.path();

        assert_eq!(
            reason(validate_choice("relative/dir", &inputs)),
            "path_not_absolute"
        );
        assert_eq!(reason(validate_choice("", &inputs)), "path_invalid");
        assert_eq!(
            reason(validate_choice(r"\\srv\share\fb", &inputs)),
            "path_not_local"
        );

        let file = p.join("a-file");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(
            reason(validate_choice(file.to_str().unwrap(), &inputs)),
            "path_is_file"
        );

        let full = p.join("documents");
        std::fs::create_dir_all(&full).unwrap();
        std::fs::write(full.join("report.docx"), b"x").unwrap();
        assert_eq!(
            reason(validate_choice(full.to_str().unwrap(), &inputs)),
            "path_not_empty"
        );

        // Inside the bin of another Forgebox root.
        let other = p.join("other-root");
        std::fs::create_dir_all(other.join("state/locks")).unwrap();
        let in_bin = other.join("bin").join("php");
        assert_eq!(
            reason(validate_choice(in_bin.to_str().unwrap(), &inputs)),
            "path_inside_bin"
        );
        assert!(!in_bin.exists(), "a refused path is not created");
    }

    #[test]
    fn rejects_the_bin_of_the_current_root() {
        let sandbox = tempfile::tempdir().unwrap();
        let inputs = inputs(sandbox.path());
        let current = sandbox.path().join("current");
        std::fs::create_dir_all(&current).unwrap();
        let pointer = ForgeRoot::new(&current).unwrap();
        RootPointer::write_to(&sandbox.path().join("cfg/root.toml"), &pointer).unwrap();
        let bin = current.join("BIN").join("x");
        assert_eq!(
            reason(validate_choice(bin.to_str().unwrap(), &inputs)),
            "path_inside_bin"
        );
    }

    #[test]
    fn rejects_any_override() {
        let sandbox = tempfile::tempdir().unwrap();
        let target = sandbox.path().join("target");

        let mut env = inputs(sandbox.path());
        env.env_root = Some(sandbox.path().join("forced").into_os_string());
        assert_eq!(
            reason(validate_choice(target.to_str().unwrap(), &env)),
            "root_overridden_env"
        );

        let portable = inputs(sandbox.path());
        std::fs::create_dir_all(sandbox.path().join("install")).unwrap();
        std::fs::write(sandbox.path().join("install/portable.flag"), b"").unwrap();
        assert_eq!(
            reason(validate_choice(target.to_str().unwrap(), &portable)),
            "root_overridden_portable"
        );
        assert!(!target.exists());
    }

    #[test]
    fn accepts_a_missing_folder_an_empty_one_and_an_existing_root() {
        let sandbox = tempfile::tempdir().unwrap();
        let inputs = inputs(sandbox.path());
        let p = sandbox.path();

        let fresh = p.join("deep").join("fresh");
        let root = validate_choice(fresh.to_str().unwrap(), &inputs).unwrap();
        assert!(fresh.is_dir());
        assert_eq!(root.path(), fresh);
        assert!(
            std::fs::read_dir(&fresh).unwrap().next().is_none(),
            "probe removed"
        );

        let empty = p.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        validate_choice(empty.to_str().unwrap(), &inputs).unwrap();

        let existing = p.join("existing");
        std::fs::create_dir_all(existing.join("etc")).unwrap();
        std::fs::write(existing.join("etc/forgebox.toml"), b"").unwrap();
        std::fs::write(existing.join("notes.txt"), b"").unwrap();
        validate_choice(existing.to_str().unwrap(), &inputs).unwrap();
    }

    #[test]
    fn rejection_is_an_invalid_input_with_its_reason() {
        let err = ApiError::from(RootRejection::PathNotEmpty);
        assert_eq!(err.code, "invalid_input");
        assert_eq!(err.details.as_ref().unwrap()["reason"], "path_not_empty");
    }

    #[test]
    fn only_root_toml_and_default_are_changeable() {
        assert_eq!(RootOrigin::from(RootSource::Env), RootOrigin::Env);
        assert_eq!(RootOrigin::from(RootSource::Portable), RootOrigin::Portable);
        assert!(RootOrigin::from(RootSource::RootToml).changeable());
        assert!(RootOrigin::from(RootSource::Default).changeable());
        assert!(!RootOrigin::Env.changeable());
        assert!(!RootOrigin::Portable.changeable());
    }

    #[test]
    fn onboarding_survives_a_restart_and_is_keyed_per_root() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("gui").join(ONBOARDING_FILE);
        let a = root_key(&ForgeRoot::new(dir.path().join("A")).unwrap());
        let b = root_key(&ForgeRoot::new(dir.path().join("b")).unwrap());
        assert_ne!(a, b);

        let store = OnboardingStore::new(file.clone());
        assert_eq!(store.get(&a), OnboardingEntry::default());
        assert!(!file.exists(), "reading never writes");
        let mut choices = Map::new();
        choices.insert("php".into(), json!("8.4"));
        choices.insert("dns".into(), json!(true));
        store
            .update(
                &a,
                OnboardingPatch {
                    step: Some("tools".into()),
                    choices: Some(choices),
                    done: None,
                },
            )
            .unwrap();
        let mut remove = Map::new();
        remove.insert("dns".into(), Value::Null);
        store
            .update(
                &a,
                OnboardingPatch {
                    choices: Some(remove),
                    ..Default::default()
                },
            )
            .unwrap();
        let mut tmp = file.clone().into_os_string();
        tmp.push(".tmp");
        assert!(!PathBuf::from(tmp).exists());

        // "Restart": a new store over the same file.
        let again = OnboardingStore::new(file);
        let entry = again.get(&a);
        assert_eq!(entry.step, "tools");
        assert_eq!(entry.choices.get("php"), Some(&json!("8.4")));
        assert!(!entry.choices.contains_key("dns"));
        assert!(!entry.done);
        assert_eq!(
            again.get(&b),
            OnboardingEntry::default(),
            "another root starts fresh"
        );

        again
            .update(
                &a,
                OnboardingPatch {
                    done: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(again.get(&a).done);
        assert_eq!(again.get(&a).step, "tools", "absent fields stay");
    }

    #[test]
    fn remote_contexts_are_keyed_by_name_and_garbage_files_are_ignored() {
        assert_eq!(context_key("hml"), "context:hml");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(ONBOARDING_FILE);
        std::fs::write(&file, "not json").unwrap();
        let store = OnboardingStore::new(file);
        assert_eq!(store.get("context:hml"), OnboardingEntry::default());
        store
            .update(
                "context:hml",
                OnboardingPatch {
                    done: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(store.get("context:hml").done);
    }
}
