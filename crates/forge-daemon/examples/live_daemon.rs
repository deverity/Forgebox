//! `forgeboxd` with fake system backends, for the E3 live suite (`scripts/gui-e2e.ps1 -Suite e3`,
//! M2-E3-T21): the same boot as `src/main.rs`, but the certificate store is a
//! `MemoryTrustStore` and the UAC prompt a `FakeElevation`, so the doctor "Fix" flow
//! (`tls.ca_install`) runs end to end without ever writing a Windows certificate store, NRPT or
//! HKCU. The GUI starts it through `FORGE_DAEMON_EXE`. Development only: nothing here ships.
//!
//!   cargo build -p forge-daemon --example live_daemon

use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use forge_core::config::ForgeConfig;
use forge_core::helper::FakeElevation;
use forge_core::paths::{ForgeRoot, InitOptions};
use forge_core::platform::{self, MemoryTrustStore, TrustStore};
use forge_core::runtime::{RuntimeConfig, build_runtime};
use forge_daemon::args::Args;
use forge_daemon::boot::{self, BootOptions};
use forge_daemon::daemon::ShutdownRequest;
use forge_daemon::instance_lock::InstanceLock;
use forge_daemon::{exit, logging, shutdown};

fn main() {
    std::process::exit(run(Args::parse()));
}

fn run(args: Args) -> i32 {
    let root = match ForgeRoot::discover(args.root.clone()).and_then(|(root, _)| {
        std::fs::create_dir_all(root.path())
            .map_err(|e| forge_core::ForgeError::io_at("creating", root.path(), e))?;
        ForgeRoot::new(platform::long_path(root.path()))
    }) {
        Ok(root) => root,
        Err(err) => {
            eprintln!("live_daemon: root: {err}");
            return exit::FAILED;
        }
    };
    let lock = match InstanceLock::acquire(&root) {
        Ok(lock) => lock,
        Err(err) if err.code() == "conflict" => return exit::ALREADY_RUNNING,
        Err(err) => {
            eprintln!("live_daemon: lock: {err}");
            return exit::FAILED;
        }
    };
    let prepared = ForgeConfig::load_or_default(&root.forgebox_toml()).and_then(|config| {
        let report = root.init(InitOptions {
            case_sensitive: config.case_sensitive,
        })?;
        Ok((config, report))
    });
    let (config, report) = match prepared {
        Ok(prepared) => prepared,
        Err(err) => {
            eprintln!("live_daemon: root folders: {err}");
            return exit::FAILED;
        }
    };
    let _logs = match logging::init(&root.logs(), &args.log_level, args.foreground) {
        Ok(guard) => guard,
        Err(err) => {
            eprintln!("live_daemon: logs: {err}");
            return exit::USAGE;
        }
    };
    let runtime = match build_runtime(&RuntimeConfig::default().thread_name("forgeboxd")) {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("live_daemon: runtime: {err}");
            return exit::FAILED;
        }
    };
    let code = runtime.block_on(async move {
        let options = BootOptions {
            platform: Some(
                platform::current().with_elevation(Arc::new(FakeElevation::accepting())),
            ),
            trust_store: Some(Arc::new(|helper| {
                Arc::new(MemoryTrustStore::new(Some(helper))) as Arc<dyn TrustStore>
            })),
            ..BootOptions::default()
        };
        let booted = match boot::boot_with(args, root, &lock, config, report, options).await {
            Ok(booted) => booted,
            Err(err) => {
                tracing::error!(%err, "boot failed");
                return exit::FAILED;
            }
        };
        let daemon = booted.daemon.clone();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => daemon.shutdown.request(ShutdownRequest {
                reason: "signal".into(),
                restart: false,
                keep_services: false,
            }),
            () = daemon.shutdown.wait() => {}
        }
        drop(daemon);
        shutdown::run(booted, lock).await;
        exit::OK
    });
    runtime.shutdown_timeout(Duration::from_secs(5));
    code
}
