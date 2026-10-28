//! The bridge against a real `forgeboxd` on a temporary portable root (M2-E1-T8):
//! opening the `local` context starts the daemon through `connect_or_start`
//! (connecting → starting → connected), killing it is an unexpected loss that the policy
//! answers with one restart (reconnecting → starting → connected), and `daemon.stop`
//! (`daemon.shutdown {reason: user}`) is a deliberate stop that is never answered.
//!
//! One test per binary: the daemon the bridge spawns inherits this process's environment
//! (`FORGE_DAEMON_EXE`, the offline registry), which is set before any thread exists.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use common::{Recorder, eventually};
use forge_ipc::base::paths::ForgeRoot;
use forgebox_desktop::bridge::{
    Bridge, BridgeOptions, CallArgs, ConnectionSink, ConnectionStatus, Connector, LOCAL_CONTEXT,
    LocalConnector, desktop_client_options,
};

const BOOT: Duration = Duration::from_secs(60);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .map(|p| plain(&p))
        .unwrap()
}

/// `\\?\C:\x` → `C:\x`.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned())
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// `forgeboxd.exe` of this target directory, built first (a no-op when up to date).
fn daemon_exe() -> PathBuf {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "-p", "forge-daemon", "--bin", "forgeboxd"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("running cargo to build forgeboxd");
    assert!(status.success(), "cargo build -p forge-daemon failed");
    // target/debug/deps/bridge_daemon-<hash>.exe → target/debug/forgeboxd.exe
    let exe = std::env::current_exe()
        .unwrap()
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join(forge_ipc::client::local::DAEMON_EXE);
    assert!(exe.is_file(), "{} does not exist", exe.display());
    exe
}

/// Kills whatever daemon serves the root when the test ends, even on panic.
struct Cleanup {
    root: PathBuf,
}

impl Cleanup {
    fn daemon_pid(&self) -> Option<u32> {
        let text = std::fs::read_to_string(self.root.join("state/locks/forgeboxd.lock")).ok()?;
        serde_json::from_str::<Value>(&text).ok()?["pid"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(pid) = self.daemon_pid() {
            kill(pid);
        }
    }
}

fn kill(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn alive(pid: u32) -> bool {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
}

async fn daemon_pid(bridge: &Bridge) -> u32 {
    let status = bridge
        .call(CallArgs {
            context: LOCAL_CONTEXT.into(),
            method: "daemon.status".into(),
            ..CallArgs::default()
        })
        .await
        .unwrap();
    u32::try_from(status["pid"].as_u64().unwrap()).unwrap()
}

#[test]
fn a_real_daemon_is_started_restarted_after_a_kill_and_left_alone_after_a_user_stop() {
    let exe = daemon_exe();
    // SAFETY: the only test of this binary, before any thread of its own is started; the
    // daemon spawned by the bridge inherits these.
    unsafe {
        std::env::set_var(forge_ipc::client::local::DAEMON_EXE_ENV, &exe);
        std::env::set_var("FORGEBOX_OFFICIAL_REGISTRY", repo_root().join("registry"));
        std::env::remove_var("FORGEBOX_ROOT");
        std::env::remove_var("RUST_LOG");
    }
    let dir = tempfile::Builder::new()
        .prefix("fbd-bridge-")
        .tempdir()
        .unwrap();
    let root_path = plain(dir.path());
    std::fs::create_dir_all(root_path.join("etc")).unwrap();
    // Initialized (state/ exists), portable (no user PATH or registry writes), no DNS.
    std::fs::create_dir_all(root_path.join("state")).unwrap();
    std::fs::write(root_path.join("portable.flag"), "").unwrap();
    std::fs::write(
        root_path.join("etc/forgebox.toml"),
        format!(
            "[dns]\nenabled = false\n\n[ports.ranges]\ntls-crl = [{}]\ninternal = [{}]\n",
            free_port(),
            free_port()
        ),
    )
    .unwrap();
    let cleanup = Cleanup {
        root: root_path.clone(),
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = ForgeRoot::new(&root_path).unwrap();
        let connector: Arc<dyn Connector> =
            Arc::new(LocalConnector::with_root(root, desktop_client_options()));
        let recorder = Arc::new(Recorder::default());
        let bridge = Bridge::with_connectors(
            vec![(LOCAL_CONTEXT.to_owned(), connector)],
            Arc::clone(&recorder) as Arc<dyn ConnectionSink>,
            BridgeOptions::default(),
        );
        let wait = |status: ConnectionStatus| {
            let recorder = Arc::clone(&recorder);
            async move {
                assert!(
                    eventually(BOOT, || recorder.last().is_some_and(|s| s.status == status)).await,
                    "status {status:?} expected; seen {:?}; last {:?}",
                    recorder.statuses(),
                    recorder.last()
                );
            }
        };

        // 1. Opening the context starts the daemon (nothing was running).
        let first = bridge.state(LOCAL_CONTEXT).await.unwrap();
        assert_eq!(first.status, ConnectionStatus::Connecting);
        wait(ConnectionStatus::Connected).await;
        let statuses = recorder.statuses();
        assert_eq!(statuses.first(), Some(&ConnectionStatus::Connecting));
        assert!(
            statuses.contains(&ConnectionStatus::Starting),
            "{statuses:?}"
        );
        let connected = recorder.last().unwrap();
        assert!(connected.hello.as_ref().unwrap().local);
        assert!(connected.autostart.initialized);
        let pid = daemon_pid(&bridge).await;
        assert_eq!(cleanup.daemon_pid(), Some(pid));
        println!("daemon {pid} started; statuses {statuses:?}");

        // 2. Kill it: reconnecting, then one automatic restart.
        let seen = recorder.all().len();
        kill(pid);
        wait(ConnectionStatus::Reconnecting).await;
        wait(ConnectionStatus::Connected).await;
        let after_kill: Vec<ConnectionStatus> = {
            let mut s: Vec<_> = recorder.all()[seen..].iter().map(|s| s.status).collect();
            s.dedup();
            s
        };
        assert_eq!(
            after_kill,
            [
                ConnectionStatus::Reconnecting,
                ConnectionStatus::Starting,
                ConnectionStatus::Connected
            ],
            "{after_kill:?}"
        );
        let restarted = daemon_pid(&bridge).await;
        assert_ne!(restarted, pid);
        assert!(!alive(pid));
        assert_eq!(recorder.last().unwrap().autostart.restarts, 1);
        println!("daemon {restarted} restarted after killing {pid}");

        // 3. A user stop is final: no restart.
        let ack = bridge
            .call(CallArgs {
                context: LOCAL_CONTEXT.into(),
                method: "daemon.stop".into(),
                params: Some(json!({})),
                ..CallArgs::default()
            })
            .await
            .unwrap();
        assert_eq!(ack["ok"], true);
        wait(ConnectionStatus::Stopped).await;
        let stopped = recorder.last().unwrap();
        // `user` when the announcement arrived; `exited` when the daemon closed the
        // connection before writing it (it then still removed its lock file).
        let reason = stopped.autostart.stopped_reason.clone();
        assert!(
            matches!(reason.as_deref(), Some("user" | "exited")),
            "{reason:?}"
        );
        println!("stopped on purpose: {reason:?}");
        assert!(
            eventually(Duration::from_secs(20), || !alive(restarted)).await,
            "the daemon exits"
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(
            bridge.state(LOCAL_CONTEXT).await.unwrap().status,
            ConnectionStatus::Stopped
        );
        assert_eq!(recorder.last().unwrap().autostart.restarts, 1);
        assert!(
            cleanup.daemon_pid().is_none_or(|p| !alive(p)),
            "no daemon was started after the user stop"
        );
        bridge.shutdown().await;
    });
    drop(cleanup);
}
