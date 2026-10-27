//! Development tasks for Forgebox, run as `cargo xtask <command>`.

use std::path::{Path, PathBuf};
use std::process::Command as Process;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use forge_core::Timestamp;
use forge_manifest::Manifest;
use forge_manifest::index::{build_entries, build_index, render_index};
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
    /// Regenerate registry/index.yaml (sha256 of every manifest, placeholder signature).
    RegistryIndex {
        /// Registry folder (default: registry/ of the repository).
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Registry name written in the index (default: official, or the folder name with --dir).
        #[arg(long)]
        name: Option<String>,
    },
    /// Sign index.yaml with a minisign secret key (fills spec.signature).
    RegistrySign {
        /// Secret key file (unencrypted, or protected by FORGEBOX_REGISTRY_KEY_PASSWORD).
        #[arg(long)]
        key: PathBuf,
        /// Registry folder (default: registry/ of the repository).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Generate a development minisign key pair (never overwrites).
    RegistryKeygen {
        /// Where to write the secret key (outside the repository).
        #[arg(long)]
        secret: PathBuf,
        /// Where to write the public key (registry/keys/dev.pub).
        #[arg(long)]
        public: PathBuf,
    },
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
    let signature = signature_issue(&registry);
    if let Some(problem) = &signature {
        println!("[signature] {problem}");
    }
    let issues = report.issues.len() + usize::from(signature.is_some());
    println!(
        "linted {} file(s), {} issue(s){}",
        report.files_checked,
        issues,
        if per_rule.is_empty() {
            String::new()
        } else {
            format!(" ({})", per_rule.join(", "))
        }
    );
    Ok(issues)
}

/// Name and URL written in `metadata` of the official index.
const REGISTRY_NAME: &str = "official";
const REGISTRY_URL: &str = "https://github.com/forgebox/registry";

/// Regenerates `registry/index.yaml`. `updated_at` only moves when the listed entries change,
/// so running it twice leaves no diff. Returns whether the file was (re)written.
fn registry_index(root: &Path) -> Result<bool> {
    registry_index_at(&root.join("registry"), REGISTRY_NAME, Some(REGISTRY_URL))
}

/// [`registry_index`] for any registry folder (fixtures, third-party registries).
fn registry_index_at(registry: &Path, name: &str, url: Option<&str>) -> Result<bool> {
    let dir = RegistryDir::new(registry);
    let (tools, stacks) = build_entries(&dir).context("indexing the registry")?;
    let path = dir.index_path();
    let previous = forge_manifest::load_file(&path).ok().and_then(|m| match m {
        Manifest::Registry(r) => Some(r),
        _ => None,
    });
    let unchanged = previous.as_ref().filter(|r| {
        r.spec.tools == tools
            && r.spec.stacks == stacks
            && r.metadata.name == name
            && r.metadata.url.as_deref() == url
    });
    if let Some(previous) = unchanged {
        let text = render_index(previous)?;
        if std::fs::read_to_string(&path).is_ok_and(|current| current == text) {
            println!("{} is up to date", path.display());
            return Ok(false);
        }
    }
    let updated_at = match unchanged {
        Some(r) => r.spec.updated_at,
        None => Timestamp::from_second(Timestamp::now().as_second())?,
    };
    let index = build_index(&dir, name, url, updated_at)?;
    write_file(&path, &render_index(&index)?)?;
    println!(
        "wrote {} ({} tool(s), {} stack(s))",
        path.display(),
        index.spec.tools.len(),
        index.spec.stacks.len()
    );
    Ok(true)
}

/// Environment variable with the password of an encrypted registry secret key.
const KEY_PASSWORD_ENV: &str = "FORGEBOX_REGISTRY_KEY_PASSWORD";

/// Signs `<registry>/index.yaml` with the minisign secret key at `key_path`, filling
/// `spec.signature`, and checks the result with the loader's own verification.
fn registry_sign(registry: &Path, key_path: &Path) -> Result<()> {
    let key_text = std::fs::read_to_string(key_path)
        .with_context(|| format!("reading {}", key_path.display()))?;
    let key_box = minisign::SecretKeyBox::from_string(&key_text)
        .map_err(|e| anyhow::anyhow!("unreadable secret key: {e}"))?;
    let secret = match std::env::var(KEY_PASSWORD_ENV) {
        Ok(password) => key_box.into_secret_key(Some(password)),
        Err(_) => key_box.into_unencrypted_secret_key(),
    }
    .map_err(|e| anyhow::anyhow!("cannot unlock the secret key: {e}"))?;
    let public = minisign::PublicKey::from_secret_key(&secret)
        .map_err(|e| anyhow::anyhow!("deriving the public key: {e}"))?;
    let public_b64 = public.to_base64();
    let key_id = forge_registry::index::key_id_of_public_key(&public_b64)?;

    let path = RegistryDir::new(registry).index_path();
    let Manifest::Registry(mut index) =
        forge_manifest::load_file(&path).with_context(|| format!("loading {}", path.display()))?
    else {
        bail!("{} is not a Registry manifest", path.display());
    };
    index.spec.signature = None;
    let payload = forge_registry::index::signing_payload(&index)?;
    let trusted = format!(
        "forgebox registry {} updated_at {}",
        index.metadata.name, index.spec.updated_at
    );
    let signature = minisign::sign(
        Some(&public),
        &secret,
        std::io::Cursor::new(payload),
        Some(&trusted),
        Some("signature from forgebox registry-sign"),
    )
    .map_err(|e| anyhow::anyhow!("signing: {e}"))?;
    index.spec.signature = Some(forge_manifest::schema::registry::Signature {
        kind: forge_manifest::schema::registry::SignatureType::Minisign,
        key_id: key_id.clone(),
        sig: forge_registry::index::encode_signature(&signature.into_string()),
    });
    // Never write an index the loader would refuse.
    forge_registry::index::verify_parsed(
        &index,
        &forge_registry::index::VerifyOptions {
            public_key: Some(public_b64),
            trusted_unsigned: false,
        },
    )?;
    write_file(&path, &render_index(&index)?)?;
    println!("signed {} with key {key_id}", path.display());
    Ok(())
}

/// Writes a new unencrypted development key pair. Refuses to overwrite either file.
fn registry_keygen(secret_path: &Path, public_path: &Path) -> Result<()> {
    for path in [secret_path, public_path] {
        if path.exists() {
            bail!("{} already exists, refusing to overwrite", path.display());
        }
    }
    let pair = minisign::KeyPair::generate_unencrypted_keypair()
        .map_err(|e| anyhow::anyhow!("generating the key pair: {e}"))?;
    let secret = pair
        .sk
        .to_box(Some("forgebox registry dev secret key"))
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .into_string();
    let public = pair
        .pk
        .to_box()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .into_string();
    write_file(secret_path, &secret)?;
    write_file(public_path, &public)?;
    println!(
        "wrote {} and {} (key {})",
        secret_path.display(),
        public_path.display(),
        forge_registry::index::key_id_of_public_key(&pair.pk.to_base64())?
    );
    Ok(())
}

/// Checks the signature of `registry/index.yaml` against `registry/keys/dev.pub` when that
/// key exists (docs/04 §6). Returns the problem found, if any.
fn signature_issue(registry: &Path) -> Option<String> {
    let key_path = registry.join("keys/dev.pub");
    let key = std::fs::read_to_string(&key_path).ok()?;
    let index_path = RegistryDir::new(registry).index_path();
    let bytes = match std::fs::read(&index_path) {
        Ok(bytes) => bytes,
        Err(e) => return Some(format!("{}: cannot read: {e}", index_path.display())),
    };
    let options = forge_registry::index::VerifyOptions {
        public_key: Some(key),
        trusted_unsigned: false,
    };
    forge_registry::index::verify_index(bytes, &options)
        .err()
        .map(|e| {
            format!(
                "{}: {e} (run `cargo xtask registry-sign`)",
                index_path.display()
            )
        })
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
        Command::RegistryIndex { dir, name } => match dir {
            None => registry_index(&root).map(|_| ()),
            Some(dir) => {
                let name = name.unwrap_or_else(|| {
                    dir.file_name()
                        .map_or_else(|| "local".to_owned(), |n| n.to_string_lossy().into_owned())
                });
                registry_index_at(&dir, &name, None).map(|_| ())
            }
        },
        Command::RegistrySign { key, dir } => {
            registry_sign(&dir.unwrap_or_else(|| root.join("registry")), &key)
        }
        Command::RegistryKeygen { secret, public } => registry_keygen(&secret, &public),

        Command::Ci => ci(&root),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_subcommands() {
        for arg in ["schema", "gen-ts", "lint-registry", "registry-index", "ci"] {
            assert!(Cli::try_parse_from(["xtask", arg]).is_ok(), "{arg}");
        }
        assert!(Cli::try_parse_from(["xtask", "registry-sign", "--key", "k"]).is_ok());
        assert!(Cli::try_parse_from(["xtask", "registry-sign"]).is_err());
        assert!(
            Cli::try_parse_from(["xtask", "registry-keygen", "--secret", "s", "--public", "p"])
                .is_ok()
        );
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

    /// A registry folder with one tool and one stack, plus the stub icon.
    fn sample_registry(dir: &Path) {
        let tools = dir.join("registry/tools");
        let stacks = dir.join("registry/stacks");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::create_dir_all(&stacks).unwrap();
        let extra = r#"    releases:
      - { version: "1.2.0", url: "https://example.org/a.zip", sha256: "1111111111111111111111111111111111111111111111111111111111111111" }
      - { version: "1.10.0", url: "https://example.org/b.zip", sha256: "2222222222222222222222222222222222222222222222222222222222222222" }
      - { version: "2.0.0-rc1", url: "https://example.org/c.zip", sha256: "3333333333333333333333333333333333333333333333333333333333333333" }
"#;
        let text = format!(
            "apiVersion: forgebox.dev/v1
kind: Tool
metadata: {{ id: zeta, name: Zeta, category: devtool }}
spec:
  platforms: [windows-x64]
  source:
    providers:
      - type: mysql
{extra}"
        );
        std::fs::write(tools.join("zeta.yaml"), text).unwrap();
        std::fs::write(tools.join("alpha.yaml"), tool_yaml("alpha", "")).unwrap();
        std::fs::write(
            stacks.join("s1.yaml"),
            "apiVersion: forgebox.dev/v1
kind: Stack
metadata: { id: s1, name: S1 }
spec:
  tools:
    - { id: alpha }
",
        )
        .unwrap();
    }

    #[test]
    fn registry_index_lists_sorted_entries_with_latest_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        sample_registry(dir.path());
        assert!(registry_index(dir.path()).unwrap());
        let path = dir.path().join("registry/index.yaml");
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.starts_with("# generated by cargo xtask registry-index"));
        assert!(first.contains("key_id: dev"), "{first}");
        let alpha = first.find("id: alpha").unwrap();
        let zeta = first.find("id: zeta").unwrap();
        assert!(alpha < zeta, "{first}");
        assert!(first.contains("latest: 1.10.0"), "{first}");
        assert!(!first.contains("2.0.0-rc1"), "{first}");
        // Same inputs: nothing is rewritten and updated_at does not move.
        assert!(!registry_index(dir.path()).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        // A changed manifest changes its hash in the index.
        let alpha_path = dir.path().join("registry/tools/alpha.yaml");
        let mut text = std::fs::read_to_string(&alpha_path).unwrap();
        text.push_str(
            "# touched
",
        );
        std::fs::write(&alpha_path, text).unwrap();
        assert!(registry_index(dir.path()).unwrap());
        assert_ne!(std::fs::read_to_string(&path).unwrap(), first);
    }

    #[test]
    fn generated_index_passes_the_lint() {
        let dir = tempfile::tempdir().unwrap();
        sample_registry(dir.path());
        registry_index(dir.path()).unwrap();
        assert_eq!(lint_registry(dir.path()).unwrap(), 0);
        // Editing a manifest after indexing is caught by the sha256 check.
        let path = dir.path().join("registry/tools/alpha.yaml");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(
            "# edited
",
        );
        std::fs::write(&path, text).unwrap();
        assert_eq!(lint_registry(dir.path()).unwrap(), 1);
    }
}

#[cfg(test)]
mod signing_tests {
    use super::*;

    /// A registry folder (not a repo root) with one tool, indexed.
    fn indexed(dir: &Path) -> PathBuf {
        let registry = dir.join("reg");
        std::fs::create_dir_all(registry.join("tools")).unwrap();
        std::fs::write(
            registry.join("tools/alpha.yaml"),
            "apiVersion: forgebox.dev/v1
kind: Tool
metadata: { id: alpha, name: Alpha, category: devtool }
spec:
  platforms: [windows-x64]
  source:
    providers:
      - type: nodejs-dist
",
        )
        .unwrap();
        assert!(registry_index_at(&registry, "reg", None).unwrap());
        registry
    }

    #[test]
    fn keygen_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let (secret, public) = (dir.path().join("keys/dev.key"), dir.path().join("dev.pub"));
        registry_keygen(&secret, &public).unwrap();
        assert!(
            std::fs::read_to_string(&public)
                .unwrap()
                .starts_with("untrusted comment")
        );
        assert!(registry_keygen(&secret, &public).is_err());
    }

    #[test]
    fn sign_then_verify_with_the_matching_public_key_only() {
        let dir = tempfile::tempdir().unwrap();
        let (secret, public) = (dir.path().join("dev.key"), dir.path().join("dev.pub"));
        registry_keygen(&secret, &public).unwrap();
        let registry = indexed(dir.path());
        registry_sign(&registry, &secret).unwrap();

        // No keys/dev.pub in the registry yet: lint has nothing to check.
        assert_eq!(signature_issue(&registry), None);
        std::fs::create_dir_all(registry.join("keys")).unwrap();
        std::fs::copy(&public, registry.join("keys/dev.pub")).unwrap();
        assert_eq!(signature_issue(&registry), None);

        // Editing the index after signing breaks the signature.
        let index = registry.join("index.yaml");
        let text = std::fs::read_to_string(&index).unwrap();
        std::fs::write(&index, text.replace("name: reg", "name: other")).unwrap();
        assert!(signature_issue(&registry).is_some());

        // A different key does not verify.
        registry_sign(&registry, &secret).unwrap();
        let other = (dir.path().join("o.key"), dir.path().join("o.pub"));
        registry_keygen(&other.0, &other.1).unwrap();
        std::fs::copy(&other.1, registry.join("keys/dev.pub")).unwrap();
        assert!(signature_issue(&registry).is_some());
    }

    #[test]
    fn an_unsigned_index_fails_the_signature_lint() {
        let dir = tempfile::tempdir().unwrap();
        let (secret, public) = (dir.path().join("dev.key"), dir.path().join("dev.pub"));
        registry_keygen(&secret, &public).unwrap();
        let registry = indexed(dir.path());
        std::fs::create_dir_all(registry.join("keys")).unwrap();
        std::fs::copy(&public, registry.join("keys/dev.pub")).unwrap();
        let problem = signature_issue(&registry).expect("placeholder signature is not enough");
        assert!(problem.contains("registry-sign"), "{problem}");
    }

    #[test]
    fn indexing_again_after_signing_keeps_the_signature() {
        let dir = tempfile::tempdir().unwrap();
        let (secret, public) = (dir.path().join("dev.key"), dir.path().join("dev.pub"));
        registry_keygen(&secret, &public).unwrap();
        let registry = indexed(dir.path());
        registry_sign(&registry, &secret).unwrap();
        let signed = std::fs::read_to_string(registry.join("index.yaml")).unwrap();
        assert!(!registry_index_at(&registry, "reg", None).unwrap());
        assert_eq!(
            std::fs::read_to_string(registry.join("index.yaml")).unwrap(),
            signed
        );
    }
}
