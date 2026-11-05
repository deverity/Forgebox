//! Helpers of the online tests (`e2e_online`, `proxy_keepalive_online`): archives from the
//! shared cache (sha256 pinned, downloaded when missing), a temporary ForgeRoot, installs,
//! process cleanup.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use forge_core::paths::{ForgeRoot, InitOptions};
use forge_render::{Engine, RenderContext, targets};
use sha2::{Digest, Sha256};

pub const NGINX_VERSION: &str = "1.30.6";
pub const NGINX_URL: &str = "https://nginx.org/download/nginx-1.30.6.zip";
pub const NGINX_SHA256: &str = "c6ee35edb0a861063323f709192ae50d1eefaf2ea7d00678bd2b22d8542dc61e";

pub const APACHE_VERSION: &str = "2.4.69";
pub const APACHE_URL: &str =
    "https://www.apachelounge.com/download/VS18/binaries/httpd-2.4.69-261002-Win64-VS18.zip";
pub const APACHE_SHA256: &str = "77d715d0d9cee731e2251e7ddc2090a7a8e6d41fedef2e9827cd918593c5a652";

pub const PHP_VERSION: &str = "8.4.27";
pub const PHP_URL: &str =
    "https://windows.php.net/downloads/releases/php-8.4.27-1-nts-Win32-vs17-x64.zip";
pub const PHP_SHA256: &str = "dbae154a0a89791f36c3d48e68bae797878333e7d6cb5721e74617de59b63ab7";

/// Caddy 2.11.7 (curated sha256 of `registry/tools/caddy.yaml`).
pub const CADDY_VERSION: &str = "2.11.7";
pub const CADDY_URL: &str =
    "https://github.com/caddyserver/caddy/releases/download/v2.11.7/caddy_2.11.7_windows_amd64.zip";
pub const CADDY_SHA256: &str = "0a1edc0b799512051c57071ce0e798d3f2cf67dc98171366f1d2b326072e3b06";

pub const SYSTEM32: &str = r"C:\Windows\System32";

pub fn cache_dir() -> PathBuf {
    let dir = std::env::var_os("FORGEBOX_TEST_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\apps_src\Forgebox-wt\cache"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn sha256_file(path: &Path) -> String {
    let bytes = fs::read(path).unwrap();
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Returns the cached archive, downloading it when missing. Always verifies the hash.
pub fn fetch(url: &str, sha256: &str) -> PathBuf {
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

pub fn extract(archive: &Path, dest: &Path) {
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

pub struct Env {
    pub _tmp: tempfile::TempDir,
    pub root: ForgeRoot,
}

/// Temporary ForgeRoot with the standard layout, the fake certificates, the document roots
/// and the log/tmp folders the servers open while testing the configuration.
pub fn temp_root() -> Env {
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
        root.logs().join("caddy"),
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

/// Extracts `archive` and moves `inner_dir` (the archive root when empty) to
/// `bin/<tool>/<version>`.
pub fn install(root: &ForgeRoot, archive: &Path, inner_dir: &str, tool: &str, version: &str) {
    let staging = root.tmp_stage().join(tool);
    extract(archive, &staging);
    let target = root.bin().join(tool).join(version);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let from = if inner_dir.is_empty() {
        staging
    } else {
        staging.join(inner_dir)
    };
    fs::rename(from, &target).unwrap();
}

pub fn render_to_etc(root: &ForgeRoot, tool: &str, ctx: &RenderContext) {
    let target = targets::by_id(tool).unwrap();
    let (_, drifts) = target
        .apply(&Engine::for_root(root), ctx, &root.etc(), false)
        .unwrap();
    assert!(drifts.is_empty());
}

pub fn report(label: &str, out: &std::process::Output) -> String {
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

/// Kills the child processes even when an assertion fails.
pub struct Kill(pub Vec<std::process::Child>);

impl Drop for Kill {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// A free loopback port (bound and released).
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Polls until something accepts connections on `port` (10 s).
pub fn wait_listening(port: u16) -> bool {
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}
