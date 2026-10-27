//! `lint_registry` over whole registry folders (docs/04 §6).

mod common;

use common::{TINY_SVG, TempRegistry, codes, context, fixtures, read, stub_tool};
use forge_manifest::IssueCode;
use forge_manifest::lint::{LintContext, MAX_ICON_BYTES};
use pretty_assertions::assert_eq;

const ZERO_SHA: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The valid fixtures plus stubs for the tools they reference, icons and an index.
fn full_registry() -> TempRegistry {
    let registry = TempRegistry::new();
    for name in ["php", "mysql", "xdebug"] {
        registry.add_manifest(name, &read(&fixtures().join(format!("valid/{name}.yaml"))));
    }
    for stub in ["composer", "nginx", "memurai", "mailpit", "nodejs"] {
        registry.add_manifest(stub, &stub_tool(stub));
    }
    registry.add_manifest("laravel", &read(&fixtures().join("valid/laravel.yaml")));
    registry.add_icons(None);
    write_index(
        &registry,
        &[
            "composer", "mailpit", "memurai", "mysql", "nginx", "nodejs", "php", "xdebug",
        ],
        &["laravel"],
    );
    registry
}

/// The sha256 the index must carry for `relative` (`tools/php.yaml`).
fn sha_of(registry: &TempRegistry, relative: &str) -> String {
    forge_manifest::index::sha256_hex(read(&registry.root().join(relative)).as_bytes())
}

fn write_index(registry: &TempRegistry, tools: &[&str], stacks: &[&str]) {
    let mut text = String::from(
        "apiVersion: forgebox.dev/v1\nkind: Registry\nmetadata: { name: test }\n\
         spec:\n  schema_version: 1\n  updated_at: 2026-10-26T00:00:00Z\n  tools:\n",
    );
    for id in tools {
        text.push_str(&format!(
            "    - {{ id: {id}, path: tools/{id}.yaml, sha256: \"{}\" }}\n",
            sha_of(registry, &format!("tools/{id}.yaml"))
        ));
    }
    text.push_str("  stacks:\n");
    for id in stacks {
        text.push_str(&format!(
            "    - {{ id: {id}, path: stacks/{id}.yaml, sha256: \"{}\" }}\n",
            sha_of(registry, &format!("stacks/{id}.yaml"))
        ));
    }
    registry.write("index.yaml", &text);
}

#[test]
fn full_registry_is_clean() {
    let registry = full_registry();
    let report = registry.lint(&context());
    assert!(report.is_clean(), "{:#?}", report.issues);
    assert_eq!(report.files_checked, 10);
}

#[test]
fn missing_templates_are_found_in_registry_or_extra_roots() {
    let registry = full_registry();
    // Without the embedded list, both php templates are missing (rule 4).
    let report = registry.lint(&LintContext::default());
    assert_eq!(
        codes(&report),
        ["missing_template".to_owned()].into_iter().collect()
    );
    assert_eq!(report.issues.len(), 2);
    assert!(report.issues.iter().all(|i| i.rule == 4));

    // `<registry>/templates/` satisfies rule 4.
    registry.write("templates/php/php.ini.j2", "x");
    let report = registry.lint(&LintContext::default());
    assert_eq!(report.issues.len(), 1, "{:#?}", report.issues);
    assert!(report.issues[0].message.contains("php/xdebug.ini.j2"));

    // And so does an extra template root.
    let extra = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(extra.path().join("php")).unwrap();
    std::fs::write(extra.path().join("php/xdebug.ini.j2"), "x").unwrap();
    let ctx = LintContext::new(Vec::new(), vec![extra.path().to_owned()]);
    assert!(registry.lint(&ctx).is_clean());
}

#[test]
fn index_must_match_the_manifest_files() {
    let registry = full_registry();
    // nodejs not listed, a ghost entry, and an id that does not match its file.
    let mut text = read(&registry.root().join("index.yaml"));
    text = text.replace(
        &format!(
            "    - {{ id: nodejs, path: tools/nodejs.yaml, sha256: \"{}\" }}\n",
            sha_of(&registry, "tools/nodejs.yaml")
        ),
        &format!(
            "    - {{ id: ghost, path: tools/ghost.yaml, sha256: \"{ZERO_SHA}\" }}\n\
             \x20   - {{ id: node, path: tools/nginx.yaml, sha256: \"{}\" }}\n",
            sha_of(&registry, "tools/nginx.yaml")
        ),
    );
    registry.write("index.yaml", &text);
    let report = registry.lint(&context());
    assert_eq!(
        codes(&report),
        ["index_mismatch".to_owned()].into_iter().collect()
    );
    let messages: Vec<String> = report.issues.iter().map(ToString::to_string).collect();
    assert_eq!(report.issues.len(), 3, "{messages:#?}");
    assert!(
        messages
            .iter()
            .any(|m| m.contains("nodejs.yaml") && m.contains("not listed"))
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("ghost.yaml does not exist"))
    );
    assert!(messages.iter().any(|m| m.contains("entry `node`")));
}

#[test]
fn index_sha256_must_match_the_file() {
    let registry = full_registry();
    let nodejs = sha_of(&registry, "tools/nodejs.yaml");
    let text = read(&registry.root().join("index.yaml")).replace(&nodejs, ZERO_SHA);
    registry.write("index.yaml", &text);
    let report = registry.lint(&context());
    assert_eq!(
        codes(&report),
        ["index_mismatch".to_owned()].into_iter().collect()
    );
    assert_eq!(report.issues.len(), 1, "{:#?}", report.issues);
    let message = report.issues[0].to_string();
    assert!(
        message.contains("spec.tools") && message.contains(&nodejs),
        "{message}"
    );

    // Editing a manifest after indexing it is caught the same way.
    let registry = full_registry();
    let path = registry.root().join("tools/nginx.yaml");
    registry.write("tools/nginx.yaml", &format!("{}# edited\n", read(&path)));
    let report = registry.lint(&context());
    assert_eq!(report.issues.len(), 1, "{:#?}", report.issues);
    assert!(report.issues[0].message.contains("sha256"));
}

#[test]
fn dev_signature_may_be_empty_but_a_real_key_may_not() {
    let registry = full_registry();
    let text = read(&registry.root().join("index.yaml"));
    let dev = format!("{text}  signature: {{ type: minisign, key_id: dev, sig: \"\" }}\n");
    registry.write("index.yaml", &dev);
    assert!(registry.lint(&context()).is_clean());
    let real =
        format!("{text}  signature: {{ type: minisign, key_id: RWR0000000000000, sig: \"\" }}\n");
    registry.write("index.yaml", &real);
    let report = registry.lint(&context());
    assert_eq!(report.issues.len(), 1, "{:#?}", report.issues);
    assert_eq!(
        report.issues[0].field.as_deref(),
        Some("spec.signature.sig")
    );
}

#[test]
fn index_with_wrong_kind_or_bad_yaml_is_reported() {
    let registry = full_registry();
    registry.write("index.yaml", &stub_tool("index"));
    let report = registry.lint(&context());
    assert_eq!(
        codes(&report),
        ["misplaced_manifest".to_owned()].into_iter().collect()
    );

    registry.write("index.yaml", "apiVersion: [unclosed\n");
    let report = registry.lint(&context());
    assert_eq!(codes(&report), ["schema".to_owned()].into_iter().collect());
    assert!(report.issues[0].location.is_some());
}

#[test]
fn icons_folder_rules() {
    let registry = full_registry();
    registry.write("icons/logo.png", "png");
    registry.write(
        "icons/evil.svg",
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>",
    );
    registry.write("icons/fake.svg", "not really");
    let big = format!("<svg>{}</svg>", "x".repeat(MAX_ICON_BYTES as usize));
    registry.write("icons/big.svg", &big);
    registry.write("icons/ok.svg", TINY_SVG);
    let report = registry.lint(&context());
    assert_eq!(
        codes(&report),
        ["invalid_icon".to_owned()].into_iter().collect()
    );
    let files: Vec<String> = report
        .issues
        .iter()
        .map(|i| i.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files, ["big.svg", "evil.svg", "fake.svg", "logo.png"]);
    assert!(report.issues.iter().all(|i| i.rule == 7));
}

#[test]
fn referenced_icon_must_exist() {
    let registry = full_registry();
    std::fs::remove_file(registry.root().join("icons/php.svg")).unwrap();
    let report = registry.lint(&context());
    assert_eq!(report.issues.len(), 1, "{:#?}", report.issues);
    assert_eq!(report.issues[0].code, IssueCode::InvalidIcon);
    assert_eq!(report.issues[0].field.as_deref(), Some("metadata.icon"));
}

#[test]
fn cross_references_need_known_ids() {
    let registry = full_registry();
    std::fs::remove_file(registry.root().join("tools/composer.yaml")).unwrap();
    write_index(
        &registry,
        &[
            "mailpit", "memurai", "mysql", "nginx", "nodejs", "php", "xdebug",
        ],
        &["laravel"],
    );
    let report = registry.lint(&context());
    assert_eq!(
        codes(&report),
        ["unknown_tool".to_owned()].into_iter().collect()
    );
    let fields: Vec<_> = report
        .issues
        .iter()
        .map(|i| i.field.clone().unwrap())
        .collect();
    // php suggests composer; the laravel stack uses it.
    assert_eq!(fields, ["spec.suggests[0]", "spec.tools[1].id"]);

    // A stack extension must extend that tool.
    let registry = full_registry();
    let stack = read(&registry.root().join("stacks/laravel.yaml"))
        .replace("{ id: composer }", "{ id: composer, extensions: [xdebug] }");
    registry.write("stacks/laravel.yaml", &stack);
    write_index(
        &registry,
        &[
            "composer", "mailpit", "memurai", "mysql", "nginx", "nodejs", "php", "xdebug",
        ],
        &["laravel"],
    );
    let report = registry.lint(&context());
    assert_eq!(report.issues.len(), 1, "{:#?}", report.issues);
    assert_eq!(
        report.issues[0].field.as_deref(),
        Some("spec.tools[1].extensions[0]")
    );
}

#[test]
fn manifests_must_live_in_the_right_folder_with_their_id_as_name() {
    let registry = full_registry();
    registry.write("stacks/nginx.yaml", &stub_tool("nginx"));
    registry.write("tools/web.yaml", &stub_tool("caddy"));
    let report = registry.lint(&context());
    let found: Vec<(String, String)> = report
        .issues
        .iter()
        .map(|i| {
            (
                i.path.file_name().unwrap().to_string_lossy().into_owned(),
                i.code.as_str().to_owned(),
            )
        })
        .collect();
    assert!(
        found.contains(&("nginx.yaml".into(), "misplaced_manifest".into())),
        "{found:?}"
    );
    assert!(
        found.contains(&("web.yaml".into(), "misplaced_manifest".into())),
        "{found:?}"
    );
    assert!(
        found.contains(&("web.yaml".into(), "index_mismatch".into())),
        "{found:?}"
    );
}

#[test]
fn report_groups_by_file_and_rule() {
    let registry = full_registry();
    registry.write("icons/logo.png", "png");
    registry.write("tools/extra.yaml", &stub_tool("extra"));
    let report = registry.lint(&context());
    let by_file = report.by_file();
    assert_eq!(by_file.len(), 2);
    let by_rule = report.count_by_rule();
    assert_eq!(by_rule.get(&7), Some(&1));
    assert_eq!(by_rule.get(&1), Some(&1));
}
