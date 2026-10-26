//! Loading manifests from YAML ([`load_str`]) and their semantic validation
//! ([`validate`]), docs/04.
//!
//! `load_str` reads `apiVersion` and `kind` first and then parses the document straight
//! into the struct of that kind, so every schema error (unknown key, wrong type) keeps its
//! line and column. `validate` checks what serde cannot: references between fields,
//! versions and constraints, port ranges, template variables (docs/04 §1.4), the
//! `post_install` sandbox (rule 6) and icons. Problems that need the file system
//! (templates, icon files, other manifests) are checked by [`crate::lint`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use forge_core::ForgeError;
use forge_core::config::PortsConfig;
use forge_core::ids::ToolId;
use regex::Regex;
use serde::Deserialize;

use crate::issue::{IssueCode, ValidationIssue};
use crate::schema::Manifest;
use crate::schema::common::{API_VERSION, Category, Kind, VersionScheme};
use crate::schema::extension::ExtensionManifest;
use crate::schema::project::ProjectManifest;
use crate::schema::registry::{REGISTRY_SCHEMA_VERSION, RegistryEntry, RegistryManifest};
use crate::schema::stack::{PostCreateAction, StackManifest};
use crate::schema::tool::{
    ArchiveFormat, Config, Discover, Install, PostInstallAction, Provider, QuickSettingType,
    Service, ServiceKind, Source, ToolManifest, ToolMetadata, Ui, Update, UpdatePolicy, Variant,
    Versioning,
};
use crate::template::{self, VarRef};
use crate::version::{Constraint, Version, validate_series_key};
use crate::yaml::{self, Location, YamlError};

/// Directories available as `{{ paths.<dir> }}` (docs/04 §1.4).
pub const PATHS_KEYS: [&str; 9] = [
    "root", "bin", "etc", "data", "logs", "tmp", "www", "certs", "version",
];

/// A manifest could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    /// YAML syntax error or mismatch with the schema of the kind.
    #[error("{0}")]
    Yaml(#[from] YamlError),
    /// `apiVersion` missing or not `forgebox.dev/v1`.
    #[error("{}", api_version_message(.found.as_deref(), .location.as_ref()))]
    ApiVersion {
        found: Option<String>,
        location: Option<Location>,
    },
    /// `kind` missing or unknown.
    #[error("{}", kind_message(.found.as_deref(), .location.as_ref()))]
    Kind {
        found: Option<String>,
        location: Option<Location>,
    },
    #[error("empty manifest")]
    Empty,
    #[error("cannot read {path}: {message}")]
    Io { path: PathBuf, message: String },
}

fn at(location: Option<&Location>) -> String {
    location.map(|l| format!(" at {l}")).unwrap_or_default()
}

fn api_version_message(found: Option<&str>, location: Option<&Location>) -> String {
    match found {
        Some(v) => format!(
            "unsupported apiVersion {v:?}{}: expected \"{API_VERSION}\"",
            at(location)
        ),
        None => format!("missing `apiVersion` (expected \"{API_VERSION}\")"),
    }
}

fn kind_message(found: Option<&str>, location: Option<&Location>) -> String {
    let kinds: Vec<&str> = Kind::ALL.iter().map(|k| k.as_str()).collect();
    match found {
        Some(k) => format!(
            "unknown kind {k:?}{}: expected one of {}",
            at(location),
            kinds.join(", ")
        ),
        None => format!("missing `kind` (one of {})", kinds.join(", ")),
    }
}

impl ManifestError {
    /// Position of the problem, when known.
    pub fn location(&self) -> Option<Location> {
        match self {
            ManifestError::Yaml(e) => e.location,
            ManifestError::ApiVersion { location, .. } | ManifestError::Kind { location, .. } => {
                *location
            }
            ManifestError::Empty | ManifestError::Io { .. } => None,
        }
    }
}

impl From<ManifestError> for ForgeError {
    fn from(err: ManifestError) -> Self {
        ForgeError::ConfigInvalid(format!("manifest: {err}"))
    }
}

#[derive(Deserialize)]
struct Header {
    #[serde(rename = "apiVersion", default)]
    api_version: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

/// Position of a top-level `key:` (column 1), to point at `apiVersion`/`kind` errors.
fn key_location(text: &str, key: &str) -> Option<Location> {
    text.lines().enumerate().find_map(|(i, line)| {
        let rest = line.strip_prefix(key)?;
        rest.trim_start().starts_with(':').then_some(Location {
            line: i + 1,
            column: 1,
        })
    })
}

/// Parses one manifest. A leading UTF-8 BOM is tolerated.
pub fn load_str(text: &str) -> Result<Manifest, ManifestError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.trim().is_empty() {
        return Err(ManifestError::Empty);
    }
    let header: Header = yaml::from_str(text)?;
    if header.api_version.as_deref() != Some(API_VERSION) {
        return Err(ManifestError::ApiVersion {
            found: header.api_version,
            location: key_location(text, "apiVersion"),
        });
    }
    let kind = match header.kind.as_deref().map(str::parse::<Kind>) {
        Some(Ok(kind)) => kind,
        _ => {
            return Err(ManifestError::Kind {
                found: header.kind,
                location: key_location(text, "kind"),
            });
        }
    };
    Ok(match kind {
        Kind::Tool => Manifest::Tool(yaml::from_str(text)?),
        Kind::Extension => Manifest::Extension(yaml::from_str(text)?),
        Kind::Stack => Manifest::Stack(yaml::from_str(text)?),
        Kind::Project => Manifest::Project(yaml::from_str(text)?),
        Kind::Registry => Manifest::Registry(yaml::from_str(text)?),
    })
}

/// Reads and parses a manifest file.
///
/// Blocking file I/O: call from `spawn_blocking` in async code.
pub fn load_file(path: &Path) -> Result<Manifest, ManifestError> {
    let text = std::fs::read_to_string(path).map_err(|e| ManifestError::Io {
        path: path.to_owned(),
        message: e.to_string(),
    })?;
    load_str(&text)
}

/// Writes a manifest as YAML in the form of docs/04 (LF, no BOM).
pub fn to_yaml(manifest: &Manifest) -> Result<String, ManifestError> {
    Ok(yaml::to_string(manifest)?)
}

/// Inputs of [`validate_with`] that come from outside the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidateOptions {
    /// Names of the port ranges (`forgebox.toml` `[ports.ranges]`, rule 3).
    pub port_ranges: Vec<String>,
    /// The manifest comes from a third-party registry: `post_install.run` is refused
    /// (docs/11 §2).
    pub third_party: bool,
}

impl Default for ValidateOptions {
    /// Default port ranges of `forge_core::config` (docs/03 §4), official registry.
    fn default() -> Self {
        Self::from_ports(&PortsConfig::default())
    }
}

impl ValidateOptions {
    pub fn from_ports(ports: &PortsConfig) -> Self {
        Self {
            port_ranges: ports.ranges.keys().cloned().collect(),
            third_party: false,
        }
    }
}

/// Semantic validation with [`ValidateOptions::default`].
pub fn validate(manifest: &Manifest) -> Vec<ValidationIssue> {
    validate_with(manifest, &ValidateOptions::default())
}

/// Semantic validation; an empty result means the manifest is valid.
pub fn validate_with(manifest: &Manifest, options: &ValidateOptions) -> Vec<ValidationIssue> {
    let mut c = Checker {
        options,
        issues: Vec::new(),
    };
    match manifest {
        Manifest::Tool(m) => c.tool(m),
        Manifest::Extension(m) => c.extension(m),
        Manifest::Stack(m) => c.stack(m),
        Manifest::Project(m) => c.project(m),
        Manifest::Registry(m) => c.registry(m),
    }
    c.issues
}

// ------------------------------------------------------------------- helpers

/// Identifier usable in templates (`php_ini`, `main`, `ts`).
fn is_key(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_env_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Variant values end up in folder names: lowercase and path-safe.
fn is_variant_value(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// Instance and process names (`default`, `legacy`, `queue-worker`).
fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
}

fn has_dotdot(path: &str) -> bool {
    path.split(['/', '\\']).any(|s| s == "..")
}

fn is_absolute(path: &str) -> bool {
    path.starts_with(['/', '\\']) || path.as_bytes().get(1) == Some(&b':')
}

/// Relative path that stays inside its base folder, with `/` separators.
fn relative_path_problem(path: &str) -> Option<&'static str> {
    if path.trim().is_empty() {
        Some("must not be empty")
    } else if is_absolute(path) {
        Some("must be relative")
    } else if path.contains('\\') {
        Some("must use `/` as separator")
    } else if has_dotdot(path) {
        Some("must not contain `..`")
    } else {
        None
    }
}

fn host_problem(host: &str) -> Option<&'static str> {
    if host.is_empty() || host.len() > 253 {
        return Some("must have 1 to 253 characters");
    }
    if host.bytes().any(|b| b.is_ascii_uppercase()) {
        return Some("must be lowercase");
    }
    let name = host.strip_prefix("*.").unwrap_or(host);
    if !name.contains('.') {
        return Some("needs a domain and a TLD (`loja.test`)");
    }
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Some("has an empty or too long label");
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Some("may only use a-z, 0-9, `-` and `.`");
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Some("labels must not start or end with `-`");
        }
    }
    None
}

/// First token of a command line; quotes and `{{ ... }}` (which may contain spaces) are
/// kept together.
fn first_token(cmd: &str) -> &str {
    let s = cmd.trim_start();
    if let Some(rest) = s.strip_prefix('"') {
        return &rest[..rest.find('"').unwrap_or(rest.len())];
    }
    let mut i = 0;
    while i < s.len() {
        let tail = &s[i..];
        if tail.starts_with("{{") {
            match tail.find("}}") {
                Some(end) => {
                    i += end + 2;
                    continue;
                }
                None => return s,
            }
        }
        let Some(c) = tail.chars().next() else { break };
        if c.is_whitespace() {
            break;
        }
        i += c.len_utf8();
    }
    &s[..i]
}

/// Which `{{ }}` variables a manifest may use (docs/04 §1.4).
struct Scope {
    kind: Kind,
    /// `None` = any key (extensions use the target tool's).
    variant_keys: Option<BTreeSet<String>>,
    config_ids: Option<BTreeSet<String>>,
    port_names: Option<BTreeSet<String>>,
    /// Tools usable in `{{ instances.<tool>.<instance>... }}` (stacks, projects).
    instance_tools: Option<BTreeSet<String>>,
}

fn check_member(set: Option<&BTreeSet<String>>, value: &str, what: &str) -> Result<(), String> {
    match set {
        Some(set) if !set.contains(value) => {
            let known: Vec<&str> = set.iter().map(String::as_str).collect();
            Err(format!(
                "{what} `{value}` is not declared (declared: {})",
                if known.is_empty() {
                    "none".to_owned()
                } else {
                    known.join(", ")
                }
            ))
        }
        _ => Ok(()),
    }
}

fn instance_ref(rest: &[String], scope: &Scope) -> Result<(), String> {
    match rest {
        [field] if field == "name" || field == "data_dir" => Ok(()),
        [ports, name] if ports == "ports" => check_member(scope.port_names.as_ref(), name, "port"),
        [config, id] if config == "config" => {
            check_member(scope.config_ids.as_ref(), id, "config file")
        }
        _ => Err("instance variables are name, data_dir, ports.<name> and config.<id>".to_owned()),
    }
}

/// Checks one variable against the scope; `field` is the dotted path of the string.
fn check_var(scope: &Scope, field: &str, var: &VarRef) -> Result<(), String> {
    let segs = &var.segments;
    let leaf = segs.len() == 1;
    let unknown = |allowed: &str| {
        Err(format!(
            "unknown template variable `{}` (allowed here: {allowed})",
            var.path()
        ))
    };
    let paths = || {
        if segs.len() == 2 && PATHS_KEYS.contains(&segs[1].as_str()) {
            Ok(())
        } else {
            Err(format!(
                "`{}`: paths are {}",
                var.path(),
                PATHS_KEYS.map(|k| format!("paths.{k}")).join(", ")
            ))
        }
    };
    match scope.kind {
        Kind::Tool | Kind::Extension => {
            let in_quick = field.contains(".quick_settings[");
            let in_pool = field.starts_with("spec.service.pool");
            let pool_keys = |keys: &[&str]| {
                if segs.len() == 2 && keys.contains(&segs[1].as_str()) {
                    Ok(())
                } else {
                    Err(format!("`{}`: keys are {}", var.path(), keys.join(", ")))
                }
            };
            match segs[0].as_str() {
                "version" | "series" if leaf => Ok(()),
                "value" if leaf && in_quick => Ok(()),
                "port" if leaf && in_pool => Ok(()),
                "variant" if segs.len() == 2 => {
                    check_member(scope.variant_keys.as_ref(), &segs[1], "variant")
                }
                "paths" => paths(),
                "instance" => instance_ref(&segs[1..], scope),
                "prefs" if segs.len() >= 2 => Ok(()),
                "recycle" if in_pool => pool_keys(&["max_requests", "max_rss_mb", "max_age"]),
                "scaling" if in_pool => pool_keys(&["min", "max", "idle_timeout"]),
                _ => unknown(
                    "version, series, variant.<key>, paths.<dir>, instance.*, prefs.*; \
                     `value` in quick_settings; `port`, `recycle.*`, `scaling.*` in service.pool",
                ),
            }
        }
        Kind::Stack | Kind::Project => match segs[0].as_str() {
            "dir" if leaf => Ok(()),
            "site" if segs.len() == 2 && ["slug", "host", "root"].contains(&segs[1].as_str()) => {
                Ok(())
            }
            "paths" => paths(),
            "prefs" if segs.len() >= 2 => Ok(()),
            "instances" if segs.len() >= 4 => {
                check_member(scope.instance_tools.as_ref(), &segs[1], "tool")?;
                instance_ref(&segs[3..], scope)
            }
            _ => unknown(
                "dir, site.slug|host|root, paths.<dir>, prefs.*, \
                 instances.<tool>.<instance>.ports.<name>|config.<id>|name|data_dir",
            ),
        },
        Kind::Registry => unknown("none (registries are not templated)"),
    }
}

fn collect_strings(value: &serde_json::Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        serde_json::Value::String(s) => {
            if s.contains("{{") || s.contains("{%") {
                out.push((path.to_owned(), s.clone()));
            }
        }
        serde_json::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                collect_strings(item, &format!("{path}[{i}]"), out);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                collect_strings(item, &format!("{path}.{key}"), out);
            }
        }
        _ => {}
    }
}

fn set_of<'a>(items: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    items.into_iter().map(str::to_owned).collect()
}

// ------------------------------------------------------------------- checker

struct Checker<'a> {
    options: &'a ValidateOptions,
    issues: Vec<ValidationIssue>,
}

impl Checker<'_> {
    fn push(&mut self, code: IssueCode, field: impl Into<String>, message: impl Into<String>) {
        self.issues.push(ValidationIssue::new(code, field, message));
    }

    fn not_empty(&mut self, field: &str, value: &str) {
        if value.trim().is_empty() {
            self.push(IssueCode::InvalidValue, field, "must not be empty");
        }
    }

    fn url(&mut self, field: &str, url: &str, https_only: bool) {
        let rest = url
            .strip_prefix("https://")
            .or_else(|| (!https_only).then(|| url.strip_prefix("http://")).flatten());
        match rest {
            Some(rest) if !rest.is_empty() && !rest.starts_with('/') => {}
            _ => self.push(
                IssueCode::InvalidUrl,
                field,
                if https_only {
                    format!("{url:?} must be an https:// URL")
                } else {
                    format!("{url:?} must be an http(s):// URL")
                },
            ),
        }
    }

    fn regex(&mut self, field: &str, pattern: &str) {
        if let Err(e) = Regex::new(pattern) {
            self.push(IssueCode::InvalidRegex, field, e.to_string());
        }
    }

    fn relative_path(&mut self, field: &str, path: &str) {
        if let Some(problem) = relative_path_problem(path) {
            self.push(IssueCode::InvalidPath, field, format!("{path:?} {problem}"));
        }
    }

    fn env_names<'k>(&mut self, field: &str, keys: impl IntoIterator<Item = &'k String>) {
        for key in keys {
            if !is_env_name(key) {
                self.push(
                    IssueCode::InvalidId,
                    format!("{field}.{key}"),
                    "environment variable names use [A-Za-z0-9_] and do not start with a digit",
                );
            }
        }
    }

    fn duplicate(&mut self, field: &str, seen: &mut BTreeSet<String>, value: &str) {
        if !seen.insert(value.to_owned()) {
            self.push(
                IssueCode::Duplicate,
                field,
                format!("`{value}` appears twice"),
            );
        }
    }

    /// Rule 7, the part that needs no file system.
    fn icon(&mut self, field: &str, icon: Option<&str>) {
        let Some(icon) = icon else { return };
        let svg = icon
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .ends_with(".svg");
        let problem = if icon.starts_with("https://") {
            (!svg).then_some("an icon URL must point to an .svg file")
        } else if icon.contains("://") {
            Some("icons are a file in registry/icons/ or an https:// URL")
        } else if !svg {
            Some("icons must be SVG files")
        } else {
            relative_path_problem(icon)
        };
        if let Some(problem) = problem {
            self.push(
                IssueCode::InvalidIcon,
                field,
                format!("{icon:?}: {problem}"),
            );
        }
    }

    /// Rule 6: writes stay inside the root (`{{ paths.<dir> }}...`).
    fn write_target(&mut self, field: &str, path: &str) {
        match template::root_dir_prefix(path) {
            Some((dir, rest)) if PATHS_KEYS.contains(&dir.as_str()) && !has_dotdot(rest) => {}
            _ => self.push(
                IssueCode::WriteOutsideRoot,
                field,
                format!("{path:?} must start with `{{{{ paths.<dir> }}}}` and stay inside it"),
            ),
        }
    }

    /// Rule 6: `post_install.run` only runs executables of the installed version.
    fn run_command(&mut self, field: &str, command: &str) {
        let token = first_token(command);
        if token.is_empty() {
            self.push(IssueCode::InvalidValue, field, "command must not be empty");
            return;
        }
        let problem = if token.starts_with("{{") {
            match template::root_dir_prefix(token) {
                Some((dir, rest))
                    if dir == "version"
                        && rest.starts_with(['/', '\\'])
                        && !rest.contains("{{")
                        && !rest.contains(['%', '$'])
                        && !has_dotdot(rest) =>
                {
                    None
                }
                _ => Some("is outside the version folder"),
            }
        } else if token.contains("{{") || token.contains(['%', '$']) {
            Some("uses variables in the executable")
        } else if is_absolute(token) {
            Some("is an absolute path")
        } else if has_dotdot(token) {
            Some("leaves the version folder")
        } else {
            None
        };
        if let Some(problem) = problem {
            self.push(
                IssueCode::RunOutsideVersion,
                field,
                format!(
                    "`{token}` {problem}: post_install.run may only run executables inside \
                     {{{{ paths.version }}}} (relative paths are resolved there, never via PATH)"
                ),
            );
        }
    }

    fn post_install(&mut self, prefix: &str, actions: &[PostInstallAction]) {
        for (i, action) in actions.iter().enumerate() {
            let f = format!("{prefix}[{i}]");
            match action {
                PostInstallAction::Copy(copy) => {
                    let from = format!("{f}.copy.from");
                    if copy.from.starts_with("{{") {
                        self.write_target(&from, &copy.from);
                    } else {
                        self.relative_path(&from, &copy.from);
                    }
                    self.write_target(&format!("{f}.copy.to"), &copy.to);
                }
                PostInstallAction::Render(render) => {
                    self.relative_path(&format!("{f}.render.template"), &render.template);
                    self.write_target(&format!("{f}.render.to"), &render.to);
                }
                PostInstallAction::Mkdir(dir) => self.write_target(&format!("{f}.mkdir"), dir),
                PostInstallAction::Run(run) => {
                    let field = format!("{f}.run.command");
                    if self.options.third_party {
                        self.push(
                            IssueCode::RunNotAllowed,
                            &field,
                            "third-party registries cannot declare post_install.run",
                        );
                    }
                    self.run_command(&field, &run.command);
                }
            }
        }
    }

    fn uninstall_keep(&mut self, prefix: &str, keep: &[String]) {
        for (i, path) in keep.iter().enumerate() {
            if template::root_dir_prefix(path)
                .is_none_or(|(dir, rest)| !PATHS_KEYS.contains(&dir.as_str()) || has_dotdot(rest))
            {
                self.push(
                    IssueCode::InvalidPath,
                    format!("{prefix}[{i}]"),
                    format!("{path:?} must start with `{{{{ paths.<dir> }}}}`"),
                );
            }
        }
    }

    /// Rule 5 over every string of `spec`.
    fn templates(&mut self, spec: &impl serde::Serialize, scope: &Scope) {
        let value = match serde_json::to_value(spec) {
            Ok(value) => value,
            Err(e) => {
                self.push(IssueCode::Schema, "spec", e.to_string());
                return;
            }
        };
        let mut strings = Vec::new();
        collect_strings(&value, "spec", &mut strings);
        for (field, text) in strings {
            for found in template::scan(&text) {
                match found {
                    Err(message) => self.push(IssueCode::InvalidTemplate, &field, message),
                    Ok(var) => {
                        if let Err(message) = check_var(scope, &field, &var) {
                            self.push(IssueCode::UnknownTemplateVariable, &field, message);
                        }
                    }
                }
            }
        }
    }

    fn tool_metadata(&mut self, m: &ToolMetadata) {
        self.not_empty("metadata.name", &m.name);
        self.icon("metadata.icon", m.icon.as_deref());
        if let Some(url) = &m.homepage {
            self.url("metadata.homepage", url, false);
        }
        if let Some(url) = &m.docs {
            self.url("metadata.docs", url, false);
        }
        for (i, maintainer) in m.maintainers.iter().enumerate() {
            self.not_empty(&format!("metadata.maintainers[{i}].name"), &maintainer.name);
            if let Some(url) = &maintainer.url {
                self.url(&format!("metadata.maintainers[{i}].url"), url, false);
            }
        }
        if m.replaced_by.as_ref() == Some(&m.id) {
            self.push(
                IssueCode::InvalidReference,
                "metadata.replaced_by",
                "a tool cannot replace itself",
            );
        }
    }

    fn versioning(&mut self, v: &Versioning) {
        if let Err(e) = validate_series_key(&v.series_key) {
            self.push(
                IssueCode::InvalidSeriesKey,
                "spec.versioning.series_key",
                e.to_string(),
            );
        }
        for (name, channel) in &v.channels {
            let field = format!("spec.versioning.channels.{name}");
            if ToolId::new(name.as_str()).is_err() {
                self.push(IssueCode::InvalidId, &field, "channel names use [a-z0-9-]");
            }
            if let Err(e) = Constraint::parse(v.scheme, &channel.constraint) {
                self.push(
                    IssueCode::InvalidConstraint,
                    format!("{field}.constraint"),
                    e.to_string(),
                );
            }
        }
        let mut keys = BTreeSet::new();
        for (i, variant) in v.variants.iter().enumerate() {
            let f = format!("spec.versioning.variants[{i}]");
            if !is_key(&variant.key) {
                self.push(
                    IssueCode::InvalidId,
                    format!("{f}.key"),
                    "variant keys use [a-z0-9_] and start with a letter",
                );
            }
            self.duplicate(&format!("{f}.key"), &mut keys, &variant.key);
            if variant.values.is_empty() {
                self.push(
                    IssueCode::InvalidVariant,
                    format!("{f}.values"),
                    "must not be empty",
                );
            }
            let mut values = BTreeSet::new();
            for value in &variant.values {
                if !is_variant_value(value) {
                    self.push(
                        IssueCode::InvalidVariant,
                        format!("{f}.values"),
                        format!("{value:?}: values use [a-z0-9._-] (they become folder names)"),
                    );
                }
                self.duplicate(&format!("{f}.values"), &mut values, value);
            }
            if let Some(default) = &variant.default {
                if !variant.values.contains(default) {
                    self.push(
                        IssueCode::InvalidVariant,
                        format!("{f}.default"),
                        format!("{default:?} is not one of `values`"),
                    );
                }
            }
            for key in variant.label.keys() {
                if !variant.values.contains(key) {
                    self.push(
                        IssueCode::InvalidVariant,
                        format!("{f}.label.{key}"),
                        "label for a value that is not in `values`",
                    );
                }
            }
        }
    }

    fn source(&mut self, source: &Source, scheme: VersionScheme, variants: Option<&[Variant]>) {
        if source.providers.is_empty() {
            self.push(
                IssueCode::MissingProvider,
                "spec.source.providers",
                "at least one provider is required",
            );
        }
        let curated =
            !source.releases.is_empty() && source.releases.iter().all(|r| r.sha256.is_some());
        for (i, provider) in source.providers.iter().enumerate() {
            let f = format!("spec.source.providers[{i}]");
            match provider {
                Provider::Http(http) => {
                    self.url(&format!("{f}.url"), &http.url, true);
                    if let Some(checksum) = &http.checksum {
                        if !checksum.url.starts_with("{url}") {
                            self.url(&format!("{f}.checksum.url"), &checksum.url, true);
                        }
                    }
                    for key in http.vars.keys() {
                        if !is_key(key) {
                            self.push(
                                IssueCode::InvalidId,
                                format!("{f}.vars.{key}"),
                                "use [a-z0-9_]",
                            );
                        }
                    }
                    match &http.version_source {
                        Some(vs) => {
                            self.url(&format!("{f}.version_source.url"), &vs.url, true);
                            match (&vs.regex, &vs.json_path) {
                                (Some(re), None) => self.regex(&format!("{f}.version_source.regex"), re),
                                (None, Some(path)) => self.not_empty(&format!("{f}.version_source.json_path"), path),
                                _ => self.push(
                                    IssueCode::InvalidValue,
                                    format!("{f}.version_source"),
                                    "needs exactly one of `regex` and `json_path`",
                                ),
                            }
                        }
                        None if source.releases.is_empty() => self.push(
                            IssueCode::InvalidValue,
                            &f,
                            "an http provider needs `source.releases` or `version_source` to list versions",
                        ),
                        None => {}
                    }
                }
                Provider::GithubRelease(gh) => {
                    let valid_repo = gh.repo.split_once('/').is_some_and(|(owner, name)| {
                        let ok = |s: &str| {
                            !s.is_empty()
                                && s.chars().all(|c| {
                                    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
                                })
                        };
                        ok(owner) && ok(name)
                    });
                    if !valid_repo {
                        self.push(
                            IssueCode::InvalidValue,
                            format!("{f}.repo"),
                            "must be `owner/name`",
                        );
                    }
                    self.regex(&format!("{f}.asset"), &gh.asset);
                    if let Some(re) = &gh.version_regex {
                        self.regex(&format!("{f}.version_regex"), re);
                    }
                    if let Some(checksum) = &gh.checksum {
                        self.not_empty(&format!("{f}.checksum.asset"), &checksum.asset);
                    }
                }
                Provider::ApacheLounge(al) => {
                    if al.builds.is_empty() {
                        self.push(
                            IssueCode::InvalidValue,
                            format!("{f}.builds"),
                            "must not be empty",
                        );
                    }
                    for build in &al.builds {
                        if !is_variant_value(build) {
                            self.push(
                                IssueCode::InvalidValue,
                                format!("{f}.builds"),
                                format!("{build:?}: use lowercase like vs17"),
                            );
                        }
                    }
                    if let Some(re) = &al.asset {
                        self.regex(&format!("{f}.asset"), re);
                    }
                }
                Provider::PhpWindows(_)
                | Provider::NginxOrg(_)
                | Provider::Mysql(_)
                | Provider::Mariadb(_)
                | Provider::PostgresqlEdb(_)
                | Provider::NodejsDist(_)
                | Provider::PythonEmbed(_)
                | Provider::Local(_) => {}
            }
            if !provider.has_upstream_checksum() && !curated {
                self.push(
                    IssueCode::MissingChecksum,
                    &f,
                    format!(
                        "provider `{}` has no upstream checksum: add `checksum` (http, github-release) \
                         or a `sha256` to every `source.releases[]`",
                        provider.type_name()
                    ),
                );
            }
        }
        for (i, mirror) in source.mirrors.iter().enumerate() {
            let f = format!("spec.source.mirrors[{i}]");
            self.url(&f, mirror, true);
            if !mirror.contains("{file}") {
                self.push(
                    IssueCode::InvalidValue,
                    &f,
                    "a mirror needs the `{file}` placeholder",
                );
            }
        }
        let mut seen = BTreeSet::new();
        for (i, release) in source.releases.iter().enumerate() {
            let f = format!("spec.source.releases[{i}]");
            if let Err(e) = Version::parse(scheme, &release.version) {
                self.push(
                    IssueCode::InvalidVersion,
                    format!("{f}.version"),
                    e.to_string(),
                );
            }
            if let Some(url) = &release.url {
                self.url(&format!("{f}.url"), url, true);
            }
            if let Some(declared) = variants {
                for (key, value) in &release.variants {
                    match declared.iter().find(|v| &v.key == key) {
                        None => self.push(
                            IssueCode::InvalidVariant,
                            format!("{f}.variants.{key}"),
                            "variant not declared in spec.versioning.variants",
                        ),
                        Some(v) if !v.values.contains(value) => self.push(
                            IssueCode::InvalidVariant,
                            format!("{f}.variants.{key}"),
                            format!("{value:?} is not one of {:?}", v.values),
                        ),
                        Some(_) => {}
                    }
                }
            }
            let identity = format!("{} {:?}", release.version, release.variants);
            if !seen.insert(identity) {
                self.push(
                    IssueCode::Duplicate,
                    &f,
                    format!("release {} appears twice", release.version),
                );
            }
        }
    }

    fn install(&mut self, install: &Install) {
        if install.archive == Some(ArchiveFormat::Msi) {
            self.push(
                IssueCode::Unsupported,
                "spec.install.archive",
                "msi is not supported in v1",
            );
        }
        for (i, path) in install.path.iter().enumerate() {
            self.relative_path(&format!("spec.install.path[{i}]"), path);
        }
        let mut shims = BTreeSet::new();
        for (i, shim) in install.shims.iter().enumerate() {
            let f = format!("spec.install.shims[{i}]");
            if shim.is_empty()
                || !shim
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            {
                self.push(
                    IssueCode::InvalidValue,
                    &f,
                    format!("{shim:?}: shim names use [A-Za-z0-9._-]"),
                );
            }
            self.duplicate(&f, &mut shims, shim);
        }
        if let Some(verify) = &install.verify {
            self.not_empty("spec.install.verify.command", &verify.command);
            if let Some(re) = &verify.regex {
                self.regex("spec.install.verify.regex", re);
            }
        }
        self.post_install("spec.install.post_install", &install.post_install);
        self.uninstall_keep("spec.install.uninstall.keep", &install.uninstall.keep);
    }

    /// Returns the ids of `config.files`.
    fn config(&mut self, config: &Config) -> BTreeSet<String> {
        let mut ids = BTreeSet::new();
        for (i, file) in config.files.iter().enumerate() {
            let f = format!("spec.config.files[{i}]");
            if !is_key(&file.id) {
                self.push(
                    IssueCode::InvalidId,
                    format!("{f}.id"),
                    "file ids use [a-z0-9_] and start with a letter",
                );
            }
            self.duplicate(&format!("{f}.id"), &mut ids, &file.id);
            self.not_empty(&format!("{f}.path"), &file.path);
        }
        let mut keys = BTreeSet::new();
        for (i, qs) in config.quick_settings.iter().enumerate() {
            let f = format!("spec.config.quick_settings[{i}]");
            if !is_key(&qs.key) {
                self.push(
                    IssueCode::InvalidId,
                    format!("{f}.key"),
                    "keys use [a-z0-9_] and start with a letter",
                );
            }
            self.duplicate(&format!("{f}.key"), &mut keys, &qs.key);
            if let Some(file) = &qs.file {
                if !ids.contains(file) {
                    self.push(
                        IssueCode::InvalidReference,
                        format!("{f}.file"),
                        format!("no config.files entry with id `{file}`"),
                    );
                }
            }
            let targets = [&qs.ini, &qs.directive, &qs.pointer, &qs.arg]
                .iter()
                .filter(|t| t.is_some())
                .count();
            if targets > 1 {
                self.push(
                    IssueCode::InvalidValue,
                    &f,
                    "use only one of `ini`, `directive`, `pointer`, `arg`",
                );
            }
            self.quick_setting_values(&f, qs);
        }
        if let Some(ext) = &config.extensions {
            if !ext.pattern.contains("{name}") {
                self.push(
                    IssueCode::InvalidValue,
                    "spec.config.extensions.pattern",
                    "needs the `{name}` placeholder",
                );
            }
            self.not_empty("spec.config.extensions.ini_dir", &ext.ini_dir);
        }
        self.env_names("spec.config.env", config.env.keys());
        ids
    }

    fn quick_setting_values(&mut self, f: &str, qs: &crate::schema::tool::QuickSetting) {
        use serde_json::Value;
        let kind = qs.kind;
        let has_options = matches!(kind, QuickSettingType::Select | QuickSettingType::Multi);
        if has_options && qs.options.is_empty() {
            self.push(
                IssueCode::InvalidValue,
                format!("{f}.options"),
                "select and multi need `options`",
            );
        }
        if !has_options && !qs.options.is_empty() {
            self.push(
                IssueCode::InvalidValue,
                format!("{f}.options"),
                "`options` only applies to select and multi",
            );
        }
        if kind != QuickSettingType::Toggle && (qs.on.is_some() || qs.off.is_some()) {
            self.push(
                IssueCode::InvalidValue,
                f,
                "`on`/`off` only apply to toggle",
            );
        }
        if kind != QuickSettingType::Port && qs.reserve {
            self.push(
                IssueCode::InvalidValue,
                format!("{f}.reserve"),
                "`reserve` only applies to port",
            );
        }
        let numeric = matches!(kind, QuickSettingType::Number | QuickSettingType::Slider);
        if !numeric && (qs.min.is_some() || qs.max.is_some() || qs.step.is_some()) {
            self.push(
                IssueCode::InvalidValue,
                f,
                "`min`/`max`/`step` only apply to number and slider",
            );
        }
        if kind == QuickSettingType::Slider && (qs.min.is_none() || qs.max.is_none()) {
            self.push(IssueCode::InvalidValue, f, "slider needs `min` and `max`");
        }
        if let (Some(min), Some(max)) = (qs.min, qs.max) {
            if min > max {
                self.push(IssueCode::InvalidValue, f, "`min` is greater than `max`");
            }
        }
        if qs.step.is_some_and(|s| s <= 0.0) {
            self.push(
                IssueCode::InvalidValue,
                format!("{f}.step"),
                "must be positive",
            );
        }
        let Some(default) = &qs.default else { return };
        let field = format!("{f}.default");
        let ok = match kind {
            QuickSettingType::Toggle => match default {
                Value::Bool(_) => true,
                Value::String(s) => matches!(s.to_ascii_lowercase().as_str(), "on" | "off"),
                _ => false,
            },
            QuickSettingType::Select => qs.options.contains(default),
            QuickSettingType::Multi => default
                .as_array()
                .is_some_and(|items| items.iter().all(|v| qs.options.contains(v))),
            QuickSettingType::Port => default.as_u64().is_some_and(|p| (1..=65535).contains(&p)),
            QuickSettingType::Number | QuickSettingType::Slider => {
                default.as_f64().is_some_and(|n| {
                    qs.min.is_none_or(|min| n >= min) && qs.max.is_none_or(|max| n <= max)
                })
            }
            QuickSettingType::Text | QuickSettingType::Path => {
                matches!(default, Value::String(_) | Value::Number(_))
            }
        };
        if !ok {
            let expected = match kind {
                QuickSettingType::Toggle => "true/false or on/off",
                QuickSettingType::Select => "one of `options`",
                QuickSettingType::Multi => "a list of `options`",
                QuickSettingType::Port => "a port 1-65535",
                QuickSettingType::Number | QuickSettingType::Slider => "a number within min/max",
                QuickSettingType::Text | QuickSettingType::Path => "text",
            };
            self.push(
                IssueCode::InvalidValue,
                field,
                format!("default {default} must be {expected}"),
            );
        }
    }

    /// Returns the port names of the daemon (for `{{ instance.ports.<n> }}`).
    fn service(&mut self, service: &Service, self_id: &ToolId) -> BTreeSet<String> {
        let blocks = [
            (ServiceKind::Daemon, service.daemon.is_some()),
            (ServiceKind::Pool, service.pool.is_some()),
            (ServiceKind::Oneshot, service.oneshot.is_some()),
            (ServiceKind::Gui, service.gui.is_some()),
        ];
        for (kind, present) in blocks {
            if present && kind != service.kind {
                self.push(
                    IssueCode::ServiceMismatch,
                    format!("spec.service.{}", kind.as_str()),
                    format!("`service.{0}` only applies to `kind: {0}`", kind.as_str()),
                );
            }
            if !present && kind == service.kind {
                self.push(
                    IssueCode::ServiceMismatch,
                    "spec.service",
                    format!("`kind: {0}` needs a `service.{0}` block", kind.as_str()),
                );
            }
        }
        let mut port_names = BTreeSet::new();
        if let Some(daemon) = &service.daemon {
            let f = "spec.service.daemon";
            self.relative_path(&format!("{f}.exe"), &daemon.exe);
            for (i, port) in daemon.ports.iter().enumerate() {
                let pf = format!("{f}.ports[{i}]");
                if !is_key(&port.name) {
                    self.push(
                        IssueCode::InvalidId,
                        format!("{pf}.name"),
                        "port names use [a-z0-9_] and start with a letter",
                    );
                }
                self.duplicate(&format!("{pf}.name"), &mut port_names, &port.name);
                if port.default == Some(0) {
                    self.push(
                        IssueCode::InvalidValue,
                        format!("{pf}.default"),
                        "port 0 is not allowed",
                    );
                }
                if let Some(env) = &port.env {
                    if !is_env_name(env) {
                        self.push(
                            IssueCode::InvalidId,
                            format!("{pf}.env"),
                            "not a valid environment variable name",
                        );
                    }
                }
                self.port_range(&format!("{pf}.range"), &port.range);
            }
            if daemon.start_timeout.is_zero() {
                self.push(
                    IssueCode::InvalidValue,
                    format!("{f}.start_timeout"),
                    "must be greater than zero",
                );
            }
            if let Some(health) = &daemon.health {
                let probes = [
                    health.tcp.is_some(),
                    health.http.is_some(),
                    health.command.is_some(),
                ]
                .iter()
                .filter(|p| **p)
                .count();
                if probes != 1 {
                    self.push(
                        IssueCode::InvalidValue,
                        format!("{f}.health"),
                        "needs exactly one of `tcp`, `http`, `command`",
                    );
                }
                if let Some(http) = &health.http {
                    self.url(&format!("{f}.health.http.url"), &http.url, false);
                }
                if health.interval.is_zero() {
                    self.push(
                        IssueCode::InvalidValue,
                        format!("{f}.health.interval"),
                        "must be greater than zero",
                    );
                }
                if health.success_threshold == 0 || health.failure_threshold == 0 {
                    self.push(
                        IssueCode::InvalidValue,
                        format!("{f}.health"),
                        "thresholds must be at least 1",
                    );
                }
            }
            if daemon.stop.timeout.is_zero() {
                self.push(
                    IssueCode::InvalidValue,
                    format!("{f}.stop.timeout"),
                    "must be greater than zero",
                );
            }
            if let Some(reload) = &daemon.reload {
                if reload.command.is_some() == reload.signal.is_some() {
                    self.push(
                        IssueCode::InvalidValue,
                        format!("{f}.reload"),
                        "needs exactly one of `command` and `signal`",
                    );
                }
            }
            if daemon.restart_policy.max > 0 && daemon.restart_policy.window.is_zero() {
                self.push(
                    IssueCode::InvalidValue,
                    format!("{f}.restart_policy.window"),
                    "must be greater than zero",
                );
            }
            for (i, log) in daemon.logs.iter().enumerate() {
                self.not_empty(&format!("{f}.logs[{i}].label"), &log.label);
                self.not_empty(&format!("{f}.logs[{i}].path"), &log.path);
            }
            if let Some(data) = &daemon.data {
                self.not_empty(&format!("{f}.data.dir"), &data.dir);
            }
            if daemon.depends_on.contains(self_id) {
                self.push(
                    IssueCode::InvalidReference,
                    format!("{f}.depends_on"),
                    "a service cannot depend on itself",
                );
            }
            self.env_names(&format!("{f}.env"), daemon.env.keys());
        }
        if let Some(pool) = &service.pool {
            let f = "spec.service.pool";
            self.relative_path(&format!("{f}.exe"), &pool.exe);
            self.port_range(&format!("{f}.port_range"), &pool.port_range);
            if pool.scaling.max == 0 || pool.scaling.min > pool.scaling.max {
                self.push(
                    IssueCode::InvalidValue,
                    format!("{f}.scaling"),
                    "needs 1 <= max and min <= max",
                );
            }
            if let Some(health) = &pool.health {
                if !health.fastcgi.starts_with('/') {
                    self.push(
                        IssueCode::InvalidValue,
                        format!("{f}.health.fastcgi"),
                        "must be a path starting with /",
                    );
                }
            }
            self.env_names(&format!("{f}.env"), pool.env.keys());
        }
        for (name, program) in [("oneshot", &service.oneshot), ("gui", &service.gui)] {
            if let Some(program) = program {
                self.relative_path(&format!("spec.service.{name}.exe"), &program.exe);
                self.env_names(&format!("spec.service.{name}.env"), program.env.keys());
            }
        }
        port_names
    }

    /// Rule 3.
    fn port_range(&mut self, field: &str, range: &str) {
        if !self.options.port_ranges.iter().any(|r| r == range) {
            self.push(
                IssueCode::UnknownPortRange,
                field,
                format!(
                    "unknown port range `{range}` (known: {})",
                    self.options.port_ranges.join(", ")
                ),
            );
        }
    }

    fn discover(&mut self, discover: &Discover, variants: &[Variant]) {
        let f = "spec.discover";
        if discover.match_files.is_empty() {
            self.push(
                IssueCode::InvalidValue,
                format!("{f}.match_files"),
                "must not be empty",
            );
        }
        for (i, file) in discover.match_files.iter().enumerate() {
            self.relative_path(&format!("{f}.match_files[{i}]"), file);
        }
        if let Some(re) = &discover.version_regex {
            self.regex(&format!("{f}.version_regex"), re);
        }
        for (key, detect) in &discover.variant_detect {
            let vf = format!("{f}.variant_detect.{key}");
            self.regex(&format!("{vf}.regex"), &detect.regex);
            match variants.iter().find(|v| &v.key == key) {
                None => self.push(
                    IssueCode::InvalidVariant,
                    &vf,
                    "variant not declared in spec.versioning.variants",
                ),
                Some(variant) => {
                    for value in detect.map.values() {
                        if !variant.values.contains(value) {
                            self.push(
                                IssueCode::InvalidVariant,
                                format!("{vf}.map"),
                                format!("{value:?} is not one of {:?}", variant.values),
                            );
                        }
                    }
                }
            }
        }
    }

    fn update(&mut self, update: &Update, category: Category) {
        if update.check_interval.is_zero() {
            self.push(
                IssueCode::InvalidValue,
                "spec.update.check_interval",
                "must be greater than zero",
            );
        }
        if category == Category::Database
            && matches!(
                update.policy,
                UpdatePolicy::AutoPatch | UpdatePolicy::AutoMinor | UpdatePolicy::Auto
            )
        {
            self.push(
                IssueCode::DatabaseAutoUpdate,
                "spec.update.policy",
                "databases are never updated automatically (docs/05 §4): use notify or manual",
            );
        }
        for (i, action) in update.migrate.iter().enumerate() {
            let crate::schema::tool::MigrateAction::CopyDir(copy) = action;
            self.relative_path(
                &format!("spec.update.migrate[{i}].copy_dir.from"),
                &copy.from,
            );
            self.relative_path(&format!("spec.update.migrate[{i}].copy_dir.to"), &copy.to);
        }
    }

    fn ui(&mut self, ui: &Ui, config_ids: &BTreeSet<String>) {
        if let Some(color) = &ui.color {
            let ok = color.len() == 7
                && color.starts_with('#')
                && color[1..].bytes().all(|b| b.is_ascii_hexdigit());
            if !ok {
                self.push(IssueCode::InvalidValue, "spec.ui.color", "must be #RRGGBB");
            }
        }
        for (i, action) in ui.actions.iter().enumerate() {
            let f = format!("spec.ui.actions[{i}]");
            self.not_empty(&format!("{f}.label"), &action.label);
            let kinds = [
                action.open.is_some(),
                action.shell.is_some(),
                action.browse.is_some(),
            ]
            .iter()
            .filter(|k| **k)
            .count();
            if kinds != 1 {
                self.push(
                    IssueCode::InvalidValue,
                    &f,
                    "needs exactly one of `open`, `shell`, `browse`",
                );
            }
            if let Some(id) = action
                .open
                .as_deref()
                .and_then(|o| o.strip_prefix("config:"))
            {
                if !config_ids.contains(id) {
                    self.push(
                        IssueCode::InvalidReference,
                        format!("{f}.open"),
                        format!("no config.files entry with id `{id}`"),
                    );
                }
            }
            if let Some(url) = &action.browse {
                self.url(&format!("{f}.browse"), url, false);
            }
        }
    }

    fn tool(&mut self, m: &ToolManifest) {
        self.tool_metadata(&m.metadata);
        let spec = &m.spec;
        if spec.platforms.is_empty() {
            self.push(
                IssueCode::EmptyPlatforms,
                "spec.platforms",
                "at least one platform is required",
            );
        }
        let mut platforms = BTreeSet::new();
        for p in &spec.platforms {
            if !platforms.insert(*p) {
                self.push(
                    IssueCode::Duplicate,
                    "spec.platforms",
                    format!("{p:?} appears twice"),
                );
            }
        }
        for (i, dep) in spec.runtime_deps.iter().enumerate() {
            if ToolId::new(dep.as_str()).is_err() {
                self.push(
                    IssueCode::InvalidId,
                    format!("spec.runtime_deps[{i}]"),
                    format!("{dep:?}: ids use [a-z0-9-]"),
                );
            }
        }
        if spec.depends_on.contains(&m.metadata.id) {
            self.push(
                IssueCode::InvalidReference,
                "spec.depends_on",
                "a tool cannot depend on itself",
            );
        }
        self.versioning(&spec.versioning);
        self.source(
            &spec.source,
            spec.versioning.scheme,
            Some(&spec.versioning.variants),
        );
        self.install(&spec.install);
        let config_ids = self.config(&spec.config);
        let port_names = self.service(&spec.service, &m.metadata.id);
        if let Some(discover) = &spec.discover {
            self.discover(discover, &spec.versioning.variants);
        }
        self.update(&spec.update, m.metadata.category);
        self.ui(&spec.ui, &config_ids);
        let scope = Scope {
            kind: Kind::Tool,
            variant_keys: Some(set_of(spec.versioning.variant_order())),
            config_ids: Some(config_ids),
            port_names: Some(port_names),
            instance_tools: None,
        };
        self.templates(spec, &scope);
    }

    fn extension(&mut self, m: &ExtensionManifest) {
        self.tool_metadata(&m.metadata);
        let spec = &m.spec;
        if spec.extends == m.metadata.id {
            self.push(
                IssueCode::InvalidReference,
                "spec.extends",
                "an extension cannot extend itself",
            );
        }
        for (i, compat) in spec.compat.iter().enumerate() {
            let f = format!("spec.compat[{i}]");
            let tool_ok = Constraint::parse(VersionScheme::Semver, &compat.tool).is_ok()
                || Constraint::parse(VersionScheme::String, &compat.tool).is_ok();
            if !tool_ok {
                self.push(
                    IssueCode::InvalidConstraint,
                    format!("{f}.tool"),
                    format!("invalid constraint {:?}", compat.tool),
                );
            }
            if let Err(e) = Constraint::parse(VersionScheme::Semver, &compat.version) {
                self.push(
                    IssueCode::InvalidConstraint,
                    format!("{f}.version"),
                    e.to_string(),
                );
            }
            for key in compat.variant.keys() {
                if !is_key(key) {
                    self.push(
                        IssueCode::InvalidId,
                        format!("{f}.variant.{key}"),
                        "variant keys use [a-z0-9_]",
                    );
                }
            }
        }
        self.source(&spec.source, VersionScheme::Semver, None);
        let install = &spec.install;
        if install.archive == Some(ArchiveFormat::Msi) {
            self.push(
                IssueCode::Unsupported,
                "spec.install.archive",
                "msi is not supported in v1",
            );
        }
        match template::root_dir_prefix(&install.target) {
            Some((dir, rest)) if dir == "version" && !has_dotdot(rest) => {}
            _ => self.push(
                IssueCode::WriteOutsideRoot,
                "spec.install.target",
                "must start with `{{ paths.version }}` (inside the target version)",
            ),
        }
        self.post_install("spec.install.post_install", &install.post_install);
        self.uninstall_keep("spec.install.uninstall.keep", &install.uninstall.keep);
        self.config(&spec.config);
        if let Some(toggle) = &spec.toggle {
            self.not_empty("spec.toggle.enable", &toggle.enable);
            self.not_empty("spec.toggle.disable", &toggle.disable);
        }
        let scope = Scope {
            kind: Kind::Extension,
            variant_keys: None,
            config_ids: None,
            port_names: None,
            instance_tools: None,
        };
        self.templates(spec, &scope);
    }

    fn series_value(&mut self, field: &str, value: &str) {
        let ok = !value.is_empty()
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'));
        if !ok {
            self.push(
                IssueCode::InvalidVersion,
                field,
                format!("{value:?} is not a version or series"),
            );
        }
    }

    fn stack(&mut self, m: &StackManifest) {
        self.not_empty("metadata.name", &m.metadata.name);
        self.icon("metadata.icon", m.metadata.icon.as_deref());
        if let Some(url) = &m.metadata.homepage {
            self.url("metadata.homepage", url, false);
        }
        let spec = &m.spec;
        if spec.tools.is_empty() {
            self.push(
                IssueCode::InvalidValue,
                "spec.tools",
                "a stack needs at least one tool",
            );
        }
        let mut ids = BTreeSet::new();
        for (i, tool) in spec.tools.iter().enumerate() {
            let f = format!("spec.tools[{i}]");
            self.duplicate(&format!("{f}.id"), &mut ids, tool.id.as_str());
            if let Some(series) = &tool.series {
                self.series_value(&format!("{f}.series"), series);
            }
            if let Some(version) = &tool.version {
                self.series_value(&format!("{f}.version"), version);
            }
            if let Some(instance) = &tool.instance {
                if !is_name(instance) {
                    self.push(
                        IssueCode::InvalidId,
                        format!("{f}.instance"),
                        "instance names use [a-z0-9_-]",
                    );
                }
            }
        }
        if let Some(site) = &spec.site {
            if let Some(php) = &site.php {
                self.series_value("spec.site.php", php);
            }
            self.env_names("spec.site.env_patch", site.env_patch.keys());
            for (i, action) in site.post_create.iter().enumerate() {
                let f = format!("spec.site.post_create[{i}]");
                match action {
                    PostCreateAction::CreateDatabase(db) => {
                        let tool = db.instance.split_once('/').map(|(t, _)| t);
                        match tool {
                            Some(tool) if ids.contains(tool) => {}
                            _ => self.push(
                                IssueCode::InvalidReference,
                                format!("{f}.create_database.instance"),
                                format!(
                                    "{:?} must be `<tool>/<instance>` with a tool of spec.tools",
                                    db.instance
                                ),
                            ),
                        }
                        self.not_empty(&format!("{f}.create_database.name"), &db.name);
                    }
                    PostCreateAction::Run(run) => {
                        self.not_empty(&format!("{f}.run.command"), &run.command);
                    }
                }
            }
        }
        let scope = Scope {
            kind: Kind::Stack,
            variant_keys: None,
            config_ids: None,
            port_names: None,
            instance_tools: Some(ids),
        };
        self.templates(spec, &scope);
    }

    fn host(&mut self, field: &str, host: &str, hosts: &mut BTreeSet<String>) {
        if let Some(problem) = host_problem(host) {
            self.push(IssueCode::InvalidHost, field, format!("{host:?} {problem}"));
        }
        self.duplicate(field, hosts, host);
    }

    fn project(&mut self, m: &ProjectManifest) {
        self.not_empty("metadata.name", &m.metadata.name);
        let spec = &m.spec;
        let mut hosts = BTreeSet::new();
        if let Some(site) = &spec.site {
            if let Some(host) = &site.host {
                self.host("spec.site.host", host, &mut hosts);
            }
            for (i, alias) in site.aliases.iter().enumerate() {
                self.host(&format!("spec.site.aliases[{i}]"), alias, &mut hosts);
            }
            if let Some(root) = &site.root {
                self.relative_path("spec.site.root", root);
            }
            if let Some(php) = &site.php {
                self.series_value("spec.site.php", php);
            }
            for (key, value) in &site.php_ini {
                if value.is_array() || value.is_object() || value.is_null() {
                    self.push(
                        IssueCode::InvalidValue,
                        format!("spec.site.php_ini.{key}"),
                        "php.ini values are scalars",
                    );
                }
            }
        }
        let mut services = BTreeSet::new();
        for (i, service) in spec.services.iter().enumerate() {
            let f = format!("spec.services[{i}]");
            let instance = service.instance.as_deref().unwrap_or("default");
            if !is_name(instance) {
                self.push(
                    IssueCode::InvalidId,
                    format!("{f}.instance"),
                    "instance names use [a-z0-9_-]",
                );
            }
            self.duplicate(&f, &mut services, &format!("{}/{instance}", service.tool));
            for (name, value) in [("series", &service.series), ("version", &service.version)] {
                if let Some(value) = value {
                    self.series_value(&format!("{f}.{name}"), value);
                }
            }
        }
        for (i, runtime) in spec.runtimes.iter().enumerate() {
            let f = format!("spec.runtimes[{i}]");
            for (name, value) in [("series", &runtime.series), ("version", &runtime.version)] {
                if let Some(value) = value {
                    self.series_value(&format!("{f}.{name}"), value);
                }
            }
        }
        for (name, process) in &spec.processes {
            let f = format!("spec.processes.{name}");
            if !is_name(name) {
                self.push(IssueCode::InvalidId, &f, "process names use [a-z0-9_-]");
            }
            self.not_empty(&format!("{f}.cmd"), &process.cmd);
            if let Some(runtime) = &process.runtime {
                let ok = runtime.split_once('@').is_some_and(|(tool, version)| {
                    ToolId::new(tool).is_ok() && !version.is_empty()
                });
                if !ok {
                    self.push(
                        IssueCode::InvalidValue,
                        format!("{f}.runtime"),
                        "must be `<tool>@<version>` (nodejs@22)",
                    );
                }
            }
            if let Some(path) = &process.proxy_path {
                if !path.starts_with('/') {
                    self.push(
                        IssueCode::InvalidValue,
                        format!("{f}.proxy_path"),
                        "must start with /",
                    );
                }
            }
            self.env_names(&format!("{f}.env"), process.env.keys());
        }
        for (i, proxy) in spec.proxies.iter().enumerate() {
            let f = format!("spec.proxies[{i}]");
            self.host(&format!("{f}.host"), &proxy.host, &mut hosts);
            self.url(&format!("{f}.upstream"), &proxy.upstream, false);
        }
        let scope = Scope {
            kind: Kind::Project,
            variant_keys: None,
            config_ids: None,
            port_names: None,
            instance_tools: Some(spec.services.iter().map(|s| s.tool.to_string()).collect()),
        };
        self.templates(spec, &scope);
    }

    fn registry_entries(&mut self, section: &str, entries: &[RegistryEntry]) {
        let mut ids = BTreeSet::new();
        for (i, entry) in entries.iter().enumerate() {
            let f = format!("spec.{section}[{i}]");
            self.duplicate(&format!("{f}.id"), &mut ids, entry.id.as_str());
            self.relative_path(&format!("{f}.path"), &entry.path);
            if !entry.path.ends_with(".yaml") {
                self.push(
                    IssueCode::InvalidPath,
                    format!("{f}.path"),
                    "manifests are .yaml files",
                );
            }
        }
    }

    fn registry(&mut self, m: &RegistryManifest) {
        if ToolId::new(m.metadata.name.as_str()).is_err() {
            self.push(
                IssueCode::InvalidId,
                "metadata.name",
                "registry names use [a-z0-9-]",
            );
        }
        if let Some(url) = &m.metadata.url {
            self.url("metadata.url", url, true);
        }
        if m.spec.schema_version != REGISTRY_SCHEMA_VERSION {
            self.push(
                IssueCode::Unsupported,
                "spec.schema_version",
                format!(
                    "schema_version {} is not supported (expected {REGISTRY_SCHEMA_VERSION})",
                    m.spec.schema_version
                ),
            );
        }
        self.registry_entries("tools", &m.spec.tools);
        self.registry_entries("stacks", &m.spec.stacks);
        if let Some(signature) = &m.spec.signature {
            self.not_empty("spec.signature.key_id", &signature.key_id);
            self.not_empty("spec.signature.sig", &signature.sig);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_token_keeps_quotes_and_templates() {
        assert_eq!(first_token("bin/mysqld.exe --initialize"), "bin/mysqld.exe");
        assert_eq!(
            first_token("  {{ paths.version }}/bin/x.exe -v"),
            "{{ paths.version }}/bin/x.exe"
        );
        assert_eq!(first_token("\"bin/my tool.exe\" -v"), "bin/my tool.exe");
        assert_eq!(first_token(""), "");
    }

    #[test]
    fn hosts_must_be_lowercase_and_qualified() {
        assert_eq!(host_problem("loja.test"), None);
        assert_eq!(host_problem("*.loja.test"), None);
        assert_eq!(host_problem("Loja.test"), Some("must be lowercase"));
        assert!(host_problem("loja").is_some());
        assert!(host_problem("-a.test").is_some());
        assert!(host_problem("a..test").is_some());
        assert!(host_problem("a_b.test").is_some());
    }

    #[test]
    fn key_location_points_at_top_level_keys() {
        let text = "# c\napiVersion: x\nkind : Tool\n";
        assert_eq!(key_location(text, "apiVersion").unwrap().line, 2);
        assert_eq!(key_location(text, "kind").unwrap().line, 3);
        assert_eq!(key_location(text, "metadata"), None);
    }

    #[test]
    fn header_errors() {
        assert_eq!(load_str("  \n"), Err(ManifestError::Empty));
        let err = load_str("apiVersion: forgebox.dev/v2\nkind: Tool\n").unwrap_err();
        assert!(matches!(err, ManifestError::ApiVersion { .. }), "{err}");
        assert_eq!(err.location().unwrap().line, 1);
        assert!(err.to_string().contains("forgebox.dev/v2"), "{err}");
        let err = load_str("kind: Tool\n").unwrap_err();
        assert!(err.to_string().contains("missing `apiVersion`"), "{err}");
        let err = load_str("apiVersion: forgebox.dev/v1\nkind: Gadget\n").unwrap_err();
        assert!(matches!(err, ManifestError::Kind { .. }), "{err}");
        assert_eq!(err.location().unwrap().line, 2);
        let err = load_str("apiVersion: forgebox.dev/v1\n").unwrap_err();
        assert!(err.to_string().contains("missing `kind`"), "{err}");
        let err = load_str("apiVersion: [unclosed\n").unwrap_err();
        assert!(matches!(err, ManifestError::Yaml(_)), "{err}");
        assert!(err.location().is_some());
        let forge: ForgeError = err.into();
        assert_eq!(forge.code(), "config_invalid");
    }

    #[test]
    fn default_options_use_forge_core_ranges() {
        let options = ValidateOptions::default();
        for range in [
            "web",
            "web-alt",
            "php-pools",
            "databases",
            "apps",
            "mail",
            "storage",
            "xdebug",
        ] {
            assert!(options.port_ranges.iter().any(|r| r == range), "{range}");
        }
        assert!(!options.third_party);
    }
}
