//! Extraction of file references from source text, by regular expressions (no full
//! parser). Each extractor returns [`RawRef`]s: the written path, its byte range in the
//! file, and how to resolve it (bases, separators, candidates). Heuristics and limits are
//! documented in the module docs of [`super`].

use std::ops::Range;
use std::path::PathBuf;
use std::sync::LazyLock;

use regex::{Captures, Regex};

use super::RefKind;
use super::resolve::Candidate;

/// Where a written path is resolved from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Base {
    /// Folder of the file holding the reference.
    FileDir,
    /// Project root.
    Root,
    /// `<root>/<sub>`.
    Under(&'static str),
    /// Document root(s) of the site (`public/`...).
    DocRoot,
}

/// How the written path is split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seps {
    Slash,
    /// Laravel view names: `partials.nav` or `partials/nav`.
    DotOrSlash,
}

impl Seps {
    pub(crate) fn chars(self) -> &'static [char] {
        match self {
            Self::Slash => &['/'],
            Self::DotOrSlash => &['.', '/'],
        }
    }
}

/// Spellings tried for the last segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Expand {
    Exact,
    /// Node/Vite resolution: as written, with an extension, or a folder with `index`.
    Js,
    /// Sass `@use`/`@import`: as written, with an extension, as a partial (`_name`).
    Sass,
    /// Laravel views: `.blade.php`, `.php`.
    View,
}

const JS_EXTS: [&str; 10] = [
    ".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".vue", ".json", ".svelte", ".mts",
];
const JS_INDEX: [&str; 6] = [
    "index.js",
    "index.ts",
    "index.jsx",
    "index.tsx",
    "index.vue",
    "index.mjs",
];

impl Expand {
    pub(crate) fn candidates(self) -> Vec<Candidate> {
        match self {
            Self::Exact => vec![Candidate::EXACT],
            Self::Js => {
                let mut out = vec![Candidate::EXACT];
                out.extend(JS_EXTS.iter().map(|e| Candidate::suffix(e)));
                out.extend(JS_INDEX.iter().map(|f| Candidate::index(f)));
                out
            }
            Self::Sass => vec![
                Candidate::EXACT,
                Candidate::suffix(".scss"),
                Candidate::suffix(".sass"),
                Candidate::suffix(".css"),
                Candidate::partial(".scss"),
                Candidate::partial(".sass"),
                Candidate::partial(""),
            ],
            Self::View => vec![Candidate::suffix(".blade.php"), Candidate::suffix(".php")],
        }
    }
}

/// A reference as found in the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawRef {
    pub kind: RefKind,
    /// Byte range of the written path in the file.
    pub span: Range<usize>,
    pub text: String,
    pub bases: Vec<Base>,
    pub seps: Seps,
    pub expand: Expand,
    /// The path may name a folder (manifest entries: PSR-4 roots, `files`).
    pub dir_ok: bool,
}

/// What a file is, by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileKind {
    Php,
    Blade,
    Twig,
    Html,
    Js,
    /// Vue/Svelte single-file components: script, template and style.
    Sfc,
    Css,
    Sass,
    Composer,
    PackageJson,
}

/// Kind of the file named `name`, or `None` for files that are not scanned.
pub(crate) fn file_kind(name: &str) -> Option<FileKind> {
    let lower = name.to_ascii_lowercase();
    if lower == "composer.json" {
        return Some(FileKind::Composer);
    }
    if lower == "package.json" {
        return Some(FileKind::PackageJson);
    }
    if lower.ends_with(".blade.php") {
        return Some(FileKind::Blade);
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e)?;
    Some(match ext {
        "php" | "phtml" | "inc" => FileKind::Php,
        "twig" => FileKind::Twig,
        "html" | "htm" => FileKind::Html,
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" | "mts" | "cts" => FileKind::Js,
        "vue" | "svelte" => FileKind::Sfc,
        "css" | "less" => FileKind::Css,
        "scss" | "sass" => FileKind::Sass,
        _ => return None,
    })
}

/// Text of the first non-empty group among `groups` and its byte range.
fn group(caps: &Captures<'_>, groups: &[usize]) -> Option<(String, Range<usize>)> {
    groups
        .iter()
        .find_map(|&g| caps.get(g))
        .map(|m| (m.as_str().to_owned(), m.range()))
}

fn push(
    out: &mut Vec<RawRef>,
    kind: RefKind,
    found: Option<(String, Range<usize>)>,
    bases: Vec<Base>,
    seps: Seps,
    expand: Expand,
) {
    if let Some((text, span)) = found {
        if !text.trim().is_empty() {
            out.push(RawRef {
                kind,
                span,
                text,
                bases,
                seps,
                expand,
                dir_ok: false,
            });
        }
    }
}

macro_rules! re {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($pattern).expect(stringify!($name)));
    };
}

// PHP string literal without interpolation or escapes, as group(s) 'a' | "b".
macro_rules! php_str {
    () => {
        r#"(?:'([^'\\\r\n]*)'|"([^"\\$\{\r\n]*)")"#
    };
}

re!(
    PHP_INCLUDE,
    concat!(
        r"(?i)\b(?:require|include)(?:_once)?\b\s*\(?\s*",
        r"(__DIR__\s*\.\s*|dirname\s*\(\s*__FILE__\s*\)\s*\.\s*)?",
        php_str!(),
        r"\s*\)?\s*;"
    )
);
re!(
    PHP_HELPER,
    concat!(
        r"\b(base_path|app_path|resource_path|public_path|storage_path|database_path|config_path|lang_path)\s*\(\s*",
        php_str!(),
        r"\s*\)"
    )
);
re!(
    PHP_VIEW,
    concat!(
        r"(@extends|@include|@includeIf|@component|@each|\bview|\bView::make|->view|->markdown)\s*\(\s*",
        php_str!()
    )
);
re!(
    ASSET,
    concat!(
        r"(\bVite::asset|\bsecure_asset|\basset|\bmix)\s*\(\s*",
        php_str!()
    )
);
re!(VITE_CALL, r"(?:@vite|\bvite)\s*\(([^)]*)\)");
re!(QUOTED, r#"'([^'\\\r\n]*)'|"([^"\\\r\n]*)""#);
re!(
    HTML_ATTR,
    r#"(?i)\b(?:src|href|data-src|poster)\s*=\s*(?:"([^"]*)"|'([^']*)')"#
);
re!(
    TWIG_TAG,
    r#"\{%-?\s*(?:include|extends|embed|import|from|use)\s+(?:'([^'\r\n]*)'|"([^"\r\n]*)")"#
);
re!(
    TWIG_FN,
    r#"\{\{-?\s*(?:include|source)\s*\(\s*(?:'([^'\r\n]*)'|"([^"\r\n]*)")"#
);
re!(
    JS_FROM,
    r#"\b(?:import|export)\b[^'"`;]*?\bfrom\s*(?:'([^'\r\n]*)'|"([^"\r\n]*)")"#
);
re!(JS_SIDE, r#"\bimport\s*(?:'([^'\r\n]*)'|"([^"\r\n]*)")"#);
re!(
    JS_CALL,
    r#"\b(?:require|import)\s*\(\s*(?:'([^'\r\n]*)'|"([^"\r\n]*)")\s*\)"#
);
re!(
    CSS_URL,
    r#"(?i)\burl\(\s*(?:'([^'\r\n]*)'|"([^"\r\n]*)"|([^'"()\s]+))\s*\)"#
);
re!(
    CSS_IMPORT,
    r#"@(?:import|use|forward)\s+(?:url\(\s*)?(?:'([^'\r\n]*)'|"([^"\r\n]*)")"#
);

/// A URL-ish value of `src=`/`href=`/`url()` that names a local file: no scheme, no
/// protocol-relative `//`, no template expression. Returns the path part (query and
/// fragment removed) and its length.
fn local_path(value: &str) -> Option<&str> {
    let v = value.trim();
    if v.is_empty()
        || v.starts_with('#')
        || v.starts_with("//")
        || v.starts_with('~')
        || v.contains("://")
        || v.contains("{{")
        || v.contains("{%")
        || v.contains("<?")
        || v.contains('$')
        || v.contains('{')
        || v.contains('}')
        || v.contains('(')
        || v.contains('@')
        || v.contains('\\')
    {
        return None;
    }
    let lower = v.to_ascii_lowercase();
    if ["data:", "mailto:", "tel:", "javascript:", "blob:", "about:"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return None;
    }
    let end = v.find(['?', '#']).unwrap_or(v.len());
    let path = &v[..end];
    (!path.is_empty() && path != "/").then_some(path)
}

/// Turns a URL-ish match into a reference (trimming query/fragment and leading spaces).
fn url_ref(
    out: &mut Vec<RawRef>,
    kind: RefKind,
    found: Option<(String, Range<usize>)>,
    relative_bases: &[Base],
) {
    let Some((value, span)) = found else {
        return;
    };
    let Some(path) = local_path(&value) else {
        return;
    };
    let offset = value.find(path).unwrap_or(0);
    let start = span.start + offset;
    let bases = if path.starts_with('/') {
        vec![Base::DocRoot, Base::Root]
    } else {
        relative_bases.to_vec()
    };
    out.push(RawRef {
        kind,
        span: start..start + path.len(),
        text: path.to_owned(),
        bases,
        seps: Seps::Slash,
        expand: Expand::Exact,
        dir_ok: false,
    });
}

fn php(text: &str, out: &mut Vec<RawRef>) {
    for caps in PHP_INCLUDE.captures_iter(text) {
        let found = group(&caps, &[2, 3]);
        let bases = if caps.get(1).is_some() {
            vec![Base::FileDir]
        } else {
            // include_path is `.` (the entry script's folder) plus the including file's
            // folder: try the file's folder, then the project and the document root.
            vec![Base::FileDir, Base::Root, Base::DocRoot]
        };
        push(
            out,
            RefKind::PhpInclude,
            found,
            bases,
            Seps::Slash,
            Expand::Exact,
        );
    }
    for caps in PHP_HELPER.captures_iter(text) {
        let base = match &caps[1] {
            "app_path" => Base::Under("app"),
            "resource_path" => Base::Under("resources"),
            "public_path" => Base::DocRoot,
            "storage_path" => Base::Under("storage"),
            "database_path" => Base::Under("database"),
            "config_path" => Base::Under("config"),
            "lang_path" => Base::Under("lang"),
            _ => Base::Root,
        };
        push(
            out,
            RefKind::PhpPath,
            group(&caps, &[2, 3]),
            vec![base],
            Seps::Slash,
            Expand::Exact,
        );
    }
    for caps in PHP_VIEW.captures_iter(text) {
        let Some((name, span)) = group(&caps, &[2, 3]) else {
            continue;
        };
        // Namespaced (`mail::message`) and dynamic names are not files of the project.
        if name.contains("::") || name.is_empty() {
            continue;
        }
        let kind = if caps[1].starts_with('@') {
            RefKind::BladeInclude
        } else {
            RefKind::PhpView
        };
        push(
            out,
            kind,
            Some((name, span)),
            vec![Base::Under("resources/views")],
            Seps::DotOrSlash,
            Expand::View,
        );
    }
}

fn assets(text: &str, out: &mut Vec<RawRef>) {
    for caps in ASSET.captures_iter(text) {
        let base = if &caps[1] == "Vite::asset" {
            Base::Root
        } else {
            Base::DocRoot
        };
        let found = group(&caps, &[2, 3]).map(|(t, r)| {
            // asset('/img/x.png') is the same as asset('img/x.png')
            let trimmed = t.trim_start_matches('/');
            let cut = t.len() - trimmed.len();
            (trimmed.to_owned(), r.start + cut..r.end)
        });
        push(
            out,
            RefKind::Asset,
            found,
            vec![base],
            Seps::Slash,
            Expand::Exact,
        );
    }
    for call in VITE_CALL.captures_iter(text) {
        let Some(args) = call.get(1) else { continue };
        for caps in QUOTED.captures_iter(args.as_str()) {
            let found = group(&caps, &[1, 2])
                .map(|(t, r)| (t, args.start() + r.start..args.start() + r.end));
            push(
                out,
                RefKind::Asset,
                found,
                vec![Base::Root],
                Seps::Slash,
                Expand::Exact,
            );
        }
    }
}

fn html(text: &str, out: &mut Vec<RawRef>, template: bool) {
    // In templates a relative URL is relative to the page, usually served from the
    // document root; in plain HTML files it is relative to the file.
    let bases: &[Base] = if template {
        &[Base::DocRoot, Base::FileDir]
    } else {
        &[Base::FileDir, Base::DocRoot]
    };
    for caps in HTML_ATTR.captures_iter(text) {
        url_ref(out, RefKind::Html, group(&caps, &[1, 2]), bases);
    }
}

fn twig(text: &str, out: &mut Vec<RawRef>) {
    for re in [&*TWIG_TAG, &*TWIG_FN] {
        for caps in re.captures_iter(text) {
            let Some((name, span)) = group(&caps, &[1, 2]) else {
                continue;
            };
            if name.starts_with('@') || name.contains('{') {
                continue;
            }
            push(
                out,
                RefKind::TwigInclude,
                Some((name, span)),
                vec![Base::Under("templates"), Base::FileDir],
                Seps::Slash,
                Expand::Exact,
            );
        }
    }
}

fn js(text: &str, out: &mut Vec<RawRef>) {
    for re in [&*JS_FROM, &*JS_SIDE, &*JS_CALL] {
        for caps in re.captures_iter(text) {
            let Some((spec, span)) = group(&caps, &[1, 2]) else {
                continue;
            };
            let (text, span, bases) = if spec.starts_with("./") || spec.starts_with("../") {
                (spec, span, vec![Base::FileDir])
            } else if let Some(rest) = spec.strip_prefix("@/") {
                // The usual Vite/Vue CLI alias.
                (
                    rest.to_owned(),
                    span.start + 2..span.end,
                    vec![Base::Under("resources/js"), Base::Under("src")],
                )
            } else {
                continue;
            };
            if text.contains("${") || text.contains('*') {
                continue;
            }
            push(
                out,
                RefKind::JsImport,
                Some((text, span)),
                bases,
                Seps::Slash,
                Expand::Js,
            );
        }
    }
}

fn css(text: &str, out: &mut Vec<RawRef>, sass: bool) {
    for caps in CSS_URL.captures_iter(text) {
        url_ref(
            out,
            RefKind::Css,
            group(&caps, &[1, 2, 3]),
            &[Base::FileDir],
        );
    }
    for caps in CSS_IMPORT.captures_iter(text) {
        let Some((value, span)) = group(&caps, &[1, 2]) else {
            continue;
        };
        // `@use 'sass:math'`, packages and URLs are not project files.
        if value.contains(':') || local_path(&value).is_none() {
            continue;
        }
        // Sass resolves `@use 'variables'` relative to the file as well.
        let bases = if value.starts_with('/') {
            vec![Base::DocRoot]
        } else {
            vec![Base::FileDir]
        };
        push(
            out,
            RefKind::Css,
            Some((value, span)),
            bases,
            Seps::Slash,
            if sass { Expand::Sass } else { Expand::Exact },
        );
    }
}

/// Every reference of `text`, for a file of `kind`. Duplicates (same span, found by two
/// patterns, like `@import url(...)`) are removed.
pub(crate) fn extract(kind: FileKind, text: &str) -> Vec<RawRef> {
    let mut out = Vec::new();
    match kind {
        FileKind::Php => {
            php(text, &mut out);
            assets(text, &mut out);
            html(text, &mut out, false);
        }
        FileKind::Blade => {
            php(text, &mut out);
            assets(text, &mut out);
            html(text, &mut out, true);
        }
        FileKind::Twig => {
            twig(text, &mut out);
            assets(text, &mut out);
            html(text, &mut out, true);
        }
        FileKind::Html => html(text, &mut out, false),
        FileKind::Js => js(text, &mut out),
        FileKind::Sfc => {
            js(text, &mut out);
            html(text, &mut out, true);
            css(text, &mut out, false);
        }
        FileKind::Css => css(text, &mut out, false),
        FileKind::Sass => css(text, &mut out, true),
        // JSON manifests are handled by `super::manifests`.
        FileKind::Composer | FileKind::PackageJson => {}
    }
    out.sort_by_key(|r| (r.span.start, r.span.end));
    out.dedup_by_key(|r| r.span.start);
    out
}

/// Paths of a parsed manifest with their byte ranges in the text (JSON strings that are
/// written without escapes; others cannot be located and are skipped).
pub(crate) fn locate_strings(text: &str, values: &[String]) -> Vec<(String, Range<usize>)> {
    let mut out = Vec::new();
    let mut used: Vec<usize> = Vec::new();
    for value in values {
        let needle = format!("\"{value}\"");
        let mut from = 0;
        while let Some(pos) = text[from..].find(&needle) {
            let start = from + pos + 1;
            from = start;
            if used.contains(&start) {
                continue;
            }
            used.push(start);
            out.push((value.clone(), start..start + value.len()));
            break;
        }
    }
    out
}

/// Strings under the PSR-4/classmap/files sections and `bin` of a `composer.json`.
pub(crate) fn composer_paths(json: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    for section in ["autoload", "autoload-dev"] {
        let Some(sec) = json.get(section) else {
            continue;
        };
        if let Some(map) = sec.get("psr-4").and_then(|v| v.as_object()) {
            for dirs in map.values() {
                strings(dirs, &mut out);
            }
        }
        for key in ["classmap", "files"] {
            if let Some(v) = sec.get(key) {
                strings(v, &mut out);
            }
        }
    }
    if let Some(bin) = json.get("bin") {
        strings(bin, &mut out);
    }
    out.retain(|p| !p.is_empty() && !p.contains('*'));
    out
}

/// File paths of a `package.json`: `main`, `module`, `types`, `typings`, `browser`
/// (string form), `bin`, `files` (without globs) and the relative targets of `exports`.
pub(crate) fn package_paths(json: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    for key in ["main", "module", "types", "typings", "browser"] {
        if let Some(s) = json.get(key).and_then(|v| v.as_str()) {
            out.push(s.to_owned());
        }
    }
    if let Some(bin) = json.get("bin") {
        strings(bin, &mut out);
    }
    if let Some(files) = json.get("files") {
        strings(files, &mut out);
    }
    if let Some(exports) = json.get("exports") {
        let mut all = Vec::new();
        strings(exports, &mut all);
        out.extend(all.into_iter().filter(|s| s.starts_with("./")));
    }
    out.retain(|p| !p.is_empty() && !p.contains('*') && !p.contains("://"));
    out
}

/// Every string inside `v` (string, array or object values, recursively).
fn strings(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|i| strings(i, out)),
        serde_json::Value::Object(map) => map.values().for_each(|i| strings(i, out)),
        _ => {}
    }
}

/// `composer.json` PSR-4 roots: `(namespace prefix, [dirs])` of `autoload` and
/// `autoload-dev`.
pub(crate) fn psr4_roots(json: &serde_json::Value) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for section in ["autoload", "autoload-dev"] {
        let Some(map) = json
            .get(section)
            .and_then(|s| s.get("psr-4"))
            .and_then(|v| v.as_object())
        else {
            continue;
        };
        for (prefix, dirs) in map {
            let mut list = Vec::new();
            strings(dirs, &mut list);
            out.push((prefix.clone(), list));
        }
    }
    out
}

re!(
    PHP_NAMESPACE,
    r"(?m)^\s*namespace\s+([A-Za-z_\\][A-Za-z0-9_\\]*)\s*[;{]"
);
re!(
    PHP_CLASS,
    r"(?m)^[ \t]*(?:(?:abstract|final|readonly)\s+)*(?:class|interface|trait|enum)\s+([A-Za-z_][A-Za-z0-9_]*)"
);

/// Namespace and first class-like declaration of a PHP file, with the byte offset of
/// the class name.
pub(crate) fn php_declaration(text: &str) -> Option<(String, String, usize)> {
    let class = PHP_CLASS.captures(text)?.get(1)?;
    let namespace = PHP_NAMESPACE
        .captures(text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim_matches('\\').to_owned())
        .unwrap_or_default();
    Some((namespace, class.as_str().to_owned(), class.start()))
}

/// Base folders (relative to the project root) used by [`Base::Under`].
pub(crate) fn under(root: &std::path::Path, sub: &str) -> PathBuf {
    sub.split('/').fold(root.to_path_buf(), |p, s| p.join(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(kind: FileKind, src: &str) -> Vec<(RefKind, String)> {
        extract(kind, src)
            .into_iter()
            .map(|r| {
                assert_eq!(&src[r.span.clone()], r.text, "span must point at the text");
                (r.kind, r.text)
            })
            .collect()
    }

    #[test]
    fn php_includes_helpers_and_views() {
        let src = r#"<?php
require_once __DIR__ . '/../Views/header.php';
include 'partials/Menu.php';
require "config/$env.php";
include_once dirname(__FILE__) . "/lib/A.php";
$x = base_path('Views/header.php');
return view('Pages.Home', []);
echo view('mail::message');
"#;
        assert_eq!(
            texts(FileKind::Php, src),
            vec![
                (RefKind::PhpInclude, "/../Views/header.php".into()),
                (RefKind::PhpInclude, "partials/Menu.php".into()),
                (RefKind::PhpInclude, "/lib/A.php".into()),
                (RefKind::PhpPath, "Views/header.php".into()),
                (RefKind::PhpView, "Pages.Home".into()),
            ]
        );
    }

    #[test]
    fn blade_assets_directives_and_html() {
        let src = r##"@extends('layouts.App')
@include('partials.nav', ['a' => 1])
<img src="{{ asset('img/Logo.png') }}">
<link href="/css/App.css?v=3" rel="stylesheet">
<a href="https://example.com/x.png">x</a>
<a href="#top">top</a>
@vite(['resources/css/app.css', 'resources/js/App.js'])
<script src="js/main.js"></script>
"##;
        assert_eq!(
            texts(FileKind::Blade, src),
            vec![
                (RefKind::BladeInclude, "layouts.App".into()),
                (RefKind::BladeInclude, "partials.nav".into()),
                (RefKind::Asset, "img/Logo.png".into()),
                (RefKind::Html, "/css/App.css".into()),
                (RefKind::Asset, "resources/css/app.css".into()),
                (RefKind::Asset, "resources/js/App.js".into()),
                (RefKind::Html, "js/main.js".into()),
            ]
        );
    }

    #[test]
    fn js_imports() {
        let src = r#"import Header from './Header';
import { a,
  b } from "../lib/Util.js";
import './styles/App.css';
export * from './Types';
const x = require('./Config');
const y = await import('./Lazy.vue');
import vue from 'vue';
import Comp from '@/components/Comp.vue';
const t = import(`./pages/${name}.js`);
"#;
        assert_eq!(
            texts(FileKind::Js, src),
            vec![
                (RefKind::JsImport, "./Header".into()),
                (RefKind::JsImport, "../lib/Util.js".into()),
                (RefKind::JsImport, "./styles/App.css".into()),
                (RefKind::JsImport, "./Types".into()),
                (RefKind::JsImport, "./Config".into()),
                (RefKind::JsImport, "./Lazy.vue".into()),
                (RefKind::JsImport, "components/Comp.vue".into()),
            ]
        );
    }

    #[test]
    fn css_urls_and_imports() {
        let src = r#"@import url("Base.css");
@use 'Variables';
@use 'sass:math';
.a { background: url(../img/Bg.png) }
.b { background: url('data:image/png;base64,xx') }
.c { src: url("/fonts/Inter.woff2#iefix") }
"#;
        assert_eq!(
            texts(FileKind::Sass, src),
            vec![
                (RefKind::Css, "Base.css".into()),
                (RefKind::Css, "Variables".into()),
                (RefKind::Css, "../img/Bg.png".into()),
                (RefKind::Css, "/fonts/Inter.woff2".into()),
            ]
        );
    }

    #[test]
    fn twig_templates() {
        let src = r#"{% extends 'Base.html.twig' %}
{% include "partials/_Nav.html.twig" %}
{% include '@Bundle/x.html.twig' %}
{{ include('blocks/Card.html.twig') }}
<img src="{{ asset('images/Logo.png') }}">
"#;
        assert_eq!(
            texts(FileKind::Twig, src),
            vec![
                (RefKind::TwigInclude, "Base.html.twig".into()),
                (RefKind::TwigInclude, "partials/_Nav.html.twig".into()),
                (RefKind::TwigInclude, "blocks/Card.html.twig".into()),
                (RefKind::Asset, "images/Logo.png".into()),
            ]
        );
    }

    #[test]
    fn file_kinds() {
        assert_eq!(file_kind("welcome.blade.php"), Some(FileKind::Blade));
        assert_eq!(file_kind("Home.PHP"), Some(FileKind::Php));
        assert_eq!(file_kind("App.vue"), Some(FileKind::Sfc));
        assert_eq!(file_kind("composer.json"), Some(FileKind::Composer));
        assert_eq!(file_kind("tsconfig.json"), None);
        assert_eq!(file_kind("README"), None);
    }

    #[test]
    fn manifest_paths() {
        let composer: serde_json::Value = serde_json::from_str(
            r#"{"autoload":{"psr-4":{"App\\":"app/","Lib\\":["src/","lib/"]},"files":["helpers.php"],"classmap":["database/Seeds"]},
               "autoload-dev":{"psr-4":{"Tests\\":"tests/"}},"bin":["bin/Tool"]}"#,
        )
        .unwrap();
        assert_eq!(
            composer_paths(&composer),
            [
                "app/",
                "src/",
                "lib/",
                "database/Seeds",
                "helpers.php",
                "tests/",
                "bin/Tool"
            ]
        );
        let roots = psr4_roots(&composer);
        assert_eq!(roots[0], ("App\\".to_owned(), vec!["app/".to_owned()]));
        let package: serde_json::Value = serde_json::from_str(
            r#"{"main":"dist/Index.js","bin":{"x":"bin/X.js"},"files":["dist","*.md"],"exports":{".":{"import":"./dist/Index.mjs"},"./pkg":"pkg"}}"#,
        )
        .unwrap();
        assert_eq!(
            package_paths(&package),
            ["dist/Index.js", "bin/X.js", "dist", "./dist/Index.mjs"]
        );
        let text = r#"{"main": "a.js", "x": "a.js"}"#;
        let spans = locate_strings(text, &["a.js".to_owned(), "a.js".to_owned()]);
        assert_eq!(spans.len(), 2);
        assert_ne!(spans[0].1, spans[1].1);
    }

    #[test]
    fn php_declarations() {
        let src =
            "<?php\n\nnamespace App\\Models;\n\nuse X;\n\nfinal class User extends Model\n{\n}\n";
        let (ns, class, at) = php_declaration(src).unwrap();
        assert_eq!((ns.as_str(), class.as_str()), ("App\\Models", "User"));
        assert_eq!(&src[at..at + 4], "User");
        assert!(php_declaration("<?php echo 'class Foo';").is_none());
    }
}
