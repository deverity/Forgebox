//! Lint of a registry folder: the seven rules of docs/04 §6.
//!
//! [`lint_registry`] parses every manifest of `tools/*.yaml` and `stacks/*.yaml` (and
//! `index.yaml` when present), runs [`crate::parse::validate_with`] on each and adds the
//! checks that need the whole folder:
//!
//! 1. ids unique per namespace (`tools/`: Tool + Extension; `stacks/`), file name
//!    `<id>.yaml`, kind matching the folder, references to other manifests
//!    (`depends_on`, `suggests`, `extends`, stack tools/extensions) and `index.yaml`
//!    consistency;
//! 4. `render.template` exists in [`LintContext::known_templates`] (the templates embedded
//!    by forge-render), in `<registry>/templates/` or in [`LintContext::template_roots`];
//! 7. icons in `<registry>/icons/` are SVG, at most 20 KB and without scripts.
//!
//! Rules 2, 3, 5 and 6 come from the validator. The lint is blocking file I/O: call it
//! from `spawn_blocking` in async code.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use forge_core::config::PortsConfig;
use forge_core::ids::ToolId;

use crate::issue::IssueCode;
use crate::locate::locate;
use crate::parse::{self, ManifestError, ValidateOptions};
use crate::schema::common::Kind;
use crate::schema::tool::PostInstallAction;
use crate::schema::{Manifest, RegistryManifest};
use crate::yaml::Location;

/// Largest icon accepted (rule 7).
pub const MAX_ICON_BYTES: u64 = 20 * 1024;

/// A registry folder: `index.yaml`, `tools/`, `stacks/`, `icons/`, `templates/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryDir {
    root: PathBuf,
}

impl RegistryDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn index_path(&self) -> PathBuf {
        self.root.join("index.yaml")
    }

    pub fn tools_dir(&self) -> PathBuf {
        self.root.join("tools")
    }

    pub fn stacks_dir(&self) -> PathBuf {
        self.root.join("stacks")
    }

    pub fn icons_dir(&self) -> PathBuf {
        self.root.join("icons")
    }

    pub fn templates_dir(&self) -> PathBuf {
        self.root.join("templates")
    }

    /// `tools/*.yaml` then `stacks/*.yaml`, each sorted; missing folders are empty.
    pub fn manifest_files(&self) -> std::io::Result<Vec<PathBuf>> {
        let mut files = yaml_files(&self.tools_dir())?;
        files.extend(yaml_files(&self.stacks_dir())?);
        Ok(files)
    }
}

fn yaml_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "yaml") {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

/// What the lint needs from outside the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintContext {
    /// Templates embedded in forge-render, relative with `/` (`php/php.ini.j2`). Passed in
    /// because forge-manifest does not depend on forge-render; `cargo xtask lint-registry`
    /// lists `crates/forge-render/templates/`.
    pub known_templates: Vec<String>,
    /// Extra folders searched for templates (besides `<registry>/templates/`).
    pub template_roots: Vec<PathBuf>,
    /// Names of the known port ranges (rule 3).
    pub port_ranges: Vec<String>,
}

impl Default for LintContext {
    /// No known templates; port ranges of `forge_core::config` (docs/03 §4).
    fn default() -> Self {
        Self::new(Vec::new(), Vec::new())
    }
}

impl LintContext {
    /// Context with the default port ranges of `forge_core::config`.
    pub fn new(known_templates: Vec<String>, template_roots: Vec<PathBuf>) -> Self {
        Self {
            known_templates,
            template_roots,
            port_ranges: PortsConfig::default().ranges.keys().cloned().collect(),
        }
    }

    fn validate_options(&self) -> ValidateOptions {
        ValidateOptions {
            port_ranges: self.port_ranges.clone(),
            third_party: false,
        }
    }
}

/// One problem found by the lint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintIssue {
    pub path: PathBuf,
    /// Rule of docs/04 §6 (1..=7).
    pub rule: u8,
    pub code: IssueCode,
    /// Dotted field path, when the problem is inside the manifest.
    pub field: Option<String>,
    pub location: Option<Location>,
    pub message: String,
}

impl LintIssue {
    fn new(
        path: &Path,
        code: IssueCode,
        field: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            path: path.to_owned(),
            rule: code.lint_rule(),
            code,
            field,
            location: None,
            message: message.into(),
        }
    }
}

impl fmt::Display for LintIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path.display())?;
        if let Some(location) = self.location {
            write!(f, ":{}:{}", location.line, location.column)?;
        }
        write!(f, ": [rule {} {}] ", self.rule, self.code)?;
        if let Some(field) = &self.field {
            write!(f, "{field}: ")?;
        }
        f.write_str(&self.message)
    }
}

/// Result of [`lint_registry`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LintReport {
    /// Manifest files read (index included).
    pub files_checked: usize,
    pub issues: Vec<LintIssue>,
}

impl LintReport {
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }

    /// Issues grouped by file.
    pub fn by_file(&self) -> BTreeMap<&Path, Vec<&LintIssue>> {
        let mut map: BTreeMap<&Path, Vec<&LintIssue>> = BTreeMap::new();
        for issue in &self.issues {
            map.entry(issue.path.as_path()).or_default().push(issue);
        }
        map
    }

    /// Number of issues per rule.
    pub fn count_by_rule(&self) -> BTreeMap<u8, usize> {
        let mut map = BTreeMap::new();
        for issue in &self.issues {
            *map.entry(issue.rule).or_default() += 1;
        }
        map
    }
}

/// A manifest of `tools/` or `stacks/` that parsed.
struct Loaded {
    path: PathBuf,
    manifest: Manifest,
}

impl Loaded {
    fn id(&self) -> Option<&ToolId> {
        self.manifest.id()
    }
}

fn schema_issue(path: &Path, err: &ManifestError) -> LintIssue {
    let mut issue = LintIssue::new(path, IssueCode::Schema, None, err.to_string());
    issue.location = err.location();
    if let ManifestError::Io { .. } = err {
        issue.code = IssueCode::Io;
    }
    issue
}

/// Lints the registry at `dir` (blocking).
pub fn lint_registry(dir: &RegistryDir, ctx: &LintContext) -> LintReport {
    let mut report = LintReport::default();
    let files = match dir.manifest_files() {
        Ok(files) => files,
        Err(e) => {
            report.issues.push(LintIssue::new(
                dir.root(),
                IssueCode::Io,
                None,
                format!("cannot list manifests: {e}"),
            ));
            return report;
        }
    };
    let options = ctx.validate_options();
    let mut loaded = Vec::new();
    for path in files {
        report.files_checked += 1;
        match parse::load_file(&path) {
            Err(err) => report.issues.push(schema_issue(&path, &err)),
            Ok(manifest) => {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                lint_one(
                    dir,
                    ctx,
                    &options,
                    &path,
                    &manifest,
                    &text,
                    &mut report.issues,
                );
                loaded.push(Loaded { path, manifest });
            }
        }
    }
    cross_check(&loaded, &mut report.issues);
    let index = dir.index_path();
    if index.is_file() {
        report.files_checked += 1;
        lint_index(&index, &options, &loaded, &mut report.issues);
    }
    lint_icons_dir(dir, &mut report.issues);
    report
}

/// Rules that only need one manifest (plus templates and icons on disk).
fn lint_one(
    dir: &RegistryDir,
    ctx: &LintContext,
    options: &ValidateOptions,
    path: &Path,
    manifest: &Manifest,
    text: &str,
    issues: &mut Vec<LintIssue>,
) {
    let folder = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let kind = manifest.kind();
    let expected: &[Kind] = match folder {
        "tools" => &[Kind::Tool, Kind::Extension],
        "stacks" => &[Kind::Stack],
        _ => &[],
    };
    if !expected.contains(&kind) {
        issues.push(LintIssue::new(
            path,
            IssueCode::MisplacedManifest,
            Some("kind".to_owned()),
            format!("kind {kind} does not belong in {folder}/"),
        ));
    }
    if let Some(id) = manifest.id() {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if stem != id.as_str() {
            issues.push(LintIssue::new(
                path,
                IssueCode::MisplacedManifest,
                Some("metadata.id".to_owned()),
                format!("file name must be `{id}.yaml`"),
            ));
        }
    }
    for issue in parse::validate_with(manifest, options) {
        let location = locate(text, &issue.field);
        let mut lint_issue = LintIssue::new(path, issue.code, Some(issue.field), issue.message);
        lint_issue.location = location;
        issues.push(lint_issue);
    }
    // Rule 4.
    let post_install = match manifest {
        Manifest::Tool(m) => m.spec.install.post_install.as_slice(),
        Manifest::Extension(m) => m.spec.install.post_install.as_slice(),
        _ => &[],
    };
    for (i, action) in post_install.iter().enumerate() {
        if let PostInstallAction::Render(render) = action {
            let template = render.template.as_str();
            let safe = !template.is_empty()
                && !template.starts_with(['/', '\\'])
                && !template.contains(['\\', ':'])
                && !template.split('/').any(|s| s == "..");
            if safe && !template_exists(dir, ctx, template) {
                issues.push(LintIssue::new(
                    path,
                    IssueCode::MissingTemplate,
                    Some(format!("spec.install.post_install[{i}].render.template")),
                    format!(
                        "template `{template}` exists neither in forge-render/templates/ nor in \
                         the registry templates/"
                    ),
                ));
            }
        }
    }
    // Rule 7 (file part); the name itself is checked by the validator.
    if let Some(icon) = manifest.icon() {
        let relative = !icon.contains("://")
            && !icon.starts_with(['/', '\\'])
            && !icon.contains(['\\', ':'])
            && !icon.split('/').any(|s| s == "..");
        if relative && icon.to_ascii_lowercase().ends_with(".svg") {
            if let Some(problem) = icon_problem(&dir.icons_dir().join(icon)) {
                issues.push(LintIssue::new(
                    path,
                    IssueCode::InvalidIcon,
                    Some("metadata.icon".to_owned()),
                    format!("icons/{icon}: {problem}"),
                ));
            }
        }
    }
}

fn template_exists(dir: &RegistryDir, ctx: &LintContext, template: &str) -> bool {
    ctx.known_templates.iter().any(|t| t == template)
        || std::iter::once(dir.templates_dir())
            .chain(ctx.template_roots.iter().cloned())
            .any(|root| root.join(template).is_file())
}

/// Rule 7 for one file: exists, at most 20 KB, an SVG, no scripts.
fn icon_problem(path: &Path) -> Option<String> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => meta,
        _ => return Some("file not found".to_owned()),
    };
    if meta.len() > MAX_ICON_BYTES {
        return Some(format!(
            "{} bytes, the limit is {MAX_ICON_BYTES} (20 KB)",
            meta.len()
        ));
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text.to_ascii_lowercase(),
        Err(e) => return Some(format!("cannot read: {e}")),
    };
    if !text.contains("<svg") {
        return Some("not an SVG document".to_owned());
    }
    if text.contains("<script") || text.contains("javascript:") {
        return Some("SVG icons must not contain scripts".to_owned());
    }
    None
}

fn lint_icons_dir(dir: &RegistryDir, issues: &mut Vec<LintIssue>) {
    let icons = dir.icons_dir();
    let Ok(entries) = std::fs::read_dir(&icons) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths.into_iter().filter(|p| p.is_file()) {
        let svg = path.extension().is_some_and(|e| e == "svg");
        let problem = if svg {
            icon_problem(&path)
        } else {
            Some("only .svg files belong in icons/".to_owned())
        };
        if let Some(problem) = problem {
            issues.push(LintIssue::new(&path, IssueCode::InvalidIcon, None, problem));
        }
    }
}

/// Rule 1 across files: unique ids and references between manifests.
fn cross_check(loaded: &[Loaded], issues: &mut Vec<LintIssue>) {
    let mut tools: BTreeMap<&str, &Loaded> = BTreeMap::new();
    let mut stacks: BTreeMap<&str, &Loaded> = BTreeMap::new();
    for item in loaded {
        let Some(id) = item.id() else { continue };
        let namespace = match item.manifest.kind() {
            Kind::Tool | Kind::Extension => &mut tools,
            Kind::Stack => &mut stacks,
            Kind::Project | Kind::Registry => continue,
        };
        if let Some(first) = namespace.get(id.as_str()) {
            issues.push(LintIssue::new(
                &item.path,
                IssueCode::DuplicateId,
                Some("metadata.id".to_owned()),
                format!("id `{id}` is already used by {}", first.path.display()),
            ));
        } else {
            namespace.insert(id.as_str(), item);
        }
    }
    let kind_of = |id: &ToolId| tools.get(id.as_str()).map(|l| l.manifest.kind());
    let mut unknown = |path: &Path, field: String, id: &ToolId, what: &str| {
        issues.push(LintIssue::new(
            path,
            IssueCode::UnknownTool,
            Some(field),
            format!("`{id}` is not a {what} of this registry"),
        ));
    };
    for item in loaded {
        let path = item.path.as_path();
        match &item.manifest {
            Manifest::Tool(m) => {
                let spec = &m.spec;
                let lists = [
                    ("spec.depends_on", &spec.depends_on),
                    ("spec.suggests", &spec.suggests),
                ];
                for (field, ids) in lists {
                    for (i, id) in ids.iter().enumerate() {
                        if kind_of(id).is_none() {
                            unknown(path, format!("{field}[{i}]"), id, "tool or extension");
                        }
                    }
                }
                if let Some(daemon) = &spec.service.daemon {
                    for (i, id) in daemon.depends_on.iter().enumerate() {
                        if kind_of(id) != Some(Kind::Tool) {
                            unknown(
                                path,
                                format!("spec.service.daemon.depends_on[{i}]"),
                                id,
                                "tool",
                            );
                        }
                    }
                }
                if let Some(id) = &m.metadata.replaced_by {
                    if kind_of(id).is_none() {
                        unknown(path, "metadata.replaced_by".to_owned(), id, "tool");
                    }
                }
            }
            Manifest::Extension(m) => {
                if kind_of(&m.spec.extends) != Some(Kind::Tool) {
                    unknown(path, "spec.extends".to_owned(), &m.spec.extends, "tool");
                }
            }
            Manifest::Stack(m) => {
                for (i, tool) in m.spec.tools.iter().enumerate() {
                    if kind_of(&tool.id) != Some(Kind::Tool) {
                        unknown(path, format!("spec.tools[{i}].id"), &tool.id, "tool");
                    }
                    for (j, ext) in tool.extensions.iter().enumerate() {
                        let fits = tools.get(ext.as_str()).is_some_and(|l| {
                            matches!(&l.manifest, Manifest::Extension(e) if e.spec.extends == tool.id)
                        });
                        if !fits {
                            unknown(
                                path,
                                format!("spec.tools[{i}].extensions[{j}]"),
                                ext,
                                &format!("extension of `{}`", tool.id),
                            );
                        }
                    }
                }
            }
            Manifest::Project(_) | Manifest::Registry(_) => {}
        }
    }
}

/// Rule 1 for `index.yaml`: valid, and consistent with the manifest files.
fn lint_index(
    index: &Path,
    options: &ValidateOptions,
    loaded: &[Loaded],
    issues: &mut Vec<LintIssue>,
) {
    let manifest = match parse::load_file(index) {
        Ok(manifest) => manifest,
        Err(err) => {
            issues.push(schema_issue(index, &err));
            return;
        }
    };
    let Manifest::Registry(registry) = &manifest else {
        issues.push(LintIssue::new(
            index,
            IssueCode::MisplacedManifest,
            Some("kind".to_owned()),
            format!(
                "index.yaml must be kind Registry, found {}",
                manifest.kind()
            ),
        ));
        return;
    };
    for issue in parse::validate_with(&manifest, options) {
        issues.push(LintIssue::new(
            index,
            issue.code,
            Some(issue.field),
            issue.message,
        ));
    }
    let root = index.parent().unwrap_or(Path::new("."));
    let listed = index_entries(registry, root, loaded, index, issues);
    for item in loaded {
        let canonical = item.path.canonicalize().ok();
        if !listed.iter().any(|p| Some(p) == canonical.as_ref()) {
            issues.push(LintIssue::new(
                &item.path,
                IssueCode::IndexMismatch,
                None,
                "not listed in index.yaml",
            ));
        }
    }
}

/// Checks the entries and returns the canonical paths they list.
fn index_entries(
    registry: &RegistryManifest,
    root: &Path,
    loaded: &[Loaded],
    index: &Path,
    issues: &mut Vec<LintIssue>,
) -> BTreeSet<PathBuf> {
    let mut listed = BTreeSet::new();
    let sections = [
        (
            "tools",
            &registry.spec.tools,
            &[Kind::Tool, Kind::Extension][..],
        ),
        ("stacks", &registry.spec.stacks, &[Kind::Stack][..]),
    ];
    for (section, entries, kinds) in sections {
        for (i, entry) in entries.iter().enumerate() {
            let field = format!("spec.{section}[{i}]");
            if entry.path.split(['/', '\\']).any(|s| s == "..")
                || entry.path.starts_with(['/', '\\'])
            {
                continue; // reported by the validator
            }
            let Ok(target) = root.join(&entry.path).canonicalize() else {
                issues.push(LintIssue::new(
                    index,
                    IssueCode::IndexMismatch,
                    Some(format!("{field}.path")),
                    format!("{} does not exist", entry.path),
                ));
                continue;
            };
            let item = loaded
                .iter()
                .find(|l| l.path.canonicalize().ok().as_ref() == Some(&target));
            match item {
                Some(item) => {
                    let kind = item.manifest.kind();
                    match std::fs::read(&item.path) {
                        Ok(bytes) => {
                            let actual = crate::index::sha256_hex(&bytes);
                            if actual != entry.sha256.as_str() {
                                issues.push(LintIssue::new(
                                    index,
                                    IssueCode::IndexMismatch,
                                    Some(format!("{field}.sha256")),
                                    format!(
                                        "sha256 of {} is {actual}, the index says {}",
                                        entry.path, entry.sha256
                                    ),
                                ));
                            }
                        }
                        Err(e) => issues.push(LintIssue::new(
                            index,
                            IssueCode::Io,
                            Some(field.clone()),
                            format!("cannot read {}: {e}", entry.path),
                        )),
                    }
                    if item.id() != Some(&entry.id) || !kinds.contains(&kind) {
                        issues.push(LintIssue::new(
                            index,
                            IssueCode::IndexMismatch,
                            Some(field),
                            format!(
                                "entry `{}` points to {} ({kind} `{}`)",
                                entry.id,
                                entry.path,
                                item.id().map(ToolId::as_str).unwrap_or("?"),
                            ),
                        ));
                    }
                }
                None => issues.push(LintIssue::new(
                    index,
                    IssueCode::IndexMismatch,
                    Some(format!("{field}.path")),
                    format!(
                        "{} is not a valid manifest of tools/ or stacks/",
                        entry.path
                    ),
                )),
            }
            listed.insert(target);
        }
    }
    listed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_display_has_location_rule_and_field() {
        let mut issue = LintIssue::new(
            Path::new("tools/php.yaml"),
            IssueCode::UnknownPortRange,
            Some("spec.service.pool.port_range".into()),
            "unknown",
        );
        issue.location = Some(Location { line: 3, column: 7 });
        assert_eq!(
            issue.to_string(),
            "tools/php.yaml:3:7: [rule 3 unknown_port_range] spec.service.pool.port_range: unknown"
        );
    }

    #[test]
    fn missing_registry_folders_are_empty() {
        let dir = tempfile::tempdir().unwrap();
        let report = lint_registry(&RegistryDir::new(dir.path()), &LintContext::default());
        assert!(report.is_clean());
        assert_eq!(report.files_checked, 0);
    }

    #[test]
    fn default_context_knows_the_port_ranges() {
        let ctx = LintContext::default();
        assert!(ctx.port_ranges.iter().any(|r| r == "php-pools"));
        assert!(ctx.known_templates.is_empty());
    }
}
