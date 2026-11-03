//! The official registry (`registry/`, M2-E2-T3): its index is signed with the development key
//! and up to date, exactly four stacks carry the `onboarding` tag in the order the onboarding
//! shows them (docs/07 §5.1), and each of them only references tools that exist.

use std::path::{Path, PathBuf};

use forge_manifest::index::build_entries;
use forge_manifest::lint::RegistryDir;
use forge_manifest::load_file;
use forge_manifest::schema::{Manifest, RegistryManifest, StackManifest};
use forge_registry::index::{DEV_PUBLIC_KEY, SIGNATURE_FILE, VerifyOptions, verify_index};

fn registry_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry")
}

fn official_index() -> RegistryManifest {
    match load_file(&registry_dir().join("index.yaml")).expect("index.yaml parses") {
        Manifest::Registry(index) => index,
        other => panic!("index.yaml is a {}", other.kind()),
    }
}

fn load_stack(path: &str) -> StackManifest {
    match load_file(&registry_dir().join(path)).unwrap_or_else(|e| panic!("{path}: {e}")) {
        Manifest::Stack(stack) => stack,
        other => panic!("{path} is a {}", other.kind()),
    }
}

/// Stacks of the index, in index order, with their manifests.
fn indexed_stacks() -> Vec<(String, StackManifest)> {
    official_index()
        .spec
        .stacks
        .iter()
        .map(|entry| (entry.id.as_str().to_owned(), load_stack(&entry.path)))
        .collect()
}

#[test]
fn the_official_index_verifies_with_the_dev_key() {
    let dir = registry_dir();
    let bytes = std::fs::read(dir.join("index.yaml")).unwrap();
    let signature = std::fs::read(dir.join(SIGNATURE_FILE)).unwrap();
    let options = VerifyOptions {
        public_key: Some(DEV_PUBLIC_KEY.to_owned()),
        trusted_unsigned: false,
    };
    let verified = verify_index(bytes, Some(signature), &options)
        .expect("index.yaml.minisig is a valid signature of index.yaml");
    assert!(verified.verification.is_verified());
}

#[test]
fn the_official_index_matches_the_manifest_files() {
    let (_, stacks) = build_entries(&RegistryDir::new(registry_dir())).unwrap();
    let index = official_index();
    assert_eq!(
        index.spec.stacks, stacks,
        "run `cargo xtask registry-index`"
    );
}

#[test]
fn exactly_four_stacks_are_onboarding_in_the_docs_order() {
    let onboarding: Vec<String> = indexed_stacks()
        .into_iter()
        .filter(|(_, stack)| stack.metadata.tags.iter().any(|t| t == "onboarding"))
        .map(|(id, _)| id)
        .collect();
    assert_eq!(onboarding, ["laravel", "wordpress", "php", "nextjs"]);
}

#[test]
fn every_onboarding_stack_only_references_existing_tools() {
    let index = official_index();
    let tools: Vec<&str> = index.spec.tools.iter().map(|e| e.id.as_str()).collect();
    for (id, stack) in indexed_stacks() {
        if !stack.metadata.tags.iter().any(|t| t == "onboarding") {
            continue;
        }
        for tool in &stack.spec.tools {
            assert!(
                tools.contains(&tool.id.as_str()),
                "stack {id} references missing tool {}",
                tool.id.as_str()
            );
            for extension in &tool.extensions {
                assert!(
                    tools.contains(&extension.as_str()),
                    "stack {id} references missing extension {}",
                    extension.as_str()
                );
            }
        }
    }
}

#[test]
fn the_php_stack_is_the_generic_site_without_a_framework_step() {
    let stack = load_stack("stacks/php.yaml");
    assert_eq!(stack.metadata.name, "PHP genérico");
    let site = stack.spec.site.expect("php stack has a site");
    assert_eq!(site.driver.as_deref(), Some("generic-php"));
    assert_eq!(site.php.as_deref(), Some("8.4"));
    assert!(site.create_with.is_none(), "no framework is created");
    let tools: Vec<&str> = stack.spec.tools.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(tools, ["php", "composer", "nginx", "mysql", "mailpit"]);
}
