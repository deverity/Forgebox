//! Architecture guard (CLAUDE.md, docs/01 §2, docs/02 §3): checks the workspace dependency
//! graph reported by `cargo metadata` so the layering cannot erode silently.
//!
//! Rules:
//! 1. `forge-cli`, `forge-mcp` and `forgebox-desktop` (the API clients) may depend on no other
//!    workspace crate than `forge-ipc`. Every feature goes through the daemon API.
//! 2. `forgebox-desktop` must not depend directly on `windows` / `windows-sys` / `windows-core`:
//!    Win32 lives in `forge-core::platform::windows` behind traits (ADR-0035). Transitive use by
//!    Tauri/wry/window-vibrancy is fine.
//!
//! Every dependency kind (normal, build, dev) and every `cfg(target)` is checked.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// API clients: the only workspace crate they may use is [`ALLOWED_CLIENT_DEP`].
pub const CLIENT_CRATES: [&str; 3] = ["forge-cli", "forge-mcp", "forgebox-desktop"];
/// The one workspace crate every client may depend on.
pub const ALLOWED_CLIENT_DEP: &str = "forge-ipc";
/// The GUI crate (rule 2).
pub const GUI_CRATE: &str = "forgebox-desktop";
/// Win32 binding crates the GUI must not depend on directly.
pub const FORBIDDEN_GUI_DEPS: [&str; 3] = ["windows", "windows-sys", "windows-core"];

/// Runs `cargo metadata --no-deps` for the workspace at `root`.
pub fn load_metadata(root: &Path) -> Result<Value> {
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .output()
        .context("spawning cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).context("parsing cargo metadata output")
}

/// Violations found in `metadata` (the JSON of `cargo metadata --no-deps`); empty when clean.
pub fn check(metadata: &Value) -> Vec<String> {
    let packages = metadata["packages"].as_array().cloned().unwrap_or_default();
    // With --no-deps every package is a workspace member.
    let workspace: BTreeSet<&str> = packages.iter().filter_map(|p| p["name"].as_str()).collect();
    let mut violations = Vec::new();

    for client in CLIENT_CRATES {
        if !workspace.contains(client) {
            violations.push(format!(
                "`{client}` is not a workspace member; the guard cannot verify it (renamed or removed?)"
            ));
        }
    }

    for package in &packages {
        let Some(name) = package["name"].as_str() else {
            continue;
        };
        let is_client = CLIENT_CRATES.contains(&name);
        for dep in package["dependencies"].as_array().into_iter().flatten() {
            let Some(dep_name) = dep["name"].as_str() else {
                continue;
            };
            let kind = dep["kind"].as_str().unwrap_or("normal");
            if is_client && workspace.contains(dep_name) && dep_name != ALLOWED_CLIENT_DEP {
                violations.push(format!(
                    "`{name}` has a {kind} dependency on workspace crate `{dep_name}`; \
                     clients may depend only on `{ALLOWED_CLIENT_DEP}` (go through the daemon API)"
                ));
            }
            if name == GUI_CRATE && FORBIDDEN_GUI_DEPS.contains(&dep_name) {
                violations.push(format!(
                    "`{name}` has a {kind} dependency on `{dep_name}`; Win32 belongs to \
                     forge-core::platform::windows behind traits (ADR-0035)"
                ));
            }
        }
    }
    violations
}

/// Loads the metadata of the workspace at `root` and fails listing every violation.
pub fn run(root: &Path) -> Result<()> {
    let violations = check(&load_metadata(root)?);
    if violations.is_empty() {
        println!("arch-guard: ok");
        return Ok(());
    }
    for violation in &violations {
        println!("arch-guard: {violation}");
    }
    bail!("arch-guard found {} violation(s)", violations.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn package(name: &str, deps: &[(&str, Option<&str>)]) -> Value {
        let deps: Vec<Value> = deps
            .iter()
            .map(|(dep, kind)| json!({ "name": dep, "kind": kind, "target": null }))
            .collect();
        json!({ "name": name, "dependencies": deps })
    }

    /// A clean graph shaped like the real workspace.
    fn clean() -> Vec<Value> {
        vec![
            package("forge-core", &[("serde", None)]),
            package("forge-ipc", &[("forge-core", None)]),
            package("forge-daemon", &[("forge-core", None), ("forge-ipc", None)]),
            package(
                "forge-cli",
                &[
                    ("forge-ipc", None),
                    ("clap", None),
                    ("tempfile", Some("dev")),
                ],
            ),
            package("forge-mcp", &[("forge-ipc", None)]),
            package(
                "forgebox-desktop",
                &[
                    ("forge-ipc", None),
                    ("tauri", None),
                    ("tauri-build", Some("build")),
                ],
            ),
        ]
    }

    fn metadata(packages: Vec<Value>) -> Value {
        json!({ "packages": packages })
    }

    fn with_extra_dep(package_name: &str, dep: &str, kind: Option<&str>) -> Value {
        let mut packages = clean();
        for p in &mut packages {
            if p["name"] == package_name {
                p["dependencies"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({ "name": dep, "kind": kind, "target": null }));
            }
        }
        metadata(packages)
    }

    #[test]
    fn clean_graph_has_no_violations() {
        assert_eq!(check(&metadata(clean())), Vec::<String>::new());
    }

    #[test]
    fn clients_may_not_use_other_workspace_crates() {
        for client in CLIENT_CRATES {
            for domain in ["forge-core", "forge-daemon"] {
                for kind in [None, Some("dev"), Some("build")] {
                    let found = check(&with_extra_dep(client, domain, kind));
                    assert_eq!(found.len(), 1, "{client} -> {domain} ({kind:?}): {found:?}");
                    assert!(found[0].contains(client) && found[0].contains(domain));
                }
            }
        }
    }

    #[test]
    fn non_client_crates_are_free_to_use_domain_crates() {
        let found = check(&with_extra_dep("forge-ipc", "forge-daemon", None));
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn gui_may_not_depend_on_win32_bindings() {
        for dep in FORBIDDEN_GUI_DEPS {
            let found = check(&with_extra_dep(GUI_CRATE, dep, None));
            assert_eq!(found.len(), 1, "{dep}: {found:?}");
            assert!(found[0].contains(dep));
        }
    }

    #[test]
    fn other_crates_may_use_win32_bindings() {
        let found = check(&with_extra_dep("forge-core", "windows", None));
        assert!(found.is_empty(), "{found:?}");
        let found = check(&with_extra_dep("forge-cli", "windows-sys", None));
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn missing_client_crate_is_reported() {
        let packages: Vec<Value> = clean()
            .into_iter()
            .filter(|p| p["name"] != "forge-mcp")
            .collect();
        let found = check(&metadata(packages));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("forge-mcp"));
    }

    /// The real repository must satisfy its own rules.
    #[test]
    fn workspace_satisfies_the_guard() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let found = check(&load_metadata(root).unwrap());
        assert!(found.is_empty(), "{found:#?}");
    }
}
