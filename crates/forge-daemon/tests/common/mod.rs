//! Helpers of the integration tests: a temporary installation root, the real
//! `forgeboxd` binary started on it as a separate process, and an API client connected
//! through the named pipe of that root.
#![allow(dead_code)]

pub mod online;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::paths::ForgeRoot;
use forge_ipc::transport::Transport;
use forge_ipc::{Client, ClientOptions, Credentials};

/// The repository's `registry/` (signed with the development key): the official registry
/// of the test daemons, offline.
pub fn repo_registry() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../registry")
        .canonicalize()
        .map(|p| forge_core::paths::plain_path(&p))
        .unwrap()
}

/// Compiles `tests/fixtures/<name>.rs` (no dependencies) with rustc into
/// `CARGO_TARGET_TMPDIR/<name>/<name>-<source hash>.exe` and returns the executable.
///
/// Safe across processes (cargo nextest runs every test in its own process): one
/// executable per source content, linked under a per-process name and published by
/// rename, so no process links over an executable that another one is running.
pub fn compile_fixture(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    // A single file (`name.rs`) or a small project folder (`name/main.rs`).
    let single = fixtures.join(format!("{name}.rs"));
    let source = if single.is_file() {
        single
    } else {
        fixtures.join(name).join("main.rs")
    };
    // FNV-1a of the source: a new executable whenever the fixture changes.
    let hash = std::fs::read(&source)
        .unwrap()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
        });
    let exe = dir.join(format!("{name}-{hash:016x}.exe"));
    if !exe.exists() {
        let partial = dir.join(format!("{name}-{hash:016x}-{}.exe", std::process::id()));
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let output = Command::new(rustc)
            .args(["--edition", "2024", "-O", "-o"])
            .arg(&partial)
            .arg(&source)
            .output()
            .expect("running rustc");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // Another process may have published the same executable meanwhile and may be
        // running it, so it cannot be replaced: keep theirs.
        if let Err(e) = std::fs::rename(&partial, &exe) {
            assert!(exe.exists(), "publishing {}: {e}", exe.display());
            let _ = std::fs::remove_file(&partial);
        }
    }
    exe
}

/// Samples each latency window needs: its p99 then ignores at most two of them.
pub const LATENCY_SAMPLES: usize = 200;

/// No single status answer may take this long while something else is stuck (on a machine
/// whose own noise stays under it): an order of magnitude over the 50 ms budget, yet far
/// under what a daemon waiting on the stuck thing takes (a stop cascade step is seconds
/// long, an actor ask times out after 1 s, a client that never reads never frees anything).
pub const LATENCY_CEILING: Duration = Duration::from_millis(500);

/// How much worse than the windows without the stuck thing (measured right before and
/// after, under the same outside load) the window with it may be when the machine itself
/// is over the budget.
pub const LOAD_FACTOR: u32 = 2;

/// The 99th percentile of `samples`.
pub fn p99(samples: &[Duration]) -> Duration {
    let mut sorted = samples.to_vec();
    sorted.sort();
    let i = (sorted.len() * 99).div_ceil(100).saturating_sub(1);
    sorted.get(i).copied().unwrap_or_default()
}

/// The status-latency budget of docs/03 §7.6 for a window measured while something is
/// stuck, against windows measured right before and after without it.
///
/// The budget is a p99 (`budget`, 50 ms) plus a ceiling for any single answer
/// ([`LATENCY_CEILING`]). On a shared 4-core machine another build can starve every thread
/// of the test for whole scheduler quanta (M2-GATE-B16: p99 100-200 ms, single answers up
/// to 700 ms, with or without the stuck thing), so both limits rise to [`LOAD_FACTOR`]
/// times what the free windows measured: outside load weighs on both sides, while a daemon
/// that waits on the stuck thing is far worse than its own free windows.
#[derive(Debug)]
pub struct LatencyVerdict {
    pub samples: usize,
    pub p99: Duration,
    pub max: Duration,
    pub free_samples: Vec<usize>,
    pub free_p99: Duration,
    pub free_max: Duration,
    pub allowed_p99: Duration,
    pub allowed_max: Duration,
}

impl LatencyVerdict {
    pub fn new(budget: Duration, stuck: &[Duration], free: &[&[Duration]]) -> Self {
        let max_of = |s: &[Duration]| s.iter().max().copied().unwrap_or_default();
        let free_p99 = free.iter().map(|s| p99(s)).max().unwrap_or_default();
        let free_max = free.iter().map(|s| max_of(s)).max().unwrap_or_default();
        Self {
            samples: stuck.len(),
            p99: p99(stuck),
            max: max_of(stuck),
            free_samples: free.iter().map(|s| s.len()).collect(),
            free_p99,
            free_max,
            allowed_p99: budget.max(free_p99 * LOAD_FACTOR),
            allowed_max: LATENCY_CEILING.max(free_max * LOAD_FACTOR),
        }
    }

    pub fn ok(&self) -> bool {
        self.samples >= LATENCY_SAMPLES
            && self.free_samples.iter().all(|&n| n >= LATENCY_SAMPLES)
            && self.p99 < self.allowed_p99
            && self.max < self.allowed_max
    }
}

impl std::fmt::Display for LatencyVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} samples: p99 {:?} (allowed {:?}), max {:?} (allowed {:?}); without it {:?} \
             samples: p99 {:?}, max {:?}",
            self.samples,
            self.p99,
            self.allowed_p99,
            self.max,
            self.allowed_max,
            self.free_samples,
            self.free_p99,
            self.free_max
        )
    }
}

/// `n` distinct free TCP ports, as a TOML list body. Used for a private `databases` range so
/// parallel test processes never reserve (and then fail to bind) each other's ports.
pub fn free_ports(n: usize) -> String {
    let listeners: Vec<_> = (0..n)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    listeners
        .iter()
        .map(|l| l.local_addr().unwrap().port().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A free TCP port of 127.0.0.1 right now.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A temporary root with a `forgebox.toml` fit for tests: no DNS resolver (port 53 is
/// machine-wide), a private CRL port, no parked folder scanning surprises.
pub struct TestRoot {
    pub dir: tempfile::TempDir,
    pub root: ForgeRoot,
}

impl TestRoot {
    pub fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("fbd-").tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        std::fs::create_dir_all(root.etc()).unwrap();
        // Private CRL and internal-endpoint ports: tests run in parallel.
        let toml = format!(
            "[dns]\nenabled = false\n\n[ports.ranges]\ntls-crl = [{}]\ninternal = [{}]\ndatabases = [{}]\n",
            free_port(),
            free_port(),
            free_ports(16)
        );
        std::fs::write(root.forgebox_toml(), toml).unwrap();
        Self { dir, root }
    }

    pub fn path(&self) -> &Path {
        self.root.path()
    }

    /// Files left in `state/locks/`.
    pub fn lock_files(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.root.state_locks())
            .map(|e| e.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    }

    /// Every `*.lock` file under the root.
    pub fn stray_lock_files(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "lock") {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        walk(self.path(), &mut out);
        out
    }
}

/// A running `forgeboxd`.
pub struct DaemonProcess {
    pub child: Child,
    pub log: PathBuf,
    pub started: Instant,
}

impl DaemonProcess {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Waits for the process to exit, at most `timeout`.
    pub fn wait_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn output(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Starts `forgeboxd` on `root` (stdout/stderr into a file next to the root).
pub fn spawn_daemon(root: &TestRoot, extra: &[&str]) -> DaemonProcess {
    let log = root
        .dir
        .path()
        .with_extension(format!("{}.out", std::process::id()));
    let out = std::fs::File::create(&log).unwrap();
    let err = out.try_clone().unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_forgeboxd"))
        .arg("--root")
        .arg(root.path())
        .args(["--log-level", "info", "--shutdown-timeout", "20"])
        .args(extra)
        .env("FORGEBOX_OFFICIAL_REGISTRY", repo_registry())
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .expect("starting forgeboxd");
    DaemonProcess {
        child,
        log,
        started: Instant::now(),
    }
}

pub fn options() -> ClientOptions {
    ClientOptions {
        call_timeout: Duration::from_secs(20),
        connect_timeout: Duration::from_secs(5),
        reconnect_min: Duration::from_millis(20),
        reconnect_max: Duration::from_millis(200),
        ..ClientOptions::default()
    }
}

/// Connects (retrying while the daemon boots) and authenticates; returns the client and
/// how long it took from the process start.
pub async fn connect(
    root: &TestRoot,
    daemon: &mut DaemonProcess,
    timeout: Duration,
) -> (Client, Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(Some(status)) = daemon.child.try_wait() {
            panic!(
                "forgeboxd exited during boot ({status}):\n{}",
                daemon.output()
            );
        }
        if root.root.ipc_token().is_file() {
            if let Ok(credentials) =
                Credentials::local(&root.root, "forge-daemon-test", env!("CARGO_PKG_VERSION"))
            {
                let transport = forge_ipc::transport::local_transport(&root.root);
                if let Ok(client) = Client::connect(transport, credentials, options()).await {
                    return (client, daemon.started.elapsed());
                }
            }
        }
        if Instant::now() >= deadline {
            panic!(
                "forgeboxd did not answer within {timeout:?}:\n{}",
                daemon.output()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Whether the named pipe of `root` still accepts connections.
pub async fn pipe_exists(root: &ForgeRoot) -> bool {
    let transport: Arc<dyn Transport> = forge_ipc::transport::local_transport(root);
    transport.connect().await.is_ok()
}

/// Working set (KiB) of `pid` from `tasklist`; `None` when it is not running.
pub fn working_set_kib(pid: u32) -> Option<u64> {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains(&format!("\"{pid}\"")))?;
    let mem = line.rsplit("\",\"").next()?;
    let digits: String = mem.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// PIDs of the processes named `image` whose executable lives under `dir` (a test root).
/// Unlike [`pids_of`] it ignores same-named processes of other tests running in parallel.
pub fn pids_under(image: &str, dir: &Path) -> Vec<u32> {
    let dir = dir.to_string_lossy().replace('/', "\\").to_lowercase();
    let script = format!(
        "Get-CimInstance Win32_Process -Filter \"Name='{image}'\" | ForEach-Object {{ \"$($_.ProcessId)|$($_.ExecutablePath)\" }}"
    );
    let Ok(out) = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (pid, path) = l.trim().split_once('|')?;
            let path = path.replace('/', "\\").to_lowercase();
            // The root may be spelled 8.3 or with a verbatim prefix: compare on the unique
            // temp folder name.
            let leaf = dir.rsplit('\\').next().unwrap_or(&dir);
            path.contains(&format!("\\{leaf}\\"))
                .then(|| pid.parse().ok())?
        })
        .collect()
}

/// PIDs of the processes named `image` (`forgeboxd.exe`) from `tasklist`.
pub fn pids_of(image: &str) -> Vec<u32> {
    let Ok(out) = Command::new("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {image}"), "/FO", "CSV", "/NH"])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut cols = l.split("\",\"");
            let _name = cols.next()?;
            cols.next()?.trim_matches('"').parse().ok()
        })
        .collect()
}

/// Processes (any image) whose parent is one of `parents`, with their image name.
pub fn children_of(parents: &[u32]) -> Vec<(u32, String)> {
    if parents.is_empty() {
        return Vec::new();
    }
    let filter = parents
        .iter()
        .map(|p| format!("ParentProcessId={p}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let script = format!(
        "Get-CimInstance Win32_Process -Filter \"{filter}\" | ForEach-Object {{ \"$($_.ProcessId) $($_.Name)\" }}"
    );
    let Ok(out) = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (pid, name) = l.trim().split_once(' ')?;
            Some((pid.parse().ok()?, name.to_owned()))
        })
        .collect()
}

/// Stops the daemon through the API and waits for the process to exit (the client stays
/// open: its subscriptions still deliver what was sent before the pipe closed).
pub async fn stop(
    client: &Client,
    daemon: &mut DaemonProcess,
    timeout: Duration,
) -> (ExitStatus, Duration) {
    use forge_ipc::api::methods::daemon_stop;
    let asked = Instant::now();
    client
        .call::<daemon_stop>(forge_ipc::api::DaemonStopParams::default())
        .await
        .expect("daemon.stop");
    let status = daemon.wait_exit(timeout).unwrap_or_else(|| {
        panic!(
            "forgeboxd did not exit within {timeout:?}:\n{}",
            daemon.output()
        )
    });
    (status, asked.elapsed())
}

/// The daemon's JSON log of `root` (for diagnostics), one `ts level message` per line.
pub fn daemon_log(root: &TestRoot) -> String {
    let mut out = String::new();
    for file in forge_daemon::logging::log_files(&root.root.logs()) {
        for line in std::fs::read_to_string(file).unwrap_or_default().lines() {
            let Ok(json) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            out.push_str(&format!(
                "{} {} {} {}\n",
                json["timestamp"].as_str().unwrap_or_default(),
                json["level"].as_str().unwrap_or_default(),
                json["fields"]["message"].as_str().unwrap_or_default(),
                json["fields"]
            ));
        }
    }
    out
}

/// Prints [`daemon_log`] when `FORGEBOX_TEST_SHOW_LOG` is set.
pub fn show_log(root: &TestRoot) {
    if std::env::var_os("FORGEBOX_TEST_SHOW_LOG").is_some() {
        println!("{}", daemon_log(root));
    }
}
