//! HMR through a real nginx (M1-E4-T5 acceptance, as far as it can be automated): a Node
//! server started by the `AppManager`, the vhost built by `forge-sites` and rendered by
//! `forge-render`, nginx 1.30.6 (`nginx.exe`) proxying it, and WebSocket echo + HTTP through
//! nginx. Also the `autostart: on-visit` hook: the 502 of nginx becomes the starting page
//! served by a stand-in for the daemon's `/_forgebox/starting` endpoint.
//!
//! nginx comes from the shared cache (`C:\apps_src\Forgebox-wt\cache`, sha256 pinned, downloaded
//! when missing), so the test is `#[ignore]`d:
//! `cargo test -p forge-sites --test nginx_proxy_online -- --ignored --nocapture`.
//!
//! Plain HTTP on a high port: TLS of `https://app.test` is covered by `forge-tls` and the
//! smoke of M1 (DNS + CA need the machine's trust store).

#![cfg(windows)]

mod support;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

use forge_core::store::sites::{Site, SiteKind};
use forge_render::{Engine, RenderContext, targets};
use forge_sites::apps::processes_to_json;
use forge_sites::drivers::VhostContext;
use forge_sites::vhost;
use sha2::{Digest, Sha256};
use support::{harness, http_get, wait_until, ws_echo};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const NGINX_URL: &str = "https://nginx.org/download/nginx-1.30.6.zip";
const NGINX_SHA256: &str = "c6ee35edb0a861063323f709192ae50d1eefaf2ea7d00678bd2b22d8542dc61e";
const NGINX_VERSION: &str = "1.30.6";
const SYSTEM32: &str = r"C:\Windows\System32";

fn cache_dir() -> PathBuf {
    let dir = std::env::var_os("FORGEBOX_TEST_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\apps_src\Forgebox-wt\cache"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sha256_file(path: &Path) -> String {
    Sha256::digest(std::fs::read(path).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The cached archive; downloaded (curl, HTTPS) when missing. The hash is always checked.
fn fetch(url: &str, sha256: &str) -> PathBuf {
    let path = cache_dir().join(url.rsplit('/').next().unwrap());
    if !path.exists() || sha256_file(&path) != sha256 {
        let status = Command::new(format!(r"{SYSTEM32}\curl.exe"))
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--ssl-revoke-best-effort",
            ])
            .arg("--output")
            .arg(&path)
            .arg(url)
            .status()
            .expect("curl.exe");
        assert!(status.success(), "download of {url} failed");
    }
    assert_eq!(sha256_file(&path), sha256, "sha256 mismatch for {url}");
    path
}

/// Stops nginx (`-s stop` reaches the workers too) and kills the master as a fallback.
struct Nginx {
    child: Child,
    exe: PathBuf,
    conf: PathBuf,
    prefix: String,
}

impl Drop for Nginx {
    fn drop(&mut self) {
        let _ = Command::new(&self.exe)
            .args(["-s", "stop", "-c"])
            .arg(&self.conf)
            .args(["-p", &self.prefix])
            .output();
        std::thread::sleep(Duration::from_millis(500));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn app_site(host: &str, dir: &Path, processes_yaml: &str) -> Site {
    let mut site = Site::new(host, host.replace('.', "-"));
    site.kind = SiteKind::App;
    site.root = Some(forge_core::paths::posix_string(dir));
    site.secure = false;
    site.http_redirect = false;
    site.processes = processes_to_json(&serde_yaml_ng::from_str(processes_yaml).unwrap());
    site
}

/// Stand-in for the daemon's `/_forgebox/starting?host=<host>`: what nginx asks on a 502.
async fn serve_starting_page(manager: forge_sites::AppManager, port: u16) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    loop {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let manager = manager.clone();
        tokio::spawn(async move {
            let mut buffer = vec![0_u8; 4096];
            let Ok(n) = socket.read(&mut buffer).await else {
                return;
            };
            let request = String::from_utf8_lossy(&buffer[..n]).into_owned();
            let line = request.lines().next().unwrap_or("");
            let host = line
                .split("host=")
                .nth(1)
                .and_then(|rest| rest.split([' ', '&']).next())
                .unwrap_or("");
            let reply = match manager.starting_page(host, "pt-BR").await {
                Ok(page) => format!(
                    "HTTP/1.1 {} Starting\r\nContent-Type: text/html; charset=utf-8\r\nRetry-After: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    page.status,
                    page.retry_after_secs,
                    page.html.len(),
                    page.html
                ),
                Err(e) => {
                    let body = e.to_string();
                    format!(
                        "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                }
            };
            let _ = socket.write_all(reply.as_bytes()).await;
        });
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the real nginx.exe (downloaded from nginx.org into the shared cache when missing)"]
async fn nginx_proxies_http_websocket_and_the_on_visit_page_online() {
    let Some(h) = harness().await else { return };
    let zip = fetch(NGINX_URL, NGINX_SHA256);
    let stage = h.root.tmp_stage().join("nginx");
    std::fs::create_dir_all(&stage).unwrap();
    let status = Command::new(format!(r"{SYSTEM32}\tar.exe"))
        .arg("-xf")
        .arg(&zip)
        .arg("-C")
        .arg(&stage)
        .status()
        .expect("tar.exe");
    assert!(status.success());
    let nginx_dir = h.root.bin().join("nginx").join(NGINX_VERSION);
    std::fs::create_dir_all(nginx_dir.parent().unwrap()).unwrap();
    std::fs::rename(stage.join("nginx-1.30.6"), &nginx_dir).unwrap();
    for dir in [h.root.logs().join("nginx"), h.root.tmp().join("nginx")] {
        std::fs::create_dir_all(dir).unwrap();
    }

    // Two sites: `app.test` starts with the site, `lazy.test` on the first visit.
    let dir_app = h.project("app");
    let dir_lazy = h.project("lazy");
    let app = app_site(
        "app.test",
        &dir_app,
        "web: { cmd: 'node server.js', proxy: true, runtime: 'nodejs@20' }",
    );
    let lazy = app_site(
        "lazy.test",
        &dir_lazy,
        "web: { cmd: 'node server.js', proxy: true, runtime: 'nodejs@20', autostart: on-visit }",
    );
    let sites = [&app, &lazy];
    let mut models = Vec::new();
    let mut app_sites = Vec::new();
    for site in sites {
        let app_site = forge_sites::AppSite::from_site(
            site,
            PathBuf::from(site.root.clone().unwrap()),
            vec![],
        )
        .unwrap();
        let ports = h.manager.allocate_ports(&app_site).await.unwrap();
        let ctx = VhostContext {
            certs_sites: h.root.certs_sites(),
            default_php_series: "8.3".to_owned(),
            extra: forge_render::ExtraModel::default(),
            app_ports: ports,
            proxy_options: Default::default(),
        };
        models.push(vhost::build(site, &ctx).unwrap());
        app_sites.push(app_site);
    }

    // nginx on a high port, the "daemon" page on another.
    let (http_port, internal_port) = (free_port(), free_port());
    let mut rc = RenderContext::new(&h.root, "nginx", NGINX_VERSION);
    rc.prefs.http_port = http_port;
    rc.prefs.internal_port = internal_port;
    rc.sites = models;
    let target = targets::by_id("nginx").unwrap();
    let (_, drifts) = target
        .apply(&Engine::for_root(&h.root), &rc, &h.root.etc(), false)
        .unwrap();
    assert!(drifts.is_empty());
    let conf = h.root.etc().join("nginx").join("nginx.conf");
    let site_conf =
        std::fs::read_to_string(h.root.etc().join("nginx/sites/lazy.test.conf")).unwrap();
    println!("--- etc/nginx/sites/lazy.test.conf ---\n{site_conf}");

    // Processes up: `app.test` listens, `lazy.test` is only registered.
    let status = h.manager.up(&app_sites[0]).await.unwrap();
    assert!(status.ready(), "{status:#?}");
    let lazy_status = h.manager.up(&app_sites[1]).await.unwrap();
    let lazy_port = lazy_status.process("web").unwrap().port.unwrap();
    assert!(!support::is_listening(lazy_port));
    tokio::spawn(serve_starting_page(h.manager.clone(), internal_port));

    let prefix = format!("{}/", h.root.to_posix_string());
    let exe = nginx_dir.join("nginx.exe");
    let test = Command::new(&exe)
        .arg("-t")
        .arg("-c")
        .arg(&conf)
        .args(["-p", &prefix])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&test.stdout),
        String::from_utf8_lossy(&test.stderr)
    );
    assert!(test.status.success(), "nginx -t failed:\n{text}");
    let _nginx = Nginx {
        child: Command::new(&exe)
            .arg("-c")
            .arg(&conf)
            .args(["-p", &prefix])
            .spawn()
            .unwrap(),
        exe: exe.clone(),
        conf: conf.clone(),
        prefix: prefix.clone(),
    };
    assert!(
        wait_until(Duration::from_secs(15), || support::is_listening(http_port)),
        "nginx did not start"
    );

    // HTTP through nginx, with the headers the proxy template sets.
    let port_app = status.process("web").unwrap().port.unwrap();
    let response = http_get(http_port, "app.test", "/");
    assert_eq!(
        (response.status, response.body.as_str()),
        (200, "hello from forgebox app\n")
    );
    let env = http_get(http_port, "app.test", "/env");
    let seen: serde_json::Value = serde_json::from_str(&env.body).unwrap();
    assert_eq!(seen["host"], "app.test");
    assert_eq!(seen["forwardedProto"], "http");
    assert_eq!(seen["forgeboxSite"], "app.test");
    assert_eq!(seen["env"]["PORT"], port_app.to_string());
    println!("GET app.test through nginx:{http_port} -> upstream :{port_app}");

    // WebSocket (HMR) through nginx.
    assert_eq!(
        ws_echo(http_port, "app.test", "/_next/webpack-hmr", "hmr update"),
        "hmr update"
    );
    assert_eq!(
        ws_echo(http_port, "app.test", "/", "second socket"),
        "second socket"
    );
    // Unknown host: nginx's own default, never our app.
    assert_ne!(
        http_get(http_port, "nobody.test", "/").body,
        "hello from forgebox app\n"
    );

    // On-visit: the first request gets the starting page (502 -> /_forgebox/starting), the
    // app starts, and a later request reaches it.
    let first = http_get(http_port, "lazy.test", "/");
    println!(
        "first visit -> {} {}",
        first.status,
        first
            .body
            .lines()
            .find(|l| l.contains("<h1>"))
            .unwrap_or("")
    );
    assert_eq!(first.status, 503, "{first:?}");
    assert!(first.body.contains("Iniciando"), "{}", first.body);
    assert!(first.body.contains(r#"http-equiv="refresh""#));
    assert!(
        wait_until(Duration::from_secs(60), || http_get(
            http_port,
            "lazy.test",
            "/"
        )
        .status
            == 200),
        "lazy.test never came up"
    );
    let ready = h.manager.status("lazy.test").await.unwrap();
    assert_eq!(ready.process("web").unwrap().instance.runs, 1, "{ready:#?}");
    assert_eq!(
        ws_echo(http_port, "lazy.test", "/", "after the visit"),
        "after the visit"
    );

    h.manager.shutdown().await;
}
