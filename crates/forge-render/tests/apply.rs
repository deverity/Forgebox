//! Writing rendered files to a real `etc/`: managed blocks, drift and template overrides.

mod common;

use std::fs;

use forge_core::paths::ForgeRoot;
use forge_render::{Engine, WriteOutcome, targets};

fn render(root: &ForgeRoot, engine: &Engine, version: &str) -> Vec<forge_render::RenderedFile> {
    let ctx = common::web_context(root, "nginx", version);
    targets::nginx::TARGET.render(engine, &ctx).unwrap()
}

#[test]
fn apply_is_idempotent_and_keeps_user_text_outside_the_block() {
    let tmp = tempfile::tempdir().unwrap();
    let root = ForgeRoot::new(tmp.path()).unwrap();
    let etc = root.etc();
    let engine = Engine::for_root(&root);

    let files = render(&root, &engine, "1.30.6");
    let (first, drifts) = targets::apply_all(&files, &etc, false).unwrap();
    assert!(drifts.is_empty());
    assert!(first.iter().all(|(_, o)| *o == WriteOutcome::Written));
    let (second, _) = targets::apply_all(&files, &etc, false).unwrap();
    assert!(second.iter().all(|(_, o)| *o == WriteOutcome::Unchanged));

    // The user adds text after the block and before it.
    let conf = etc.join("nginx").join("nginx.conf");
    let text = fs::read_to_string(&conf).unwrap();
    fs::write(&conf, format!("# mine\n{text}# tail\n")).unwrap();

    // A new tool version changes the block body.
    let files = render(&root, &engine, "1.30.7");
    let (outcomes, drifts) = targets::apply_all(&files, &etc, false).unwrap();
    assert!(drifts.is_empty());
    assert_eq!(outcomes[0].1, WriteOutcome::Written);
    let after = fs::read_to_string(&conf).unwrap();
    assert!(after.starts_with("# mine\n# forgebox:begin main (hash="));
    assert!(after.ends_with("# forgebox:end main\n# tail\n"));
    assert!(after.contains("bin/nginx/1.30.7/conf/mime.types"));
    assert!(!after.contains("1.30.6"));
}

#[test]
fn edit_inside_block_is_reported_kept_and_forced() {
    let tmp = tempfile::tempdir().unwrap();
    let root = ForgeRoot::new(tmp.path()).unwrap();
    let etc = root.etc();
    let engine = Engine::for_root(&root);
    targets::apply_all(&render(&root, &engine, "1.30.6"), &etc, false).unwrap();

    let conf = etc.join("nginx").join("nginx.conf");
    let edited = fs::read_to_string(&conf)
        .unwrap()
        .replace("worker_connections 1024;", "worker_connections 4096;");
    fs::write(&conf, &edited).unwrap();

    let files = render(&root, &engine, "1.30.7");
    let (outcomes, drifts) = targets::apply_all(&files, &etc, false).unwrap();
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].block, "main");
    assert!(drifts[0].file.ends_with("nginx.conf"));
    assert!(matches!(outcomes[0].1, WriteOutcome::DriftKept(_)));
    assert_eq!(fs::read_to_string(&conf).unwrap(), edited, "edit kept");

    // Standalone detection agrees with the write path.
    let found =
        forge_render::drift::detect_in_file(&conf, forge_render::CommentStyle::Hash).unwrap();
    assert_eq!(found, drifts);

    // "Regenerate": force.
    let (_, drifts) = targets::apply_all(&files, &etc, true).unwrap();
    assert!(drifts.is_empty());
    let regenerated = fs::read_to_string(&conf).unwrap();
    assert!(regenerated.contains("worker_connections 1024;"));
    assert!(
        forge_render::drift::detect_in_file(&conf, forge_render::CommentStyle::Hash)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn database_config_appends_block_to_the_initdb_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = ForgeRoot::new(tmp.path()).unwrap();
    let etc = root.etc();
    let conf = etc
        .join("postgresql")
        .join("default")
        .join("postgresql.conf");
    fs::create_dir_all(conf.parent().unwrap()).unwrap();
    fs::write(&conf, "#port = 5432\nmax_connections = 100\n").unwrap();
    let ctx = common::db_context(&root, "postgresql", "17.6", 5432);
    let files = targets::postgresql::TARGET
        .render(&Engine::for_root(&root), &ctx)
        .unwrap();
    targets::apply_all(&files, &etc, false).unwrap();
    let text = fs::read_to_string(&conf).unwrap();
    assert!(text.starts_with("#port = 5432\nmax_connections = 100\n\n# forgebox:begin main"));
}

#[test]
fn override_in_etc_templates_wins_over_embedded() {
    let tmp = tempfile::tempdir().unwrap();
    let root = ForgeRoot::new(tmp.path()).unwrap();
    let override_dir = root.etc_templates().join("nginx").join("snippets");
    fs::create_dir_all(&override_dir).unwrap();
    fs::write(
        override_dir.join("php-common.conf.j2"),
        "fastcgi_read_timeout {{ tool.version }}s;\n",
    )
    .unwrap();
    let files = render(&root, &Engine::for_root(&root), "1.30.6");
    let common = files
        .iter()
        .find(|f| f.path == "nginx/snippets/php-common.conf")
        .unwrap();
    assert_eq!(common.body, "fastcgi_read_timeout 1.30.6s;\n");
    // Everything else is still the embedded template.
    assert!(files.iter().any(|f| f.body.contains("log_format forgebox")));
}
