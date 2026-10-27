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
/// `CARGO_TARGET_TMPDIR/<name>/<name>.exe` and returns the executable.
pub fn compile_fixture(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join(format!("{name}.exe"));
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.rs"));
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .args(["--edition", "2024", "-O", "-o"])
        .arg(&exe)
        .arg(&source)
        .output()
        .expect("running rustc");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    exe
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
            "[dns]\nenabled = false\n\n[ports.ranges]\ntls-crl = [{}]\ninternal = [{}]\n",
            free_port(),
            free_port()
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
