//! End to end with the real binaries (M1-E4-T1 acceptance): render a complete `etc/` tree
//! into a temporary ForgeRoot and run `nginx -t` / `httpd -t` on it.
//!
//! Needs network the first time (downloads are cached) and Windows, so the tests are
//! `#[ignore]`d: `cargo test -p forge-render --test e2e_online -- --ignored --nocapture`.
//!
//! Downloads are pinned by SHA-256, verified on every run:
//! - nginx 1.30.6: no published checksum file on nginx.org; the value was computed by hand
//!   from a download over HTTPS from `nginx.org/download/`.
//! - Apache Lounge 2.4.69 VS18: matches the `.zip.txt` checksum file published next to it.

#![cfg(windows)]

mod common;

use std::fs;
use std::process::Command;

use common::online::*;

#[test]
#[ignore = "downloads nginx from nginx.org and runs the real nginx.exe"]
fn nginx_t_passes_with_rendered_tree_online() {
    let zip = fetch(NGINX_URL, NGINX_SHA256);
    let env = temp_root();
    let root = &env.root;
    install(root, &zip, "nginx-1.30.6", "nginx", NGINX_VERSION);
    render_to_etc(
        root,
        "nginx",
        &common::web_context(root, "nginx", NGINX_VERSION),
    );

    let exe = root
        .bin()
        .join("nginx")
        .join(NGINX_VERSION)
        .join("nginx.exe");
    let conf = root.etc().join("nginx").join("nginx.conf");
    let prefix = format!("{}/", root.to_posix_string());
    let out = Command::new(&exe)
        .arg("-t")
        .arg("-c")
        .arg(&conf)
        .arg("-p")
        .arg(&prefix)
        .output()
        .unwrap();
    let text = report("nginx -t", &out);
    assert!(out.status.success(), "nginx -t failed:\n{text}");
    assert!(text.contains("syntax is ok") && text.contains("test is successful"));
}

#[test]
#[ignore = "downloads Apache Lounge httpd and runs the real httpd.exe"]
fn httpd_t_passes_with_rendered_tree_online() {
    let zip = fetch(APACHE_URL, APACHE_SHA256);
    let env = temp_root();
    let root = &env.root;
    install(root, &zip, "Apache24", "apache", APACHE_VERSION);
    render_to_etc(
        root,
        "apache",
        &common::web_context(root, "apache", APACHE_VERSION),
    );

    let server_root = root.bin().join("apache").join(APACHE_VERSION);
    let exe = server_root.join("bin").join("httpd.exe");
    let conf = root.etc().join("apache").join("httpd.conf");
    let out = Command::new(&exe)
        .arg("-t")
        .arg("-f")
        .arg(&conf)
        .arg("-d")
        .arg(&server_root)
        .output()
        .unwrap();
    let text = report("httpd -t", &out);
    assert!(out.status.success(), "httpd -t failed:\n{text}");
    assert!(text.contains("Syntax OK"));
}

fn curl(url: &str, host: &str) -> (String, String) {
    let out = Command::new(format!(r"{SYSTEM32}\curl.exe"))
        .args(["--silent", "--max-time", "20", "--header"])
        .arg(format!("Host: {host}"))
        .args(["--write-out", "\n%{http_code}"])
        .arg(url)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let (body, code) = text.rsplit_once('\n').unwrap_or((&text, ""));
    (code.trim().to_owned(), body.to_owned())
}

/// Real php-cgi behind a real httpd with the rendered PHP vhost: `SCRIPT_FILENAME` must
/// reach php-cgi as a plain `C:/...` path (docs/06 section 2.1), which `httpd -t` cannot check.
#[test]
#[ignore = "downloads PHP and Apache Lounge and serves real requests"]
fn apache_serves_php_through_php_cgi_online() {
    let httpd_zip = fetch(APACHE_URL, APACHE_SHA256);
    let php_zip = fetch(PHP_URL, PHP_SHA256);
    let env = temp_root();
    let root = &env.root;
    install(root, &httpd_zip, "Apache24", "apache", APACHE_VERSION);
    let php_dir = root.bin().join("php").join(PHP_VERSION);
    extract(&php_zip, &php_dir);

    let www = root.www().join("live");
    fs::create_dir_all(&www).unwrap();
    fs::write(
        www.join("index.php"),
        "<?php echo 'ok:', $_SERVER['FORGEBOX_SITE'], ':', basename($_SERVER['SCRIPT_FILENAME']), ':', $_SERVER['REQUEST_URI'];",
    )
    .unwrap();

    let mut ctx = common::web_context(root, "apache", APACHE_VERSION);
    ctx.prefs.http_port = 18080;
    ctx.prefs.https_port = 18443;
    ctx.pools = vec![forge_render::UpstreamModel::new(
        "php_8_4",
        vec![forge_render::UpstreamServer {
            host: "127.0.0.1".to_owned(),
            port: 19184,
        }],
    )];
    ctx.sites = vec![forge_render::VhostModel {
        kind: forge_render::SiteKind::Php,
        hosts: vec!["live.test".to_owned()],
        root: www.to_string_lossy().into_owned(),
        php_upstream: Some("php_8_4".to_owned()),
        fallback_resource: Some("/index.php".to_owned()),
        ..forge_render::VhostModel::default()
    }];
    render_to_etc(root, "apache", &ctx);

    let server_root = root.bin().join("apache").join(APACHE_VERSION);
    let mut kill = Kill(Vec::new());
    kill.0.push(
        Command::new(php_dir.join("php-cgi.exe"))
            .args(["-n", "-b", "127.0.0.1:19184"])
            .spawn()
            .unwrap(),
    );
    kill.0.push(
        Command::new(server_root.join("bin").join("httpd.exe"))
            .arg("-f")
            .arg(root.etc().join("apache").join("httpd.conf"))
            .arg("-d")
            .arg(&server_root)
            .spawn()
            .unwrap(),
    );
    let mut up = false;
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if std::net::TcpStream::connect("127.0.0.1:18080").is_ok() {
            up = true;
            break;
        }
    }
    assert!(up, "httpd did not start");

    let (code, body) = curl("http://127.0.0.1:18080/index.php", "live.test");
    println!("/index.php -> {code} {body}");
    assert_eq!(code, "200", "{body}");
    assert_eq!(body, "ok:live.test:index.php:/index.php");
    let (code, body) = curl("http://127.0.0.1:18080/products/1", "live.test");
    println!("/products/1 -> {code} {body}");
    assert_eq!(code, "200", "{body}");
    assert!(body.starts_with("ok:live.test:index.php:"));
}
