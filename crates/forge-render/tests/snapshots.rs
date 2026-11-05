//! Snapshot of every template with a fixed context (docs/10 M1-E4-T1): site `loja.test`
//! (Laravel, PHP 8.3, TLS), proxy `app.test` with WebSocket, static `docs.test`, two pools.

mod common;

use forge_core::paths::ForgeRoot;
use forge_render::targets::{self, RenderedFile};
use forge_render::{Engine, RenderContext};

fn root() -> ForgeRoot {
    ForgeRoot::new("C:/fb-test").unwrap()
}

fn render(tool: &str, ctx: &RenderContext) -> Vec<RenderedFile> {
    let target = targets::by_id(tool).unwrap();
    target.render(&Engine::embedded_only(), ctx).unwrap()
}

fn check_invariants(file: &RenderedFile, content: &str) {
    assert!(!content.contains('\r'), "{}: CR", file.path);
    assert!(!content.starts_with('\u{feff}'), "{}: BOM", file.path);
    assert!(content.ends_with('\n'), "{}: no trailing LF", file.path);
    assert!(
        !content.contains("~*"),
        "{}: case-insensitive regex",
        file.path
    );
    // `:\"` is legitimate JSON escaping in the Apache LogFormat; a drive path is `X:\x`.
    assert!(
        !content
            .match_indices(":\\")
            .any(|(i, _)| !content[i + 2..].starts_with('"')),
        "{}: Windows path",
        file.path
    );
    assert!(!content.contains("\\\\"), "{}: double backslash", file.path);
}

fn snapshot_all(tool: &str, ctx: &RenderContext) -> Vec<RenderedFile> {
    let files = render(tool, ctx);
    assert!(!files.is_empty());
    for f in &files {
        let content = f.new_file_content().unwrap();
        check_invariants(f, &content);
        let name = f.path.replace('/', "__");
        insta::assert_snapshot!(name, content);
    }
    files
}

#[test]
fn nginx() {
    let files = snapshot_all("nginx", &common::web_context(&root(), "nginx", "1.30.6"));
    let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "nginx/nginx.conf",
            "nginx/fastcgi_params",
            "nginx/snippets/php-common.conf",
            "nginx/snippets/ssl-loja.test.conf",
            "nginx/snippets/ssl-app.test.conf",
            "nginx/upstreams/php_8_3.conf",
            "nginx/upstreams/php_8_4.conf",
            "nginx/sites/loja.test.conf",
            "nginx/sites/app.test.conf",
            "nginx/sites/docs.test.conf",
        ]
    );
}

#[test]
fn nginx_log_format_and_site_param_follow_docs() {
    let files = render("nginx", &common::web_context(&root(), "nginx", "1.30.6"));
    let main = &files[0].body;
    assert!(main.contains(
        r#"log_format forgebox escape=json '{"t":"$msec","host":"$host","s":$status,"rt":$request_time,"ut":"$upstream_response_time","b":$body_bytes_sent,"m":"$request_method","u":"$uri","up":"$upstream_addr"}';"#
    ));
    assert!(main.contains("map $http_upgrade $connection_upgrade"));
    assert!(main.contains("listen 127.0.0.1:80 default_server"));
    let loja = files
        .iter()
        .find(|f| f.path.ends_with("sites/loja.test.conf"))
        .unwrap();
    assert!(loja.body.contains("fastcgi_param FORGEBOX_SITE loja.test;"));
    assert!(
        loja.body
            .contains("fastcgi_param SCRIPT_FILENAME $realpath_root$fastcgi_script_name;")
    );
    assert!(loja.body.contains("fastcgi_pass php_8_3;"));
    for f in &files {
        if f.path.starts_with("nginx/snippets/php-common") {
            assert!(!f.body.contains("fastcgi_keep_conn on"));
        }
        if f.path.starts_with("nginx/upstreams/") {
            assert!(!f.body.contains("keepalive"));
        }
    }
    // Path-info guard: the php location must refuse non-existent script paths.
    let php_loc = &loja.body[loja.body.find(r"location ~ \.php$").unwrap()..];
    assert!(php_loc.contains("try_files $fastcgi_script_name =404;"));
    assert!(php_loc.contains(r"fastcgi_split_path_info ^(.+?\.php)(/.*)$;"));
    assert!(php_loc.contains("set $path_info $fastcgi_path_info;"));
    assert!(php_loc.contains("fastcgi_param PATH_INFO $path_info;"));
    assert!(main.contains("ssl_reject_handshake on;"));
    assert!(
        loja.body
            .contains("server_name loja.test *.loja.test admin.loja.test;")
    );
}

#[test]
fn apache() {
    let files = snapshot_all("apache", &common::web_context(&root(), "apache", "2.4.69"));
    let main = files
        .iter()
        .find(|f| f.path == "apache/httpd.conf")
        .unwrap();
    for module in [
        "proxy_module",
        "proxy_fcgi_module",
        "proxy_balancer_module",
        "lbmethod_bybusyness_module",
        "slotmem_shm_module",
        "ssl_module",
        "rewrite_module",
        "headers_module",
        "http2_module",
        "proxy_wstunnel_module",
        "log_config_module",
    ] {
        assert!(
            main.body.contains(&format!("LoadModule {module} ")),
            "{module}"
        );
    }
    assert!(main.body.contains("forgebox_json"));
    assert!(main.body.contains("Listen 127.0.0.1:443"));
    let loja = files
        .iter()
        .find(|f| f.path == "apache/sites/loja.test.conf")
        .unwrap();
    let ctype = loja.body.find("ProxyFCGIBackendType GENERIC").unwrap();
    let setenv = loja
        .body
        .find("ProxyFCGISetEnvIf \"reqenv('SCRIPT_FILENAME')")
        .unwrap();
    assert!(ctype < setenv);
    assert!(loja.body.contains(r#"SCRIPT_FILENAME "$1""#));
    for up in files
        .iter()
        .filter(|f| f.path.starts_with("apache/upstreams/"))
    {
        assert!(up.body.contains("enablereuse=off"), "{}", up.path);
        assert!(!up.body.contains("enablereuse=on"), "{}", up.path);
        assert!(up.body.contains("max=1 "), "{}", up.path);
    }
}

#[test]
fn caddy() {
    snapshot_all("caddy", &common::web_context(&root(), "caddy", "2.11.2"));
}

#[test]
fn php() {
    let files = snapshot_all("php", &common::php_context(&root()));
    assert_eq!(files.len(), 2);
    assert!(
        files[0]
            .new_file_content()
            .unwrap()
            .starts_with("; generated by forgebox")
    );
}

#[test]
fn php_zend_extensions_follow_the_model() {
    let ini = |ctx: &RenderContext| render("php", ctx)[0].new_file_content().unwrap();
    let mut ctx = common::php_context(&root());
    let with = ini(&ctx);
    assert!(with.contains(
        "zend_extension = opcache
"
    ));
    assert!(with.contains(r#"session.save_path = "C:/fb-test/tmp/php/8.3/sessions""#));
    // PHP 8.5 builds OPcache in: loading the missing DLL warns on every start.
    ctx.php.as_mut().unwrap().zend_extensions.clear();
    let without = ini(&ctx);
    assert!(!without.contains("zend_extension"));
    insta::assert_snapshot!("php__builtin_opcache__10-forgebox.ini", without);
}

#[test]
fn php_without_xdebug_skips_that_file() {
    let mut ctx = common::php_context(&root());
    ctx.php.as_mut().unwrap().xdebug = None;
    let files = render("php", &ctx);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "php/8.3/conf.d/10-forgebox.ini");
}

#[test]
fn databases_and_mail() {
    let r = root();
    snapshot_all("mysql", &common::db_context(&r, "mysql", "8.4.6", 3306));
    snapshot_all(
        "mariadb",
        &common::db_context(&r, "mariadb", "11.8.3", 3307),
    );
    let pg = snapshot_all(
        "postgresql",
        &common::db_context(&r, "postgresql", "17.6", 5432),
    );
    let hba = pg.iter().find(|f| f.path.ends_with("pg_hba.conf")).unwrap();
    assert!(hba.body.contains("127.0.0.1/32  trust"));
    assert!(hba.body.contains("::1/128       trust"));
    assert_eq!(hba.body.matches("trust").count(), 3); // two rules + the comment
    assert!(pg.iter().any(|f| f.path.ends_with("pg_ident.conf")));
    snapshot_all("mailpit", &common::mailpit_context(&r));
}

#[test]
fn rendering_is_deterministic() {
    let r = root();
    let ctx = common::web_context(&r, "nginx", "1.30.6");
    assert_eq!(render("nginx", &ctx), render("nginx", &ctx));
}

#[test]
fn unknown_php_pool_is_rejected() {
    let r = root();
    let mut ctx = common::web_context(&r, "nginx", "1.30.6");
    ctx.pools.clear();
    let err = targets::nginx::TARGET
        .render(&Engine::embedded_only(), &ctx)
        .unwrap_err();
    assert_eq!(err.code(), "invalid_input");
}

#[test]
fn lower_case_table_names_follows_instance_case_sensitivity() {
    let r = root();
    for tool in ["mysql", "mariadb"] {
        let mut ctx = common::db_context(&r, tool, "1.0.0", 3306);
        let body = |ctx: &RenderContext| {
            render(tool, ctx)
                .into_iter()
                .find(|f| f.path.ends_with("my.ini"))
                .unwrap()
                .body
        };
        assert!(body(&ctx).contains("lower_case_table_names = 0"));
        ctx.instance.case_sensitive = false;
        assert!(body(&ctx).contains("lower_case_table_names = 2"));
    }
}

/// The rendered `sites/<host>.<ext>` of the proxy site `app.test` with `proxy` changed by
/// `edit` (the fixed context: WebSocket, TLS, start on visit, keep-alive defaults).
fn proxy_site(tool: &str, edit: impl FnOnce(&mut forge_render::ProxyModel)) -> String {
    let r = root();
    let mut ctx = common::web_context(&r, tool, "1.0.0");
    let app = ctx
        .sites
        .iter_mut()
        .find(|s| s.hosts[0] == "app.test")
        .unwrap();
    edit(app.proxy.as_mut().unwrap());
    let files = render(tool, &ctx);
    let file = files
        .iter()
        .find(|f| f.path.contains("sites/app.test."))
        .unwrap();
    let content = file.new_file_content().unwrap();
    check_invariants(file, &content);
    // Whole generated file (sites/ is owned by Forgebox, docs/03 section 3.2).
    assert!(
        content.starts_with("# generated by forgebox"),
        "{}: generated header",
        file.path
    );
    assert!(content.contains("FORGEBOX_SITE"), "{}", file.path);
    file.body.clone()
}

#[test]
fn nginx_proxy_keeps_upstream_connections_alive() {
    let ws = proxy_site("nginx", |_| {});
    // One named pool per proxy site; a refused connect (app still starting) never marks the
    // only server unavailable.
    assert!(ws.contains(
        "upstream forgebox_proxy_app.test {\n    server 127.0.0.1:3000 max_fails=0;\n    keepalive 32;\n    keepalive_timeout 4s;\n}\n"
    ));
    assert!(ws.contains("proxy_pass http://forgebox_proxy_app.test;"));
    assert!(ws.contains("proxy_http_version 1.1;"));
    // WebSocket: upgrade forwarded, otherwise no Connection header (the pooled connection
    // stays open).
    assert!(ws.contains("proxy_set_header Upgrade $http_upgrade;"));
    assert!(ws.contains("proxy_set_header Connection $forgebox_upstream_connection;"));
    assert!(!ws.contains("$connection_upgrade"));
    assert!(ws.contains("proxy_set_header FORGEBOX_SITE app.test;"));
    assert!(ws.contains("error_page 502 = /_forgebox/starting;"));
    let main = &render("nginx", &common::web_context(&root(), "nginx", "1.30.6"))[0].body;
    assert!(main.contains(
        "map $http_upgrade $forgebox_upstream_connection {\n        default upgrade;\n        ''      '';\n    }"
    ));

    let plain = proxy_site("nginx", |p| p.websocket = false);
    assert!(plain.contains("proxy_set_header Connection \"\";"));
    assert!(!plain.contains("Upgrade"));
    assert!(plain.contains("keepalive 32;"));

    // HTTPS upstream: TLS to the pooled name needs the real server name for SNI/verify.
    let tls = proxy_site("nginx", |p| {
        p.upstream = "https://backend.internal".to_owned();
        p.keepalive_conns = 16;
        p.keepalive_idle_secs = 2;
    });
    assert!(tls.contains("server backend.internal:443 max_fails=0;"));
    assert!(tls.contains("keepalive 16;\n    keepalive_timeout 2s;"));
    assert!(tls.contains("proxy_pass https://forgebox_proxy_app.test;"));
    assert!(tls.contains("proxy_ssl_server_name on;\n        proxy_ssl_name backend.internal;"));

    let v6 = proxy_site("nginx", |p| p.upstream = "http://[::1]:5173".to_owned());
    assert!(v6.contains("server [::1]:5173 max_fails=0;"));
    assert!(!v6.contains("proxy_ssl"));

    // keepalive_conns = 0: the pre-M2-E3-T22 shape, a new connection per request.
    let off = proxy_site("nginx", |p| p.keepalive_conns = 0);
    assert!(!off.contains("upstream "));
    assert!(!off.contains("keepalive"));
    assert!(off.contains("proxy_pass http://127.0.0.1:3000;"));
    assert!(off.contains("proxy_set_header Connection $connection_upgrade;"));
}

#[test]
fn apache_proxy_reuses_backend_connections() {
    let site = proxy_site("apache", |_| {});
    // mod_proxy_http reuses the worker's connections unless told otherwise: the template
    // only bounds the idle time below the app's own keep-alive timeout.
    assert!(site.contains(
        "ProxyPass \"/\" \"http://127.0.0.1:3000/\" upgrade=websocket ttl=4 timeout=3600\n    ProxyPassReverse \"/\" \"http://127.0.0.1:3000/\"\n"
    ));
    for closing in [
        "disablereuse",
        "enablereuse=off",
        "proxy-nokeepalive",
        "force-proxy-request-1.0",
        "Connection close",
    ] {
        assert!(!site.contains(closing), "{closing}");
    }
    let off = proxy_site("apache", |p| {
        p.keepalive_conns = 0;
        p.websocket = false;
    });
    assert!(
        off.contains("ProxyPass \"/\" \"http://127.0.0.1:3000/\" disablereuse=On timeout=3600\n")
    );
}

#[test]
fn caddy_proxy_keeps_upstream_connections_alive() {
    let site = proxy_site("caddy", |_| {});
    assert!(site.contains(
        "transport http {\n            read_timeout 3600s\n            # Idle upstream connections are reused (docs/06 section 4.1.1).\n            keepalive 4s\n            keepalive_idle_conns_per_host 32\n        }"
    ));
    assert!(!site.contains("keepalive off"));
    let off = proxy_site("caddy", |p| p.keepalive_conns = 0);
    assert!(off.contains("keepalive off"));
    assert!(!off.contains("keepalive_idle_conns_per_host"));
}
