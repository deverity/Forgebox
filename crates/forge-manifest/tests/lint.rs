use std::path::PathBuf;

use forge_manifest::lint::{LintError, lint_file};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn valid_manifest_has_no_issues() {
    assert!(lint_file(&fixture("valid-tool.yaml")).unwrap().is_empty());
}

#[test]
fn invalid_api_version_and_kind_are_reported() {
    let issues = lint_file(&fixture("invalid-kind.yaml")).unwrap();
    assert_eq!(issues.len(), 2);
    assert!(issues.iter().all(|i| (1..=7).contains(&i.rule)));
}

#[test]
fn broken_yaml_is_an_issue() {
    let issues = lint_file(&fixture("broken.yaml")).unwrap();
    assert_eq!(issues.len(), 1);
    assert!(issues[0].message.contains("invalid YAML"));
}

#[test]
fn missing_file_is_an_error() {
    assert!(matches!(
        lint_file(&fixture("nope.yaml")),
        Err(LintError::Io { .. })
    ));
}
