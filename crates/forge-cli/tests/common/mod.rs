//! Harness of the integration tests: a temporary installation root, a real `forgeboxd`
//! started on it, and the real `forge` binary run against it with `--root`.
//!
//! * Every `forge` run has a hard timeout of [`RUN_TIMEOUT`] (30 s): a command that hangs
//!   fails the test instead of the build.
//! * Colors are off (`NO_COLOR`), the client profiles live in a temporary folder
//!   (`FORGE_CONFIG_DIR`) and the daemon uses the repository's offline registry.
//! * `forgeboxd` is the one built next to `forge` (`cargo build -p forge-daemon` runs once
//!   per test process so it is never stale).
#![allow(dead_code)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::Value;

/// A `forge` run that lasts longer than this is a hang.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// The repository's `registry/` (signed with the development key), offline.
pub fn repo_registry() -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../registry")
        .canonicalize()
        .expect("registry/ of the repository");
    plain(&path)
}

/// `\\?\C:\x` -> `C:\x`.
pub fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned())
}

/// A free TCP port of 127.0.0.1 right now.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub fn forge_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_forge"))
}

/// `forgeboxd.exe` next to `forge.exe`, rebuilt first (a no-op when up to date).
pub fn daemon_exe() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "-p", "forge-daemon", "--bin", "forgeboxd"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .expect("running cargo to build forgeboxd");
        assert!(status.success(), "cargo build -p forge-daemon failed");
        let exe = forge_exe().with_file_name("forgeboxd.exe");
        assert!(exe.is_file(), "{} does not exist", exe.display());
        exe
    })
    .clone()
}

/// A temporary installation root and a temporary folder for the client's own files.
pub struct TestRoot {
    pub dir: tempfile::TempDir,
    pub config: tempfile::TempDir,
    pub path: PathBuf,
}

impl TestRoot {
    /// `portable`: a `portable.flag` keeps the daemon away from the user `PATH`.
    pub fn new(portable: bool) -> Self {
        let dir = tempfile::Builder::new().prefix("fbc-").tempdir().unwrap();
        let config = tempfile::Builder::new().prefix("fbcfg-").tempdir().unwrap();
        let path = plain(dir.path());
        std::fs::create_dir_all(path.join("etc")).unwrap();
        // No DNS resolver (port 53 is machine-wide); private CRL and internal ports.
        let toml = format!(
            "[dns]\nenabled = false\n\n[ports.ranges]\ntls-crl = [{}]\ninternal = [{}]\n",
            free_port(),
            free_port()
        );
        std::fs::write(path.join("etc").join("forgebox.toml"), toml).unwrap();
        if portable {
            std::fs::write(path.join("portable.flag"), "").unwrap();
        }
        Self { dir, config, path }
    }

    /// `<root>/www/<name>`, created.
    pub fn www(&self, name: &str) -> PathBuf {
        let dir = self.path.join("www").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A `forge` invocation against this root.
    pub fn forge(&self) -> Forge<'_> {
        Forge {
            root: self,
            env: Vec::new(),
            envs_removed: Vec::new(),
            with_root: true,
        }
    }

    /// Runs `forge --root <root> <args>`.
    pub fn run(&self, args: &[&str]) -> Run {
        self.forge().run(args)
    }

    /// Runs `forge --json ...` and parses stdout.
    pub fn json(&self, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let run = self.run(&all);
        run.assert_success();
        run.json()
    }

    /// The pid in `state/locks/forgeboxd.lock`, if a daemon described itself there.
    pub fn daemon_pid(&self) -> Option<u32> {
        let text = std::fs::read_to_string(self.path.join("state/locks/forgeboxd.lock")).ok()?;
        serde_json::from_str::<Value>(&text).ok()?["pid"]
            .as_u64()
            .map(|p| p as u32)
    }

    /// Stops whatever daemon serves this root: `forge daemon stop`, then `taskkill`.
    pub fn stop_daemon(&self) {
        let _ = self.forge().run(&["daemon", "stop"]);
        if let Some(pid) = self.daemon_pid() {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// Command builder for `forge`.
pub struct Forge<'a> {
    root: &'a TestRoot,
    env: Vec<(String, String)>,
    envs_removed: Vec<String>,
    with_root: bool,
}

impl Forge<'_> {
    pub fn env(mut self, key: &str, value: impl Into<String>) -> Self {
        self.env.push((key.to_owned(), value.into()));
        self
    }

    pub fn env_remove(mut self, key: &str) -> Self {
        self.envs_removed.push(key.to_owned());
        self
    }

    /// Without `--root` (to test discovery).
    pub fn without_root(mut self) -> Self {
        self.with_root = false;
        self
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(forge_exe());
        if self.with_root {
            command.arg("--root").arg(&self.root.path);
        }
        command
            .args(args)
            .env("NO_COLOR", "1")
            .env("FORGE_CONFIG_DIR", self.root.config.path())
            .env("FORGEBOX_OFFICIAL_REGISTRY", repo_registry())
            .env_remove("FORGE_LANG")
            .env_remove("FORGEBOX_ROOT")
            .env_remove("CLICOLOR_FORCE")
            .env_remove("RUST_LOG")
            .env_remove("FORGE_DAEMON_EXE")
            .env("FORGE_EDITOR", "cmd /c rem")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for key in &self.envs_removed {
            command.env_remove(key);
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }

    /// Runs to completion (or kills at [`RUN_TIMEOUT`] and fails the test).
    pub fn run(&self, args: &[&str]) -> Run {
        let _ = daemon_exe();
        let started = Instant::now();
        let mut child = self.command(args).spawn().expect("starting forge");
        let stdout = drain(child.stdout.take().unwrap());
        let stderr = drain(child.stderr.take().unwrap());
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() >= RUN_TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "`forge {}` did not finish within {RUN_TIMEOUT:?}",
                    args.join(" ")
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let elapsed = started.elapsed();
        Run {
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            code: status.code().unwrap_or(-1),
            stdout: stdout.finish(args),
            stderr: stderr.finish(args),
            elapsed,
        }
    }

    /// Starts `forge` and leaves it running (streams): the caller reads and kills.
    pub fn spawn(&self, args: &[&str]) -> Streaming {
        let _ = daemon_exe();
        let mut child = self.command(args).spawn().expect("starting forge");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Streaming { child, lines: rx }
    }
}

/// A pipe read to its end on a thread.
struct Drain(std::sync::mpsc::Receiver<String>);

fn drain(mut pipe: impl Read + Send + 'static) -> Drain {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        let _ = tx.send(String::from_utf8_lossy(&bytes).into_owned());
    });
    Drain(rx)
}

impl Drain {
    /// The text, which must be complete soon after the process exited: a pipe that stays
    /// open means a child of `forge` (the autostarted daemon) inherited it, and then
    /// `forge status | more` would hang in a real shell too.
    fn finish(self, args: &[&str]) -> String {
        self.0.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| {
            panic!(
                "`forge {}` exited but its output pipe is still open: a child process inherited it",
                args.join(" ")
            )
        })
    }
}

/// The outcome of one run.
#[derive(Debug)]
pub struct Run {
    pub args: Vec<String>,
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}

impl Run {
    pub fn command_line(&self) -> String {
        format!("forge {}", self.args.join(" "))
    }

    pub fn assert_success(&self) {
        assert_eq!(self.code, 0, "{} failed: {self:#?}", self.command_line());
    }

    /// stdout as one JSON document.
    pub fn json(&self) -> Value {
        serde_json::from_str(self.stdout.trim()).unwrap_or_else(|e| {
            panic!(
                "{} printed invalid JSON ({e}): {self:#?}",
                self.command_line()
            )
        })
    }

    /// `error.code` of a failed `--json` run.
    pub fn error_code(&self) -> String {
        self.json()["error"]["code"]
            .as_str()
            .unwrap_or_else(|| panic!("no error.code in {self:#?}"))
            .to_owned()
    }

    /// Neither a Rust panic nor a crash.
    pub fn assert_no_panic(&self) {
        let text = format!("{}{}", self.stdout, self.stderr);
        assert!(
            !text.contains("panicked at") && !text.contains("RUST_BACKTRACE") && self.code != 101,
            "{} panicked: {self:#?}",
            self.command_line()
        );
    }
}

/// A `forge` process that streams lines.
pub struct Streaming {
    pub child: Child,
    pub lines: std::sync::mpsc::Receiver<String>,
}

impl Streaming {
    /// Next stdout line within `timeout`.
    pub fn next_line(&self, timeout: Duration) -> Option<String> {
        self.lines.recv_timeout(timeout).ok()
    }

    /// Kills the process and returns whether it was still running.
    pub fn kill(&mut self) -> bool {
        let running = matches!(self.child.try_wait(), Ok(None));
        let _ = self.child.kill();
        let _ = self.child.wait();
        running
    }
}

impl Drop for Streaming {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A `forgeboxd` started directly (not by the CLI) on a root.
pub struct DaemonProcess {
    pub child: Child,
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Starts `forgeboxd` on `root` and waits until `forge daemon status` answers.
pub fn start_daemon(root: &TestRoot) -> DaemonProcess {
    let log = root.config.path().join("daemon.out");
    let out = std::fs::File::create(&log).unwrap();
    let err = out.try_clone().unwrap();
    let mut child = Command::new(daemon_exe())
        .arg("--root")
        .arg(&root.path)
        .args(["--log-level", "info", "--shutdown-timeout", "20"])
        .env("FORGEBOX_OFFICIAL_REGISTRY", repo_registry())
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .expect("starting forgeboxd");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            panic!(
                "forgeboxd exited during boot ({status}): {}",
                std::fs::read_to_string(&log).unwrap_or_default()
            );
        }
        let status = root.run(&["--no-daemon", "--json", "daemon", "status"]);
        if status.code == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "forgeboxd did not answer in time: {}",
            std::fs::read_to_string(&log).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    DaemonProcess { child }
}
