//! `forgeboxd`: the Forgebox daemon. Order of a run: command line → root discovery →
//! single-instance mutex (a second daemon of the same root fails here, fast, exit 4) →
//! root folders and preferences → logs → tokio runtime → boot (docs/01 §5.1) → wait for
//! Ctrl+C / console close / `daemon.stop` → graceful shutdown → exit 0.

use std::time::{Duration, Instant};

use clap::Parser;
use forge_core::config::ForgeConfig;
use forge_core::paths::{ForgeRoot, InitOptions, InitReport};
use forge_core::runtime::{RuntimeConfig, build_runtime};
use forge_daemon::args::Args;
use forge_daemon::daemon::ShutdownRequest;
use forge_daemon::instance_lock::{InstanceLock, mutex_name};
use forge_daemon::{boot, exit, logging, messages, shutdown};

fn main() {
    let args = Args::parse();
    std::process::exit(run(args));
}

/// The root folder exists and is spelled with long names: every path rendered into a
/// server config derives from it, and the Windows nginx refuses 8.3 short names
/// (docs/03 §3.1).
fn long_root(root: ForgeRoot) -> forge_core::Result<ForgeRoot> {
    std::fs::create_dir_all(root.path())
        .map_err(|e| forge_core::ForgeError::io_at("creating", root.path(), e))?;
    ForgeRoot::new(forge_core::platform::long_path(root.path()))
}

fn prepare_root(root: &ForgeRoot) -> forge_core::Result<(ForgeConfig, InitReport)> {
    let config = ForgeConfig::load_or_default(&root.forgebox_toml())?;
    let report = root.init(InitOptions {
        case_sensitive: config.case_sensitive,
    })?;
    Ok((config, report))
}

fn run(args: Args) -> i32 {
    if args.self_update.is_some() {
        eprintln!("{}", messages::not_available("--self-update", "M4-E1"));
        return exit::USAGE;
    }
    if args.listen.is_some() {
        eprintln!("{}", messages::not_available("--listen", "M6-E3"));
        return exit::USAGE;
    }
    let root = match ForgeRoot::discover(args.root.clone()).and_then(|(root, _)| long_root(root)) {
        Ok(root) => root,
        Err(err) => {
            eprintln!("{}", messages::root_failed("?", &err.to_string()));
            return exit::FAILED;
        }
    };
    // 1. Single instance, before touching anything else.
    let lock = match InstanceLock::acquire(&root) {
        Ok(lock) => lock,
        Err(err) if err.code() == "conflict" => {
            eprintln!(
                "{}",
                messages::already_running(&root.to_posix_string(), &mutex_name(&root))
            );
            return exit::ALREADY_RUNNING;
        }
        Err(err) => {
            eprintln!("{}", messages::boot_failed(&err.to_string()));
            return exit::FAILED;
        }
    };
    let (config, report) = match prepare_root(&root) {
        Ok(prepared) => prepared,
        Err(err) => {
            eprintln!(
                "{}",
                messages::root_failed(&root.to_posix_string(), &err.to_string())
            );
            return exit::FAILED;
        }
    };
    let _logs = match logging::init(&root.logs(), &args.log_level, args.foreground) {
        Ok(guard) => guard,
        Err(err) => {
            eprintln!("{}", messages::boot_failed(&err.to_string()));
            return exit::USAGE;
        }
    };
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        root = %root,
        "forgeboxd starting"
    );
    let runtime = match build_runtime(&RuntimeConfig::default().thread_name("forgeboxd")) {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!(%err, "tokio runtime");
            eprintln!("{}", messages::boot_failed(&err.to_string()));
            return exit::FAILED;
        }
    };
    let code = runtime.block_on(serve(args, root, lock, config, report));
    runtime.shutdown_timeout(Duration::from_secs(5));
    code
}

/// Resolves on Ctrl+C, Ctrl+Break, console close, logoff or system shutdown.
async fn signal() {
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        let (Ok(mut c), Ok(mut b), Ok(mut close), Ok(mut shutdown)) = (
            windows::ctrl_c(),
            windows::ctrl_break(),
            windows::ctrl_close(),
            windows::ctrl_shutdown(),
        ) else {
            std::future::pending::<()>().await;
            return;
        };
        tokio::select! {
            _ = c.recv() => {}
            _ = b.recv() => {}
            _ = close.recv() => {}
            _ = shutdown.recv() => {}
        }
    }
    #[cfg(not(windows))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn serve(
    args: Args,
    root: ForgeRoot,
    lock: InstanceLock,
    config: ForgeConfig,
    report: InitReport,
) -> i32 {
    let started = Instant::now();
    let booted = match boot::boot(args.clone(), root.clone(), &lock, config, report).await {
        Ok(booted) => booted,
        Err(err) => {
            tracing::error!(%err, "boot failed");
            eprintln!("{}", messages::boot_failed(&err.to_string()));
            return if err.code() == "conflict" {
                exit::ALREADY_RUNNING
            } else {
                exit::FAILED
            };
        }
    };
    if args.foreground {
        println!(
            "{}",
            messages::ready(&booted.daemon.endpoint, started.elapsed().as_millis())
        );
    }
    let daemon = booted.daemon.clone();
    tokio::select! {
        () = signal() => daemon.shutdown.request(ShutdownRequest {
            reason: "signal".into(),
            restart: false,
            keep_services: false,
        }),
        () = daemon.shutdown.wait() => {}
    }
    let restart = daemon.shutdown.requested().is_some_and(|r| r.restart);
    drop(daemon);
    let report = shutdown::run(booted, lock).await;
    if args.foreground {
        println!("{}", messages::stopped(report.elapsed_ms));
    }
    if restart {
        match std::env::current_exe() {
            Ok(exe) => match forge_core::platform::spawn_detached(
                &exe,
                &args.restart_args(root.path()),
                root.path(),
            ) {
                Ok(pid) => tracing::info!(pid, "new daemon started"),
                Err(err) => tracing::error!(%err, "the new daemon did not start"),
            },
            Err(err) => tracing::error!(%err, "cannot find the daemon executable"),
        }
    }
    exit::OK
}
