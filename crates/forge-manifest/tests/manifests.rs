//! Valid and invalid fixtures (docs/04), round-trip and defaults.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{TempRegistry, codes, context, fixtures, read, yaml_files};
use forge_manifest::schema::common::{Duration, Kind, RestartOn, VersionScheme};
use forge_manifest::schema::tool::{Layout, ServiceKind, StopSignal, StripRoot, UpdatePolicy};
use forge_manifest::version::{Version, canonical_name};
use forge_manifest::{IssueCode, Manifest, ValidateOptions, load_str, validate, validate_with};
use pretty_assertions::assert_eq;

fn load_fixture(relative: &str) -> Manifest {
    let path = fixtures().join(relative);
    load_str(&read(&path)).unwrap_or_else(|e| panic!("{relative}: {e}"))
}

#[test]
fn valid_fixtures_parse_and_validate_clean() {
    let expected: BTreeMap<&str, Kind> = [
        ("php.yaml", Kind::Tool),
        ("mysql.yaml", Kind::Tool),
        ("xdebug.yaml", Kind::Extension),
        ("laravel.yaml", Kind::Stack),
        ("loja.yaml", Kind::Project),
        ("index.yaml", Kind::Registry),
    ]
    .into_iter()
    .collect();
    let files = yaml_files(&fixtures().join("valid"));
    assert_eq!(files.len(), expected.len());
    let mut kinds = BTreeSet::new();
    for file in files {
        let name = file.file_name().unwrap().to_str().unwrap().to_owned();
        let manifest = load_str(&read(&file)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(manifest.kind(), expected[name.as_str()], "{name}");
        kinds.insert(manifest.kind());
        let issues = validate(&manifest);
        assert!(issues.is_empty(), "{name}: {issues:#?}");
    }
    assert_eq!(kinds.len(), 5, "one valid fixture per kind at least");
}

struct Expectation {
    codes: BTreeSet<String>,
    with: Vec<String>,
    icon_bytes: Option<usize>,
}

fn expectation(text: &str) -> Expectation {
    let mut exp = Expectation {
        codes: BTreeSet::new(),
        with: Vec::new(),
        icon_bytes: None,
    };
    for line in text.lines().take_while(|l| l.starts_with('#')) {
        if let Some(list) = line.strip_prefix("# expect:") {
            exp.codes = list.split(',').map(|c| c.trim().to_owned()).collect();
        } else if let Some(path) = line.strip_prefix("# with:") {
            exp.with.push(path.trim().to_owned());
        } else if let Some(bytes) = line.strip_prefix("# icon-bytes:") {
            exp.icon_bytes = Some(bytes.trim().parse().unwrap());
        }
    }
    assert!(!exp.codes.is_empty(), "fixture without `# expect:`");
    exp
}

/// Issue codes for an invalid fixture: parse error → `schema`; Project/Registry → the
/// validator; Tool/Extension/Stack → the lint of a registry holding the fixture.
fn issue_codes(stem: &str, text: &str, exp: &Expectation) -> BTreeSet<String> {
    let manifest = match load_str(text) {
        Err(_) => return ["schema".to_owned()].into_iter().collect(),
        Ok(m) => m,
    };
    if matches!(manifest.kind(), Kind::Project | Kind::Registry) {
        return validate(&manifest)
            .iter()
            .map(|i| i.code.as_str().to_owned())
            .collect();
    }
    let registry = TempRegistry::new();
    registry.add_manifest(stem, text);
    for companion in &exp.with {
        let path = fixtures().join("invalid").join(companion);
        let companion_stem = path.file_stem().unwrap().to_str().unwrap().to_owned();
        registry.add_manifest(&companion_stem, &read(&path));
    }
    let icon = manifest.icon().map(str::to_owned);
    registry.add_icons(icon.as_deref().zip(exp.icon_bytes));
    codes(&registry.lint(&context()))
}

#[test]
fn invalid_fixtures_fail_with_the_expected_codes() {
    let files = yaml_files(&fixtures().join("invalid"));
    assert!(files.len() >= 10, "at least 10 invalid fixtures");
    let mut rules = BTreeSet::new();
    for file in files {
        let stem = file.file_stem().unwrap().to_str().unwrap().to_owned();
        let text = read(&file);
        let exp = expectation(&text);
        let found = issue_codes(&stem, &text, &exp);
        assert_eq!(found, exp.codes, "{stem}");
        for code in &found {
            rules.insert(code_rule(code));
        }
    }
    assert_eq!(
        rules,
        (1..=7).collect::<BTreeSet<u8>>(),
        "every lint rule has a failing fixture"
    );
}

#[test]
fn install_time_variable_message_lists_what_is_allowed() {
    let text = read(&fixtures().join("invalid/install-instance-variable.yaml"));
    let registry = TempRegistry::new();
    registry.add_manifest("install-instance-variable", &text);
    let report = registry.lint(&context());
    let message = report
        .issues
        .iter()
        .find(|i| i.code.as_str() == "unknown_template_variable")
        .map(ToString::to_string)
        .expect("unknown_template_variable issue");
    assert!(
        message.contains(concat!(
            "is not available at install time ",
            "(allowed in install: version, series, variant.<key>, paths.<dir>)"
        )),
        "{message}"
    );
    assert!(!message.contains("  "), "stray spaces: {message:?}");
}

fn code_rule(code: &str) -> u8 {
    match code {
        "missing_checksum" => 2,
        "unknown_port_range" => 3,
        "missing_template" => 4,
        "unknown_template_variable" | "invalid_template" => 5,
        "run_outside_version"
        | "run_not_allowed"
        | "write_outside_root"
        | "exe_outside_version" => 6,
        "invalid_icon" => 7,
        _ => 1,
    }
}

#[test]
fn schema_errors_carry_line_and_column() {
    let text = read(&fixtures().join("invalid/unknown-nested-field.yaml"));
    let err = load_str(&text).unwrap_err();
    let location = err.location().expect("location");
    let portz_line = text.lines().position(|l| l.contains("portz")).unwrap() + 1;
    assert_eq!(location.line, portz_line, "{err}");
    assert!(location.column > 1, "{err}");
    assert!(err.to_string().contains("portz"), "{err}");

    let text = read(&fixtures().join("invalid/bad-id.yaml"));
    let err = load_str(&text).unwrap_err();
    let id_line = text.lines().position(|l| l.contains("PHP_8")).unwrap() + 1;
    assert_eq!(err.location().unwrap().line, id_line, "{err}");

    // Inside an internally tagged enum (providers) the position is the enclosing node.
    let text = read(&fixtures().join("invalid/unknown-provider-field.yaml"));
    let err = load_str(&text).unwrap_err();
    assert!(err.location().is_some(), "{err}");
    assert!(err.to_string().contains("checksumz"), "{err}");
}

#[test]
fn tool_round_trips_through_yaml_and_json() {
    let manifest = load_fixture("valid/php.yaml");
    let yaml = forge_manifest::parse::to_yaml(&manifest).unwrap();
    assert!(!yaml.contains('\r'));
    assert!(yaml.contains("- copy:"), "{yaml}");
    let again = load_str(&yaml).unwrap_or_else(|e| panic!("{e}\n{yaml}"));
    assert_eq!(again, manifest);

    let json = serde_json::to_value(&manifest).unwrap();
    let from_json: Manifest = serde_json::from_value(json).unwrap();
    assert_eq!(from_json, manifest);

    for other in [
        "valid/mysql.yaml",
        "valid/xdebug.yaml",
        "valid/laravel.yaml",
        "valid/loja.yaml",
        "valid/index.yaml",
    ] {
        let m = load_fixture(other);
        let yaml = forge_manifest::parse::to_yaml(&m).unwrap();
        assert_eq!(load_str(&yaml).unwrap(), m, "{other}");
    }
}

#[test]
fn documented_defaults() {
    let text = "apiVersion: forgebox.dev/v1\nkind: Tool\n\
                metadata: { id: tiny, name: Tiny, category: devtool }\n\
                spec:\n  platforms: [windows-x64]\n  source:\n    providers:\n      - type: nodejs-dist\n\
                \x20 service:\n    kind: daemon\n    daemon:\n      exe: bin/tiny.exe\n";
    let Manifest::Tool(tool) = load_str(text).unwrap() else {
        panic!("not a tool")
    };
    let spec = &tool.spec;
    assert_eq!(spec.versioning.scheme, VersionScheme::Semver);
    assert_eq!(spec.versioning.series_key, "{major}.{minor}");
    assert_eq!(spec.install.archive, None);
    assert_eq!(spec.install.strip_root, StripRoot::Auto);
    assert_eq!(spec.install.layout, Layout::Versioned);
    assert!(spec.install.current_link);
    assert!(spec.install.shims.is_empty() && spec.install.version_files.is_empty());
    assert_eq!(spec.install.project_key, None);
    assert_eq!(spec.update.policy, UpdatePolicy::Notify);
    assert_eq!(spec.update.retention, 1);
    assert_eq!(spec.update.check_interval, Duration::from_secs(6 * 3600));
    assert_eq!(spec.service.kind, ServiceKind::Daemon);
    let daemon = spec.service.daemon.as_ref().unwrap();
    assert!(daemon.console_group);
    assert!(!daemon.multi_instance && !daemon.autostart);
    assert_eq!(daemon.start_timeout, Duration::from_secs(30));
    assert_eq!(daemon.stop.timeout, Duration::from_secs(15));
    assert_eq!(daemon.stop.then, StopSignal::CtrlBreak);
    assert_eq!(daemon.restart_policy.on, RestartOn::Failure);
    assert_eq!(daemon.restart_policy.max, 3);
    assert_eq!(daemon.restart_policy.window, Duration::from_secs(60));
    assert!(validate(&Manifest::Tool(tool.clone())).is_empty());

    let Manifest::Tool(php) = load_fixture("valid/php.yaml") else {
        panic!()
    };
    let pool = php.spec.service.pool.as_ref().unwrap();
    assert_eq!(pool.scaling.min, 2);
    assert_eq!(pool.recycle.max_age, Duration::from_secs(3600));
}

#[test]
fn shims_take_names_or_objects() {
    let text = "apiVersion: forgebox.dev/v1\nkind: Tool\n\
                metadata: { id: composer, name: Composer, category: devtool }\n\
                spec:\n  platforms: [windows-x64]\n  source:\n    providers:\n      - type: nodejs-dist\n\
                \x20 install:\n    shims:\n      - composer-x\n\
                \x20     - { name: composer, file: composer.phar, interpreter: php }\n\
                \x20   version_files: [.composer-version]\n";
    let manifest = load_str(text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert!(validate(&manifest).is_empty(), "{:#?}", validate(&manifest));
    let Manifest::Tool(tool) = &manifest else {
        panic!("not a tool")
    };
    let shims = &tool.spec.install.shims;
    assert_eq!(shims[0].name(), "composer-x");
    assert_eq!(shims[0].file(), None);
    assert_eq!(shims[1].name(), "composer");
    assert_eq!(shims[1].file(), Some("composer.phar"));
    assert_eq!(shims[1].interpreter(), Some("php"));
    let yaml = forge_manifest::parse::to_yaml(&manifest).unwrap();
    assert_eq!(load_str(&yaml).unwrap(), manifest);
}

#[test]
fn canonical_names_from_manifest_variant_order() {
    let Manifest::Tool(php) = load_fixture("valid/php.yaml") else {
        panic!()
    };
    let order = php.spec.versioning.variant_order();
    assert_eq!(order, ["ts", "compiler", "arch"]);
    let release = &php.spec.source.releases[0];
    let version = Version::parse(php.spec.versioning.scheme, &release.version).unwrap();
    assert_eq!(
        version.series(&php.spec.versioning.series_key).unwrap(),
        "8.3"
    );
    assert_eq!(
        canonical_name(&release.version, &release.variants, &order),
        "8.3.37-nts-vs16-x64"
    );
    // Apache: compiler + arch; Nginx: no variants, just the version.
    let apache: BTreeMap<String, String> = [("compiler", "vs18"), ("arch", "x64")]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    assert_eq!(
        canonical_name("2.4.69", &apache, &["compiler", "arch"]),
        "2.4.69-vs18-x64"
    );
    let Manifest::Tool(mysql) = load_fixture("valid/mysql.yaml") else {
        panic!()
    };
    let none = mysql.spec.versioning.variant_order();
    assert_eq!(canonical_name("1.27.3", &BTreeMap::new(), &none), "1.27.3");
}

#[test]
fn post_install_run_sandbox() {
    let base = "apiVersion: forgebox.dev/v1\nkind: Tool\n\
                metadata: { id: db, name: Db, category: database }\n\
                spec:\n  platforms: [windows-x64]\n  source:\n    providers:\n      - type: nodejs-dist\n\
                \x20 install:\n    post_install:\n";
    let ok = format!(
        "{base}      - run: {{ command: \"bin/mysqld.exe --initialize-insecure\" }}\n\
         \x20     - run: {{ command: \"{{{{ paths.version }}}}/bin/mysqld.exe --version\", timeout: 30s }}\n\
         \x20     - run: {{ command: \"\\\"bin/my tool.exe\\\" -v\" }}\n"
    );
    let manifest = load_str(&ok).unwrap_or_else(|e| panic!("{e}\n{ok}"));
    assert!(validate(&manifest).is_empty(), "{:#?}", validate(&manifest));

    let third_party = ValidateOptions {
        third_party: true,
        ..ValidateOptions::default()
    };
    let issues = validate_with(&manifest, &third_party);
    assert_eq!(issues.len(), 3);
    assert!(
        issues
            .iter()
            .all(|i| i.code == IssueCode::RunNotAllowed && i.lint_rule() == 6)
    );
}

#[test]
fn port_ranges_come_from_options() {
    let text = read(&fixtures().join("valid/mysql.yaml"));
    let manifest = load_str(&text).unwrap();
    let options = ValidateOptions {
        port_ranges: vec!["web".to_owned()],
        third_party: false,
    };
    let issues = validate_with(&manifest, &options);
    assert_eq!(issues.len(), 1, "{issues:#?}");
    assert_eq!(issues[0].code, IssueCode::UnknownPortRange);
    assert_eq!(issues[0].field, "spec.service.daemon.ports[0].range");
}

#[test]
fn every_invalid_fixture_has_matching_id_and_stem() {
    // Keeps the lint harness honest: only duplicate-id is allowed to differ.
    for file in yaml_files(&fixtures().join("invalid")) {
        let stem = file.file_stem().unwrap().to_str().unwrap();
        if let Ok(manifest) = load_str(&read(&file)) {
            if let Some(id) = manifest.id() {
                assert!(stem == id.as_str() || stem == "duplicate-id", "{stem}");
            }
        }
    }
}

#[test]
fn unquoted_versions_keep_their_text() {
    // The YAML scalar is read as text, so `8.10` never turns into the number 8.1.
    let text =
        read(&fixtures().join("valid/laravel.yaml")).replace("series: \"8.4\"", "series: 8.10");
    let Manifest::Stack(stack) = load_str(&text).unwrap() else {
        panic!("not a stack")
    };
    assert_eq!(stack.spec.tools[0].series.as_deref(), Some("8.10"));
}

#[test]
fn temp_registry_helper_places_by_kind() {
    let registry = TempRegistry::new();
    assert_eq!(
        registry.add_manifest("x", &common::stub_tool("x")),
        "tools/x.yaml"
    );
    let stack = read(&fixtures().join("valid/laravel.yaml"));
    assert_eq!(
        registry.add_manifest("laravel", &stack),
        "stacks/laravel.yaml"
    );
}
