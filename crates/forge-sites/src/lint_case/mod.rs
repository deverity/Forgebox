//! Case Mismatch Finder (docs/15 §6): `forge site lint-case <site> [--fix]`.
//!
//! Walks a project, collects every reference to a file it can recognize and resolves it
//! against the **real names on disk** (case-exact folder listings), ignoring how the
//! file system would resolve it. A reference whose path only exists when case is
//! ignored is a [`IssueCategory::Mismatch`]: it works on an insensitive Windows folder
//! and breaks on Linux (and in a case-sensitive Forgebox folder). It also reports names
//! that differ only by case in one folder, names Windows cannot hold (`aux`, `con`,
//! `a:b`, trailing dot), `.env` keys that differ only by case and scripts whose shebang
//! line ends in CRLF. Works the same on case-sensitive and insensitive folders.
//!
//! # What is recognized (regular expressions, no full parser)
//!
//! | Kind (`kind`) | Where | Resolved from |
//! |---|---|---|
//! | `php-include` | `require`/`include`(`_once`) of a literal, `__DIR__ . '...'`, `dirname(__FILE__) . '...'` | the file's folder; for bare literals also the project root and document root |
//! | `php-path` | `base_path()`, `app_path()`, `resource_path()`, `public_path()`, `storage_path()`, `database_path()`, `config_path()`, `lang_path()` | the matching folder |
//! | `php-view` / `blade-include` | `view()`, `View::make()`, `->view()`, `->markdown()`, `@extends`, `@include`, `@includeIf`, `@component`, `@each` (dots or `/` → folders, `.blade.php`/`.php`) | `resources/views` |
//! | `asset` | `asset()`, `secure_asset()`, `mix()` (document root); `Vite::asset()`, `@vite([...])`, `vite()` (project root) | |
//! | `html` | `src=`, `href=`, `data-src=`, `poster=` in PHP, Blade, Twig, HTML, Vue/Svelte | `/x` from the document root; relative from the file (HTML/PHP) or the document root first (templates) |
//! | `twig-include` | `{% include/extends/embed/import/from/use %}`, `{{ include()/source() }}` | `templates/`, then the file's folder |
//! | `js-import` | `import ... from`, `export ... from`, `import '...'`, `import()`, `require()` with `./`/`../` (and the `@/` alias → `resources/js`, `src`) | the file's folder, with Node/Vite resolution: extensions `.js .jsx .ts .tsx .mjs .cjs .vue .json .svelte .mts` and `index.*` |
//! | `css` | `url()`, `@import`, Sass `@use`/`@forward` (partials `_name.scss`) | the file's folder; `/x` from the document root |
//! | `composer` | `autoload(-dev)` PSR-4 roots, `classmap`, `files`; `bin` | the folder of `composer.json` |
//! | `package-json` | `main`, `module`, `types`, `typings`, `browser`, `bin`, `files`, relative `exports` | the folder of `package.json` |
//! | `psr4` | the first class/interface/trait/enum of each PHP file under a PSR-4 root of the root `composer.json`, against its path | the PSR-4 root |
//!
//! # Heuristics and limits
//!
//! - Only literal paths are checked: anything with interpolation (`"$x.php"`, `${}`),
//!   concatenation after the literal, a scheme (`https:`, `data:`), a template expression
//!   (`{{ }}`, `{% %}`, `<?`) or a backslash is skipped.
//! - A reference is reported only when **no** base/spelling resolves exactly and at least
//!   one resolves ignoring case. References that resolve nowhere (dynamic paths, routes,
//!   files generated at build time) are ignored: no false "missing file" noise.
//! - Comments are not stripped: a commented-out `require` is checked too.
//! - Not covered: table names in migrations vs queries (docs/15 §6, future), PSR-0,
//!   `tsconfig` path aliases other than `@/`, webpack `~` imports, `srcset`.
//! - Folders named `vendor`, `node_modules`, `.git`, `storage`, `cache` (and `.svn`,
//!   `.hg`) are not walked ([`LintOptions::ignore_dirs`]); references into them are
//!   still resolved, but never fixed by renaming. Files over
//!   [`LintOptions::max_file_bytes`] or not valid UTF-8 are skipped.
//!
//! # Fixing
//!
//! [`fix()`] applies one strategy per mismatch: **rename** the file on disk to the
//! spelling the reference uses, or **edit** the reference to the on-disk spelling.
//! See [`fix`] for the automatic choice, the preview (dry run) and how conflicting
//! choices are resolved. Output is stable JSON ([`LintReport`], version
//! [`LINT_CASE_VERSION`]), the shape of `forge_ipc::api::fs_parity::CaseLintReportDto`.

pub mod fix;
mod refs;
mod resolve;

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::path::{Path, PathBuf};

use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::fs::case::fold_case;
use forge_core::paths::posix_string;
use serde::{Deserialize, Serialize};

pub use self::fix::{FixOptions, StrategyChoice, fix};
use self::refs::{Base, FileKind, RawRef};
use self::resolve::{Candidate, DirIndex, Found, Lookup};

/// Version of the [`LintReport`] JSON.
pub const LINT_CASE_VERSION: u32 = 1;

/// Folders never walked by default.
pub const DEFAULT_IGNORED_DIRS: [&str; 7] = [
    "vendor",
    "node_modules",
    ".git",
    "storage",
    "cache",
    ".svn",
    ".hg",
];

/// What kind of reference (or check) produced an issue. Wire names are kebab-case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RefKind {
    PhpInclude,
    PhpPath,
    PhpView,
    Psr4,
    JsImport,
    Html,
    Asset,
    BladeInclude,
    TwigInclude,
    Css,
    Composer,
    PackageJson,
    /// Names in a folder (duplicates by case, reserved names).
    Fs,
    /// `.env` keys.
    Env,
    /// Executable scripts.
    Script,
}

/// Category of an issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueCategory {
    /// A reference whose case differs from the file on disk (fixable).
    Mismatch,
    /// Names in one folder that differ only by case (collide on Windows/macOS clones).
    CaseDuplicate,
    /// A name Windows cannot hold: device names (`aux`, `con`, `nul`, `com1`...),
    /// `<>:"|?*`, control characters, trailing dot or space. `detail` has the reason.
    ReservedName,
    /// `.env` keys that differ only by case (environment variables are insensitive on
    /// Windows: no parity possible, docs/15 §7).
    EnvCaseDuplicate,
    /// A script whose shebang line ends in CRLF (`/usr/bin/env: 'php\r'` on Linux).
    CrlfShebang,
}

/// How a mismatch is fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixStrategy {
    /// Rename the file (and folders) on disk to the spelling of the reference.
    Rename,
    /// Rewrite the reference with the spelling on disk.
    Edit,
}

/// A planned or applied fix of one issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseFix {
    pub strategy: FixStrategy,
    /// Rename: current path (relative, `/`); edit: the reference as written.
    pub from: String,
    /// Rename: new path; edit: the reference rewritten.
    pub to: String,
    /// Edit: the source line before and after (preview).
    pub before: Option<String>,
    pub after: Option<String>,
    pub applied: bool,
    /// Why it could not be applied.
    pub error: Option<String>,
}

/// Internal data a fix needs (not serialized).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MismatchDetail {
    pub file_abs: PathBuf,
    /// Byte range of the reference in the file (edit).
    pub span: Option<Range<usize>>,
    /// The reference rewritten with the on-disk spelling (edit).
    pub corrected: Option<String>,
    pub target_abs: PathBuf,
    pub expected_abs: PathBuf,
    /// The source line and the offset of the reference in it (preview).
    pub line_text: String,
    pub line_offset: usize,
}

/// One finding. `id` (`<file>:<line>:<column>`, plus the category for non-mismatches) is
/// stable between scans and selects the item in [`FixOptions`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseIssue {
    pub id: String,
    pub category: IssueCategory,
    pub kind: RefKind,
    /// File holding the reference, or the offending entry (relative to the project, `/`).
    pub file: String,
    /// 1-based; 0 when not about a line.
    pub line: u32,
    /// 1-based, in characters; 0 when not about a line.
    pub column: u32,
    /// As written (`Views/header.php`), or the offending name.
    pub reference: String,
    /// The reference with the spelling on disk (`views/Header.php`); for duplicates the
    /// other names.
    pub actual: String,
    /// Real file the reference reaches (relative, or absolute when outside).
    pub target: Option<String>,
    /// Path the reference names (what a rename would produce).
    pub expected: Option<String>,
    /// More than one entry matched ignoring case; the first was taken.
    pub ambiguous: bool,
    /// Extra information (reason code of a reserved name).
    pub detail: Option<String>,
    /// Strategies that can fix it (empty: report only).
    pub fixes: Vec<FixStrategy>,
    /// Planned (dry run) or applied fix.
    pub fix: Option<CaseFix>,
    pub fixed: bool,
    #[serde(skip)]
    pub(crate) internal: Option<MismatchDetail>,
}

/// Result of a scan (and of a fix run).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LintReport {
    pub v: u32,
    /// Project folder scanned (`/`).
    pub root: String,
    pub files_scanned: u32,
    pub references_checked: u32,
    /// Sorted by file, line, column.
    pub issues: Vec<CaseIssue>,
    /// This run fixed (or planned fixes for) the mismatches.
    pub fix: bool,
    pub dry_run: bool,
    /// Mismatches left after fixing (new scan); `None` when nothing was applied.
    pub remaining: Option<u32>,
    /// Targets reached by exact references, with the count (automatic strategy).
    #[serde(skip)]
    pub(crate) exact_targets: HashMap<PathBuf, u32>,
}

impl LintReport {
    /// Issues of the mismatch category.
    pub fn mismatches(&self) -> impl Iterator<Item = &CaseIssue> {
        self.issues
            .iter()
            .filter(|i| i.category == IssueCategory::Mismatch)
    }
}

/// What to scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintOptions {
    /// Project folder (where `composer.json`/`package.json` live).
    pub root: PathBuf,
    /// Document roots (`public/`). Empty: the existing ones of `public`, `web`,
    /// `public_html`, `htdocs`, else the project folder.
    pub doc_roots: Vec<PathBuf>,
    /// Folder names not walked (compared ignoring case).
    pub ignore_dirs: Vec<String>,
    /// Larger files are not read.
    pub max_file_bytes: u64,
}

impl LintOptions {
    /// Defaults for the project at `root`: auto document root, [`DEFAULT_IGNORED_DIRS`],
    /// 2 MiB per file.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            doc_roots: Vec::new(),
            ignore_dirs: DEFAULT_IGNORED_DIRS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            max_file_bytes: 2 * 1024 * 1024,
        }
    }
}

/// Project folder for a site whose document root is `doc_root`: the closest of
/// `doc_root`, its parent and grandparent holding a project marker (`composer.json`,
/// `package.json`, `.git`, `forgebox.yaml`, `artisan`), else `doc_root` itself.
pub fn project_dir_for_root(doc_root: &Path) -> PathBuf {
    const MARKERS: [&str; 5] = [
        "composer.json",
        "package.json",
        ".git",
        "forgebox.yaml",
        "artisan",
    ];
    let mut cur = Some(doc_root);
    for _ in 0..3 {
        let Some(dir) = cur else { break };
        if MARKERS.iter().any(|m| dir.join(m).exists()) {
            return dir.to_path_buf();
        }
        cur = dir.parent();
    }
    doc_root.to_path_buf()
}

/// Line starts of a text, for byte offset → line/column.
struct Lines<'t> {
    text: &'t str,
    starts: Vec<usize>,
}

impl<'t> Lines<'t> {
    fn new(text: &'t str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { text, starts }
    }

    /// 1-based line and column (characters), the line text (without `\r\n`) and the
    /// byte offset of `offset` inside it.
    fn at(&self, offset: usize) -> (u32, u32, String, usize) {
        let idx = self
            .starts
            .partition_point(|&s| s <= offset)
            .saturating_sub(1);
        let start = self.starts[idx];
        let end = self.starts.get(idx + 1).copied().unwrap_or(self.text.len());
        let line = self.text[start..end]
            .trim_end_matches(['\n', '\r'])
            .to_owned();
        let column = self.text[start..offset].chars().count() + 1;
        (
            u32::try_from(idx + 1).unwrap_or(u32::MAX),
            u32::try_from(column).unwrap_or(u32::MAX),
            line,
            offset - start,
        )
    }
}

/// Reason a name cannot exist on Windows, if any.
fn reserved_reason(name: &str) -> Option<&'static str> {
    const DEVICES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    let upper = name.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or("").trim_end();
    let numbered = |p: &str| {
        stem.strip_prefix(p)
            .is_some_and(|d| d.len() == 1 && d.chars().all(|c| ('1'..='9').contains(&c)))
    };
    if DEVICES.contains(&stem) || numbered("COM") || numbered("LPT") {
        return Some("reserved_device");
    }
    if name
        .chars()
        .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (c as u32) < 32)
    {
        return Some("illegal_character");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some("trailing_dot_or_space");
    }
    None
}

fn rel_of(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(posix_string)
        .unwrap_or_else(|_| posix_string(path))
}

/// Whether `rel` (relative path) goes through an ignored folder.
fn in_ignored(rel: &Path, ignore: &[String]) -> bool {
    rel.components().any(|c| {
        let c = c.as_os_str().to_string_lossy();
        ignore.iter().any(|i| fold_case(i) == fold_case(&c))
    })
}

/// A PSR-4 root: namespace prefix (without trailing `\`) and folder (on-disk spelling).
#[derive(Debug, Clone)]
struct Psr4Root {
    prefix: String,
    dir: PathBuf,
}

struct Scanner<'a> {
    opts: &'a LintOptions,
    root: PathBuf,
    doc_roots: Vec<PathBuf>,
    index: DirIndex,
    psr4: Vec<Psr4Root>,
    issues: Vec<CaseIssue>,
    exact_targets: HashMap<PathBuf, u32>,
    files_scanned: u32,
    references_checked: u32,
}

impl<'a> Scanner<'a> {
    fn new(opts: &'a LintOptions) -> Result<Self> {
        let root = std::path::absolute(&opts.root).at("resolving", &opts.root)?;
        let meta = std::fs::metadata(&root).at("inspecting", &root)?;
        if !meta.is_dir() {
            return Err(ForgeError::InvalidInput(format!(
                "{} is not a directory",
                root.display()
            )));
        }
        let mut doc_roots: Vec<PathBuf> = opts
            .doc_roots
            .iter()
            .filter_map(|d| std::path::absolute(d).ok())
            .filter(|d| d.is_dir())
            .collect();
        if doc_roots.is_empty() {
            doc_roots = ["public", "web", "public_html", "htdocs"]
                .iter()
                .map(|d| root.join(d))
                .filter(|d| d.is_dir())
                .collect();
        }
        if doc_roots.is_empty() {
            doc_roots.push(root.clone());
        }
        Ok(Self {
            opts,
            root,
            doc_roots,
            index: DirIndex::default(),
            psr4: Vec::new(),
            issues: Vec::new(),
            exact_targets: HashMap::new(),
            files_scanned: 0,
            references_checked: 0,
        })
    }

    fn bases(&self, file_abs: &Path, bases: &[Base]) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for b in bases {
            match b {
                Base::FileDir => {
                    if let Some(p) = file_abs.parent() {
                        out.push(p.to_path_buf());
                    }
                }
                Base::Root => out.push(self.root.clone()),
                Base::Under(sub) => out.push(refs::under(&self.root, sub)),
                Base::DocRoot => out.extend(self.doc_roots.iter().cloned()),
            }
        }
        out.dedup();
        out
    }

    /// PSR-4 roots of the project's `composer.json`, resolved to on-disk spelling.
    fn load_psr4(&mut self) {
        let path = self.root.join("composer.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            return;
        };
        for (prefix, dirs) in refs::psr4_roots(&json) {
            let prefix = prefix.trim_end_matches('\\').to_owned();
            for dir in dirs {
                let segs = resolve::split(&dir, &['/']);
                let found = match resolve::lookup(
                    &mut self.index,
                    &self.root,
                    &segs,
                    &Candidate::EXACT,
                    true,
                ) {
                    Lookup::Exact(p) => p,
                    Lookup::Insensitive(f) => f.target,
                    Lookup::Missing => continue,
                };
                if found.is_dir() {
                    self.psr4.push(Psr4Root {
                        prefix: prefix.clone(),
                        dir: found,
                    });
                }
            }
        }
    }

    fn walk(&mut self, progress: &mut dyn FnMut(u32), cancel: &dyn Fn() -> bool) -> Result<()> {
        self.load_psr4();
        let mut reported = 0;
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            if cancel() {
                return Err(ForgeError::Cancelled("case mismatch scan".to_owned()));
            }
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<(String, std::fs::Metadata, PathBuf)> = rd
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let meta = e.metadata().ok()?;
                    Some((e.file_name().to_string_lossy().into_owned(), meta, e.path()))
                })
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let dir_rel = rel_of(&self.root, &dir);
            self.check_names(&dir_rel, entries.iter().map(|e| e.0.as_str()));
            for (name, meta, path) in entries.into_iter().rev() {
                // `aux.php` opened through a normal path is the AUX device: never read
                // or walk an entry with a reserved name (it was reported above).
                if reserved_reason(&name).is_some() {
                    continue;
                }
                if meta.is_dir() {
                    let ignored = self
                        .opts
                        .ignore_dirs
                        .iter()
                        .any(|i| fold_case(i) == fold_case(&name));
                    if !ignored && !meta.file_type().is_symlink() {
                        stack.push(path);
                    }
                    continue;
                }
                if !meta.is_file() || meta.len() > self.opts.max_file_bytes {
                    continue;
                }
                self.scan_file(&path, &name);
                if self.files_scanned >= reported + 50 {
                    reported = self.files_scanned;
                    progress(reported);
                }
            }
        }
        progress(self.files_scanned);
        Ok(())
    }

    fn push_issue(&mut self, issue: CaseIssue) {
        self.issues.push(issue);
    }

    fn check_names<'n>(&mut self, dir_rel: &str, names: impl Iterator<Item = &'n str>) {
        let join = |name: &str| {
            if dir_rel.is_empty() {
                name.to_owned()
            } else {
                format!("{dir_rel}/{name}")
            }
        };
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for name in names {
            if let Some(reason) = reserved_reason(name) {
                let file = join(name);
                self.push_issue(CaseIssue {
                    id: format!("reserved_name:{file}"),
                    category: IssueCategory::ReservedName,
                    kind: RefKind::Fs,
                    file,
                    line: 0,
                    column: 0,
                    reference: name.to_owned(),
                    actual: String::new(),
                    target: None,
                    expected: None,
                    ambiguous: false,
                    detail: Some(reason.to_owned()),
                    fixes: Vec::new(),
                    fix: None,
                    fixed: false,
                    internal: None,
                });
            }
            groups
                .entry(fold_case(name))
                .or_default()
                .push(name.to_owned());
        }
        for names in groups.into_values().filter(|g| g.len() > 1) {
            let file = join(&names[0]);
            self.push_issue(CaseIssue {
                id: format!("case_duplicate:{file}"),
                category: IssueCategory::CaseDuplicate,
                kind: RefKind::Fs,
                file,
                line: 0,
                column: 0,
                reference: names[0].clone(),
                actual: names[1..].join(", "),
                target: None,
                expected: None,
                ambiguous: false,
                detail: None,
                fixes: Vec::new(),
                fix: None,
                fixed: false,
                internal: None,
            });
        }
    }

    fn scan_file(&mut self, path: &Path, name: &str) {
        let kind = refs::file_kind(name);
        let rel = rel_of(&self.root, path);
        let has_ext = name.contains('.') && !name.starts_with('.');
        let script_like = !has_ext
            || ["sh", "bash", "php", "py", "pl", "rb"]
                .iter()
                .any(|e| name.to_ascii_lowercase().ends_with(&format!(".{e}")));
        let is_env = rel == ".env";
        if kind.is_none() && !script_like && !is_env {
            return;
        }
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        if script_like && bytes.starts_with(b"#!") {
            let first = bytes.split(|b| *b == b'\n').next().unwrap_or(&[]);
            if first.ends_with(b"\r") {
                let line = String::from_utf8_lossy(first).trim_end().to_owned();
                self.push_issue(CaseIssue {
                    id: format!("crlf_shebang:{rel}"),
                    category: IssueCategory::CrlfShebang,
                    kind: RefKind::Script,
                    file: rel.clone(),
                    line: 1,
                    column: 1,
                    reference: line,
                    actual: String::new(),
                    target: None,
                    expected: None,
                    ambiguous: false,
                    detail: None,
                    fixes: Vec::new(),
                    fix: None,
                    fixed: false,
                    internal: None,
                });
            }
        }
        let Ok(text) = String::from_utf8(bytes) else {
            return;
        };
        if is_env {
            self.check_env(&rel, &text);
        }
        let Some(kind) = kind else {
            return;
        };
        self.files_scanned += 1;
        let lines = Lines::new(&text);
        let raws = match kind {
            FileKind::Composer | FileKind::PackageJson => manifest_refs(kind, &text),
            _ => refs::extract(kind, &text),
        };
        for raw in raws {
            self.check_ref(path, &rel, &lines, raw);
        }
        if kind == FileKind::Php {
            self.check_psr4(path, &rel, &lines, &text);
        }
    }

    fn check_env(&mut self, rel: &str, text: &str) {
        let mut seen: HashMap<String, String> = HashMap::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let Some((key, _)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            match seen.get(&key.to_ascii_uppercase()) {
                Some(first) if first != key => {
                    let line_no = u32::try_from(i + 1).unwrap_or(u32::MAX);
                    self.push_issue(CaseIssue {
                        id: format!("env_case_duplicate:{rel}:{line_no}"),
                        category: IssueCategory::EnvCaseDuplicate,
                        kind: RefKind::Env,
                        file: rel.to_owned(),
                        line: line_no,
                        column: 1,
                        reference: key.to_owned(),
                        actual: first.clone(),
                        target: None,
                        expected: None,
                        ambiguous: false,
                        detail: None,
                        fixes: Vec::new(),
                        fix: None,
                        fixed: false,
                        internal: None,
                    });
                }
                Some(_) => {}
                None => {
                    seen.insert(key.to_ascii_uppercase(), key.to_owned());
                }
            }
        }
    }

    fn check_ref(&mut self, file_abs: &Path, rel: &str, lines: &Lines<'_>, raw: RawRef) {
        self.references_checked += 1;
        let segs = resolve::split(&raw.text, raw.seps.chars());
        let bases = self.bases(file_abs, &raw.bases);
        let candidates = raw.expand.candidates();
        let mut first: Option<(Found, Candidate)> = None;
        for base in &bases {
            for cand in &candidates {
                match resolve::lookup(&mut self.index, base, &segs, cand, raw.dir_ok) {
                    Lookup::Exact(path) => {
                        *self.exact_targets.entry(path).or_default() += 1;
                        return;
                    }
                    Lookup::Insensitive(found) => {
                        if first.is_none() {
                            first = Some((found, cand.clone()));
                        }
                    }
                    Lookup::Missing => {}
                }
            }
        }
        let Some((found, cand)) = first else {
            return;
        };
        let corrected = resolve::corrected_text(&raw.text, &segs, &cand, &found);
        let (line, column, line_text, line_offset) = lines.at(raw.span.start);
        let target = rel_of(&self.root, &found.target);
        let expected = rel_of(&self.root, &found.expected);
        let mut fixes = Vec::new();
        if self.can_rename(&found.target) {
            fixes.push(FixStrategy::Rename);
        }
        if corrected.is_some() {
            fixes.push(FixStrategy::Edit);
        }
        self.push_issue(CaseIssue {
            id: format!("{rel}:{line}:{column}"),
            category: IssueCategory::Mismatch,
            kind: raw.kind,
            file: rel.to_owned(),
            line,
            column,
            reference: raw.text.clone(),
            actual: corrected.clone().unwrap_or_else(|| target.clone()),
            target: Some(target),
            expected: Some(expected),
            ambiguous: found.ambiguous,
            detail: None,
            fixes,
            fix: None,
            fixed: false,
            internal: Some(MismatchDetail {
                file_abs: file_abs.to_path_buf(),
                span: Some(raw.span),
                corrected,
                target_abs: found.target,
                expected_abs: found.expected,
                line_text,
                line_offset,
            }),
        });
    }

    /// Renaming is offered only for files of the project itself.
    fn can_rename(&self, target: &Path) -> bool {
        target.strip_prefix(&self.root).is_ok_and(|rel| {
            !rel.as_os_str().is_empty() && !in_ignored(rel, &self.opts.ignore_dirs)
        })
    }

    fn check_psr4(&mut self, file_abs: &Path, rel: &str, lines: &Lines<'_>, text: &str) {
        let Some(root) = self
            .psr4
            .iter()
            .filter(|r| file_abs.starts_with(&r.dir))
            .max_by_key(|r| r.dir.components().count())
            .cloned()
        else {
            return;
        };
        let Some((namespace, class, at)) = refs::php_declaration(text) else {
            return;
        };
        let rest = if root.prefix.is_empty() {
            namespace.as_str()
        } else if namespace == root.prefix {
            ""
        } else if let Some(rest) = namespace.strip_prefix(&format!("{}\\", root.prefix)) {
            rest
        } else {
            return;
        };
        let mut expected = root.dir.clone();
        for part in rest.split('\\').filter(|p| !p.is_empty()) {
            expected.push(part);
        }
        expected.push(format!("{class}.php"));
        if expected == file_abs {
            return;
        }
        let actual_rel = rel_of(&self.root, file_abs);
        let expected_rel = rel_of(&self.root, &expected);
        if fold_case(&actual_rel) != fold_case(&expected_rel) {
            // Declared somewhere else entirely: not a case problem.
            return;
        }
        let (line, column, line_text, line_offset) = lines.at(at);
        let fixes = if self.can_rename(file_abs) {
            vec![FixStrategy::Rename]
        } else {
            Vec::new()
        };
        self.push_issue(CaseIssue {
            id: format!("{rel}:{line}:{column}"),
            category: IssueCategory::Mismatch,
            kind: RefKind::Psr4,
            file: rel.to_owned(),
            line,
            column,
            reference: expected_rel.clone(),
            actual: actual_rel.clone(),
            target: Some(actual_rel),
            expected: Some(expected_rel),
            ambiguous: false,
            detail: Some(if namespace.is_empty() {
                class.clone()
            } else {
                format!("{namespace}\\{class}")
            }),
            fixes,
            fix: None,
            fixed: false,
            internal: Some(MismatchDetail {
                file_abs: file_abs.to_path_buf(),
                span: None,
                corrected: None,
                target_abs: file_abs.to_path_buf(),
                expected_abs: expected,
                line_text,
                line_offset,
            }),
        });
    }

    fn finish(mut self) -> LintReport {
        self.issues.sort_by(|a, b| {
            (&a.file, a.line, a.column, a.category).cmp(&(&b.file, b.line, b.column, b.category))
        });
        self.issues.dedup_by(|a, b| a.id == b.id);
        LintReport {
            v: LINT_CASE_VERSION,
            root: posix_string(&self.root),
            files_scanned: self.files_scanned,
            references_checked: self.references_checked,
            issues: self.issues,
            fix: false,
            dry_run: false,
            remaining: None,
            exact_targets: self.exact_targets,
        }
    }
}

/// References of a `composer.json` or `package.json` (folders allowed).
fn manifest_refs(kind: FileKind, text: &str) -> Vec<RawRef> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let (paths, ref_kind, expand) = match kind {
        FileKind::Composer => (
            refs::composer_paths(&json),
            RefKind::Composer,
            refs::Expand::Exact,
        ),
        _ => (
            refs::package_paths(&json),
            RefKind::PackageJson,
            refs::Expand::Js,
        ),
    };
    refs::locate_strings(text, &paths)
        .into_iter()
        .map(|(value, span)| RawRef {
            kind: ref_kind,
            span,
            text: value,
            bases: vec![Base::FileDir],
            seps: refs::Seps::Slash,
            expand,
            dir_ok: true,
        })
        .collect()
}

/// Scans the project of `opts`, calling `progress` with the number of files read and
/// polling `cancel`. Blocking (file I/O): async callers use `spawn_blocking`.
pub fn scan(
    opts: &LintOptions,
    progress: &mut dyn FnMut(u32),
    cancel: &dyn Fn() -> bool,
) -> Result<LintReport> {
    let mut scanner = Scanner::new(opts)?;
    scanner.walk(progress, cancel)?;
    Ok(scanner.finish())
}

/// [`scan`] without progress or cancellation.
pub fn lint(opts: &LintOptions) -> Result<LintReport> {
    scan(opts, &mut |_| {}, &|| false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_names() {
        assert_eq!(reserved_reason("aux.php"), Some("reserved_device"));
        assert_eq!(reserved_reason("CON"), Some("reserved_device"));
        assert_eq!(reserved_reason("com1.txt"), Some("reserved_device"));
        assert_eq!(reserved_reason("com10.txt"), None);
        assert_eq!(reserved_reason("console.php"), None);
        assert_eq!(reserved_reason("a:b"), Some("illegal_character"));
        assert_eq!(reserved_reason("name."), Some("trailing_dot_or_space"));
        assert_eq!(reserved_reason("auxiliary.php"), None);
    }

    #[test]
    fn line_and_column() {
        let text = "a\r\nbé c\nd";
        let lines = Lines::new(text);
        let off = text.find('c').unwrap();
        let (line, col, line_text, in_line) = lines.at(off);
        assert_eq!((line, col), (2, 4));
        assert_eq!(line_text, "bé c");
        assert_eq!(&line_text[in_line..], "c");
        assert_eq!(lines.at(0).0, 1);
        assert_eq!(lines.at(text.len() - 1).0, 3);
    }

    #[test]
    fn project_dir_from_document_root() {
        let t = tempfile::tempdir().unwrap();
        let proj = t.path().join("loja");
        std::fs::create_dir_all(proj.join("public")).unwrap();
        std::fs::write(proj.join("composer.json"), "{}").unwrap();
        assert_eq!(project_dir_for_root(&proj.join("public")), proj);
        let bare = t.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(project_dir_for_root(&bare), bare);
    }
}
