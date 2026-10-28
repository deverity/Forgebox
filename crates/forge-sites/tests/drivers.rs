//! Driver detection and `Site` to `VhostModel` snapshots over minimal project trees
//! (`tests/fixtures/projects/<name>/`, marker files only, no vendor).

use std::path::{Path, PathBuf};

use forge_core::paths::ForgeRoot;
use forge_core::store::sites::{Site, SiteKind};
use forge_render::{Engine, RenderContext, UpstreamModel, UpstreamServer, targets};
use forge_sites::drivers::{self, Confidence};
use forge_sites::{VhostContext, vhost};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("projects")
        .join(name)
}

fn fake_root() -> ForgeRoot {
    ForgeRoot::new("C:/fb-test").unwrap()
}

fn ctx() -> VhostContext {
    VhostContext {
        certs_sites: fake_root().certs_sites(),
        default_php_series: "8.3".to_owned(),
        extra: forge_render::ExtraModel::default(),
        app_ports: Default::default(),
        proxy_options: Default::default(),
    }
}

/// `(fixture, driver id, document root relative to the fixture)`.
const CASES: &[(&str, &str, &str)] = &[
    ("laravel", "laravel", "public"),
    ("symfony", "symfony", "public"),
    ("wordpress", "wordpress", ""),
    ("wordpress-bedrock", "wordpress", "web"),
    ("cakephp", "cakephp", "webroot"),
    ("drupal", "drupal", ""),
    ("drupal-composer", "drupal", "web"),
    ("magento", "magento", "pub"),
    ("generic-php", "generic-php", ""),
    ("generic-php-public", "generic-php", "public"),
    ("generic-php-loose", "generic-php", ""),
    ("static", "static", ""),
    ("spa", "spa", "dist"),
];

#[test]
fn every_fixture_is_detected_with_its_root() {
    for (name, driver, root) in CASES {
        let dir = fixture(name);
        let found = drivers::detect(&dir, None).unwrap();
        assert_eq!(found.driver.id(), *driver, "driver of {name}");
        assert!(!found.overridden);
        let expected = if root.is_empty() {
            dir.clone()
        } else {
            dir.join(root)
        };
        assert_eq!(found.root, expected, "root of {name}");
        assert_eq!(found.driver.root(&dir), expected);
    }
}

#[test]
fn every_driver_is_covered_by_a_fixture() {
    for driver in drivers::all() {
        assert!(
            CASES.iter().any(|(_, id, _)| *id == driver.id()),
            "no fixture for {}",
            driver.id()
        );
    }
}

#[test]
fn confidence_breaks_ties_between_frameworks_and_generic_php() {
    // A Laravel tree also has index.php under public/: generic-php matches too, but with
    // lower confidence.
    let dir = fixture("laravel");
    let generic = drivers::by_id("generic-php").unwrap();
    assert_eq!(generic.detect(&dir), Some(Confidence::Medium));
    assert_eq!(
        drivers::by_id("laravel").unwrap().detect(&dir),
        Some(Confidence::High)
    );
    assert_eq!(drivers::detect(&dir, None).unwrap().driver.id(), "laravel");

    // WordPress also has index.php at the root.
    assert_eq!(
        drivers::by_id("generic-php")
            .unwrap()
            .detect(&fixture("wordpress")),
        Some(Confidence::Medium)
    );
    assert_eq!(
        drivers::detect(&fixture("wordpress"), None)
            .unwrap()
            .driver
            .id(),
        "wordpress"
    );

    // A PHP project that ships a dist/ is not an SPA.
    let dir = fixture("laravel-and-spa");
    assert_eq!(drivers::by_id("spa").unwrap().detect(&dir), None);
    assert_eq!(drivers::detect(&dir, None).unwrap().driver.id(), "laravel");

    // A Vite source tree with a built dist/ and an index.html at the root: spa wins.
    let spa = fixture("spa");
    assert_eq!(
        drivers::by_id("static").unwrap().detect(&spa),
        Some(Confidence::Medium)
    );
    assert_eq!(drivers::detect(&spa, None).unwrap().driver.id(), "spa");

    // Only a loose .php file: Low, and still the fallback driver.
    assert_eq!(
        generic.detect(&fixture("generic-php-loose")),
        Some(Confidence::Low)
    );
}

#[test]
fn manual_override_wins_over_detection() {
    let dir = fixture("laravel");
    let forced = drivers::detect(&dir, Some("generic-php")).unwrap();
    assert_eq!(forced.driver.id(), "generic-php");
    assert!(forced.overridden);
    assert_eq!(forced.confidence, None);
    assert_eq!(forced.root, dir.join("public"));
    assert!(drivers::detect(&dir, Some("nope")).is_err());
}

#[test]
fn unrecognized_folder_falls_back_to_generic_php() {
    let dir = fixture("empty");
    let found = drivers::detect(&dir, None).unwrap();
    assert_eq!(found.driver.id(), "generic-php");
    assert_eq!(found.confidence, None);
    assert_eq!(found.root, dir);
}

/// A site as the daemon would store it, with a stable fake location.
fn site_for(name: &str, driver: &str, root_rel: &str) -> Site {
    let host = format!("{name}.test");
    let mut site = Site::new(&host, host.replace('.', "-"));
    let project = format!("C:/fb-test/www/{name}");
    site.root = Some(if root_rel.is_empty() {
        project
    } else {
        format!("{project}/{root_rel}")
    });
    site.driver = Some(driver.to_owned());
    site.kind = drivers::by_id(driver).unwrap().kind();
    site.aliases = vec![format!("admin.{name}.test")];
    site
}

fn model_json(name: &str, driver: &str, root_rel: &str) -> String {
    let site = site_for(name, driver, root_rel);
    let model = vhost::build(&site, &ctx()).unwrap();
    serde_json::to_string_pretty(&model).unwrap()
}

#[test]
fn vhost_snapshots() {
    for (name, driver, root) in CASES {
        insta::assert_snapshot!(format!("vhost__{name}"), model_json(name, driver, root));
    }
}

#[test]
fn vhost_model_follows_the_spec() {
    let mut site = site_for("loja", "laravel", "public");
    site.php.series = Some("8.4".to_owned());
    site.env.insert("APP_ENV".to_owned(), "local".to_owned());
    let model = vhost::build(&site, &ctx()).unwrap();
    assert_eq!(model.hosts, ["loja.test", "*.loja.test", "admin.loja.test"]);
    assert_eq!(model.php_upstream.as_deref(), Some("php_8_4"));
    assert_eq!(model.root, "C:/fb-test/www/loja/public");
    for deny in [r"/\.env", r"/\.git", r"/\.ht"] {
        assert!(model.deny.iter().any(|d| d == deny), "{deny}");
    }
    let ssl = model.ssl.as_ref().unwrap();
    assert_eq!(ssl.cert, "C:/fb-test/certs/sites/loja.test.crt");
    assert_eq!(ssl.key, "C:/fb-test/certs/sites/loja.test.key");
    assert!(model.http_redirect);
    assert_eq!(model.env["APP_ENV"], "local");

    // exact version pins its own pool; no pin uses the default series
    site.php.exact = Some("8.3.36".to_owned());
    let model = vhost::build(&site, &ctx()).unwrap();
    assert_eq!(model.php_upstream.as_deref(), Some("php_8_3_36"));
    site.php = Default::default();
    let model = vhost::build(&site, &ctx()).unwrap();
    assert_eq!(model.php_upstream.as_deref(), Some("php_8_3"));

    // plain HTTP: no ssl, no redirect
    site.secure = false;
    let model = vhost::build(&site, &ctx()).unwrap();
    assert!(model.ssl.is_none());
    assert!(!model.http_redirect);

    // user extras are passed through verbatim
    let mut with_extra = ctx();
    with_extra.extra.nginx = Some("add_header X-Test 1;".to_owned());
    let model = vhost::build(&site, &with_extra).unwrap();
    assert_eq!(model.extra.nginx.as_deref(), Some("add_header X-Test 1;"));
}

#[test]
fn static_and_spa_sites_are_static_kind_without_pool() {
    for (name, driver) in [("static", "static"), ("spa", "spa")] {
        let site = site_for(name, driver, "");
        assert_eq!(site.kind, SiteKind::Static);
        let model = vhost::build(&site, &ctx()).unwrap();
        assert_eq!(model.kind, forge_render::SiteKind::Static);
        assert!(model.php_upstream.is_none());
    }
    let spa = vhost::build(&site_for("spa", "spa", "dist"), &ctx()).unwrap();
    assert_eq!(spa.try_files, ["$uri", "$uri/", "/index.html"]);
    assert_eq!(spa.fallback_resource.as_deref(), Some("/index.html"));
}

#[test]
fn proxy_sites_build_a_proxy_model() {
    let mut site = Site::new("app.test", "app-test");
    site.kind = SiteKind::Proxy;
    site.upstream = Some("http://127.0.0.1:3000/".to_owned());
    let model = vhost::build(&site, &ctx()).unwrap();
    let proxy = model.proxy.unwrap();
    assert_eq!(proxy.upstream, "http://127.0.0.1:3000");
    assert!(proxy.websocket);
}

fn render_site(tool: &str, version: &str, name: &str, driver: &str, root: &str) -> String {
    let model = vhost::build(&site_for(name, driver, root), &ctx()).unwrap();
    let mut rc = RenderContext::new(&fake_root(), tool, version);
    rc.sites = vec![model];
    rc.pools = vec![UpstreamModel::new(
        "php_8_3",
        vec![UpstreamServer {
            host: "127.0.0.1".to_owned(),
            port: 19083,
        }],
    )];
    let files = targets::by_id(tool)
        .unwrap()
        .render(&Engine::embedded_only(), &rc)
        .unwrap();
    let wanted = format!("sites/{name}.test.conf");
    let file = files
        .iter()
        .find(|f| f.path.ends_with(&wanted))
        .unwrap_or_else(|| panic!("{wanted} not rendered"));
    file.new_file_content().unwrap()
}

#[test]
fn rendered_vhost_snapshots() {
    insta::assert_snapshot!(
        "render__nginx__laravel",
        render_site("nginx", "1.30.6", "laravel", "laravel", "public")
    );
    insta::assert_snapshot!(
        "render__apache__laravel",
        render_site("apache", "2.4.69", "laravel", "laravel", "public")
    );
    insta::assert_snapshot!(
        "render__nginx__spa",
        render_site("nginx", "1.30.6", "spa", "spa", "dist")
    );
    insta::assert_snapshot!(
        "render__apache__wordpress",
        render_site("apache", "2.4.69", "wordpress", "wordpress", "")
    );
}

#[test]
fn rendered_configs_keep_the_text_invariants() {
    for text in [
        render_site("nginx", "1.30.6", "wordpress", "wordpress", ""),
        render_site("apache", "2.4.69", "drupal", "drupal", ""),
    ] {
        assert!(!text.contains('\r'));
        assert!(!text.contains("~*"), "case-insensitive regex");
    }
}
