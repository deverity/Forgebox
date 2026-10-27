//! `ForgeRoot`: the single installation folder and every path inside it (docs/08 §2).
//!
//! Every path the daemon uses is derived from the root, so the product is portable by
//! construction (ADR-0008). Paths stored in the database or sent over the API are
//! attributes, never identities (ADR-0009). [`ForgeRoot::to_posix_string`] and
//! [`posix_string`] give the `/` form used in generated configs (docs/15 §7).
//!
//! Everything here is blocking file I/O: async callers use `spawn_blocking`.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::{ForgeError, IoResultExt, Result};
use crate::ids::ToolId;
use crate::platform::{self, CaseSensitiveFs, CaseSensitivitySupport};

/// Environment variable that overrides the root (after `--root`).
pub const ROOT_ENV: &str = "FORGEBOX_ROOT";
/// File next to the executables that turns on portable mode (docs/08 §2.2).
pub const PORTABLE_FLAG: &str = "portable.flag";
/// Name of the junction that points at the active CLI version (ADR-0005).
pub const CURRENT_LINK: &str = "current";

/// Top-level directories that get the NTFS case-sensitive flag (docs/08 §2.4).
pub const CASE_SENSITIVE_DIRS: &[&str] = &["www", "etc", "data", "logs", "tmp", "backups"];

/// Absolute path of a Forgebox installation root (`C:\Forgebox` by default).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ForgeRoot(PathBuf);

/// Where [`ForgeRoot::discover`] found the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootSource {
    /// `--root <dir>` on the command line.
    Flag,
    /// `FORGEBOX_ROOT` environment variable.
    Env,
    /// `portable.flag` next to the executable: the root is the executable's folder.
    Portable,
    /// `%LOCALAPPDATA%\Forgebox\root.toml` (per-user install, docs/08 §2.1).
    RootToml,
    /// Built-in default (`C:\Forgebox` on Windows).
    Default,
}

/// Inputs of the discovery, injectable for tests. [`DiscoverInputs::from_env`] reads
/// the real process environment.
#[derive(Debug, Clone, Default)]
pub struct DiscoverInputs {
    /// Value of `--root`.
    pub flag: Option<PathBuf>,
    /// Value of `FORGEBOX_ROOT`.
    pub env_root: Option<OsString>,
    /// Folder of the running executable.
    pub exe_dir: Option<PathBuf>,
    /// `%LOCALAPPDATA%` (Windows) where `Forgebox\root.toml` may live.
    pub local_app_data: Option<PathBuf>,
}

impl DiscoverInputs {
    /// Reads the environment of the current process.
    pub fn from_env(flag: Option<PathBuf>) -> Self {
        Self {
            flag,
            env_root: std::env::var_os(ROOT_ENV),
            exe_dir: std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf)),
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        }
    }
}

/// Options of [`ForgeRoot::init`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitOptions {
    /// Apply the case-sensitive flag to [`CASE_SENSITIVE_DIRS`] (docs/15). Default: true.
    pub case_sensitive: bool,
}

impl Default for InitOptions {
    fn default() -> Self {
        Self {
            case_sensitive: true,
        }
    }
}

/// What [`ForgeRoot::init`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitReport {
    /// Directories created by this call (pre-existing ones are not listed).
    pub created: Vec<PathBuf>,
    /// Directories that are case-sensitive after the call (newly flagged or already).
    pub case_sensitive_applied: Vec<PathBuf>,
    /// Why the flag could not be used at all on this volume/OS, if so. Init still
    /// succeeds; the daemon emits `fs.case_sensitivity_unavailable`.
    pub case_sensitive_unavailable: Option<String>,
    /// Directories where applying the flag failed for a local reason (permissions,
    /// conflicting names...), with the error message.
    pub case_sensitive_failed: Vec<(PathBuf, String)>,
}

impl ForgeRoot {
    /// Wraps `path`, made absolute against the current directory (no `\\?\` prefix,
    /// no symlink resolution).
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Err(ForgeError::InvalidInput("root path is empty".to_owned()));
        }
        let absolute = std::path::absolute(path).at("resolving", path)?;
        Ok(Self(absolute))
    }

    /// Finds the root of the current process (`--root` value in `flag`), in order:
    /// `--root` → `FORGEBOX_ROOT` → `portable.flag` next to the executable →
    /// `%LOCALAPPDATA%\Forgebox\root.toml` → default.
    pub fn discover(flag: Option<PathBuf>) -> Result<(Self, RootSource)> {
        Self::discover_with(&DiscoverInputs::from_env(flag))
    }

    /// [`ForgeRoot::discover`] with explicit inputs.
    pub fn discover_with(inputs: &DiscoverInputs) -> Result<(Self, RootSource)> {
        if let Some(flag) = &inputs.flag {
            return Ok((Self::new(flag)?, RootSource::Flag));
        }
        if let Some(env) = inputs.env_root.as_ref().filter(|v| !v.is_empty()) {
            return Ok((Self::new(env)?, RootSource::Env));
        }
        if let Some(exe_dir) = &inputs.exe_dir {
            if exe_dir.join(PORTABLE_FLAG).is_file() {
                return Ok((Self::new(exe_dir)?, RootSource::Portable));
            }
        }
        if let Some(lad) = &inputs.local_app_data {
            let file = lad.join("Forgebox").join("root.toml");
            if file.is_file() {
                return Ok((Self::from_root_toml(&file)?, RootSource::RootToml));
            }
        }
        Ok((Self::new(default_root())?, RootSource::Default))
    }

    /// Reads `root = "D:/Forgebox"` from a `root.toml` file. A relative value is
    /// resolved against the file's folder.
    fn from_root_toml(file: &Path) -> Result<Self> {
        #[derive(serde::Deserialize)]
        struct RootToml {
            root: PathBuf,
        }
        let text = std::fs::read_to_string(file).at("reading", file)?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let parsed: RootToml = toml::from_str(text)
            .map_err(|e| ForgeError::ConfigInvalid(format!("{}: {e}", file.display())))?;
        let root = if parsed.root.is_absolute() {
            parsed.root
        } else {
            file.parent().unwrap_or(Path::new(".")).join(parsed.root)
        };
        Self::new(root)
    }

    /// The root itself.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The root with `/` separators, for templates.
    pub fn to_posix_string(&self) -> String {
        posix_string(&self.0)
    }

    fn join(&self, parts: &[&str]) -> PathBuf {
        let mut path = self.0.clone();
        path.extend(parts);
        path
    }

    /// `app/`: GUI resources not embedded in the executable.
    pub fn app(&self) -> PathBuf {
        self.join(&["app"])
    }
    /// `bin/`: installed tools, `bin/<tool>/<version>`.
    pub fn bin(&self) -> PathBuf {
        self.join(&["bin"])
    }
    /// `bin/shims/`: optional `.cmd` shims.
    pub fn bin_shims(&self) -> PathBuf {
        self.join(&["bin", "shims"])
    }
    /// `etc/`: rendered configuration.
    pub fn etc(&self) -> PathBuf {
        self.join(&["etc"])
    }
    /// `etc/templates/`: user overrides of embedded templates.
    pub fn etc_templates(&self) -> PathBuf {
        self.join(&["etc", "templates"])
    }
    /// `etc/sites/`: readable YAML export of each site.
    pub fn etc_sites(&self) -> PathBuf {
        self.join(&["etc", "sites"])
    }
    /// `data/`: instance data (never touched by updates).
    pub fn data(&self) -> PathBuf {
        self.join(&["data"])
    }
    /// `www/`: default parked directory.
    pub fn www(&self) -> PathBuf {
        self.join(&["www"])
    }
    /// `certs/`.
    pub fn certs(&self) -> PathBuf {
        self.join(&["certs"])
    }
    /// `certs/ca/`: local CA.
    pub fn certs_ca(&self) -> PathBuf {
        self.join(&["certs", "ca"])
    }
    /// `certs/sites/`: leaf certificates.
    pub fn certs_sites(&self) -> PathBuf {
        self.join(&["certs", "sites"])
    }
    /// `logs/`.
    pub fn logs(&self) -> PathBuf {
        self.join(&["logs"])
    }
    /// `state/`: internal state (not edited by hand).
    pub fn state(&self) -> PathBuf {
        self.join(&["state"])
    }
    /// `state/registry/`: registry caches.
    pub fn state_registry(&self) -> PathBuf {
        self.join(&["state", "registry"])
    }
    /// `state/locks/`.
    pub fn state_locks(&self) -> PathBuf {
        self.join(&["state", "locks"])
    }
    /// `tmp/`: ephemeral, cleaned automatically.
    pub fn tmp(&self) -> PathBuf {
        self.join(&["tmp"])
    }
    /// `tmp/downloads/`.
    pub fn tmp_downloads(&self) -> PathBuf {
        self.join(&["tmp", "downloads"])
    }
    /// `tmp/stage/`: extraction staging before the atomic move into `bin/`.
    pub fn tmp_stage(&self) -> PathBuf {
        self.join(&["tmp", "stage"])
    }
    /// `backups/`.
    pub fn backups(&self) -> PathBuf {
        self.join(&["backups"])
    }
    /// `manifests/local/`: user manifests outside the registry.
    pub fn manifests_local(&self) -> PathBuf {
        self.join(&["manifests", "local"])
    }

    /// `etc/forgebox.toml`.
    pub fn forgebox_toml(&self) -> PathBuf {
        self.join(&["etc", "forgebox.toml"])
    }
    /// `state/forgebox.db`.
    pub fn db(&self) -> PathBuf {
        self.join(&["state", "forgebox.db"])
    }
    /// `state/ipc.token`.
    pub fn ipc_token(&self) -> PathBuf {
        self.join(&["state", "ipc.token"])
    }
    /// `portable.flag`.
    pub fn portable_flag(&self) -> PathBuf {
        self.join(&[PORTABLE_FLAG])
    }
    /// `VERSION`.
    pub fn version_file(&self) -> PathBuf {
        self.join(&["VERSION"])
    }

    /// Whether this installation runs in portable mode (`portable.flag` present).
    pub fn is_portable(&self) -> bool {
        self.portable_flag().is_file()
    }

    /// `bin/<tool>/`.
    pub fn tool_dir(&self, tool: &ToolId) -> PathBuf {
        self.join(&["bin", tool.as_str()])
    }

    /// `bin/<tool>/<canonical_name>` (docs/08 §5, e.g. `8.3.37-nts-vs17-x64`).
    /// `canonical_name` must be a single, non-reserved path component.
    pub fn tool_version_dir(&self, tool: &ToolId, canonical_name: &str) -> Result<PathBuf> {
        validate_component(canonical_name)?;
        Ok(self.join(&["bin", tool.as_str(), canonical_name]))
    }

    /// `bin/<tool>/current`: junction to the active CLI version (ADR-0005).
    pub fn tool_current_link(&self, tool: &ToolId) -> PathBuf {
        self.join(&["bin", tool.as_str(), CURRENT_LINK])
    }

    /// Every directory [`ForgeRoot::init`] creates, parents before children.
    pub fn layout_dirs(&self) -> Vec<PathBuf> {
        vec![
            self.app(),
            self.bin(),
            self.bin_shims(),
            self.etc(),
            self.etc_templates(),
            self.etc_sites(),
            self.data(),
            self.www(),
            self.certs(),
            self.certs_ca(),
            self.certs_sites(),
            self.logs(),
            self.state(),
            self.state_registry(),
            self.state_locks(),
            self.tmp(),
            self.tmp_downloads(),
            self.tmp_stage(),
            self.backups(),
            self.join(&["manifests"]),
            self.manifests_local(),
        ]
    }

    /// Creates the whole tree of docs/08 §2 and, when `opts.case_sensitive`, flags
    /// [`CASE_SENSITIVE_DIRS`] (and the subfolders created here) as case-sensitive
    /// before anything is put inside them. `bin/`, `app/`, `state/`, `certs/` and
    /// `manifests/` stay case-insensitive. Idempotent.
    ///
    /// Not being able to use the flag is not an error: it is reported in
    /// [`InitReport`]. Existing flags are never removed here.
    pub fn init(&self, opts: InitOptions) -> Result<InitReport> {
        let platform = platform::current();
        self.init_with(opts, platform.case_sensitive_fs())
    }

    /// [`ForgeRoot::init`] with an explicit [`CaseSensitiveFs`] (tests, other platforms).
    pub fn init_with(&self, opts: InitOptions, cs: &dyn CaseSensitiveFs) -> Result<InitReport> {
        let mut report = InitReport::default();
        create_dir(&self.0, &mut report)?;

        let mut cs_enabled = opts.case_sensitive;
        if cs_enabled {
            if let CaseSensitivitySupport::Unsupported(reason) = cs.support(&self.0) {
                report.case_sensitive_unavailable = Some(reason);
                cs_enabled = false;
            }
        }

        let cs_roots: Vec<PathBuf> = CASE_SENSITIVE_DIRS
            .iter()
            .map(|d| self.join(&[d]))
            .collect();
        for dir in self.layout_dirs() {
            create_dir(&dir, &mut report)?;
            let wants_flag = cs_roots.iter().any(|root| dir.starts_with(root));
            if cs_enabled && wants_flag {
                match ensure_case_sensitive(cs, &dir) {
                    Ok(()) => report.case_sensitive_applied.push(dir),
                    Err(ForgeError::UnsupportedPlatform(reason)) => {
                        report.case_sensitive_unavailable = Some(reason);
                        cs_enabled = false;
                    }
                    Err(err) => report.case_sensitive_failed.push((dir, err.to_string())),
                }
            }
        }
        Ok(report)
    }
}

fn create_dir(dir: &Path, report: &mut InitReport) -> Result<()> {
    match std::fs::create_dir(dir) {
        Ok(()) => {
            report.created.push(dir.to_path_buf());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir).at("creating", dir)?;
            report.created.push(dir.to_path_buf());
            Ok(())
        }
        Err(e) => Err(ForgeError::io_at("creating", dir, e)),
    }
}

fn ensure_case_sensitive(cs: &dyn CaseSensitiveFs, dir: &Path) -> Result<()> {
    if cs.get(dir)? {
        return Ok(());
    }
    cs.set(dir, true)?;
    if cs.get(dir)? {
        Ok(())
    } else {
        Err(ForgeError::Conflict(format!(
            "{}: flag set but not reported back",
            dir.display()
        )))
    }
}

impl fmt::Display for ForgeRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.display(), f)
    }
}

impl AsRef<Path> for ForgeRoot {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

/// Built-in default root for this OS.
pub fn default_root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\Forgebox")
    } else if let Some(data) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        PathBuf::from(data).join("forgebox")
    } else if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        PathBuf::from(home).join(".local/share/forgebox")
    } else {
        PathBuf::from("/opt/forgebox")
    }
}

/// `path` with `/` separators and without the Win32 verbatim prefix (`\\?\`), as used
/// in generated configs (docs/15 §7).
pub fn posix_string(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = text
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .unwrap_or_else(|| text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned());
    text.replace('\\', "/")
}

/// `path` without the Win32 verbatim prefix (`\\?\C:\x` → `C:\x`, `\\?\UNC\srv\share` →
/// `\\srv\share`), separators unchanged: the form other programs and Windows APIs expect
/// (firewall rules, Defender exclusions, messages). Other verbatim forms (volume GUIDs)
/// are kept as they are.
pub fn plain_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// Rejects anything that is not one plain path component (no separators, no `.`/`..`,
/// no drive, no `current`).
fn validate_component(name: &str) -> Result<()> {
    let invalid = |why: &str| Err(ForgeError::InvalidInput(format!("{name:?}: {why}")));
    if name.is_empty() || name == "." || name == ".." {
        return invalid("not a folder name");
    }
    if name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
        || name.chars().any(char::is_control)
    {
        return invalid("contains characters not allowed in a folder name");
    }
    if name.ends_with(['.', ' ']) {
        return invalid("must not end with '.' or space");
    }
    if name.eq_ignore_ascii_case(CURRENT_LINK) {
        return invalid("reserved for the active version junction");
    }
    Ok(())
}

/// Writes `bytes` to `path` atomically: `<path>.tmp` then rename over the target, so
/// readers never see a half-written file. Blocking.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes).at("writing", &tmp)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(ForgeError::io_at("replacing", path, e));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Mutex;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::platform::CaseSensitivitySupport;

    fn root(dir: &Path) -> ForgeRoot {
        ForgeRoot::new(dir).unwrap()
    }

    #[test]
    fn subdirectories_follow_release_layout() {
        let base = tempfile::tempdir().unwrap();
        let r = root(base.path());
        let rel = |p: PathBuf| posix_string(p.strip_prefix(r.path()).unwrap());
        assert_eq!(rel(r.app()), "app");
        assert_eq!(rel(r.bin()), "bin");
        assert_eq!(rel(r.bin_shims()), "bin/shims");
        assert_eq!(rel(r.etc()), "etc");
        assert_eq!(rel(r.etc_templates()), "etc/templates");
        assert_eq!(rel(r.etc_sites()), "etc/sites");
        assert_eq!(rel(r.data()), "data");
        assert_eq!(rel(r.www()), "www");
        assert_eq!(rel(r.certs()), "certs");
        assert_eq!(rel(r.certs_ca()), "certs/ca");
        assert_eq!(rel(r.certs_sites()), "certs/sites");
        assert_eq!(rel(r.logs()), "logs");
        assert_eq!(rel(r.state()), "state");
        assert_eq!(rel(r.state_registry()), "state/registry");
        assert_eq!(rel(r.state_locks()), "state/locks");
        assert_eq!(rel(r.tmp()), "tmp");
        assert_eq!(rel(r.tmp_downloads()), "tmp/downloads");
        assert_eq!(rel(r.tmp_stage()), "tmp/stage");
        assert_eq!(rel(r.backups()), "backups");
        assert_eq!(rel(r.manifests_local()), "manifests/local");
        assert_eq!(rel(r.forgebox_toml()), "etc/forgebox.toml");
        assert_eq!(rel(r.db()), "state/forgebox.db");
        assert_eq!(rel(r.ipc_token()), "state/ipc.token");
        assert_eq!(rel(r.portable_flag()), "portable.flag");
        assert_eq!(rel(r.version_file()), "VERSION");
    }

    #[test]
    fn tool_paths() {
        let base = tempfile::tempdir().unwrap();
        let r = root(base.path());
        let php = ToolId::new("php").unwrap();
        let dir = r.tool_version_dir(&php, "8.3.37-nts-vs17-x64").unwrap();
        assert_eq!(dir, r.bin().join("php").join("8.3.37-nts-vs17-x64"));
        assert_eq!(
            r.tool_current_link(&php),
            r.bin().join("php").join("current")
        );
        assert_eq!(r.tool_dir(&php), r.bin().join("php"));
        for bad in [
            "", ".", "..", "../x", "a/b", "a\\b", "C:", "current", "CURRENT", "x.", "x ", "a\u{0}",
        ] {
            let err = r.tool_version_dir(&php, bad).unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{bad:?}");
        }
    }

    #[test]
    fn new_makes_absolute() {
        let r = ForgeRoot::new("relative-root").unwrap();
        assert!(r.path().is_absolute());
        assert!(ForgeRoot::new("").is_err());
    }

    #[test]
    fn posix_strings() {
        assert_eq!(
            posix_string(Path::new(r"C:\Forgebox\www")),
            "C:/Forgebox/www"
        );
        assert_eq!(posix_string(Path::new(r"\\?\C:\Forgebox")), "C:/Forgebox");
        assert_eq!(
            posix_string(Path::new(r"\\?\UNC\srv\share\x")),
            "//srv/share/x"
        );
        assert_eq!(posix_string(Path::new("/opt/forgebox")), "/opt/forgebox");
        assert_eq!(
            plain_path(Path::new(r"\\?\C:\Forgebox\bin")),
            PathBuf::from(r"C:\Forgebox\bin")
        );
        assert_eq!(
            plain_path(Path::new(r"\\?\UNC\srv\share\x")),
            PathBuf::from(r"\\srv\share\x")
        );
        assert_eq!(
            plain_path(Path::new(r"\\?\Volume{x}\a")),
            PathBuf::from(r"\\?\Volume{x}\a"),
            "volume GUID paths keep their prefix"
        );
        assert_eq!(plain_path(Path::new("C:/x")), PathBuf::from("C:/x"));
        let base = tempfile::tempdir().unwrap();
        assert!(!root(base.path()).to_posix_string().contains('\\'));
    }

    #[test]
    fn discover_order() {
        let base = tempfile::tempdir().unwrap();
        let exe_dir = base.path().join("exe");
        let lad = base.path().join("lad");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::create_dir_all(lad.join("Forgebox")).unwrap();
        std::fs::write(
            lad.join("Forgebox").join("root.toml"),
            "root = \"from-root-toml\"\n",
        )
        .unwrap();

        let mut inputs = DiscoverInputs {
            flag: Some(base.path().join("flag")),
            env_root: Some(base.path().join("env").into_os_string()),
            exe_dir: Some(exe_dir.clone()),
            local_app_data: Some(lad.clone()),
        };
        let (r, src) = ForgeRoot::discover_with(&inputs).unwrap();
        assert_eq!(
            (r.path(), src),
            (base.path().join("flag").as_path(), RootSource::Flag)
        );

        inputs.flag = None;
        let (r, src) = ForgeRoot::discover_with(&inputs).unwrap();
        assert_eq!(
            (r.path(), src),
            (base.path().join("env").as_path(), RootSource::Env)
        );

        inputs.env_root = Some(OsString::new());
        std::fs::write(exe_dir.join(PORTABLE_FLAG), "").unwrap();
        let (r, src) = ForgeRoot::discover_with(&inputs).unwrap();
        assert_eq!((r.path(), src), (exe_dir.as_path(), RootSource::Portable));
        assert!(r.is_portable());

        std::fs::remove_file(exe_dir.join(PORTABLE_FLAG)).unwrap();
        let (r, src) = ForgeRoot::discover_with(&inputs).unwrap();
        assert_eq!(src, RootSource::RootToml);
        assert_eq!(r.path(), lad.join("Forgebox").join("from-root-toml"));

        inputs.local_app_data = None;
        let (r, src) = ForgeRoot::discover_with(&inputs).unwrap();
        assert_eq!(
            (r.path(), src),
            (default_root().as_path(), RootSource::Default)
        );
    }

    #[test]
    fn discover_rejects_broken_root_toml() {
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("Forgebox")).unwrap();
        std::fs::write(base.path().join("Forgebox").join("root.toml"), "nope").unwrap();
        let inputs = DiscoverInputs {
            local_app_data: Some(base.path().to_path_buf()),
            ..DiscoverInputs::default()
        };
        let err = ForgeRoot::discover_with(&inputs).unwrap_err();
        assert_eq!(err.code(), "config_invalid");
    }

    #[test]
    fn write_atomic_replaces_content() {
        let base = tempfile::tempdir().unwrap();
        let file = base.path().join("x.toml");
        write_atomic(&file, b"a").unwrap();
        write_atomic(&file, b"b").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"b");
        assert!(!base.path().join("x.toml.tmp").exists());
    }

    /// In-memory flag store standing in for NTFS, so init logic is tested everywhere.
    #[derive(Default)]
    struct FakeCs {
        unsupported: Option<String>,
        deny: Option<PathBuf>,
        flagged: Mutex<HashSet<PathBuf>>,
    }

    impl CaseSensitiveFs for FakeCs {
        fn support(&self, _dir: &Path) -> CaseSensitivitySupport {
            match &self.unsupported {
                Some(r) => CaseSensitivitySupport::Unsupported(r.clone()),
                None => CaseSensitivitySupport::Supported,
            }
        }
        fn get(&self, dir: &Path) -> Result<bool> {
            Ok(self.flagged.lock().unwrap().contains(dir))
        }
        fn set(&self, dir: &Path, enabled: bool) -> Result<()> {
            if self.deny.as_deref() == Some(dir) {
                return Err(ForgeError::NeedsElevation("fs.set_case_sensitive".into()));
            }
            assert!(
                std::fs::read_dir(dir).unwrap().next().is_none(),
                "flag must be applied before content is created: {}",
                dir.display()
            );
            let mut flagged = self.flagged.lock().unwrap();
            if enabled {
                flagged.insert(dir.to_path_buf());
            } else {
                flagged.remove(dir);
            }
            Ok(())
        }
    }

    #[test]
    fn init_creates_tree_and_flags_only_cs_dirs() {
        let base = tempfile::tempdir().unwrap();
        let r = root(&base.path().join("Forgebox"));
        let cs = FakeCs::default();
        let report = r.init_with(InitOptions::default(), &cs).unwrap();

        for dir in r.layout_dirs() {
            assert!(dir.is_dir(), "{}", dir.display());
        }
        assert_eq!(report.created.len(), r.layout_dirs().len() + 1);
        assert_eq!(report.case_sensitive_unavailable, None);
        assert!(report.case_sensitive_failed.is_empty());
        for name in CASE_SENSITIVE_DIRS {
            assert!(cs.get(&r.path().join(name)).unwrap(), "{name}");
        }
        for dir in [
            r.tmp_downloads(),
            r.tmp_stage(),
            r.etc_templates(),
            r.etc_sites(),
        ] {
            assert!(
                report.case_sensitive_applied.contains(&dir),
                "{}",
                dir.display()
            );
        }
        for dir in [r.bin(), r.state(), r.app(), r.certs(), r.manifests_local()] {
            assert!(!cs.get(&dir).unwrap(), "{}", dir.display());
        }

        let again = r.init_with(InitOptions::default(), &cs).unwrap();
        assert!(again.created.is_empty(), "idempotent");
        assert_eq!(again.case_sensitive_applied, report.case_sensitive_applied);
    }

    #[test]
    fn init_reports_unsupported_without_failing() {
        let base = tempfile::tempdir().unwrap();
        let r = root(base.path());
        let cs = FakeCs {
            unsupported: Some("FAT32 volume".into()),
            ..FakeCs::default()
        };
        let report = r.init_with(InitOptions::default(), &cs).unwrap();
        assert_eq!(
            report.case_sensitive_unavailable.as_deref(),
            Some("FAT32 volume")
        );
        assert!(report.case_sensitive_applied.is_empty());
        assert!(r.www().is_dir());
    }

    #[test]
    fn init_without_case_sensitivity_touches_no_flag() {
        let base = tempfile::tempdir().unwrap();
        let r = root(base.path());
        let cs = FakeCs::default();
        let report = r
            .init_with(
                InitOptions {
                    case_sensitive: false,
                },
                &cs,
            )
            .unwrap();
        assert!(report.case_sensitive_applied.is_empty());
        assert!(cs.flagged.lock().unwrap().is_empty());
    }

    #[test]
    fn init_records_per_dir_failures() {
        let base = tempfile::tempdir().unwrap();
        let r = root(base.path());
        let cs = FakeCs {
            deny: Some(r.logs()),
            ..FakeCs::default()
        };
        let report = r.init_with(InitOptions::default(), &cs).unwrap();
        assert_eq!(report.case_sensitive_failed.len(), 1);
        assert_eq!(report.case_sensitive_failed[0].0, r.logs());
        assert!(report.case_sensitive_applied.contains(&r.www()));
    }
}
