//! `choose_root` against a real `forgeboxd` (M2-E2-T11): the `local` context first points at
//! an uninitialized throwaway root through `root.toml` (in a temporary `FORGE_CONFIG_DIR`,
//! never the real `%LOCALAPPDATA%`); choosing another folder writes the pointer, reconnects
//! the bridge, starts the daemon of the new root, and the daemon creates the layout.
//!
//! One test per binary: the daemon inherits this process's environment, set before any
//! thread exists.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use common::{Recorder, eventually};
use forge_ipc::base::paths::{
    CASE_SENSITIVE_DIRS, DiscoverInputs, ForgeRoot, RootPointer, RootSource,
};
use forgebox_desktop::bridge::{
    Bridge, BridgeOptions, ConnectionSink, ConnectionStatus, LOCAL_CONTEXT,
};
use forgebox_desktop::onboarding::{self, first_run};

const BOOT: Duration = Duration::from_secs(60);

fn plain(path: &Path) -> PathBuf {
    forge_ipc::base::paths::plain_path(path)
}

fn repo_root() -> PathBuf {
    plain(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .unwrap(),
    )
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn daemon_exe() -> PathBuf {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "-p", "forge-daemon", "--bin", "forgeboxd"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("running cargo to build forgeboxd");
    assert!(status.success(), "cargo build -p forge-daemon failed");
    let exe = std::env::current_exe()
        .unwrap()
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join(forge_ipc::client::local::DAEMON_EXE);
    assert!(exe.is_file(), "{} does not exist", exe.display());
    exe
}

/// Kills the daemon of `root` when the test ends, even on panic.
struct Cleanup {
    root: PathBuf,
}

impl Cleanup {
    fn pid(&self) -> Option<u32> {
        let text = std::fs::read_to_string(self.root.join("state/locks/forgeboxd.lock")).ok()?;
        serde_json::from_str::<Value>(&text).ok()?["pid"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(pid) = self.pid() {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

#[test]
fn choosing_a_root_starts_a_real_daemon_that_lays_out_the_root() {
    let exe = daemon_exe();
    let sandbox = tempfile::Builder::new()
        .prefix("fbd-chooseroot-")
        .tempdir()
        .unwrap();
    let base = plain(sandbox.path());
    let config_dir = base.join("cfg");
    // SAFETY: the only test of this binary, before any thread of its own exists; the
    // daemon spawned by the bridge inherits these.
    unsafe {
        std::env::set_var(forge_ipc::client::local::DAEMON_EXE_ENV, &exe);
        std::env::set_var("FORGEBOX_OFFICIAL_REGISTRY", repo_root().join("registry"));
        std::env::set_var("FORGE_CONFIG_DIR", &config_dir);
        std::env::remove_var("FORGEBOX_ROOT");
        std::env::remove_var("RUST_LOG");
    }

    // The previous root: only the pointer, nothing initialized, so the bridge opens
    // `local` as "uninitialized" and starts nothing.
    let old = base.join("old-root");
    std::fs::create_dir_all(&old).unwrap();
    let inputs = DiscoverInputs::from_env(None);
    assert_eq!(inputs.config_dir.as_deref(), Some(config_dir.as_path()));
    RootPointer::write_to(
        &RootPointer::file_for(&inputs).unwrap(),
        &ForgeRoot::new(&old).unwrap(),
    )
    .unwrap();

    // The new root: an existing Forgebox root by its forgebox.toml (a pre-made config with
    // free ports and no DNS) plus `portable.flag` so the daemon writes no user PATH/registry.
    let new_path = base.join("new-root");
    std::fs::create_dir_all(new_path.join("etc")).unwrap();
    std::fs::write(new_path.join("portable.flag"), "").unwrap();
    std::fs::write(
        new_path.join("etc/forgebox.toml"),
        format!(
            "[dns]\nenabled = false\n\n[ports.ranges]\ntls-crl = [{}]\ninternal = [{}]\n",
            free_port(),
            free_port()
        ),
    )
    .unwrap();
    let cleanup = Cleanup {
        root: new_path.clone(),
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let recorder = Arc::new(Recorder::default());
        let bridge = {
            let _guard = tokio::runtime::Handle::current().enter();
            Bridge::new(
                Arc::clone(&recorder) as Arc<dyn ConnectionSink>,
                BridgeOptions::default(),
            )
        };
        let onboarding_file = base.join("gui").join(onboarding::ONBOARDING_FILE);

        // Before: the old root, from root.toml, uninitialized, no daemon.
        let before = first_run(
            &bridge,
            onboarding_file.clone(),
            LOCAL_CONTEXT,
            inputs.clone(),
        )
        .await
        .unwrap();
        assert_eq!(before.source, Some(onboarding::RootOrigin::RootToml));
        assert!(!before.root_initialized && !before.daemon_reachable);
        assert!(before.show_root_step && !before.onboarding_done);

        // A refused choice changes nothing.
        let refused = onboarding::choose_root(&bridge, "relative/path", inputs.clone())
            .await
            .unwrap_err();
        assert_eq!(
            refused.details.as_ref().unwrap()["reason"],
            "path_not_absolute"
        );
        assert_eq!(
            ForgeRoot::discover_with(&inputs).unwrap().0,
            ForgeRoot::new(&old).unwrap()
        );

        // The choice.
        let answer = onboarding::choose_root(&bridge, new_path.to_str().unwrap(), inputs.clone())
            .await
            .unwrap();
        let (discovered, source) = ForgeRoot::discover_with(&inputs).unwrap();
        assert_eq!(source, RootSource::RootToml);
        assert_eq!(discovered, ForgeRoot::new(&new_path).unwrap());
        assert_eq!(answer.root, discovered.to_posix_string());
        assert!(
            eventually(BOOT, || recorder
                .last()
                .is_some_and(|s| s.status == ConnectionStatus::Connected))
            .await,
            "seen {:?}; last {:?}",
            recorder.statuses(),
            recorder.last()
        );

        // Hello succeeded, and the daemon of the NEW root laid the root out.
        let connected = recorder.last().unwrap();
        let hello = connected.hello.as_ref().expect("hello");
        assert!(hello.local);
        assert_eq!(
            connected.endpoint.as_deref(),
            Some(discovered.to_posix_string().as_str())
        );
        for dir in ["www", "etc", "data", "logs", "tmp"] {
            assert!(new_path.join(dir).is_dir(), "{dir}/ was not created");
        }
        let platform = forge_ipc::base::platform::current();
        let cs = platform.case_sensitive_fs();
        if cs.is_supported(&new_path) {
            for dir in CASE_SENSITIVE_DIRS {
                let path = new_path.join(dir);
                if path.is_dir() {
                    assert!(cs.get(&path).unwrap(), "{dir}/ is not case-sensitive");
                }
            }
        } else {
            println!("case-sensitive flag unsupported on this volume; flag check skipped");
        }

        let after = first_run(&bridge, onboarding_file, LOCAL_CONTEXT, inputs.clone())
            .await
            .unwrap();
        assert!(after.root_initialized && after.daemon_reachable);
        assert_eq!(after.root.as_deref(), Some(answer.root.as_str()));
        assert!(!after.show_root_step || !after.onboarding_done);

        bridge.shutdown().await;
    });
    // Stop the daemon and wait for it before the sandbox is removed.
    let pid = cleanup.pid();
    drop(cleanup);
    if let Some(pid) = pid {
        std::thread::sleep(Duration::from_secs(2));
        println!("daemon {pid} killed");
    }
}
