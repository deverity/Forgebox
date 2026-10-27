//! Procfile 2.0 → `AppSpec` and the vhost of an app site, from the fixture
//! `tests/fixtures/apps/nextjs/forgebox.yaml` (no process is started here).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use forge_core::paths::ForgeRoot;
use forge_manifest::schema::project::ProcessAutostart;
use forge_render::{Engine, RenderContext, SiteKind, UpstreamModel, targets};
use forge_sites::apps::{
    AppEnv, AppPlan, AppSite, PortRequest, RuntimeCatalog, RuntimeTool, processes_of,
};
use forge_sites::proxy::{ProxyOptions, loopback_upstream};
use forge_sites::site::SiteRules;
use forge_sites::{ProjectPlan, drivers::VhostContext, vhost};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("apps")
        .join("nextjs")
}

/// An installation with Node 20.12.2 (fake binaries) and a system folder with `cmd.exe`.
struct Machine {
    _tmp: tempfile::TempDir,
    root: ForgeRoot,
    env: AppEnv,
    system: PathBuf,
}

fn machine() -> Machine {
    let tmp = tempfile::tempdir().unwrap();
    let root = ForgeRoot::new(tmp.path().join("fb")).unwrap();
    let node = root.bin().join("nodejs").join("20.12.2");
    let system = tmp.path().join("System32");
    std::fs::create_dir_all(&node).unwrap();
    std::fs::create_dir_all(&system).unwrap();
    for (dir, file) in [
        (&node, "node.exe"),
        (&node, "npm.cmd"),
        (&system, "cmd.exe"),
    ] {
        std::fs::write(dir.join(file), b"").unwrap();
    }
    let manifest =
        forge_manifest::load_str(include_str!("../../../registry/tools/nodejs.yaml")).unwrap();
    let forge_manifest::Manifest::Tool(tool) = manifest else {
        panic!("nodejs.yaml is not a tool");
    };
    let catalog = RuntimeCatalog::new(root.clone(), [RuntimeTool::from_manifest(&tool)]);
    Machine {
        env: AppEnv {
            root: root.clone(),
            catalog,
            system_path: vec![system.clone()],
        },
        _tmp: tmp,
        root,
        system,
    }
}

#[test]
fn the_fixture_becomes_an_app_site_and_specs_with_the_port_substituted() {
    let m = machine();
    let plan = ProjectPlan::load(&fixture_dir(), &SiteRules::default()).unwrap();
    // A process with `proxy: true` makes the main site an app site rooted at the project.
    assert_eq!(plan.site.kind, forge_core::store::sites::SiteKind::App);
    assert_eq!(plan.site.root.as_deref(), Some(fixture_dir().as_path()));
    assert!(plan.site.driver.is_none() && plan.site.upstream.is_none());
    let names: Vec<_> = plan.processes.keys().map(String::as_str).collect();
    assert_eq!(names, ["queue", "vite", "web"]);
    assert_eq!(plan.processes["web"].autostart, ProcessAutostart::OnVisit);

    let (site, _) = plan.site.clone().into_site(&SiteRules::default()).unwrap();
    assert_eq!(processes_of(&site.processes).unwrap(), plan.processes);
    let app = AppSite::from_site(&site, plan.dir.clone(), plan.runtimes.clone()).unwrap();

    let build = |name: &str| AppPlan::build(&app, name, &app.processes[name], &m.env).unwrap();
    // web: `npm` is npm.cmd (through cmd.exe), runtime declared, port allocated.
    let web_plan = build("web");
    assert_eq!(web_plan.port_request(), PortRequest::Auto);
    let web = web_plan.finish(Some(3000)).unwrap();
    assert_eq!(web.command, "npm run dev -- --port 3000");
    assert!(web.shell);
    assert_eq!(web.exe, m.system.join("cmd.exe"));
    assert_eq!(
        web.env.get("FORGEBOX_COMMAND"),
        Some("npm run dev -- --port 3000")
    );
    assert_eq!(web.env.get("PORT"), Some("3000"));
    assert_eq!(web.env.get("FORGEBOX_SITE"), Some("app.test"));
    assert_eq!(web.env.get("NEXT_TELEMETRY_DISABLED"), Some("1"));
    assert_eq!(
        web.env.get("NEXT_PUBLIC_API"),
        Some("http://127.0.0.1:3000/api")
    );
    let root = m.root.to_posix_string();
    assert_eq!(
        web.env.get("NODE_EXTRA_CA_CERTS"),
        Some(format!("{root}/certs/ca/forgebox-ca.crt").as_str())
    );
    assert_eq!(
        web.env.get("npm_config_prefix"),
        Some(format!("{root}/data/node/20.12.2/npm-global").as_str())
    );
    assert_eq!(web.path, [m.root.bin().join("nodejs").join("20.12.2")]);
    assert_eq!(web.upstream().as_deref(), Some("http://127.0.0.1:3000"));
    assert_eq!(
        web.log_file,
        m.root.logs().join("apps").join("app.test").join("web.log")
    );

    // vite: fixed port, no runtime declared: implied by `node`, version from forgebox.yaml.
    let vite = build("vite");
    assert_eq!(vite.port_request(), PortRequest::Fixed(5173));
    let vite = vite.finish(Some(5173)).unwrap();
    assert_eq!(vite.command, "node vite.js --port 5173");
    assert!(!vite.shell);
    assert_eq!(
        vite.exe,
        m.root.bin().join("nodejs").join("20.12.2").join("node.exe")
    );
    assert_eq!(vite.args, ["vite.js", "--port", "5173"]);
    assert_eq!(vite.runtime.as_ref().unwrap().label(), "nodejs 20.12.2");

    // queue: nothing listens.
    let queue = build("queue");
    assert_eq!(queue.port_request(), PortRequest::None);
    let queue = queue.finish(None).unwrap();
    assert_eq!(queue.port, None);
    assert_eq!(queue.env.get("PORT"), None);
    assert_eq!(queue.autostart, ProcessAutostart::No);
}

fn vhost_ctx(root: &ForgeRoot) -> VhostContext {
    VhostContext {
        certs_sites: root.certs_sites(),
        default_php_series: "8.3".to_owned(),
        extra: forge_render::ExtraModel::default(),
        app_ports: BTreeMap::from([("web".to_owned(), 3000)]),
        proxy_options: BTreeMap::new(),
    }
}

fn render(
    tool: &str,
    version: &str,
    root: &ForgeRoot,
    models: Vec<forge_render::VhostModel>,
) -> BTreeMap<String, String> {
    let mut rc = RenderContext::new(root, tool, version);
    rc.sites = models;
    rc.pools = Vec::<UpstreamModel>::new();
    targets::by_id(tool)
        .unwrap()
        .render(&Engine::embedded_only(), &rc)
        .unwrap()
        .into_iter()
        .map(|f| {
            let content = f.new_file_content().unwrap();
            (f.path, content)
        })
        .collect()
}

#[test]
fn an_app_site_proxies_to_its_port_in_every_server() {
    let m = machine();
    let plan = ProjectPlan::load(&fixture_dir(), &SiteRules::default()).unwrap();
    let (site, _) = plan.site.clone().into_site(&SiteRules::default()).unwrap();
    let ctx = vhost_ctx(&m.root);

    let model = vhost::build(&site, &ctx).unwrap();
    assert_eq!(model.kind, SiteKind::Proxy);
    let proxy = model.proxy.clone().unwrap();
    assert_eq!(proxy.upstream, loopback_upstream(3000));
    assert!(proxy.websocket);
    assert!(
        proxy.start_on_visit,
        "autostart: on-visit shows the starting page on 502"
    );
    assert_eq!(model.hosts, ["app.test", "*.app.test"]);
    assert!(model.ssl.is_some() && model.http_redirect);

    let nginx = render("nginx", "1.30.6", &m.root, vec![model.clone()]);
    let nginx = &nginx["nginx/sites/app.test.conf"];
    assert!(
        nginx.contains("proxy_pass http://127.0.0.1:3000;"),
        "{nginx}"
    );
    assert!(
        nginx.contains("proxy_set_header Upgrade $http_upgrade;"),
        "{nginx}"
    );
    assert!(
        nginx.contains("error_page 502 = /_forgebox/starting;"),
        "{nginx}"
    );
    assert!(nginx.contains("proxy_read_timeout 3600s;"), "{nginx}");
    assert!(!nginx.contains("\r"), "LF only");
    let apache = render("apache", "2.4.69", &m.root, vec![model.clone()]);
    let apache = &apache["apache/sites/app.test.conf"];
    assert!(apache.contains("http://127.0.0.1:3000/"), "{apache}");
    assert!(apache.contains("upgrade=websocket"), "{apache}");
    let caddy = render("caddy", "2.11.7", &m.root, vec![model]);
    let caddy = caddy
        .values()
        .find(|c| c.contains("reverse_proxy"))
        .unwrap();
    assert!(caddy.contains("127.0.0.1:3000"), "{caddy}");

    // The port must be known: an app site cannot be rendered before it is allocated.
    let mut no_ports = vhost_ctx(&m.root);
    no_ports.app_ports.clear();
    assert_eq!(
        vhost::build(&site, &no_ports).unwrap_err().code(),
        "conflict"
    );
}

#[test]
fn fixed_ports_proxy_options_and_extra_proxy_sites() {
    let m = machine();
    let plan = ProjectPlan::load(&fixture_dir(), &SiteRules::default()).unwrap();
    // `proxies[]` of the fixture: websocket off for api.app.test.
    assert_eq!(plan.proxies.len(), 1);
    let (api, _) = plan.proxies[0]
        .clone()
        .into_site(&SiteRules::default())
        .unwrap();
    assert_eq!(api.host, "api.app.test");
    assert!(!plan.proxy_options["api.app.test"].websocket);
    let mut ctx = vhost_ctx(&m.root);
    ctx.proxy_options = plan.proxy_options.clone();
    let model = vhost::build(&api, &ctx).unwrap();
    assert!(!model.proxy.as_ref().unwrap().websocket);
    assert_eq!(
        model.proxy.as_ref().unwrap().upstream,
        "http://127.0.0.1:4000"
    );
    let nginx = render("nginx", "1.30.6", &m.root, vec![model]);
    let nginx = &nginx["nginx/sites/api.app.test.conf"];
    assert!(
        nginx.contains("proxy_pass http://127.0.0.1:4000;"),
        "{nginx}"
    );
    assert!(
        !nginx.contains("Upgrade"),
        "websocket was switched off:\n{nginx}"
    );

    // A fixed port on the proxied process is used as is, with no allocation.
    let mut app_plan = plan.clone();
    app_plan.site.processes.get_mut("web").unwrap().port =
        Some(forge_manifest::schema::project::ProcessPort::Fixed(5555));
    let (site, _) = app_plan.site.into_site(&SiteRules::default()).unwrap();
    let mut empty = vhost_ctx(&m.root);
    empty.app_ports.clear();
    let model = vhost::build(&site, &empty).unwrap();
    assert_eq!(model.proxy.unwrap().upstream, "http://127.0.0.1:5555");
}

#[test]
fn app_sites_need_exactly_one_proxied_process() {
    let plan = ProjectPlan::load(&fixture_dir(), &SiteRules::default()).unwrap();
    let mut none = plan.site.clone();
    none.processes.get_mut("web").unwrap().proxy = false;
    assert_eq!(
        none.into_site(&SiteRules::default()).unwrap_err().code(),
        "config_invalid"
    );
    let mut twice = plan.site.clone();
    twice.processes.get_mut("vite").unwrap().proxy = true;
    assert_eq!(
        twice.into_site(&SiteRules::default()).unwrap_err().code(),
        "config_invalid"
    );
    let mut no_root = plan.site;
    no_root.root = None;
    assert_eq!(
        no_root.into_site(&SiteRules::default()).unwrap_err().code(),
        "invalid_input"
    );
    let _ = ProxyOptions::default();
}
