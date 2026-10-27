//! Pre-checks against a real nginx (docs/03 §1.5, roadmap M1-E3-T3): `nginx -t` as the
//! configuration check, a running nginx reported as the holder of its port, and a restart
//! with a broken `nginx.conf` refused while the running nginx keeps serving.
//!
//! Uses the official `nginx-1.30.6.zip` from nginx.org, verified against the curated
//! SHA-256 of `registry/tools/nginx.yaml` (nginx.org publishes no checksum), cached in
//! `FORGEBOX_TEST_CACHE` (default `C:\apps_src\Forgebox-wt\cache` when it exists, else
//! under the cargo target dir) and extracted with `tar.exe` to a temp dir. Run with
//! `cargo test -p forge-supervisor --test nginx_precheck_online -- --ignored --nocapture`.
#![cfg(windows)]

mod support;

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use forge_core::ids::{InstanceId, ToolId};
use forge_supervisor::precheck::ConfigCheck;
use forge_supervisor::{
    FailureReason, HealthSpec, InstanceSpec, InstanceState, PrecheckSpec, Probe, QUICK_ASK_TIMEOUT,
    SpawnSpec, StopPlan,
};
use sha2::{Digest, Sha256};
use support::*;

const VERSION: &str = "1.30.6";
/// Curated in `registry/tools/nginx.yaml` (`source.releases`).
const SHA256: &str = "c6ee35edb0a861063323f709192ae50d1eefaf2ea7d00678bd2b22d8542dc61e";

fn system32() -> PathBuf {
    processes().system_path().remove(0)
}

fn cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FORGEBOX_TEST_CACHE") {
        return PathBuf::from(dir);
    }
    let shared = Path::new(r"C:\apps_src\Forgebox-wt\cache");
    if shared.is_dir() {
        return shared.to_path_buf();
    }
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("download-cache")
}

fn sha256_hex(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap();
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The verified nginx zip (downloaded once into the cache).
fn nginx_zip() -> PathBuf {
    let cache = cache_dir();
    std::fs::create_dir_all(&cache).unwrap();
    let name = format!("nginx-{VERSION}.zip");
    let file = cache.join(&name);
    if file.is_file() && sha256_hex(&file) == SHA256 {
        println!("using cached {} (sha256 {SHA256})", file.display());
        return file;
    }
    let partial = cache.join(format!("{name}.part"));
    let status = Command::new(system32().join("curl.exe"))
        .args(["--fail", "--location", "--silent", "--show-error"])
        .arg("--ssl-revoke-best-effort")
        .arg("-o")
        .arg(&partial)
        .arg(format!("https://nginx.org/download/{name}"))
        .status()
        .expect("running curl.exe");
    assert!(status.success(), "download failed: {status}");
    assert_eq!(sha256_hex(&partial), SHA256, "sha256 mismatch for {name}");
    std::fs::rename(&partial, &file).unwrap();
    println!("downloaded {} (sha256 {SHA256} verified)", file.display());
    file
}

/// nginx extracted to a temp dir: (guard, nginx folder).
fn extract_nginx() -> (tempfile::TempDir, PathBuf) {
    let zip = nginx_zip();
    // Not under %TEMP%: it is often an 8.3 short path (`GUSTAV~1`), and nginx for
    // Windows refuses short names in file paths ("CreateFile() ... failed (2)").
    let extracted = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let status = Command::new(system32().join("tar.exe"))
        .arg("-xf")
        .arg(&zip)
        .arg("-C")
        .arg(extracted.path())
        .status()
        .unwrap();
    assert!(status.success(), "extracting {}", zip.display());
    let dir = extracted.path().join(format!("nginx-{VERSION}"));
    assert!(dir.join("nginx.exe").is_file(), "{}", dir.display());
    (extracted, dir)
}

fn write_conf(dir: &Path, port: u16, broken: bool) {
    let listen = if broken { "lsten" } else { "listen" };
    let conf = format!(
        "worker_processes 1;\n\
         error_log logs/error.log;\n\
         pid logs/nginx.pid;\n\
         events {{ worker_connections 64; }}\n\
         http {{\n    access_log off;\n    server {{\n        {listen} 127.0.0.1:{port};\n        \
         location / {{ return 200 \"ok\"; }}\n    }}\n}}\n"
    );
    std::fs::write(dir.join("conf").join("nginx.conf"), conf).unwrap();
}

/// `nginx.exe -p <dir>/ <args>` with the nginx folder as cwd and PATH.
fn nginx(dir: &Path, args: &[&str]) -> SpawnSpec {
    let prefix = format!("{}/", dir.to_string_lossy().replace('\\', "/"));
    SpawnSpec::new(dir.join("nginx.exe"), dir)
        .args(["-p", prefix.as_str()])
        .args(args.iter().copied())
        .path_entry(dir)
}

fn nginx_spec(dir: &Path, name: &str, port: u16) -> InstanceSpec {
    let mut spec = InstanceSpec::new(
        InstanceId::new(),
        ToolId::new("nginx").unwrap(),
        name,
        nginx(dir, &["-c", "conf/nginx.conf"]),
    )
    .with_health(HealthSpec::new(Probe::Tcp {
        addr: format!("127.0.0.1:{port}"),
    }))
    .with_precheck(
        PrecheckSpec::default()
            .with_ports([port])
            .with_check(ConfigCheck::new(nginx(
                dir,
                &["-c", "conf/nginx.conf", "-t"],
            ))),
    );
    spec.stop = StopPlan {
        command: Some(nginx(dir, &["-s", "quit"])),
        timeout: Duration::from_secs(10),
        ..StopPlan::default()
    };
    spec
}

fn serves(port: u16) -> bool {
    TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(500)).is_ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads nginx from nginx.org"]
async fn nginx_prechecks_online() {
    let (_guard, dir) = tokio::task::spawn_blocking(extract_nginx).await.unwrap();
    let port = free_port();
    write_conf(&dir, port, false);
    let (supervisor, _bus) = supervisor();

    // A good configuration passes `nginx -t` and nginx starts.
    let web = supervisor
        .spawn_instance(nginx_spec(&dir, "default", port))
        .unwrap();
    let started = Instant::now();
    let pid = web.start(Duration::from_secs(60)).await.unwrap();
    println!(
        "nginx {VERSION} pid {pid} running on 127.0.0.1:{port} after {:?} (pre-checks included)",
        started.elapsed()
    );
    assert!(serves(port));

    // A second instance on the same port: the holder is the running nginx.
    let twin = supervisor
        .spawn_instance(nginx_spec(&dir, "twin", port))
        .unwrap();
    assert_eq!(twin.start(ASK).await.unwrap_err().code(), "port_in_use");
    match twin.status(QUICK_ASK_TIMEOUT).await.unwrap().state {
        InstanceState::Failed {
            reason: FailureReason::PortInUse { port: p, pid, exe },
        } => {
            let exe = exe.expect("nginx.exe path");
            println!("twin failed: port {p} in use by {exe} (PID {pid:?})");
            assert_eq!(p, port);
            assert!(exe.ends_with("nginx.exe"), "{exe}");
            assert!(alive(pid.unwrap()));
        }
        other => panic!("{other:?}"),
    }

    // A broken configuration: the restart is refused, the running nginx keeps serving.
    write_conf(&dir, port, true);
    let err = web.restart(Duration::from_secs(60)).await.unwrap_err();
    println!("restart refused: {err}");
    assert_eq!(err.code(), "config_invalid");
    assert!(
        err.to_string().contains("unknown directive \"lsten\""),
        "{err}"
    );
    let status = web.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!(
        (status.state, status.pid),
        (InstanceState::Running, Some(pid))
    );
    assert!(alive(pid) && serves(port));

    // Fixed: the restart goes through.
    write_conf(&dir, port, false);
    let new_pid = web.restart(Duration::from_secs(60)).await.unwrap();
    assert_ne!(new_pid, pid);
    assert!(serves(port));
    let report = web.stop(Duration::from_secs(60)).await.unwrap();
    println!("stop report: {report:?}");
    assert!(!alive(new_pid));

    // Stopped with a broken configuration: the start fails before nginx runs.
    write_conf(&dir, port, true);
    web.start(ASK).await.unwrap_err();
    match web.status(QUICK_ASK_TIMEOUT).await.unwrap().state {
        InstanceState::Failed {
            reason: FailureReason::ConfigInvalid { stderr },
        } => {
            println!("config_invalid output:\n{stderr}");
            assert!(
                stderr.contains("[emerg] unknown directive \"lsten\""),
                "{stderr}"
            );
            assert!(stderr.contains("test failed"), "{stderr}");
        }
        other => panic!("{other:?}"),
    }
    assert!(!serves(port));
}
