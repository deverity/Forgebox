//! The real registry (`registry/` at the repository root) must lint clean: the seven rules of
//! docs/04 section 6, templates from `crates/forge-render/templates/`, and `index.yaml`
//! consistent with the manifest files (ids, kinds and sha256).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use forge_manifest::lint::{LintContext, RegistryDir, lint_registry};
use forge_manifest::schema::Manifest;
use forge_manifest::{load_file, validate};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Template paths (`mysql/my.ini.j2`) under `dir`, as the xtask lists them.
fn templates(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let relative = path.strip_prefix(base).unwrap();
                let parts: Vec<_> = relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.push(parts.join("/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn context() -> LintContext {
    let dir = repo_root().join("crates/forge-render/templates");
    LintContext::new(templates(&dir), Vec::new())
}

#[test]
fn the_real_registry_has_zero_lint_issues() {
    let registry = repo_root().join("registry");
    let report = lint_registry(&RegistryDir::new(&registry), &context());
    let shown: Vec<String> = report.issues.iter().map(ToString::to_string).collect();
    assert!(report.issues.is_empty(), "{}", shown.join("\n"));
    // 12 tools/extensions + 4 stacks + index.yaml.
    assert_eq!(report.files_checked, 17);
}

#[test]
fn the_initial_catalog_is_complete() {
    let registry = repo_root().join("registry");
    for id in [
        "php",
        "apache",
        "nginx",
        "caddy",
        "composer",
        "nodejs",
        "mysql",
        "mariadb",
        "postgresql",
        "memurai",
        "mailpit",
        "xdebug",
    ] {
        let path = registry.join(format!("tools/{id}.yaml"));
        let manifest = load_file(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(validate(&manifest).is_empty(), "{id}");
        let expected = if id == "xdebug" { "Extension" } else { "Tool" };
        assert_eq!(manifest.kind().to_string(), expected, "{id}");
    }
    for id in ["laravel", "wordpress", "cakephp", "nextjs"] {
        let path = registry.join(format!("stacks/{id}.yaml"));
        let manifest = load_file(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(matches!(manifest, Manifest::Stack(_)), "{id}");
    }
}

/// Every curated release carries a real-looking hash (not a placeholder) and an https URL.
#[test]
fn curated_releases_have_plausible_hashes() {
    let registry = repo_root().join("registry/tools");
    for entry in std::fs::read_dir(registry).unwrap() {
        let path = entry.unwrap().path();
        let releases = match load_file(&path).unwrap() {
            Manifest::Tool(m) => m.spec.source.releases,
            Manifest::Extension(m) => m.spec.source.releases,
            _ => continue,
        };
        for release in releases {
            let sha = release.sha256.expect("curated release has a sha256");
            let digits: BTreeSet<char> = sha.as_str().chars().collect();
            assert!(
                digits.len() > 4,
                "{}: placeholder-like hash {sha}",
                path.display()
            );
            if let Some(url) = release.url {
                assert!(url.starts_with("https://"), "{}: {url}", path.display());
            }
        }
    }
}
