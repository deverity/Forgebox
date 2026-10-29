//! `logs[].access` (docs/04 section 1.2): the mapping of the three official web servers and
//! the errors of invalid mappings, with line and column.

mod common;

use common::{TempRegistry, context, fixtures, read};
use forge_manifest::schema::tool::{
    AccessMulti, AccessSpec, AccessUnit, RotateStrategy, ToolManifest,
};
use forge_manifest::{Manifest, load_file, load_str};
use pretty_assertions::assert_eq;

fn registry_tool(id: &str) -> ToolManifest {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../registry/tools")
        .join(format!("{id}.yaml"));
    match load_file(&path).unwrap_or_else(|e| panic!("{id}: {e}")) {
        Manifest::Tool(t) => t,
        other => panic!("{id}: {other:?}"),
    }
}

fn access_of(id: &str) -> AccessSpec {
    let tool = registry_tool(id);
    let daemon = tool.spec.service.daemon.unwrap();
    let mut with_access = daemon.logs.into_iter().filter_map(|l| l.access);
    let access = with_access
        .next()
        .unwrap_or_else(|| panic!("{id}: no access"));
    assert!(with_access.next().is_none(), "{id}: one access log only");
    access
}

#[test]
fn nginx_maps_seconds_sums_upstreams_and_reopens_with_the_service_args() {
    let a = access_of("nginx");
    assert_eq!(
        (a.ts.pointer.as_str(), a.ts.unit, a.ts.start),
        ("/t", AccessUnit::S, false)
    );
    assert_eq!(a.host, "/host");
    assert_eq!(a.status, "/s");
    assert_eq!(
        (a.duration.pointer.as_str(), a.duration.unit),
        ("/rt", AccessUnit::S)
    );
    let up = a.upstream_duration.unwrap();
    assert_eq!(
        (up.pointer.as_str(), up.unit, up.multi),
        ("/ut", AccessUnit::S, AccessMulti::Sum)
    );
    assert_eq!(a.bytes.as_deref(), Some("/b"));
    assert_eq!(a.method.as_deref(), Some("/m"));
    assert_eq!(a.uri.unwrap().pointer, "/u");
    assert_eq!(a.upstream.as_deref(), Some("/up"));
    assert!(!a.lenient_escapes);
    let rotate = a.rotate.unwrap();
    assert_eq!(rotate.strategy, RotateStrategy::RenameReopen);
    assert_eq!((rotate.max_size.as_deref(), rotate.keep), (Some("50MB"), 5));
    let tool = registry_tool("nginx");
    let daemon = tool.spec.service.daemon.unwrap();
    let reopen = rotate.reopen.unwrap().command.unwrap();
    assert!(reopen.contains("-s reopen"), "{reopen}");
    // Same `-p`/`-c` as the reload command, so it reaches the right master process.
    let reload = daemon.reload.unwrap().command.unwrap();
    assert_eq!(reopen.replace("-s reopen", "-s reload"), reload);
}

#[test]
fn apache_maps_ms_start_and_us_duration_with_lenient_escapes() {
    let a = access_of("apache");
    assert_eq!(
        (a.ts.pointer.as_str(), a.ts.unit, a.ts.start),
        ("/t", AccessUnit::Ms, true)
    );
    assert_eq!(a.host, "/host");
    assert_eq!(
        (a.duration.pointer.as_str(), a.duration.unit),
        ("/rt_us", AccessUnit::Us)
    );
    assert!(a.upstream_duration.is_none());
    assert!(a.lenient_escapes);
    // Provisional until M2-E5-T12.
    assert_eq!(a.rotate.unwrap().strategy, RotateStrategy::None);
}

#[test]
fn caddy_maps_native_json_strips_the_query_and_rotates_by_itself() {
    let a = access_of("caddy");
    assert_eq!((a.ts.pointer.as_str(), a.ts.unit), ("/ts", AccessUnit::S));
    assert_eq!(a.host, "/request/host");
    assert_eq!(
        (a.duration.pointer.as_str(), a.duration.unit),
        ("/duration", AccessUnit::S)
    );
    let uri = a.uri.unwrap();
    assert_eq!(
        (uri.pointer.as_str(), uri.strip_query),
        ("/request/uri", true)
    );
    let rotate = a.rotate.unwrap();
    assert_eq!(rotate.strategy, RotateStrategy::SelfRotating);
    assert!(rotate.reopen.is_none());
    // The roll settings the manifest promises live in the template.
    let template = read(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../forge-render/templates/caddy/Caddyfile.j2"),
    );
    assert!(template.contains("roll_size 50MiB"), "{template}");
    assert!(template.contains("roll_keep 5"), "{template}");
}

#[test]
fn access_mapping_survives_a_yaml_round_trip() {
    for id in ["nginx", "apache", "caddy"] {
        let manifest = Manifest::Tool(registry_tool(id));
        let text = forge_manifest::parse::to_yaml(&manifest).unwrap();
        assert_eq!(load_str(&text).unwrap(), manifest, "{id}");
    }
}

/// Lints the invalid fixture and returns `(code, line, column)` of every issue.
fn lint_fixture(name: &str) -> Vec<(String, usize, usize)> {
    let text = read(&fixtures().join(format!("invalid/{name}.yaml")));
    let stem = name.to_owned();
    let registry = TempRegistry::new();
    registry.add_manifest(&stem, &text);
    registry
        .lint(&context())
        .issues
        .iter()
        .map(|i| {
            let at = i
                .location
                .unwrap_or_else(|| panic!("{name}: no location: {i}"));
            (i.code.as_str().to_owned(), at.line, at.column)
        })
        .collect()
}

fn line_of(name: &str, needle: &str) -> usize {
    let text = read(&fixtures().join(format!("invalid/{name}.yaml")));
    text.lines().position(|l| l.contains(needle)).unwrap() + 1
}

#[test]
fn missing_pointer_is_reported_with_line_and_column() {
    let issues = lint_fixture("access-missing-pointer");
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].0, "schema");
    // The position is the `ts` mapping that lacks the pointer.
    assert_eq!(issues[0].1, line_of("access-missing-pointer", "ts:"));
    assert!(issues[0].2 > 1);
}

#[test]
fn unknown_unit_is_reported_with_line_and_column() {
    let issues = lint_fixture("access-unknown-unit");
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].0, "schema");
    assert_eq!(issues[0].1, line_of("access-unknown-unit", "minutes"));
    assert!(issues[0].2 > 1);
}

#[test]
fn rename_reopen_without_reopen_points_at_the_rotate_block() {
    let issues = lint_fixture("access-rename-without-reopen");
    let line = line_of("access-rename-without-reopen", "rotate:");
    assert_eq!(issues, [("invalid_value".to_owned(), line, 13)]);
}

#[test]
fn service_with_an_access_log_must_declare_access() {
    let issues = lint_fixture("access-missing-block");
    let line = line_of("access-missing-block", "- label: Access log");
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].0, "missing_access");
    assert_eq!(issues[0].1, line);
}

#[test]
fn bad_and_overlapping_pointers_are_invalid_values() {
    let issues = lint_fixture("access-bad-pointer");
    let fields: Vec<_> = issues.iter().map(|i| i.0.as_str()).collect();
    // `t` has no leading slash (1); `/request` is a prefix of `/request/status` (flagged once).
    assert_eq!(fields, ["invalid_value"; 2], "{issues:?}");
}
