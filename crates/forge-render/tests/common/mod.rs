//! Fixed contexts shared by the snapshot tests and the online end-to-end tests.
#![allow(dead_code)]

use std::collections::BTreeMap;

use forge_core::paths::ForgeRoot;
use forge_render::{
    InstanceModel, PhpModel, ProxyModel, RenderContext, SiteKind, SslModel, StaticLocation,
    UpstreamModel, UpstreamServer, VhostModel, XdebugModel,
};

pub fn pools() -> Vec<UpstreamModel> {
    let server = |port| UpstreamServer {
        host: "127.0.0.1".to_owned(),
        port,
    };
    vec![
        UpstreamModel::new("php_8_3", vec![server(19083), server(19084)]),
        UpstreamModel::new("php_8_4", vec![server(19085)]),
    ]
}

/// `loja.test`: Laravel on PHP 8.3 with TLS; `app.test`: proxy with WebSocket;
/// `docs.test`: static, plain HTTP.
pub fn sites(root: &ForgeRoot) -> Vec<VhostModel> {
    let certs = root.certs_sites();
    let ssl = |host: &str| SslModel {
        cert: certs
            .join(format!("{host}.crt"))
            .to_string_lossy()
            .into_owned(),
        key: certs
            .join(format!("{host}.key"))
            .to_string_lossy()
            .into_owned(),
    };
    let www = root.www();
    vec![
        VhostModel {
            kind: SiteKind::Php,
            hosts: vec![
                "Loja.test".to_owned(),
                "*.loja.test".to_owned(),
                "admin.loja.test".to_owned(),
            ],
            root: www
                .join("loja")
                .join("public")
                .to_string_lossy()
                .into_owned(),
            php_upstream: Some("php_8_3".to_owned()),
            static_locations: vec![StaticLocation {
                pattern: r"\.(?:css|js|png|svg|woff2)$".to_owned(),
                cache_control: Some("public, max-age=86400".to_owned()),
            }],
            headers: BTreeMap::from([("X-Frame-Options".to_owned(), "SAMEORIGIN".to_owned())]),
            ssl: Some(ssl("loja.test")),
            http_redirect: true,
            env: BTreeMap::from([("APP_ENV".to_owned(), "local".to_owned())]),
            ..VhostModel::default()
        },
        VhostModel {
            kind: SiteKind::Proxy,
            hosts: vec!["app.test".to_owned()],
            proxy: Some(ProxyModel {
                upstream: "http://127.0.0.1:3000".to_owned(),
                websocket: true,
                read_timeout_secs: 3600,
                start_on_visit: true,
            }),
            ssl: Some(ssl("app.test")),
            http_redirect: true,
            ..VhostModel::default()
        },
        VhostModel {
            kind: SiteKind::Static,
            hosts: vec!["docs.test".to_owned()],
            root: www.join("docs").to_string_lossy().into_owned(),
            ..VhostModel::default()
        },
    ]
}

pub fn web_context(root: &ForgeRoot, tool: &str, version: &str) -> RenderContext {
    let mut ctx = RenderContext::new(root, tool, version);
    ctx.sites = sites(root);
    ctx.pools = pools();
    ctx
}

pub fn php_context(root: &ForgeRoot) -> RenderContext {
    let mut ctx = RenderContext::new(root, "php", "8.3.36-nts-vs17-x64");
    ctx.php = Some(PhpModel {
        series: "8.3".to_owned(),
        extensions: [
            "curl",
            "mbstring",
            "openssl",
            "pdo_mysql",
            "pdo_sqlite",
            "intl",
            "zip",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect(),
        zend_extensions: vec!["opcache".to_owned()],
        settings: BTreeMap::from([("memory_limit".to_owned(), "1G".to_owned())]),
        xdebug: Some(XdebugModel {
            mode: "debug".to_owned(),
            start_with_request: "trigger".to_owned(),
            client_port: 9003,
        }),
    });
    ctx
}

pub fn db_context(root: &ForgeRoot, tool: &str, version: &str, port: u16) -> RenderContext {
    let mut ctx = RenderContext::new(root, tool, version);
    ctx.instance = InstanceModel {
        name: "default".to_owned(),
        ports: BTreeMap::from([("main".to_owned(), port)]),
        data_dir: root
            .data()
            .join(tool)
            .join("default")
            .to_string_lossy()
            .into_owned(),
        case_sensitive: true,
        settings: BTreeMap::from([("slow_query_log".to_owned(), "1".to_owned())]),
    };
    ctx
}

pub fn mailpit_context(root: &ForgeRoot) -> RenderContext {
    let mut ctx = RenderContext::new(root, "mailpit", "1.28.0");
    ctx.instance = InstanceModel {
        name: "default".to_owned(),
        ports: BTreeMap::from([("http".to_owned(), 8025), ("smtp".to_owned(), 1025)]),
        data_dir: root
            .data()
            .join("mailpit")
            .join("default")
            .to_string_lossy()
            .into_owned(),
        case_sensitive: true,
        settings: BTreeMap::new(),
    };
    ctx
}
