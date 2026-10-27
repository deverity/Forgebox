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
use std::path::{Path, PathBuf};
use std::process::Command;

use forge_core::paths::{ForgeRoot, InitOptions};
use forge_render::{Engine, RenderContext, targets};
use sha2::{Digest, Sha256};

const NGINX_VERSION: &str = "1.30.6";
const NGINX_URL: &str = "https://nginx.org/download/nginx-1.30.6.zip";
const NGINX_SHA256: &str = "c6ee35edb0a861063323f709192ae50d1eefaf2ea7d00678bd2b22d8542dc61e";

const APACHE_VERSION: &str = "2.4.69";
const APACHE_URL: &str =
    "https://www.apachelounge.com/download/VS18/binaries/httpd-2.4.69-261002-Win64-VS18.zip";
const APACHE_SHA256: &str = "77d715d0d9cee731e2251e7ddc2090a7a8e6d41fedef2e9827cd918593c5a652";

const PHP_VERSION: &str = "8.4.27";
const PHP_URL: &str =
    "https://windows.php.net/downloads/releases/php-8.4.27-1-nts-Win32-vs17-x64.zip";
const PHP_SHA256: &str = "dbae154a0a89791f36c3d48e68bae797878333e7d6cb5721e74617de59b63ab7";

const SYSTEM32: &str = r"C:\Windows\System32";

fn cache_dir() -> PathBuf {
    let dir = std::env::var_os("FORGEBOX_TEST_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\apps_src\Forgebox-wt\cache"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn sha256_file(path: &Path) -> String {
    let bytes = fs::read(path).unwrap();
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Returns the cached archive, downloading it when missing. Always verifies the hash.
fn fetch(url: &str, sha256: &str) -> PathBuf {
    let file_name = url.rsplit('/').next().unwrap();
    let path = cache_dir().join(file_name);
    if !path.exists() || sha256_file(&path) != sha256 {
        // --ssl-revoke-best-effort: Schannel cannot always reach revocation servers
        // (corporate proxies); the certificate chain is still fully validated.
        let status = Command::new(format!(r"{SYSTEM32}\curl.exe"))
            .args(["--fail", "--silent", "--show-error", "--location"])
            .arg("--ssl-revoke-best-effort")
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

fn extract(archive: &Path, dest: &Path) {
    fs::create_dir_all(dest).unwrap();
    // bsdtar shipped with Windows reads zip archives.
    let status = Command::new(format!(r"{SYSTEM32}\tar.exe"))
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .status()
        .expect("tar.exe");
    assert!(status.success(), "extracting {}", archive.display());
}

struct Env {
    _tmp: tempfile::TempDir,
    root: ForgeRoot,
}

/// Temporary ForgeRoot with the standard layout, the fake certificates, the document roots
/// and the log/tmp folders the servers open while testing the configuration.
fn temp_root() -> Env {
    let tmp = tempfile::Builder::new()
        .prefix("fb-e2e-")
        .tempdir()
        .unwrap();
    // nginx rejects 8.3 short names (`GUSTAV~1`) in paths: use the long form of the temp dir.
    let long = forge_core::paths::posix_string(&fs::canonicalize(tmp.path()).unwrap());
    let root = ForgeRoot::new(long).unwrap();
    root.init(InitOptions::default()).unwrap();
    for dir in [
        root.logs().join("nginx"),
        root.logs().join("apache"),
        root.tmp().join("nginx"),
        root.tmp().join("apache"),
        root.www().join("loja").join("public"),
        root.www().join("docs"),
    ] {
        fs::create_dir_all(dir).unwrap();
    }
    for host in ["loja.test", "app.test"] {
        let cert = rcgen::generate_simple_self_signed(vec![host.to_owned()]).unwrap();
        fs::write(
            root.certs_sites().join(format!("{host}.crt")),
            cert.cert.pem(),
        )
        .unwrap();
        fs::write(
            root.certs_sites().join(format!("{host}.key")),
            cert.signing_key.serialize_pem(),
        )
        .unwrap();
    }
    Env { _tmp: tmp, root }
}

fn install(root: &ForgeRoot, archive: &Path, inner_dir: &str, tool: &str, version: &str) {
    let staging = root.tmp_stage().join(tool);
    extract(archive, &staging);
    let target = root.bin().join(tool).join(version);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::rename(staging.join(inner_dir), &target).unwrap();
}

fn render_to_etc(root: &ForgeRoot, tool: &str, ctx: &RenderContext) {
    let target = targets::by_id(tool).unwrap();
    let (_, drifts) = target
        .apply(&Engine::for_root(root), ctx, &root.etc(), false)
        .unwrap();
    assert!(drifts.is_empty());
}

fn report(label: &str, out: &std::process::Output) -> String {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    println!(
        "--- {label} (exit {:?}) ---\n{}",
        out.status.code(),
        text.trim()
    );
    text
}

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

/// Kills the child processes even when an assertion fails.
struct Kill(Vec<std::process::Child>);

impl Drop for Kill {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
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
