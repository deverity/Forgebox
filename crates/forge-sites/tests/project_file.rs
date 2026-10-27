//! `forgebox.yaml` loader (docs/04 §4).

use std::path::{Path, PathBuf};

use forge_core::error::ForgeError;
use forge_core::store::sites::SiteXdebug;
use forge_manifest::schema::project::ProcessPort;
use forge_sites::project_file::PROJECT_FILE;
use forge_sites::{ProjectPlan, SiteKind, SiteRules};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("projects")
        .join(name)
}

#[test]
fn loads_the_example_of_the_docs() {
    let dir = fixture("project-file");
    let plan = ProjectPlan::load(&dir, &SiteRules::default()).unwrap();
    assert_eq!(plan.name, "loja");
    assert_eq!(plan.dir, dir);

    let site = &plan.site;
    assert_eq!(site.host, "loja.test");
    assert_eq!(site.aliases, ["admin.loja.test"]);
    assert_eq!(site.driver.as_deref(), Some("laravel"));
    assert_eq!(site.kind, SiteKind::Php);
    assert_eq!(site.root.as_deref(), Some(dir.join("public").as_path()));
    assert_eq!(site.server.as_deref(), Some("nginx"));
    assert_eq!(site.php.series.as_deref(), Some("8.3"));
    assert_eq!(site.php.exact, None);
    assert_eq!(site.xdebug, SiteXdebug::Trigger);
    assert!(site.secure);
    assert!(!site.parked);
    assert_eq!(plan.php_ini["memory_limit"], "1G");
    assert_eq!(plan.php_ini["upload_max_filesize"], "256M");

    let tools: Vec<_> = plan.services.iter().map(|s| s.tool.as_str()).collect();
    assert_eq!(tools, ["mysql", "memurai", "mailpit"]);
    assert_eq!(plan.services[0].series.as_deref(), Some("8.4"));
    assert_eq!(plan.services[0].database.as_deref(), Some("loja"));
    assert_eq!(plan.runtimes.len(), 1);
    assert_eq!(plan.runtimes[0].tool.as_str(), "nodejs");
    assert_eq!(plan.runtimes[0].version.as_deref(), Some("22"));

    let names: Vec<_> = plan.processes.keys().map(String::as_str).collect();
    assert_eq!(names, ["queue", "sched", "vite"]);
    assert_eq!(plan.processes["vite"].port, Some(ProcessPort::Fixed(5173)));
    assert_eq!(plan.processes["queue"].cmd, "php artisan queue:work");

    assert_eq!(plan.proxies.len(), 1);
    let proxy = &plan.proxies[0];
    assert_eq!(proxy.host, "app.loja.test");
    assert_eq!(proxy.kind, SiteKind::Proxy);
    assert_eq!(proxy.upstream.as_deref(), Some("http://127.0.0.1:3000"));
    assert_eq!(
        plan.share.as_ref().unwrap().provider.as_str(),
        "cloudflared"
    );
    assert!(plan.warnings.is_empty());

    // the main site is a valid draft
    let (stored, _) = plan.site.clone().into_site(&SiteRules::default()).unwrap();
    assert_eq!(stored.slug, "loja-test");
}

#[test]
fn defaults_come_from_the_folder() {
    let dir = fixture("project-file-minimal");
    let plan = ProjectPlan::load(&dir, &SiteRules::new(["dev"])).unwrap();
    // folder name + first configured TLD, driver detected, root from the driver
    assert_eq!(plan.site.host, "project-file-minimal.dev");
    assert_eq!(plan.site.driver.as_deref(), Some("generic-php"));
    assert_eq!(plan.site.root.as_deref(), Some(dir.as_path()));
    assert_eq!(plan.name, "Minha Loja");
    assert!(plan.services.is_empty() && plan.processes.is_empty() && plan.proxies.is_empty());
}

fn project(yaml: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(PROJECT_FILE), yaml).unwrap();
    std::fs::write(tmp.path().join("index.php"), "<?php\n").unwrap();
    tmp
}

fn load_err(yaml: &str) -> ForgeError {
    let tmp = project(yaml);
    ProjectPlan::load(tmp.path(), &SiteRules::default()).unwrap_err()
}

const HEAD: &str = "apiVersion: forgebox.dev/v1\nkind: Project\nmetadata: { name: x }\n";

#[test]
fn rejects_bad_input() {
    // missing file
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(
        ProjectPlan::load(tmp.path(), &SiteRules::default()),
        Err(ForgeError::NotFound(_))
    ));
    // uppercase host is a validation issue of the manifest
    assert!(matches!(
        load_err(&format!("{HEAD}spec:\n  site: {{ host: Loja.test }}\n")),
        ForgeError::ConfigInvalid(_)
    ));
    // host outside the configured TLD
    assert!(matches!(
        load_err(&format!("{HEAD}spec:\n  site: {{ host: loja.com }}\n")),
        ForgeError::InvalidInput(_)
    ));
    // root escaping the project
    for root in ["../outside", "/abs", "C:/abs"] {
        let err = load_err(&format!("{HEAD}spec:\n  site: {{ root: \"{root}\" }}\n"));
        assert!(matches!(err, ForgeError::ConfigInvalid(_)), "{root}: {err}");
    }
    // php is a series or an exact version
    assert!(matches!(
        load_err(&format!("{HEAD}spec:\n  site: {{ php: \"8\" }}\n")),
        ForgeError::ConfigInvalid(_)
    ));
    // unknown driver, unknown key, wrong kind
    assert!(matches!(
        load_err(&format!("{HEAD}spec:\n  site: {{ driver: rails }}\n")),
        ForgeError::InvalidInput(_)
    ));
    assert!(matches!(
        load_err(&format!("{HEAD}spec:\n  nope: 1\n")),
        ForgeError::ConfigInvalid(_)
    ));
    assert!(matches!(
        load_err("apiVersion: forgebox.dev/v1\nkind: Registry\n"),
        ForgeError::ConfigInvalid(_)
    ));
}

#[test]
fn exact_php_version_keeps_its_series() {
    let tmp = project(&format!(
        "{HEAD}spec:\n  site: {{ php: \"8.3.36\", driver: static }}\n"
    ));
    let plan = ProjectPlan::load(tmp.path(), &SiteRules::default()).unwrap();
    assert_eq!(plan.site.php.exact.as_deref(), Some("8.3.36"));
    assert_eq!(plan.site.php.series.as_deref(), Some("8.3"));
    assert_eq!(plan.site.kind, SiteKind::Static);
}
