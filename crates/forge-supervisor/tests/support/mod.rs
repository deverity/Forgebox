//! Shared helpers of the forge-supervisor integration tests (Windows only).
//!
//! The fake service (`tests/fake_service`, an independent cargo project without
//! dependencies) is built once per test binary into its own target dir under
//! `CARGO_TARGET_TMPDIR`, keyed by the worktree, so the outer build lock is never needed
//! and parallel worktrees never overwrite each other's running executable.
#![allow(dead_code)]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use forge_core::actor::ActorSupervisor;
use forge_core::event::{Event, EventBus, EventKind, EventStream};
use forge_core::ids::{InstanceId, ToolId};
use forge_core::platform::{ProcessControl, ProcessControlOptions, process_control};
use forge_supervisor::{
    InstanceHandle, InstanceSpec, JobTree, LogSubscription, SpawnSpec, Supervisor,
};

/// Path of the fake service executable (built on first use).
pub fn fake_service() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(build_fake_service).clone()
}

fn build_fake_service() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = manifest_dir
        .join("tests")
        .join("fake_service")
        .join("Cargo.toml");
    let mut hasher = DefaultHasher::new();
    manifest_dir.hash(&mut hasher);
    let target = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("fake-service-{:016x}", hasher.finish()));
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .arg("build")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&target)
        .env_remove("CARGO_MAKEFLAGS")
        .env_remove("MAKEFLAGS")
        .env_remove("MFLAGS")
        .output()
        .expect("running cargo to build the fake service");
    assert!(
        output.status.success(),
        "building the fake service failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let exe = target.join("debug").join("fake_service.exe");
    assert!(exe.is_file(), "{} missing", exe.display());
    exe
}

/// The `forgebox-ctrl` helper built with this package.
pub fn ctrl_helper() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_forgebox-ctrl"))
}

/// Platform process operations using the freshly built helper.
pub fn processes() -> Arc<dyn ProcessControl> {
    process_control(ProcessControlOptions::with_ctrl_break_helper(ctrl_helper()))
}

/// A supervisor with its own root job and event bus.
pub fn supervisor() -> (Supervisor, EventBus) {
    let bus = EventBus::with_capacity(65_536);
    let jobs = JobTree::new(processes()).expect("root job");
    (
        Supervisor::new(jobs, ActorSupervisor::new(bus.clone())),
        bus,
    )
}

/// Spec running the fake service with `args` (cwd = its folder, PATH = its folder).
pub fn fake_spec(name: &str, args: &[&str]) -> InstanceSpec {
    let exe = fake_service();
    let dir = exe.parent().unwrap().to_path_buf();
    let spawn = SpawnSpec::new(&exe, &dir)
        .args(args.iter().copied())
        .path_entry(&dir);
    InstanceSpec::new(
        InstanceId::new(),
        ToolId::new("fake").unwrap(),
        name.to_owned(),
        spawn,
    )
}

/// Stop command running the fake service in one-shot mode.
pub fn fake_command(args: &[&str]) -> SpawnSpec {
    let exe = fake_service();
    let dir = exe.parent().unwrap().to_path_buf();
    SpawnSpec::new(&exe, &dir)
        .args(args.iter().copied())
        .path_entry(&dir)
}

pub const ASK: Duration = Duration::from_secs(10);

/// Waits for a line equal to (or starting with) `prefix`; returns it.
pub async fn wait_line(logs: &mut LogSubscription, prefix: &str, timeout: Duration) -> String {
    let wait = async {
        loop {
            let line = logs.recv().await.expect("log stream closed");
            if line.text.starts_with(prefix) {
                return line.text;
            }
        }
    };
    tokio::time::timeout(timeout, wait)
        .await
        .unwrap_or_else(|_| panic!("no line starting with {prefix:?} within {timeout:?}"))
}

/// Events of `instance` already published (non-blocking drain).
pub fn drain_events(stream: &mut EventStream, instance: &InstanceHandle) -> Vec<Event> {
    let id = instance.id().to_string();
    let mut events = Vec::new();
    while let Some(event) = stream.try_recv() {
        if event.payload["instance_id"] == id.as_str() {
            events.push(event);
        }
    }
    events
}

/// `(kind, strategy)` pairs, strategy empty when absent.
pub fn kinds(events: &[Event]) -> Vec<(EventKind, String)> {
    events
        .iter()
        .map(|e| {
            (
                e.kind,
                e.payload["strategy"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect()
}

/// Whether `pid` is still running.
pub fn alive(pid: u32) -> bool {
    processes().is_alive(pid).unwrap_or(false)
}

/// Waits until `pid` exits; returns how long it took, or `None` past `timeout`.
pub async fn wait_dead(pid: u32, timeout: Duration) -> Option<Duration> {
    let started = std::time::Instant::now();
    let mut exit = match processes().watch(pid) {
        Ok(exit) => exit,
        Err(_) => return Some(Duration::ZERO), // already gone
    };
    match exit.wait_timeout(timeout).await {
        Ok(Some(_)) => Some(started.elapsed()),
        _ => None,
    }
}

/// Waits for the next event of `kind` published for `instance`.
pub async fn wait_event(
    stream: &mut EventStream,
    instance: &InstanceHandle,
    kind: EventKind,
    timeout: Duration,
) -> Event {
    let id = instance.id().to_string();
    let wait = async {
        loop {
            let event = stream.recv().await.expect("event stream closed");
            if event.kind == kind && event.payload["instance_id"] == id.as_str() {
                return event;
            }
        }
    };
    tokio::time::timeout(timeout, wait)
        .await
        .unwrap_or_else(|_| panic!("no {kind} event within {timeout:?}"))
}
