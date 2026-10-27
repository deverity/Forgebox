//! Development tasks for Forgebox, run as `cargo xtask <command>`.

use std::path::{Path, PathBuf};
use std::process::Command as Process;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use forge_manifest::lint::{LintContext, RegistryDir};

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "Forgebox development tasks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate JSON Schema for manifests (registry/schema).
    Schema,
    /// Generate TypeScript types from the IPC contracts.
    GenTs,
    /// Lint the manifests in registry/.
    LintRegistry,
    /// Run fmt-check, clippy, test, deny and lint-registry; stops at the first failure.
    Ci,
}

/// Repository root (parent of the xtask crate).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives inside the repo root")
        .to_path_buf()
}

/// Writes `content` creating parent folders; always LF, UTF-8 without BOM.
fn write_file(path: &Path, content: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))
}

fn schema(root: &Path) -> Result<()> {
    let dir = root.join("registry/schema");
    for (name, schema) in forge_manifest::schema::json_schemas() {
        let mut text = serde_json::to_string_pretty(&schema)?;
        text.push('\n');
        write_file(&dir.join(&name), &text)?;
        println!("wrote registry/schema/{name}");
    }
    Ok(())
}

/// Writes `generated.ts` (types) and `methods.ts` (method table) from the IPC contracts.
fn gen_ts(root: &Path) -> Result<()> {
    for (path, content) in forge_ipc::api::contracts::generated_files()? {
        write_file(&root.join(path), &content)?;
        println!("wrote {path}");
    }
    Ok(())
}

/// Template paths (relative, `/` separators) under `dir`, recursively, sorted.
fn template_files(dir: &Path) -> Result<Vec<String>> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
        for entry in std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                walk(base, &path, out)?;
            } else if let Ok(relative) = path.strip_prefix(base) {
                let parts: Vec<String> = relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.push(parts.join("/"));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    if dir.is_dir() {
        walk(dir, dir, &mut out)?;
    }
    out.sort();
    Ok(out)
}

/// Lint context of the official registry: templates embedded by forge-render (read from
/// `crates/forge-render/templates/`), default port ranges of forge-core.
fn lint_context(root: &Path) -> Result<LintContext> {
    let templates = template_files(&root.join("crates/forge-render/templates"))?;
    Ok(LintContext::new(templates, Vec::new()))
}

/// Returns the number of issues found.
fn lint_registry(root: &Path) -> Result<usize> {
    let registry = root.join("registry");
    if !registry.is_dir() {
        println!("registry/ not found, nothing to lint");
        return Ok(0);
    }
    let ctx = lint_context(root)?;
    let report = forge_manifest::lint::lint_registry(&RegistryDir::new(&registry), &ctx);
    for issue in &report.issues {
        let shown = issue.path.strip_prefix(root).unwrap_or(&issue.path);
        let mut issue = issue.clone();
        issue.path = shown.to_path_buf();
        println!("{issue}");
    }
    let per_rule: Vec<String> = report
        .count_by_rule()
        .iter()
        .map(|(rule, n)| format!("rule {rule}: {n}"))
        .collect();
    println!(
        "linted {} file(s), {} issue(s){}",
        report.files_checked,
        report.issues.len(),
        if per_rule.is_empty() {
            String::new()
        } else {
            format!(" ({})", per_rule.join(", "))
        }
    );
    Ok(report.issues.len())
}

fn run_step(root: &Path, program: &str, args: &[&str]) -> Result<()> {
    println!("==> {program} {}", args.join(" "));
    let status = Process::new(program)
        .args(args)
        .current_dir(root)
        .status()
        .with_context(|| format!("spawning {program}"))?;
    if !status.success() {
        bail!("`{program} {}` failed with {status}", args.join(" "));
    }
    Ok(())
}

fn ci(root: &Path) -> Result<()> {
    run_step(root, "cargo", &["fmt", "--all", "--", "--check"])?;
    run_step(
        root,
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    run_step(root, "cargo", &["test", "--workspace"])?;
    run_step(root, "cargo", &["deny", "check"])?;
    println!("==> lint-registry");
    if lint_registry(root)? > 0 {
        bail!("lint-registry found issues");
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = repo_root();
    match cli.command {
        Command::Schema => schema(&root),
        Command::GenTs => gen_ts(&root),
        Command::LintRegistry => {
            if lint_registry(&root)? > 0 {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Ci => ci(&root),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_subcommands() {
        for arg in ["schema", "gen-ts", "lint-registry", "ci"] {
            assert!(Cli::try_parse_from(["xtask", arg]).is_ok(), "{arg}");
        }
    }

    #[test]
    fn rejects_unknown_subcommand() {
        assert!(Cli::try_parse_from(["xtask", "nope"]).is_err());
    }

    #[test]
    fn schema_writes_five_files() {
        let dir = tempfile::tempdir().unwrap();
        schema(dir.path()).unwrap();
        for name in [
            "tool.v1.json",
            "extension.v1.json",
            "stack.v1.json",
            "project.v1.json",
            "registry.v1.json",
        ] {
            let text =
                std::fs::read_to_string(dir.path().join("registry/schema").join(name)).unwrap();
            assert!(text.ends_with("}\n"));
            serde_json::from_str::<serde_json::Value>(&text).unwrap();
        }
    }

    #[test]
    fn gen_ts_writes_header_and_types() {
        use forge_ipc::api::contracts::TS_HEADER;
        let dir = tempfile::tempdir().unwrap();
        gen_ts(dir.path()).unwrap();
        let text =
            std::fs::read_to_string(dir.path().join("apps/desktop/src/ipc/generated.ts")).unwrap();
        assert!(text.starts_with(TS_HEADER));
        assert!(text.contains("ApiVersion") && text.contains("RequestId"));
        assert!(!text.contains('\r'));
        let methods =
            std::fs::read_to_string(dir.path().join("apps/desktop/src/ipc/methods.ts")).unwrap();
        assert!(methods.starts_with(TS_HEADER));
        assert!(methods.contains("\"sites.add\""));
        assert!(!methods.contains('\r'));
    }

    #[test]
    fn lint_without_registry_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(lint_registry(dir.path()).unwrap(), 0);
    }

    /// A minimal valid tool manifest with id `id`, plus `extra` lines under `spec:`.
    fn tool_yaml(id: &str, extra: &str) -> String {
        format!(
            r#"apiVersion: forgebox.dev/v1
kind: Tool
metadata: {{ id: {id}, name: Test, category: devtool }}
spec:
  platforms: [windows-x64]
  source:
    providers:
      - type: nodejs-dist
{extra}"#
        )
    }

    #[test]
    fn lint_counts_issues() {
        let dir = tempfile::tempdir().unwrap();
        let tools = dir.path().join("registry/tools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::write(tools.join("ok.yaml"), tool_yaml("ok", "")).unwrap();
        std::fs::write(tools.join("bad.yaml"), "kind: Nope\n").unwrap();
        assert_eq!(lint_registry(dir.path()).unwrap(), 1);
    }

    #[test]
    fn lint_uses_forge_render_templates() {
        let dir = tempfile::tempdir().unwrap();
        let templates = dir.path().join("crates/forge-render/templates/php");
        std::fs::create_dir_all(&templates).unwrap();
        std::fs::write(templates.join("php.ini.j2"), "x").unwrap();
        let tools = dir.path().join("registry/tools");
        std::fs::create_dir_all(&tools).unwrap();
        let extra = r#"  install:
    post_install:
      - render: { template: php/php.ini.j2, to: "{{ paths.etc }}/t.ini" }
"#;
        std::fs::write(tools.join("t.yaml"), tool_yaml("t", extra)).unwrap();
        assert_eq!(lint_registry(dir.path()).unwrap(), 0);
        std::fs::remove_file(templates.join("php.ini.j2")).unwrap();
        assert_eq!(lint_registry(dir.path()).unwrap(), 1);
        assert_eq!(
            template_files(&dir.path().join("crates/forge-render/templates")).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn template_files_are_relative_with_slashes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/b/c.j2"), "x").unwrap();
        std::fs::write(dir.path().join("top.j2"), "x").unwrap();
        assert_eq!(template_files(dir.path()).unwrap(), ["a/b/c.j2", "top.j2"]);
        assert!(
            template_files(&dir.path().join("missing"))
                .unwrap()
                .is_empty()
        );
    }
}
