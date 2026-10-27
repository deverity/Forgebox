//! Shared helpers of the app tests (Windows): a real supervisor, a real port allocator, a
//! Node runtime installed as `bin/nodejs/20.12.2`, and raw HTTP/WebSocket clients.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::actor::ActorSupervisor;
use forge_core::config::PortsConfig;
use forge_core::event::EventBus;
use forge_core::ids::SiteId;
use forge_core::paths::{ForgeRoot, InitOptions};
use forge_core::platform::{ProcessControlOptions, port_table, process_control};
use forge_core::store::Store;
use forge_manifest::Manifest;
use forge_net::PortAllocator;
use forge_render::Engine;
use forge_sites::apps::{AppManager, AppSite, RuntimeCatalog, RuntimeTool};
use forge_supervisor::{JobTree, Supervisor};

/// Node 20 installed on this machine, used as the runtime of the tests.
pub const NODE_DIR: &str = r"C:\Program Files\node-20";
pub const NODE_VERSION: &str = "20.12.2";

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("apps")
        .join(name)
}

/// `forgebox-ctrl.exe` next to the test executables' `deps` folder (built with the
/// workspace); built on demand otherwise.
fn ctrl_helper() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let helper = dir.join("forgebox-ctrl.exe");
    if !helper.is_file() {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let status = Command::new(cargo)
            .args([
                "build",
                "--quiet",
                "-p",
                "forge-supervisor",
                "--bin",
                "forgebox-ctrl",
            ])
            .env_remove("CARGO_MAKEFLAGS")
            .status()
            .expect("cargo build forgebox-ctrl");
        assert!(
            status.success() && helper.is_file(),
            "forgebox-ctrl.exe missing"
        );
    }
    helper
}

/// The tests of one binary share the machine's port range and the TCP table; each has its
/// own in-memory store, so they run one at a time (two allocators would both pick 3000).
static SERIAL: std::sync::OnceLock<Arc<tokio::sync::Mutex<()>>> = std::sync::OnceLock::new();

pub struct Harness {
    pub _serial: tokio::sync::OwnedMutexGuard<()>,
    pub _tmp: tempfile::TempDir,
    pub root: ForgeRoot,
    pub manager: AppManager,
    pub supervisor: Supervisor,
    pub allocator: PortAllocator,
    pub _store: Store,
    junction: Option<PathBuf>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Removing a junction with `remove_dir` never touches its target.
        if let Some(link) = self.junction.take() {
            let _ = std::fs::remove_dir(link);
        }
    }
}

/// The long form of the temp dir (nginx rejects 8.3 names such as `GUSTAV~1`).
pub fn long_temp_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("fb-apps-")
        .tempdir()
        .unwrap()
}

/// Real Node runtime as `bin/nodejs/20.12.2` (a junction to the machine's Node 20, so
/// `npm.cmd` is real too). `None` when Node 20 is not installed there.
pub async fn harness() -> Option<Harness> {
    let serial = Arc::clone(SERIAL.get_or_init(|| Arc::new(tokio::sync::Mutex::new(()))))
        .lock_owned()
        .await;
    if !Path::new(NODE_DIR).join("node.exe").is_file() {
        eprintln!("skipping: {NODE_DIR} not found");
        return None;
    }
    let tmp = long_temp_dir();
    let long = std::fs::canonicalize(tmp.path()).unwrap();
    let root = ForgeRoot::new(forge_core::paths::plain_path(&long)).unwrap();
    root.init(InitOptions::default()).unwrap();
    let tool_dir = root.bin().join("nodejs");
    std::fs::create_dir_all(&tool_dir).unwrap();
    let link = tool_dir.join(NODE_VERSION);
    let made = Command::new(r"C:\Windows\System32\cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&link)
        .arg(NODE_DIR)
        .output()
        .expect("cmd.exe");
    assert!(
        made.status.success(),
        "mklink /J failed: {}",
        String::from_utf8_lossy(&made.stdout)
    );

    let manifest = forge_manifest::load_str(include_str!("../../../../registry/tools/nodejs.yaml"))
        .expect("nodejs manifest");
    let Manifest::Tool(tool) = manifest else {
        panic!("nodejs.yaml is not a tool");
    };
    let catalog = RuntimeCatalog::new(root.clone(), [RuntimeTool::from_manifest(&tool)]);

    let store = Store::open_in_memory().unwrap();
    let allocator = PortAllocator::new(&store, port_table(), PortsConfig::default());
    let processes = process_control(ProcessControlOptions::with_ctrl_break_helper(ctrl_helper()));
    let bus = EventBus::with_capacity(65_536);
    let supervisor = Supervisor::new(JobTree::new(processes).unwrap(), ActorSupervisor::new(bus));
    let manager = AppManager::new(
        supervisor.clone(),
        root.clone(),
        catalog,
        Arc::new(allocator.clone()),
        Engine::for_root(&root),
    );
    Some(Harness {
        _serial: serial,
        _tmp: tmp,
        root,
        manager,
        supervisor,
        allocator,
        _store: store,
        junction: Some(link),
    })
}

impl Harness {
    /// A project folder under `www/` with the test server copied in.
    pub fn project(&self, name: &str) -> PathBuf {
        let dir = self.root.www().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let apps = fixture("");
        for file in ["server.js", "package.json"] {
            std::fs::copy(apps.join(file), dir.join(file)).unwrap();
        }
        dir
    }

    /// `AppSite` for `host` over `dir` with the processes given as a YAML map.
    pub fn site(&self, host: &str, dir: &Path, processes_yaml: &str) -> AppSite {
        AppSite {
            id: SiteId::new(),
            host: host.to_owned(),
            aliases: Vec::new(),
            dir: dir.to_path_buf(),
            env: [("APP_ENV".to_owned(), "local".to_owned())].into(),
            processes: serde_yaml_ng::from_str(processes_yaml).unwrap(),
            runtimes: Vec::new(),
            extra_path: Vec::new(),
        }
    }
}

/// Result of [`http_get`].
#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: String,
    pub body: String,
}

fn connect(port: u16) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    stream
}

/// `GET path` with `Host: host` over a fresh connection.
pub fn http_get(port: u16, host: &str, path: &str) -> HttpResponse {
    let mut stream = connect(port);
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    HttpResponse {
        status,
        headers: head.to_ascii_lowercase(),
        body: body.to_owned(),
    }
}

/// Opens a WebSocket (`Upgrade` through whatever is listening on `port`), sends `message`
/// as a masked text frame and returns the echo.
pub fn ws_echo(port: u16, host: &str, path: &str, message: &str) -> String {
    let mut stream = connect(port);
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
    .unwrap();
    // Response head up to the blank line (nothing of the first frame can be in it).
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("handshake response");
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
    assert!(head.starts_with("http/1.1 101"), "no upgrade: {head}");
    assert!(head.contains("sec-websocket-accept:"), "{head}");

    let payload = message.as_bytes();
    assert!(payload.len() < 126);
    let mask = [0x12_u8, 0x34, 0x56, 0x78];
    let mut frame = vec![0x81, 0x80 | u8::try_from(payload.len()).unwrap()];
    frame.extend_from_slice(&mask);
    frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    stream.write_all(&frame).unwrap();

    let mut header = [0_u8; 2];
    stream.read_exact(&mut header).expect("echo frame header");
    assert_eq!(header[0], 0x81, "text frame expected");
    let len = usize::from(header[1] & 0x7f);
    assert!(
        header[1] & 0x80 == 0 && len < 126,
        "server frames are unmasked"
    );
    let mut echo = vec![0_u8; len];
    stream.read_exact(&mut echo).expect("echo frame payload");
    // Orderly close.
    let _ = stream.write_all(&[0x88, 0x80, 0, 0, 0, 0]);
    String::from_utf8(echo).unwrap()
}

/// Whether something accepts connections on `port`.
pub fn is_listening(port: u16) -> bool {
    TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(500),
    )
    .is_ok()
}

/// Polls `check` every 100 ms until it holds or `timeout` passes.
pub fn wait_until(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
