//! CLAUDE.md: Win32 lives only in `crates/forge-core/src/platform/windows/`, behind
//! traits. This test scans the production sources of the workspace (every `src/` of the
//! crates and of the desktop shell, `xtask` included) and fails on a direct Win32 binding
//! anywhere else: the `windows`/`windows-sys` crates, the WDK namespaces, `extern
//! "system"` blocks, or the appearance/process calls of M2-E1-T5. It also checks that only
//! forge-core declares a dependency on those crates.
//!
//! Test fixtures (fake service binaries under `tests/`) are out of scope: they stand in
//! for third-party programs, not for Forgebox code.

use std::fs;
use std::path::{Path, PathBuf};

/// Text that means "calls Win32 directly" in a Rust source.
const FORBIDDEN: &[&str] = &[
    "windows::Win32",
    "windows::Wdk",
    "windows_sys",
    "windows-sys",
    "extern \"system\"",
    "windows::core::link!",
    "DwmGetColorizationColor",
    "GetSystemMetrics",
    "SystemParametersInfo",
    "RtlGetVersion",
    "SM_REMOTESESSION",
    "GetProcessTimes",
];

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
        if file.starts_with(&allowed) || file.ends_with("win32_boundary.rs") {
            continue;
        }
        let text = fs::read_to_string(file).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for needle in FORBIDDEN {
                if code.contains(needle) {
                    offenders.push(format!("{}:{}: {needle}", file.display(), n + 1));
                }
            }
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
