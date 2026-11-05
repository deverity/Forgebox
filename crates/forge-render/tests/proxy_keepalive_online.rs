//! Proxy sites keep their upstream connections alive (M2-E3-T22, docs/06 section 4.1.1),
//! with the real servers from the shared cache (sha256 pinned) and the rendered templates.
//!
//! * `proxy_sites_reuse_upstream_connections_online`: nginx 1.30.6, Apache httpd 2.4.69 and
//!   Caddy 2.11.7 in front of an upstream inside the test that counts its TCP connections.
//!   With the defaults 20 requests from 20 client connections reach the app over one or two
//!   upstream connections, without `Connection: close`; a WebSocket upgrade (echo) and a
//!   POST go through the same site. With `keepalive_conns = 0` every request opens its own
//!   upstream connection (the shape before M2-E3-T22).
//! * `nginx_proxy_keepalive_latency_and_reload_online` (measurement: run on a quiet
//!   machine): the load of the M2-E5-T13 gate (`oha -q 1000 -c 64`, upstream with a
//!   log-normal delay, median 20 ms, p99 120 ms) before (a new upstream connection per
//!   request, no `multi_accept`) and after: proxy p50/p90/p99, the latency of a 404 of the
//!   same nginx during that load, and the failed requests while nginx is reloaded every
//!   0.5 s. Re-rendering the same model writes nothing (so the daemon does not reload).
//!
//! `cargo test -p forge-render --release --test proxy_keepalive_online -- --ignored --nocapture --test-threads 1`
//!
//! `oha` comes from `FORGEBOX_OHA` (default `C:\apps_src\Forgebox-wt\cache\tools\bin\oha.exe`);
//! `FORGEBOX_PERF_ROUNDS` sets the rounds of the measurement (default 1; the median is
//! reported).

#![cfg(windows)]

mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::online::*;
use forge_core::paths::ForgeRoot;
use forge_core::platform::{
    GroupOptions, ProcessCommand, ProcessControlOptions, ProcessGroup, StdioTarget, process_control,
};
use forge_render::{
    Engine, ProxyModel, RenderContext, SiteKind, VhostModel, WriteOutcome, targets,
};

const HOST: &str = "perf.test";

// ------------------------------------------------------------------------------ upstream

/// Log-normal delay of the measurement upstream (the M2-E5-T13 gate's): median 20 ms, p99
/// 120 ms.
const MEDIAN_MS: f64 = 20.0;
const P99_MS: f64 = 120.0;
const Z_99: f64 = 2.326_347_874;

/// SplitMix64 over a shared counter.
fn next_u64(state: &AtomicU64) -> u64 {
    let mut z = state
        .fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed)
        .wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn uniform(state: &AtomicU64) -> f64 {
    ((next_u64(state) >> 11) as f64 + 0.5) / (1u64 << 53) as f64
}

fn lognormal_delay(state: &AtomicU64) -> Duration {
    let sigma = (P99_MS / MEDIAN_MS).ln() / Z_99;
    let z = (-2.0 * uniform(state).ln()).sqrt() * (std::f64::consts::TAU * uniform(state)).cos();
    Duration::from_secs_f64((MEDIAN_MS.ln() + sigma * z).exp().min(2_000.0) / 1000.0)
}

/// What the upstream saw.
#[derive(Default)]
struct Seen {
    connections: AtomicU64,
    requests: AtomicU64,
    upgrades: AtomicU64,
    /// `Connection` header of the plain requests (lowercase, `-` when absent) and the
    /// HTTP version of the request line.
    connection_headers: Mutex<BTreeMap<String, u64>>,
    versions: Mutex<BTreeMap<String, u64>>,
}

impl Seen {
    fn connections(&self) -> u64 {
        self.connections.load(Ordering::SeqCst)
    }
}

/// An HTTP/1.1 app with keep-alive, one thread per connection (like a dev server):
/// answers `ok` (after a log-normal delay when `delayed`), echoes WebSocket text frames
/// after an upgrade, and closes only when the request asks for it.
fn spawn_upstream(delayed: bool) -> (u16, Arc<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Seen::default());
    let state = Arc::new(AtomicU64::new(0x5eed_f0f0_1234_5678));
    let shared = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            shared.connections.fetch_add(1, Ordering::SeqCst);
            let (seen, state) = (Arc::clone(&shared), Arc::clone(&state));
            std::thread::spawn(move || {
                let _ = serve(stream, &seen, delayed.then_some(&*state));
            });
        }
    });
    (port, seen)
}

fn serve(stream: TcpStream, seen: &Seen, delay: Option<&AtomicU64>) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let mut headers = BTreeMap::new();
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header)? == 0 {
                return Ok(());
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        let length = headers
            .get("content-length")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0_u8; length];
        reader.read_exact(&mut body)?;
        let connection = headers
            .get("connection")
            .map_or_else(|| "-".to_owned(), |v| v.to_ascii_lowercase());
        if headers
            .get("upgrade")
            .is_some_and(|u| u.eq_ignore_ascii_case("websocket"))
        {
            seen.upgrades.fetch_add(1, Ordering::SeqCst);
            assert!(connection.contains("upgrade"), "{connection}");
            // Proxies do not check the accept key; the test client does not either.
            out.write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
                  Sec-WebSocket-Accept: forgebox-test\r\n\r\n",
            )?;
            return echo_frames(&mut reader, &mut out);
        }
        seen.requests.fetch_add(1, Ordering::SeqCst);
        *seen
            .connection_headers
            .lock()
            .unwrap()
            .entry(connection.clone())
            .or_default() += 1;
        let version = line.trim_end().rsplit(' ').next().unwrap_or("").to_owned();
        *seen
            .versions
            .lock()
            .unwrap()
            .entry(version.clone())
            .or_default() += 1;
        if let Some(state) = delay {
            // `std::thread::sleep` uses a high-resolution waitable timer.
            std::thread::sleep(lognormal_delay(state));
        }
        let close = connection.contains("close") || version == "HTTP/1.0";
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n{}\r\nok{}",
            2 + body.len(),
            if close { "Connection: close\r\n" } else { "" },
            String::from_utf8_lossy(&body)
        );
        out.write_all(reply.as_bytes())?;
        if close {
            return Ok(());
        }
    }
}

/// Echoes masked client text frames (payload < 126 bytes) until a close frame.
fn echo_frames(reader: &mut impl Read, out: &mut impl Write) -> std::io::Result<()> {
    loop {
        let mut head = [0_u8; 2];
        reader.read_exact(&mut head)?;
        let len = usize::from(head[1] & 0x7f);
        let mut mask = [0_u8; 4];
        reader.read_exact(&mut mask)?;
        let mut payload = vec![0_u8; len];
        reader.read_exact(&mut payload)?;
        if head[0] & 0x0f == 0x8 {
            out.write_all(&[0x88, 0x00])?;
            return Ok(());
        }
        let data: Vec<u8> = payload
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ mask[i % 4])
            .collect();
        let mut frame = vec![0x81, u8::try_from(data.len()).unwrap()];
        frame.extend_from_slice(&data);
        out.write_all(&frame)?;
    }
}

// ------------------------------------------------------------------------------ clients

/// One request on a new connection (`Connection: close`): status and body.
fn request(port: u16, method: &str, host: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    write!(
        stream,
        "{method} / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap();
    let status = text
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

/// WebSocket upgrade through the server and one echoed text frame.
fn ws_echo(port: u16, host: &str, message: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    write!(
        stream,
        "GET /hmr HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
    .unwrap();
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("handshake response");
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
    assert!(head.starts_with("http/1.1 101"), "no upgrade: {head}");
    let mask = [0x12_u8, 0x34, 0x56, 0x78];
    let payload = message.as_bytes();
    let mut frame = vec![0x81, 0x80 | u8::try_from(payload.len()).unwrap()];
    frame.extend_from_slice(&mask);
    frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    stream.write_all(&frame).unwrap();
    let mut header = [0_u8; 2];
    stream.read_exact(&mut header).expect("echo frame header");
    assert_eq!(header[0], 0x81, "text frame expected");
    let mut echo = vec![0_u8; usize::from(header[1] & 0x7f)];
    stream.read_exact(&mut echo).expect("echo frame payload");
    let _ = stream.write_all(&[0x88, 0x80, 0, 0, 0, 0]);
    String::from_utf8(echo).unwrap()
}

// ------------------------------------------------------------------------------ servers

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Server {
    Nginx,
    Apache,
    Caddy,
}

impl Server {
    fn tool(self) -> &'static str {
        match self {
            Self::Nginx => "nginx",
            Self::Apache => "apache",
            Self::Caddy => "caddy",
        }
    }

    fn version(self) -> &'static str {
        match self {
            Self::Nginx => NGINX_VERSION,
            Self::Apache => APACHE_VERSION,
            Self::Caddy => CADDY_VERSION,
        }
    }
}

/// A running server; the whole process tree goes away with it.
struct Running {
    group: Arc<dyn ProcessGroup>,
    /// What was rendered.
    ctx: RenderContext,
    http_port: u16,
    exe: PathBuf,
    reload_args: Vec<String>,
    cwd: PathBuf,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.group.terminate_all(0);
    }
}

impl Running {
    /// `nginx -s reload` (graceful: new workers, old ones finish what they serve).
    fn reload(&self) {
        let out = std::process::Command::new(&self.exe)
            .args(&self.reload_args)
            .current_dir(&self.cwd)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", report("reload", &out));
    }
}

/// The proxy site `perf.test` → `http://127.0.0.1:<upstream>` (WebSocket on).
fn proxy_site(upstream: u16, edit: impl FnOnce(&mut ProxyModel)) -> VhostModel {
    let mut proxy = ProxyModel {
        websocket: true,
        ..ProxyModel::new(format!("http://127.0.0.1:{upstream}"))
    };
    edit(&mut proxy);
    VhostModel {
        kind: SiteKind::Proxy,
        hosts: vec![HOST.to_owned()],
        proxy: Some(proxy),
        ..VhostModel::default()
    }
}

fn posix(path: &Path) -> String {
    forge_core::paths::posix_string(path)
}

/// Installs `server` into `root`, renders `site` with the real templates (`patch` may edit
/// the rendered main file) and starts it on free ports.
fn start(
    server: Server,
    root: &ForgeRoot,
    site: VhostModel,
    patch: impl FnOnce(String) -> String,
) -> Running {
    let tool = server.tool();
    let version = server.version();
    if !root.bin().join(tool).join(version).exists() {
        match server {
            Server::Nginx => install(
                root,
                &fetch(NGINX_URL, NGINX_SHA256),
                "nginx-1.30.6",
                tool,
                version,
            ),
            Server::Apache => install(
                root,
                &fetch(APACHE_URL, APACHE_SHA256),
                "Apache24",
                tool,
                version,
            ),
            Server::Caddy => install(root, &fetch(CADDY_URL, CADDY_SHA256), "", tool, version),
        }
    }
    let mut ctx = RenderContext::new(root, tool, version);
    ctx.prefs.http_port = free_port();
    ctx.prefs.https_port = free_port();
    ctx.prefs.internal_port = free_port();
    ctx.sites = vec![site];
    render_to_etc(root, tool, &ctx);
    let dir = root.bin().join(tool).join(version);
    let etc = root.etc().join(tool);
    let (exe, args, reload_args, cwd) = match server {
        Server::Nginx => {
            let conf = etc.join("nginx.conf");
            std::fs::write(&conf, patch(std::fs::read_to_string(&conf).unwrap())).unwrap();
            let common = vec![
                "-p".to_owned(),
                format!("{}/", posix(&dir)),
                "-c".to_owned(),
                posix(&conf),
            ];
            let mut reload = vec!["-s".to_owned(), "reload".to_owned()];
            reload.extend(common.clone());
            (dir.join("nginx.exe"), common, reload, dir.clone())
        }
        Server::Apache => {
            let conf = etc.join("httpd.conf");
            std::fs::write(&conf, patch(std::fs::read_to_string(&conf).unwrap())).unwrap();
            let args = vec!["-f".to_owned(), posix(&conf), "-d".to_owned(), posix(&dir)];
            (
                dir.join("bin").join("httpd.exe"),
                args,
                Vec::new(),
                dir.clone(),
            )
        }
        Server::Caddy => {
            let conf = etc.join("Caddyfile");
            let text = std::fs::read_to_string(&conf).unwrap().replace(
                "admin 127.0.0.1:2019",
                &format!("admin 127.0.0.1:{}", free_port()),
            );
            std::fs::write(&conf, patch(text)).unwrap();
            let args = ["run", "--config", &posix(&conf), "--adapter", "caddyfile"]
                .map(str::to_owned)
                .to_vec();
            (dir.join("caddy.exe"), args, Vec::new(), etc.clone())
        }
    };
    let processes = process_control(ProcessControlOptions::default());
    let group = processes.create_group(GroupOptions::ROOT).unwrap();
    let mut command = ProcessCommand::new(&exe, &cwd);
    command.args = args.into_iter().map(Into::into).collect();
    command.env = processes.base_env();
    let path = processes
        .system_path()
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(";");
    command.env.push(("PATH".into(), path.into()));
    // Caddy keeps its storage and autosave under these; keep them out of the user profile.
    for var in ["XDG_DATA_HOME", "XDG_CONFIG_HOME"] {
        command
            .env
            .push((var.into(), root.tmp().join("xdg").into()));
    }
    let logs = root.logs().join(tool);
    command.stdout = StdioTarget::File(logs.join("server.out"));
    command.stderr = StdioTarget::File(logs.join("server.err"));
    group.spawn(&command).unwrap();
    let running = Running {
        group,
        http_port: ctx.prefs.http_port,
        ctx,
        exe,
        reload_args,
        cwd,
    };
    assert!(
        wait_listening(running.http_port),
        "{tool} did not start: {}",
        std::fs::read_to_string(logs.join("server.err")).unwrap_or_default()
    );
    running
}

// ------------------------------------------------------------------------------ reuse

fn check_reuse(server: Server, keepalive: bool) {
    let env = temp_root();
    let (upstream, seen) = spawn_upstream(false);
    let site = proxy_site(upstream, |p| {
        if !keepalive {
            p.keepalive_conns = 0;
        }
    });
    let running = start(server, &env.root, site, |text| text);
    let port = running.http_port;
    // The first request may race the server's own start-up checks: wait for a 200.
    let deadline = Instant::now() + Duration::from_secs(10);
    while request(port, "GET", HOST, "").0 != 200 {
        assert!(
            Instant::now() < deadline,
            "{server:?}: proxy never answered"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    let before = seen.connections();
    for i in 0..20 {
        let (status, body) = request(port, "GET", HOST, "");
        assert_eq!(
            (status, body.as_str()),
            (200, "ok"),
            "{server:?} request {i}"
        );
    }
    let (status, body) = request(port, "POST", HOST, "payload");
    assert_eq!(
        (status, body.as_str()),
        (200, "okpayload"),
        "{server:?} POST"
    );
    let opened = seen.connections() - before;
    let headers = seen.connection_headers.lock().unwrap().clone();
    let versions = seen.versions.lock().unwrap().clone();
    println!(
        "{server:?} keepalive={keepalive}: 21 requests over {opened} new upstream connections; \
         Connection headers {headers:?}; versions {versions:?}"
    );
    if keepalive {
        assert!(opened <= 2, "{server:?}: {opened} upstream connections");
        assert!(
            !headers.keys().any(|h| h.contains("close")),
            "{server:?}: {headers:?}"
        );
        assert_eq!(
            versions.keys().collect::<Vec<_>>(),
            ["HTTP/1.1"],
            "{server:?}"
        );
    } else {
        assert_eq!(opened, 21, "{server:?}: one connection per request");
    }

    // WebSocket through the same site, and plain requests keep working afterwards.
    assert_eq!(
        ws_echo(port, HOST, "hmr update"),
        "hmr update",
        "{server:?}"
    );
    assert_eq!(seen.upgrades.load(Ordering::SeqCst), 1, "{server:?}");
    assert_eq!(request(port, "GET", HOST, "").0, 200, "{server:?} after ws");
}

#[test]
#[ignore = "runs the real nginx, Apache httpd and Caddy from the shared cache"]
fn proxy_sites_reuse_upstream_connections_online() {
    for server in [Server::Nginx, Server::Apache, Server::Caddy] {
        check_reuse(server, true);
        check_reuse(server, false);
    }
}

// ------------------------------------------------------------------------------ measurement

fn oha_path() -> PathBuf {
    let path = std::env::var_os("FORGEBOX_OHA").map_or_else(
        || PathBuf::from(r"C:\apps_src\Forgebox-wt\cache\tools\bin\oha.exe"),
        PathBuf::from,
    );
    assert!(
        path.is_file(),
        "oha not found at {} (FORGEBOX_OHA): cargo install oha --locked --root \
         C:/apps_src/Forgebox-wt/cache/tools",
        path.display()
    );
    path
}

#[derive(Debug, Clone, Default)]
struct OhaRun {
    requests: u64,
    errors: BTreeMap<String, u64>,
    non_2xx: u64,
    /// p50, p90, p99 in milliseconds.
    pct: [f64; 3],
}

impl OhaRun {
    fn failed(&self) -> u64 {
        self.errors.values().sum::<u64>() + self.non_2xx
    }
}

/// `oha -q <rate> -c <conns> -z <secs>s` against `host` on nginx `port`, waiting for the
/// requests in flight at the deadline.
fn oha(port: u16, host: &str, rate: u32, conns: u32, secs: u64, keepalive: bool) -> OhaRun {
    let mut cmd = std::process::Command::new(oha_path());
    cmd.args(["--no-tui", "--output-format", "json", "-w"])
        .args(["-q", &rate.to_string(), "-c", &conns.to_string()])
        .args(["-z", &format!("{secs}s"), "--host", host]);
    if !keepalive {
        cmd.arg("--disable-keepalive");
    }
    let out = cmd
        .arg(format!("http://127.0.0.1:{port}/"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", report("oha", &out));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let count = |key: &str| -> BTreeMap<String, u64> {
        json[key]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_u64().unwrap_or(0)))
                    .collect()
            })
            .unwrap_or_default()
    };
    let statuses = count("statusCodeDistribution");
    let pct = |key: &str| json["latencyPercentiles"][key].as_f64().unwrap_or(f64::NAN) * 1000.0;
    OhaRun {
        requests: statuses.values().sum(),
        errors: count("errorDistribution"),
        non_2xx: statuses
            .iter()
            .filter(|(k, _)| !k.starts_with('2') && k.as_str() != "404")
            .map(|(_, v)| v)
            .sum(),
        pct: [pct("p50"), pct("p90"), pct("p99")],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    /// The proxy before M2-E3-T22: a new upstream connection per request, no
    /// `multi_accept`.
    Before,
    /// The templates as rendered now.
    After,
}

#[derive(Debug, Default, Clone)]
struct Round {
    idle_404: OhaRun,
    proxy: OhaRun,
    loaded_404: OhaRun,
    /// New client connection per request while nginx reloads every 0.5 s.
    reload_fresh: OhaRun,
    /// Keep-alive clients while nginx reloads every 0.5 s.
    reload_keepalive: OhaRun,
    reloads: u32,
}

/// Runs `load` while nginx is reloaded every 0.5 s; returns oha's result and the number of
/// reloads.
fn under_reloads(running: &Running, load: impl FnOnce() -> OhaRun + Send) -> (OhaRun, u32) {
    std::thread::scope(|scope| {
        let handle = scope.spawn(load);
        let mut reloads = 0;
        std::thread::sleep(Duration::from_millis(500));
        while !handle.is_finished() {
            running.reload();
            reloads += 1;
            std::thread::sleep(Duration::from_millis(500));
        }
        (handle.join().unwrap(), reloads)
    })
}

fn measure(variant: Variant, upstream: u16) -> Round {
    let env = temp_root();
    let site = proxy_site(upstream, |p| {
        if variant == Variant::Before {
            p.keepalive_conns = 0;
        }
    });
    let running = start(Server::Nginx, &env.root, site, |text| match variant {
        Variant::Before => text.replace("multi_accept on;", ""),
        Variant::After => {
            assert!(text.contains("multi_accept on;"));
            text
        }
    });
    let port = running.http_port;
    let mut round = Round {
        idle_404: oha(port, "nobody.test", 50, 1, 10, true),
        ..Round::default()
    };
    std::thread::scope(|scope| {
        let proxy = scope.spawn(|| oha(port, HOST, 1000, 64, 30, true));
        std::thread::sleep(Duration::from_secs(5));
        round.loaded_404 = oha(port, "nobody.test", 50, 1, 20, true);
        round.proxy = proxy.join().unwrap();
    });
    let (fresh, n1) = under_reloads(&running, || oha(port, HOST, 200, 4, 30, false));
    let (kept, n2) = under_reloads(&running, || oha(port, HOST, 200, 4, 30, true));
    round.reload_fresh = fresh;
    round.reload_keepalive = kept;
    round.reloads = n1 + n2;

    // The daemon reloads only when a render wrote something: the same model again writes
    // nothing.
    if variant == Variant::After {
        let target = targets::by_id("nginx").unwrap();
        let (again, _) = target
            .apply(
                &Engine::for_root(&env.root),
                &running.ctx,
                &env.root.etc(),
                false,
            )
            .unwrap();
        assert!(!again.is_empty());
        assert!(
            again.iter().all(|(_, o)| *o == WriteOutcome::Unchanged),
            "{again:?}"
        );
    }
    round
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn summary(label: &str, runs: &[&OhaRun]) -> String {
    let pct = |i: usize| median(runs.iter().map(|r| r.pct[i]).collect());
    let failed: Vec<u64> = runs.iter().map(|r| r.failed()).collect();
    let requests: Vec<u64> = runs.iter().map(|r| r.requests).collect();
    format!(
        "{label:<34} p50 {:>7.2} p90 {:>7.2} p99 {:>7.2} ms | failed {failed:?} of {requests:?}",
        pct(0),
        pct(1),
        pct(2)
    )
}

#[test]
#[ignore = "measurement: real nginx, oha and a quiet machine (--release)"]
fn nginx_proxy_keepalive_latency_and_reload_online() {
    let rounds: usize = std::env::var("FORGEBOX_PERF_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let (upstream, _) = spawn_upstream(true);
    let mut results: BTreeMap<&str, Vec<Round>> = BTreeMap::new();
    for n in 0..rounds {
        for (name, variant) in [("before", Variant::Before), ("after", Variant::After)] {
            let round = measure(variant, upstream);
            println!("round {n} {name}: {round:#?}");
            results.entry(name).or_default().push(round);
        }
    }
    let mut lines = Vec::new();
    for name in ["before", "after"] {
        let r = &results[name];
        let pick = |f: fn(&Round) -> &OhaRun| r.iter().map(f).collect::<Vec<_>>();
        lines.push(format!("--- {name} ({} rounds)", r.len()));
        lines.push(summary("404, idle nginx", &pick(|r| &r.idle_404)));
        lines.push(summary("proxy, 1000 req/s, 64 conns", &pick(|r| &r.proxy)));
        lines.push(summary(
            "404 during the proxy load",
            &pick(|r| &r.loaded_404),
        ));
        lines.push(summary(
            "reloads, new conn per request",
            &pick(|r| &r.reload_fresh),
        ));
        lines.push(summary(
            "reloads, keep-alive clients",
            &pick(|r| &r.reload_keepalive),
        ));
        lines.push(format!(
            "reloads per round: {:?}",
            r.iter().map(|r| r.reloads).collect::<Vec<_>>()
        ));
    }
    println!("{}", lines.join("\n"));

    let after = &results["after"];
    let before = &results["before"];
    for r in after {
        // Every answer is a 2xx (no 502 from a reused upstream connection). A client-side
        // socket error of oha is tolerated at 1 in 10,000: measured once (WSAEINVAL, os
        // error 10022) in 90,000 proxied requests, not reproduced in 100,000 more without
        // an upstream (docs/06 section 4.1.1).
        assert_eq!(r.proxy.non_2xx, 0, "{:?}", r.proxy);
        let transport: u64 = r.proxy.errors.values().sum();
        assert!(transport * 10_000 <= r.proxy.requests, "{:?}", r.proxy);
        assert_eq!(r.loaded_404.failed(), 0, "{:?}", r.loaded_404);
    }
    let p90 = |rs: &[Round]| median(rs.iter().map(|r| r.loaded_404.pct[1]).collect());
    assert!(
        p90(after) < p90(before),
        "404 p90 under proxy load: before {} ms, after {} ms",
        p90(before),
        p90(after)
    );
    let fresh = |rs: &[Round]| rs.iter().map(|r| r.reload_fresh.failed()).sum::<u64>();
    assert!(
        fresh(after) <= fresh(before),
        "failed requests with reloads: before {}, after {}",
        fresh(before),
        fresh(after)
    );
}
