//! Runtimes of a process (`runtime: nodejs@22`, docs/06 §4.2–4.3): which installed version
//! of a tool a process runs on, the folders it puts on the `PATH` and the variables the
//! tool injects.
//!
//! Nothing here knows a tool by name. A [`RuntimeTool`] is read from the tool manifest:
//! `install.path` (folders on the PATH), `install.shims` (the commands it provides, so
//! `npm run dev` implies the tool that ships `npm`), `install.version_files` (`.nvmrc`,
//! `.node-version`, `.php-version`) and `config.env` (`NODE_EXTRA_CA_CERTS`,
//! `npm_config_prefix` for Node). A tool nobody described still resolves, with the version
//! folder as its only PATH entry.
//!
//! Resolution order for the version, first match wins:
//! 1. `runtime: tool@selector` of the process;
//! 2. a version file of the tool, from the process folder up to the project folder;
//! 3. `spec.runtimes[]` of `forgebox.yaml` for the tool;
//! 4. the `current` link of the tool, else the newest installed version.
//!
//! A selector is `22`, `22.12`, `22.12.0` or `v22.12.0`: the newest installed version that
//! starts with those numbers. A selector that matches nothing is an error that lists what
//! is installed; another version is never used silently.
//!
//! Everything here is blocking (reads folders): the manager runs it in `spawn_blocking`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use forge_core::error::{ForgeError, Result};
use forge_core::ids::ToolId;
use forge_core::paths::{ForgeRoot, plain_path, posix_string};
use forge_manifest::schema::tool::{Shim, ToolManifest};
use forge_manifest::template;
use serde::{Deserialize, Serialize};

use crate::messages;

/// What a tool manifest says that matters to the processes running on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeTool {
    pub id: ToolId,
    /// Commands it provides (`node`, `npm`, `npx`, `corepack`).
    pub commands: Vec<String>,
    /// Files of a project that pick its version (`.nvmrc`, `.node-version`).
    pub version_files: Vec<String>,
    /// Version subfolders on the PATH (`install.path`, `.` = the version folder).
    pub path: Vec<String>,
    /// `config.env` templates (`{{ paths.certs }}`, `{{ version }}`, `{{ series }}`...).
    pub env: BTreeMap<String, String>,
    /// `versioning.series_key` (`{major}.{minor}`).
    pub series_key: String,
}

impl RuntimeTool {
    /// A tool without a manifest: the version folder is its PATH and it injects nothing.
    pub fn bare(id: ToolId) -> Self {
        Self {
            id,
            commands: Vec::new(),
            version_files: Vec::new(),
            path: vec![".".to_owned()],
            env: BTreeMap::new(),
            series_key: forge_manifest::schema::tool::DEFAULT_SERIES_KEY.to_owned(),
        }
    }

    /// Reads the relevant parts of a tool manifest.
    pub fn from_manifest(manifest: &ToolManifest) -> Self {
        let spec = &manifest.spec;
        let commands = spec
            .install
            .shims
            .iter()
            .map(|shim| match shim {
                Shim::Name(name) => name.clone(),
                Shim::Detailed(detailed) => detailed.name.clone(),
            })
            .collect();
        let mut path = spec.install.path.clone();
        if path.is_empty() {
            path.push(".".to_owned());
        }
        Self {
            id: manifest.metadata.id.clone(),
            commands,
            version_files: spec.install.version_files.clone(),
            path,
            env: spec.config.env.clone(),
            series_key: spec.versioning.series_key.clone(),
        }
    }
}

/// Where the version of a runtime came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum RuntimeSource {
    /// `runtime: tool@selector` of the process.
    Declared(String),
    /// A version file of the project (`.nvmrc`).
    VersionFile(String),
    /// `spec.runtimes[]` of `forgebox.yaml`.
    Project(String),
    /// The `current` link, or the newest installed version.
    Current,
}

/// The installed version a process runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRuntime {
    pub tool: ToolId,
    /// Folder name under `bin/<tool>/` (`22.12.0`, `8.3.37-nts-vs17-x64`).
    pub folder: String,
    /// Numeric version (`22.12.0`).
    pub version: String,
    /// `bin/<tool>/<folder>`.
    pub dir: PathBuf,
    /// PATH entries, in order (`install.path` under `dir`).
    pub path: Vec<PathBuf>,
    /// Rendered `config.env`, in the manifest's key order.
    pub env: Vec<(String, String)>,
    pub source: RuntimeSource,
}

impl ResolvedRuntime {
    /// `nodejs 22.12.0`.
    pub fn label(&self) -> String {
        format!("{} {}", self.tool, self.version)
    }
}

/// One installed version folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledVersion {
    pub folder: String,
    /// Numeric components of the version (`[22, 12, 0]`).
    pub numbers: Vec<u64>,
}

/// Numbers of a version text: `v22.12.0-nts-vs17-x64` → `[22, 12, 0]`. `None` when it
/// does not start with a number.
pub fn version_numbers(text: &str) -> Option<Vec<u64>> {
    let text = text.trim().trim_start_matches(['v', 'V', '=']);
    let core = text.split(['-', '+']).next().unwrap_or("");
    let numbers: Option<Vec<u64>> = core.split('.').map(|part| part.parse().ok()).collect();
    numbers.filter(|n| !n.is_empty())
}

/// Series of `numbers` under `key` (`{major}.{minor}`; absent components are `0`).
fn series_of(key: &str, numbers: &[u64]) -> String {
    let part = |i: usize| numbers.get(i).copied().unwrap_or(0).to_string();
    key.replace("{major}", &part(0))
        .replace("{minor}", &part(1))
        .replace("{patch}", &part(2))
}

/// The tools a process can run on, and the installation they live in.
#[derive(Debug, Clone)]
pub struct RuntimeCatalog {
    root: ForgeRoot,
    tools: BTreeMap<ToolId, RuntimeTool>,
}

impl RuntimeCatalog {
    /// Catalog over `root` and the described `tools`.
    pub fn new(root: ForgeRoot, tools: impl IntoIterator<Item = RuntimeTool>) -> Self {
        Self {
            root,
            tools: tools.into_iter().map(|t| (t.id.clone(), t)).collect(),
        }
    }

    /// Catalog built from tool manifests.
    pub fn from_manifests<'a>(
        root: ForgeRoot,
        manifests: impl IntoIterator<Item = &'a ToolManifest>,
    ) -> Self {
        Self::new(root, manifests.into_iter().map(RuntimeTool::from_manifest))
    }

    /// The installation root.
    pub fn root(&self) -> &ForgeRoot {
        &self.root
    }

    /// The description of `tool` (a bare one when the catalog lacks it).
    pub fn tool(&self, tool: &ToolId) -> RuntimeTool {
        self.tools
            .get(tool)
            .cloned()
            .unwrap_or_else(|| RuntimeTool::bare(tool.clone()))
    }

    /// The tool that provides command `word` (`npm` → nodejs), by `install.shims`.
    pub fn tool_for_command(&self, word: &str) -> Option<&RuntimeTool> {
        let word = word.to_ascii_lowercase();
        let stem = word.rsplit_once('.').map_or(word.as_str(), |(stem, ext)| {
            if matches!(ext, "exe" | "cmd" | "bat" | "com") {
                stem
            } else {
                word.as_str()
            }
        });
        self.tools
            .values()
            .find(|tool| tool.commands.iter().any(|c| c.eq_ignore_ascii_case(stem)))
    }

    /// Installed versions of `tool` (`bin/<tool>/<folder>` with a numeric start; the
    /// `current` link and loose files are skipped), newest first.
    pub fn installed(&self, tool: &ToolId) -> Result<Vec<InstalledVersion>> {
        let dir = self.root.tool_dir(tool);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(ForgeError::io_at("reading", &dir, e)),
        };
        let mut versions = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| ForgeError::io_at("reading", &dir, e))?;
            if !entry.path().is_dir() {
                continue;
            }
            let folder = entry.file_name().to_string_lossy().into_owned();
            if let Some(numbers) = version_numbers(&folder) {
                versions.push(InstalledVersion { folder, numbers });
            }
        }
        versions.sort_by(|a, b| b.numbers.cmp(&a.numbers).then(a.folder.cmp(&b.folder)));
        Ok(versions)
    }

    /// Resolves `tool` for a process whose folder is `cwd` inside the project `project`.
    /// `declared` is the `runtime:` selector, `project_selector` the `forgebox.yaml`
    /// entry for the tool. Blocking.
    pub fn resolve(
        &self,
        tool: &ToolId,
        declared: Option<&str>,
        project_selector: Option<&str>,
        cwd: &Path,
        project: &Path,
    ) -> Result<ResolvedRuntime> {
        let description = self.tool(tool);
        let installed = self.installed(tool)?;
        let (source, selector) = if let Some(selector) = declared {
            (
                RuntimeSource::Declared(selector.to_owned()),
                Some(selector.to_owned()),
            )
        } else if let Some((file, selector)) = read_version_file(&description, cwd, project) {
            (RuntimeSource::VersionFile(file), Some(selector))
        } else if let Some(selector) = project_selector {
            (
                RuntimeSource::Project(selector.to_owned()),
                Some(selector.to_owned()),
            )
        } else {
            (RuntimeSource::Current, None)
        };
        let chosen = match &selector {
            Some(selector) => pick(&installed, selector, tool)?,
            None => self.pick_current(tool, &installed)?,
        };
        let dir = self.root.tool_version_dir(tool, &chosen.folder)?;
        self.describe(&description, &chosen, dir, source)
    }

    /// `bin/<tool>/current` when it points at an installed version, else the newest.
    fn pick_current(
        &self,
        tool: &ToolId,
        installed: &[InstalledVersion],
    ) -> Result<InstalledVersion> {
        let link = self.root.tool_current_link(tool);
        if let Ok(target) = std::fs::canonicalize(&link) {
            let target = plain_path(&target);
            let name = target.file_name().map(|n| n.to_string_lossy().into_owned());
            if let Some(found) = installed.iter().find(|v| Some(&v.folder) == name.as_ref()) {
                return Ok(found.clone());
            }
        }
        installed
            .first()
            .cloned()
            .ok_or_else(|| ForgeError::NotFound(messages::runtime_not_installed(tool.as_str()).1))
    }

    fn describe(
        &self,
        tool: &RuntimeTool,
        version: &InstalledVersion,
        dir: PathBuf,
        source: RuntimeSource,
    ) -> Result<ResolvedRuntime> {
        let numeric = version
            .numbers
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".");
        let mut path = Vec::new();
        for sub in &tool.path {
            path.push(under(&dir, sub)?);
        }
        let vars = EnvVars {
            tool: tool.id.as_str(),
            version: &numeric,
            series: series_of(&tool.series_key, &version.numbers),
            root: &self.root,
        };
        let mut env = Vec::new();
        for (name, template) in &tool.env {
            env.push((name.clone(), vars.render(template)?));
        }
        Ok(ResolvedRuntime {
            tool: tool.id.clone(),
            folder: version.folder.clone(),
            version: numeric,
            dir,
            path,
            env,
            source,
        })
    }
}

/// `dir/sub` for a manifest subfolder (`.` = `dir`); `..` and absolute paths are refused.
fn under(dir: &Path, sub: &str) -> Result<PathBuf> {
    let rel = Path::new(sub);
    let escapes = rel.is_absolute()
        || rel.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        });
    if escapes {
        return Err(ForgeError::ConfigInvalid(format!(
            "install.path {sub:?} leaves the version folder"
        )));
    }
    Ok(if sub == "." || sub.is_empty() {
        dir.to_path_buf()
    } else {
        dir.join(rel)
    })
}

/// The newest installed version that starts with the numbers of `selector`.
fn pick(installed: &[InstalledVersion], selector: &str, tool: &ToolId) -> Result<InstalledVersion> {
    let wanted = version_numbers(selector).ok_or_else(|| {
        ForgeError::InvalidInput(messages::runtime_selector_invalid(tool.as_str(), selector).1)
    })?;
    installed
        .iter()
        .find(|v| v.numbers.starts_with(&wanted))
        .cloned()
        .ok_or_else(|| {
            let have: Vec<&str> = installed.iter().map(|v| v.folder.as_str()).collect();
            ForgeError::NotFound(
                messages::runtime_version_missing(tool.as_str(), selector, &have).1,
            )
        })
}

/// The first version file of `tool` found from `cwd` up to `project` (inclusive): its
/// name and the first word of its first line that is not a comment. An empty file does
/// not decide.
pub fn read_version_file(
    tool: &RuntimeTool,
    cwd: &Path,
    project: &Path,
) -> Option<(String, String)> {
    if tool.version_files.is_empty() {
        return None;
    }
    let mut dir = Some(cwd);
    while let Some(current) = dir {
        for file in &tool.version_files {
            let Ok(text) = std::fs::read_to_string(current.join(file)) else {
                continue;
            };
            let selector = text
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with('#'))
                .and_then(|line| line.split_whitespace().next());
            if let Some(selector) = selector {
                return Some((file.clone(), selector.to_owned()));
            }
        }
        if current == project {
            break;
        }
        dir = current
            .parent()
            .filter(|parent| parent.starts_with(project));
    }
    None
}

/// Variables of `config.env` at run time (the subset of docs/04 §1.4 that needs no
/// instance): `version`, `series`, `tool` and `paths.<dir>`.
struct EnvVars<'a> {
    tool: &'a str,
    version: &'a str,
    series: String,
    root: &'a ForgeRoot,
}

impl EnvVars<'_> {
    fn lookup(&self, segments: &[String]) -> std::result::Result<String, String> {
        let dir = |p: PathBuf| posix_string(&p);
        match segments {
            [one] if one == "version" => Ok(self.version.to_owned()),
            [one] if one == "series" => Ok(self.series.clone()),
            [one] if one == "tool" => Ok(self.tool.to_owned()),
            [paths, key] if paths == "paths" => match key.as_str() {
                "root" => Ok(self.root.to_posix_string()),
                "bin" => Ok(dir(self.root.bin())),
                "etc" => Ok(dir(self.root.etc())),
                "data" => Ok(dir(self.root.data())),
                "logs" => Ok(dir(self.root.logs())),
                "tmp" => Ok(dir(self.root.tmp())),
                "www" => Ok(dir(self.root.www())),
                "certs" => Ok(dir(self.root.certs())),
                other => Err(format!("`paths.{other}` is not available to processes")),
            },
            other => Err(format!(
                "`{}` is not available to processes",
                other.join(".")
            )),
        }
    }

    /// Replaces every `{{ var }}` / `{{ var | win_path }}`. Values are POSIX paths; on
    /// Windows the variables are consumed by programs that accept `/`, and `win_path`
    /// converts when a tool insists on `\`.
    fn render(&self, text: &str) -> Result<String> {
        let invalid = |why: String| ForgeError::ConfigInvalid(format!("{text:?}: {why}"));
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        for found in template::scan(text) {
            let var = found.map_err(invalid)?;
            let start = rest
                .find("{{")
                .ok_or_else(|| invalid("unbalanced template".to_owned()))?;
            let end = rest[start..]
                .find("}}")
                .map(|e| start + e + 2)
                .ok_or_else(|| invalid("unclosed `{{`".to_owned()))?;
            out.push_str(&rest[..start]);
            let mut value = self.lookup(&var.segments).map_err(invalid)?;
            for filter in var.expr.split('|').skip(1).map(str::trim) {
                match filter {
                    "posix_path" => value = value.replace('\\', "/"),
                    "win_path" => value = value.replace('/', "\\"),
                    other => return Err(invalid(format!("unknown filter `{other}`"))),
                }
            }
            out.push_str(&value);
            rest = &rest[end..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

/// `tool@selector` of `runtime:` (`nodejs@22`).
pub fn parse_runtime_ref(text: &str) -> Result<(ToolId, String)> {
    let invalid = || {
        ForgeError::InvalidInput(format!(
            "runtime inválido {text:?}: use <ferramenta>@<versão> (nodejs@22)"
        ))
    };
    let (tool, selector) = text.split_once('@').ok_or_else(invalid)?;
    if selector.trim().is_empty() {
        return Err(invalid());
    }
    Ok((ToolId::new(tool.trim())?, selector.trim().to_owned()))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn node_tool() -> RuntimeTool {
        RuntimeTool {
            id: ToolId::new("nodejs").unwrap(),
            commands: ["node", "npm", "npx", "corepack"]
                .map(String::from)
                .to_vec(),
            version_files: vec![".nvmrc".into(), ".node-version".into()],
            path: vec![".".into()],
            env: [
                (
                    "NODE_EXTRA_CA_CERTS".to_owned(),
                    "{{ paths.certs }}/ca/forgebox-ca.crt".to_owned(),
                ),
                (
                    "npm_config_prefix".to_owned(),
                    "{{ paths.data }}/node/{{ version }}/npm-global".to_owned(),
                ),
            ]
            .into(),
            series_key: "{major}".into(),
        }
    }

    fn install(root: &ForgeRoot, tool: &str, folder: &str) -> PathBuf {
        let dir = root.bin().join(tool).join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("node.exe"), b"").unwrap();
        dir
    }

    fn setup() -> (tempfile::TempDir, RuntimeCatalog) {
        let tmp = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(tmp.path()).unwrap();
        install(&root, "nodejs", "20.12.2");
        install(&root, "nodejs", "22.11.0");
        install(&root, "nodejs", "22.23.4");
        std::fs::write(root.bin().join("nodejs").join("notes.txt"), b"x").unwrap();
        (tmp, RuntimeCatalog::new(root, [node_tool()]))
    }

    #[test]
    fn version_numbers_and_series() {
        assert_eq!(version_numbers("v22.12.0"), Some(vec![22, 12, 0]));
        assert_eq!(version_numbers("8.3.37-nts-vs17-x64"), Some(vec![8, 3, 37]));
        assert_eq!(version_numbers("22"), Some(vec![22]));
        assert_eq!(version_numbers("current"), None);
        assert_eq!(version_numbers("lts/iron"), None);
        assert_eq!(series_of("{major}.{minor}", &[8, 3, 37]), "8.3");
        assert_eq!(series_of("{major}", &[22, 1]), "22");
        assert_eq!(series_of("{major}.{minor}", &[7]), "7.0");
    }

    #[test]
    fn installed_versions_are_listed_newest_first() {
        let (_tmp, catalog) = setup();
        let tool = ToolId::new("nodejs").unwrap();
        let folders: Vec<_> = catalog
            .installed(&tool)
            .unwrap()
            .into_iter()
            .map(|v| v.folder)
            .collect();
        assert_eq!(folders, ["22.23.4", "22.11.0", "20.12.2"]);
        assert!(
            catalog
                .installed(&ToolId::new("php").unwrap())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn declared_selector_picks_the_newest_match_and_renders_the_env() {
        let (tmp, catalog) = setup();
        let tool = ToolId::new("nodejs").unwrap();
        let project = tmp.path().join("www").join("app");
        std::fs::create_dir_all(&project).unwrap();
        let resolved = catalog
            .resolve(&tool, Some("22"), None, &project, &project)
            .unwrap();
        assert_eq!(resolved.folder, "22.23.4");
        assert_eq!(resolved.version, "22.23.4");
        assert_eq!(resolved.source, RuntimeSource::Declared("22".into()));
        assert_eq!(
            resolved.dir,
            catalog.root().bin().join("nodejs").join("22.23.4")
        );
        assert_eq!(resolved.path, std::slice::from_ref(&resolved.dir));
        let root = catalog.root().to_posix_string();
        assert_eq!(
            resolved.env,
            [
                (
                    "NODE_EXTRA_CA_CERTS".to_owned(),
                    format!("{root}/certs/ca/forgebox-ca.crt")
                ),
                (
                    "npm_config_prefix".to_owned(),
                    format!("{root}/data/node/22.23.4/npm-global")
                ),
            ]
        );
        let minor = catalog
            .resolve(&tool, Some("v22.11"), None, &project, &project)
            .unwrap();
        assert_eq!(minor.folder, "22.11.0");
        let exact = catalog
            .resolve(&tool, Some("20.12.2"), None, &project, &project)
            .unwrap();
        assert_eq!(exact.folder, "20.12.2");
    }

    #[test]
    fn missing_versions_never_fall_back_silently() {
        let (tmp, catalog) = setup();
        let tool = ToolId::new("nodejs").unwrap();
        let project = tmp.path();
        let err = catalog
            .resolve(&tool, Some("18"), None, project, project)
            .unwrap_err();
        assert_eq!(err.code(), "not_found");
        let text = err.to_string();
        assert!(text.contains("18") && text.contains("22.23.4"), "{text}");
        let err = catalog
            .resolve(&tool, Some("lts/iron"), None, project, project)
            .unwrap_err();
        assert_eq!(err.code(), "invalid_input");
        let err = catalog
            .resolve(&ToolId::new("bun").unwrap(), None, None, project, project)
            .unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[test]
    fn version_files_come_before_the_project_runtimes() {
        let (tmp, catalog) = setup();
        let tool = ToolId::new("nodejs").unwrap();
        let project = tmp.path().join("www").join("app");
        let sub = project.join("web");
        std::fs::create_dir_all(&sub).unwrap();
        // Nothing declared: the project runtime wins over `current`.
        let resolved = catalog
            .resolve(&tool, None, Some("20"), &sub, &project)
            .unwrap();
        assert_eq!(
            (resolved.folder.as_str(), resolved.source),
            ("20.12.2", RuntimeSource::Project("20".into()))
        );
        // `.nvmrc` in the project folder (with a comment line first).
        std::fs::write(project.join(".nvmrc"), "# node\nv22.11.0\n").unwrap();
        let resolved = catalog
            .resolve(&tool, None, Some("20"), &sub, &project)
            .unwrap();
        assert_eq!(resolved.folder, "22.11.0");
        assert_eq!(resolved.source, RuntimeSource::VersionFile(".nvmrc".into()));
        // The closer file wins; `.node-version` is read too.
        std::fs::write(sub.join(".node-version"), "20\n").unwrap();
        let resolved = catalog.resolve(&tool, None, None, &sub, &project).unwrap();
        assert_eq!(resolved.folder, "20.12.2");
        // An empty file does not decide.
        std::fs::write(sub.join(".node-version"), "\n\n").unwrap();
        let resolved = catalog.resolve(&tool, None, None, &sub, &project).unwrap();
        assert_eq!(resolved.folder, "22.11.0");
        // A declared runtime beats every file.
        let resolved = catalog
            .resolve(&tool, Some("22.23"), None, &sub, &project)
            .unwrap();
        assert_eq!(resolved.folder, "22.23.4");
        // Files outside the project are never read.
        std::fs::remove_file(project.join(".nvmrc")).unwrap();
        std::fs::write(tmp.path().join("www").join(".nvmrc"), "20").unwrap();
        let resolved = catalog.resolve(&tool, None, None, &sub, &project).unwrap();
        assert_eq!(resolved.folder, "22.23.4");
        assert_eq!(resolved.source, RuntimeSource::Current);
    }

    #[test]
    fn commands_map_to_their_tool() {
        let (_tmp, catalog) = setup();
        for word in ["npm", "NPX", "node.exe", "npm.cmd", "corepack"] {
            assert_eq!(
                catalog.tool_for_command(word).map(|t| t.id.as_str()),
                Some("nodejs"),
                "{word}"
            );
        }
        assert!(catalog.tool_for_command("php").is_none());
        assert!(catalog.tool_for_command("node.js").is_none());
    }

    #[test]
    fn runtime_references() {
        let (tool, selector) = parse_runtime_ref("nodejs@22").unwrap();
        assert_eq!((tool.as_str(), selector.as_str()), ("nodejs", "22"));
        for bad in ["nodejs", "nodejs@", "@22", "Node JS@22"] {
            assert!(parse_runtime_ref(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn env_templates_reject_unknown_variables() {
        let (_tmp, catalog) = setup();
        let vars = EnvVars {
            tool: "nodejs",
            version: "1.0.0",
            series: "1".into(),
            root: catalog.root(),
        };
        assert!(vars.render("{{ instance.port }}").is_err());
        assert!(vars.render("{{ paths.version }}").is_err());
        assert_eq!(vars.render("plain").unwrap(), "plain");
        assert_eq!(
            vars.render("{{ version }}-{{ series | win_path }}")
                .unwrap(),
            "1.0.0-1"
        );
    }

    #[test]
    fn manifest_of_nodejs_is_understood() {
        let text = include_str!("../../../../registry/tools/nodejs.yaml");
        let manifest = forge_manifest::load_str(text).unwrap();
        let forge_manifest::Manifest::Tool(tool) = manifest else {
            panic!("not a tool");
        };
        let description = RuntimeTool::from_manifest(&tool);
        assert_eq!(description.id.as_str(), "nodejs");
        assert_eq!(description.commands, ["node", "npm", "npx", "corepack"]);
        assert_eq!(description.version_files, [".nvmrc", ".node-version"]);
        assert_eq!(description.path, ["."]);
        assert_eq!(description.series_key, "{major}");
        assert_eq!(description.env.len(), 2);
    }
}
