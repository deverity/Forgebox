//! CLAUDE.md: Win32 lives only in `crates/forge-core/src/platform/windows/`, behind
//! traits. This test scans the production sources of the workspace (every `src/` of the
//! crates and of the desktop shell, `xtask` included) and fails on a direct Win32 binding
//! anywhere else: the `windows`/`windows_sys` crate paths, the WDK namespaces, `extern
//! "system"` blocks, the appearance/process calls of M2-E1-T5, the pseudo console calls of
//! M2-E3-T3 or the machine counter and image name calls of M2-E5-T10. It also checks that
//! only forge-core declares a dependency on the `windows`/`windows-sys` crates.
//!
//! The scan looks at Rust *code* only: a small lexer blanks comments and string/char
//! literals first, so data that merely names a crate (for example the forbidden-deps list
//! of `xtask arch-guard`) is not a violation. The hyphenated package name `windows-sys` is
//! not a source needle at all: Rust code can only reach that crate as `windows_sys`. The
//! hyphenated form is checked in the `Cargo.toml` manifests instead.
//!
//! Test fixtures (fake service binaries under `tests/`) are out of scope: they stand in
//! for third-party programs, not for Forgebox code.

use std::fs;
use std::path::{Path, PathBuf};

/// Text that means "calls Win32 directly", matched against Rust code with comments and
/// string/char literals blanked.
const FORBIDDEN_CODE: &[&str] = &[
    "windows::Win32",
    "windows::Wdk",
    "windows_sys",
    "windows::core::link!",
    "DwmGetColorizationColor",
    "GetSystemMetrics",
    "SystemParametersInfo",
    "RtlGetVersion",
    "SM_REMOTESESSION",
    "GetProcessTimes",
    // Pseudo consoles (M2-E3-T3): only platform/windows/conpty.rs.
    "CreatePseudoConsole",
    "ResizePseudoConsole",
    "ClosePseudoConsole",
    // Machine counters and process image names (M2-E5-T10): only
    // platform/windows/system_stats.rs and process_stats.rs.
    "GetSystemTimes",
    "GlobalMemoryStatusEx",
    "GetDiskFreeSpaceEx",
    "QueryFullProcessImageName",
];

/// An ABI name is itself a string literal, so this needle is matched against the code with
/// comments blanked but string contents kept. Inside another string literal the quotes
/// would be escaped (`\"system\"`), so data cannot match it.
const FORBIDDEN_ABI: &str = "extern \"system\"";

/// Two views of a Rust source, both with the same line structure as the input: `code` has
/// comments and string/char literal contents replaced by spaces; `with_strings` has only
/// the comments replaced.
struct Views {
    code: String,
    with_strings: String,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn blank_of(c: char) -> char {
    if c == '\n' { '\n' } else { ' ' }
}

impl Views {
    /// A char of real code: kept in both views.
    fn code(&mut self, c: char) {
        self.code.push(c);
        self.with_strings.push(c);
    }

    /// A char of a comment: blank in both views.
    fn comment(&mut self, c: char) {
        self.code.push(blank_of(c));
        self.with_strings.push(blank_of(c));
    }

    /// A char inside a string/char literal: blank only in `code`.
    fn literal(&mut self, c: char) {
        self.code.push(blank_of(c));
        self.with_strings.push(c);
    }
}

/// Minimal Rust lexer: line and (nested) block comments, `"..."`/`b"..."` strings with
/// escapes, raw strings (`r"..."`, `r#"..."#`, `br"..."`), char literals and lifetimes.
fn split_views(src: &str) -> Views {
    let chars: Vec<char> = src.chars().collect();
    let mut v = Views {
        code: String::with_capacity(src.len()),
        with_strings: String::with_capacity(src.len()),
    };
    let at = |k: usize| chars.get(k).copied();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = at(i + 1);
        let prev_ident = i > 0 && is_ident(chars[i - 1]);

        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                v.comment(chars[i]);
                i += 1;
            }
            continue;
        }

        if c == '/' && next == Some('*') {
            let mut depth = 0usize;
            while i < chars.len() {
                if chars[i] == '/' && at(i + 1) == Some('*') {
                    depth += 1;
                    v.comment('/');
                    v.comment('*');
                    i += 2;
                } else if chars[i] == '*' && at(i + 1) == Some('/') {
                    depth -= 1;
                    v.comment('*');
                    v.comment('/');
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    v.comment(chars[i]);
                    i += 1;
                }
            }
            continue;
        }

        let raw_prefix_end = match (prev_ident, c, next) {
            (false, 'r', _) => Some(i + 1),
            (false, 'b', Some('r')) => Some(i + 2),
            _ => None,
        };
        if let Some(mut j) = raw_prefix_end {
            let mut hashes = 0;
            while at(j) == Some('#') {
                hashes += 1;
                j += 1;
            }
            if at(j) == Some('"') {
                for &p in &chars[i..=j] {
                    v.code(p);
                }
                i = j + 1;
                while i < chars.len() {
                    if chars[i] == '"' && (1..=hashes).all(|k| at(i + k) == Some('#')) {
                        for &p in &chars[i..=i + hashes] {
                            v.code(p);
                        }
                        i += hashes + 1;
                        break;
                    }
                    v.literal(chars[i]);
                    i += 1;
                }
                continue;
            }
        }

        if c == '"' {
            v.code('"');
            i += 1;
            while i < chars.len() {
                match chars[i] {
                    '\\' => {
                        v.literal('\\');
                        if let Some(e) = at(i + 1) {
                            v.literal(e);
                        }
                        i += 2;
                    }
                    '"' => {
                        v.code('"');
                        i += 1;
                        break;
                    }
                    other => {
                        v.literal(other);
                        i += 1;
                    }
                }
            }
            continue;
        }

        if c == '\'' {
            // Char literal ('x', '\n', '\'', '\u{..}') versus lifetime ('a).
            let end = if next == Some('\\') {
                chars[i + 3.min(chars.len() - i)..]
                    .iter()
                    .position(|&x| x == '\'')
                    .map(|p| i + 3 + p)
            } else if at(i + 2) == Some('\'') {
                Some(i + 2)
            } else {
                None
            };
            if let Some(end) = end {
                v.code('\'');
                for &p in &chars[i + 1..end] {
                    v.literal(p);
                }
                v.code('\'');
                i = end + 1;
                continue;
            }
        }

        v.code(c);
        i += 1;
    }
    v
}

/// `(line, needle)` for every Win32 binding in the code of `src`.
fn violations(src: &str) -> Vec<(usize, &'static str)> {
    let views = split_views(src);
    let mut out = Vec::new();
    for (n, (code, with_strings)) in views
        .code
        .lines()
        .zip(views.with_strings.lines())
        .enumerate()
    {
        for needle in FORBIDDEN_CODE {
            if code.contains(needle) {
                out.push((n + 1, *needle));
            }
        }
        if with_strings.contains(FORBIDDEN_ABI) {
            out.push((n + 1, FORBIDDEN_ABI));
        }
    }
    out
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name == "target" || name == "node_modules" || name == "gen" {
            continue;
        }
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `src/` directories of every crate, the desktop shell and xtask.
fn production_sources(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for parent in ["crates", "apps"] {
        if let Ok(entries) = fs::read_dir(root.join(parent)) {
            for entry in entries.flatten() {
                dirs.push(entry.path().join("src"));
                dirs.push(entry.path().join("src-tauri").join("src"));
            }
        }
    }
    dirs.push(root.join("xtask").join("src"));
    let mut files = Vec::new();
    for dir in dirs {
        rust_files(&dir, &mut files);
    }
    files
}

fn manifests(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for parent in ["crates", "apps"] {
        if let Ok(entries) = fs::read_dir(root.join(parent)) {
            for entry in entries.flatten() {
                for manifest in [
                    entry.path().join("Cargo.toml"),
                    entry.path().join("src-tauri").join("Cargo.toml"),
                ] {
                    if manifest.is_file() {
                        out.push(manifest);
                    }
                }
            }
        }
    }
    out.push(root.join("xtask").join("Cargo.toml"));
    out
}

#[test]
fn win32_only_in_platform_windows() {
    let root = workspace_root();
    let allowed = root
        .join("crates")
        .join("forge-core")
        .join("src")
        .join("platform")
        .join("windows");
    let files = production_sources(&root);
    assert!(files.len() > 50, "scanned only {} files", files.len());

    let mut offenders = Vec::new();
    for file in &files {
        if file.starts_with(&allowed) {
            continue;
        }
        let text = fs::read_to_string(file).unwrap();
        for (line, needle) in violations(&text) {
            offenders.push(format!("{}:{line}: {needle}", file.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "Win32 outside forge-core/src/platform/windows:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn only_forge_core_depends_on_the_windows_crates() {
    let root = workspace_root();
    let core = root.join("crates").join("forge-core").join("Cargo.toml");
    let mut offenders = Vec::new();
    for manifest in manifests(&root) {
        if manifest == core {
            continue;
        }
        let text = fs::read_to_string(&manifest).unwrap();
        for (n, line) in text.lines().enumerate() {
            let key = line.split('#').next().unwrap_or("").trim_start();
            if key.starts_with("windows =")
                || key.starts_with("windows-sys")
                || key.starts_with("windows.")
            {
                offenders.push(format!("{}:{}: {line}", manifest.display(), n + 1));
            }
        }
    }
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}

#[test]
fn scanner_flags_win32_code() {
    let src = "use windows::Win32::Foundation::HWND;\n\
               let v = windows_sys::Win32::Foo;\n\
               unsafe extern \"system\" { fn RtlGetVersion(); }\n\
               let m = GetSystemMetrics(SM_REMOTESESSION);\n";
    let found = violations(src);
    assert!(found.contains(&(1, "windows::Win32")), "{found:?}");
    assert!(found.contains(&(2, "windows_sys")), "{found:?}");
    assert!(found.contains(&(3, FORBIDDEN_ABI)), "{found:?}");
    assert!(found.contains(&(3, "RtlGetVersion")), "{found:?}");
    assert!(found.contains(&(4, "GetSystemMetrics")), "{found:?}");
    assert!(found.contains(&(4, "SM_REMOTESESSION")), "{found:?}");
}

#[test]
fn scanner_ignores_data_and_comments() {
    // The lines of `xtask/src/arch_guard.rs` (M2-E1-T1) that name the crates as data, plus
    // comments, multi-line, raw and escaped strings, char literals and lifetimes.
    let src = r##"pub const FORBIDDEN_GUI_DEPS: [&str; 3] = ["windows", "windows-sys", "windows-core"];
        let found = check(&with_extra_dep("forge-cli", "windows-sys", None));
        let msg = "use windows_sys or windows::Win32 via
                   forge-core::platform::windows (GetSystemMetrics) behind traits";
        let raw = r#"windows::Win32 "quoted" RtlGetVersion"#;
        let esc = "a \"windows_sys\" b \\";
        let url = "http://x"; let after = 1;
        let q = '"'; let e = '\''; fn f<'a>(x: &'a str) {}
        // windows_sys::Win32 in a line comment
        /* windows::Win32 /* nested */ GetProcessTimes */
        let s = "extern \"system\"";
"##;
    assert_eq!(violations(src), Vec::new());
}

#[test]
fn scanner_keeps_code_after_strings_and_lifetimes() {
    let src = "let a = \"//not a comment\"; windows_sys::x();\n\
               fn f<'a>(x: &'a str) { GetProcessTimes(); }\n\
               let c = '\\''; RtlGetVersion();\n\
               let d = '\\u{41}'; let r = br#\"x\"#; SystemParametersInfo();\n";
    assert_eq!(
        violations(src),
        vec![
            (1, "windows_sys"),
            (2, "GetProcessTimes"),
            (3, "RtlGetVersion"),
            (4, "SystemParametersInfo"),
        ]
    );
}
