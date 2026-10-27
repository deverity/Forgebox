//! `CTRL_BREAK` stops a real `php-cgi.exe` (roadmap M1-E3-T1 acceptance); a TCP health
//! check gates `Running` on its FastCGI listener and the metrics sampler reads it
//! (M1-E3-T2).
//!
//! Downloads the current PHP 8.4 NTS VS17 x64 build listed in
//! `https://windows.php.net/downloads/releases/releases.json`, verifies its SHA-256
//! against that listing, caches the zip (`FORGEBOX_TEST_CACHE`, default
//! `C:\apps_src\Forgebox-wt\cache` when it exists, else under the cargo target dir),
//! extracts it to a temp dir and runs `php-cgi.exe -b 127.0.0.1:<free port>` under the
//! supervisor. Run with `cargo test -p forge-supervisor --test php_cgi_online -- --ignored`.
#![cfg(windows)]

mod support;

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use forge_core::ids::{InstanceId, ToolId};
use forge_supervisor::{
    HealthSpec, InstanceSpec, InstanceState, Probe, QUICK_ASK_TIMEOUT, SpawnSpec, StopStrategy,
};
use sha2::{Digest, Sha256};
use support::*;

const RELEASES: &str = "https://windows.php.net/downloads/releases/";

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

fn curl(url: &str, out: &Path) {
    let status = Command::new(system32().join("curl.exe"))
        // Best-effort revocation: corporate networks often block CRL/OCSP endpoints;
        // the certificate chain is still validated and the zip is checked by SHA-256.
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--retry",
            "2",
        ])
        .arg("--ssl-revoke-best-effort")
        .arg("-o")
        .arg(out)
        .arg(url)
        .status()
        .expect("running curl.exe");
    assert!(status.success(), "download of {url} failed: {status}");
}

fn sha256_hex(path: &Path) -> String {
    use std::io::Read;

    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path).unwrap();
    let mut buf = vec![0_u8; 1 << 16];
    loop {
        let read = file.read(&mut buf).unwrap();
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Returns the verified zip of the latest PHP 8.4 NTS VS17 x64 build and its version.
fn php_zip() -> (PathBuf, String) {
    let cache = cache_dir();
    std::fs::create_dir_all(&cache).unwrap();
    let listing = cache.join("php-releases.json");
    curl(&format!("{RELEASES}releases.json"), &listing);
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&listing).unwrap()).unwrap();
    let series = &json["8.4"];
    let version = series["version"].as_str().expect("8.4 version").to_owned();
    let zip = &series["nts-vs17-x64"]["zip"];
    let name = zip["path"].as_str().expect("zip path").to_owned();
    let expected = zip["sha256"].as_str().expect("sha256").to_ascii_lowercase();
    assert!(!name.contains(['/', '\\']), "unexpected path {name}");

    let file = cache.join(&name);
    if file.is_file() && sha256_hex(&file) == expected {
        println!("using cached {} (sha256 {expected})", file.display());
        return (file, version);
    }
    let partial = cache.join(format!("{name}.part"));
    curl(&format!("{RELEASES}{name}"), &partial);
    let actual = sha256_hex(&partial);
    assert_eq!(actual, expected, "sha256 mismatch for {name}");
    std::fs::rename(&partial, &file).unwrap();
    println!("downloaded {} (sha256 {expected} verified)", file.display());
    (file, version)
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn wait_port_open(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let open = tokio::task::spawn_blocking(move || {
            TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(200))
                .is_ok()
        })
        .await
        .unwrap();
        if open {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// The verified PHP zip extracted to a temp dir: (dir guard, folder, version).
async fn extract_php() -> (tempfile::TempDir, PathBuf, String) {
    let (zip, version) = tokio::task::spawn_blocking(php_zip).await.unwrap();
    let extracted = tempfile::tempdir().unwrap();
    let status = Command::new(system32().join("tar.exe"))
        .arg("-xf")
        .arg(&zip)
        .arg("-C")
        .arg(extracted.path())
        .status()
        .unwrap();
    assert!(status.success(), "extracting {}", zip.display());
    let dir = extracted.path().to_path_buf();
    assert!(
        dir.join("php-cgi.exe").is_file(),
        "php-cgi.exe missing in {}",
        dir.display()
    );
    (extracted, dir, version)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads PHP from windows.php.net"]
async fn php_cgi_stops_with_ctrl_break_online() {
    let (_extracted, dir, version) = extract_php().await;
    let php_cgi = dir.join("php-cgi.exe");

    let port = free_port();
    let spawn = SpawnSpec::new(&php_cgi, &dir)
        .args(["-b".to_owned(), format!("127.0.0.1:{port}")])
        .path_entry(&dir);
    let spec = InstanceSpec::new(
        InstanceId::new(),
        ToolId::new("php").unwrap(),
        "8.4-online",
        spawn,
    );
    let (supervisor, bus) = supervisor();
    let instance = supervisor.spawn_instance(spec).unwrap();
    let mut events = bus.subscribe();
    let pid = instance.start(ASK).await.unwrap();
    assert!(
        wait_port_open(port, Duration::from_secs(15)).await,
        "php-cgi {version} (pid {pid}) never listened on 127.0.0.1:{port}"
    );
    println!("php-cgi {version} pid {pid} listening on 127.0.0.1:{port}");

    let started = Instant::now();
    let report = instance.stop(Duration::from_secs(60)).await.unwrap();
    let elapsed = started.elapsed();
    println!("stop report: {report:?} (measured {elapsed:?})");
    assert_eq!(report.strategy, Some(StopStrategy::CtrlBreak));
    assert_eq!(
        report.attempted,
        vec![StopStrategy::CtrlBreak],
        "no TerminateJobObject"
    );
    assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
    assert!(!alive(pid));
    assert!(
        !wait_port_open(port, Duration::from_millis(300)).await,
        "port closed"
    );
    let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!(status.state, InstanceState::Stopped);
    let stopping: Vec<String> = kinds(&drain_events(&mut events, &instance))
        .into_iter()
        .filter(|(kind, _)| *kind == forge_core::event::EventKind::ServiceStopping)
        .map(|(_, strategy)| strategy)
        .collect();
    assert_eq!(stopping, vec!["ctrl_break"]);
}

/// `Running` only once php-cgi accepts FastCGI connections (tcp probe), and the sampler
/// reports the real process.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads PHP from windows.php.net"]
async fn php_cgi_tcp_health_and_metrics_online() {
    let (_extracted, dir, version) = extract_php().await;
    let port = free_port();
    let spawn = SpawnSpec::new(dir.join("php-cgi.exe"), &dir)
        .args(["-b".to_owned(), format!("127.0.0.1:{port}")])
        .path_entry(&dir);
    let mut spec = InstanceSpec::new(
        InstanceId::new(),
        ToolId::new("php").unwrap(),
        "8.4-health",
        spawn,
    )
    .with_health(HealthSpec::new(Probe::Tcp {
        addr: format!("127.0.0.1:{port}"),
    }));
    spec.metrics.interval = Duration::from_millis(500);
    let (supervisor, _bus) = supervisor();
    let instance = supervisor.spawn_instance(spec).unwrap();
    let started = Instant::now();
    let pid = instance.start(ASK).await.unwrap();
    println!(
        "php-cgi {version} pid {pid} Running after {:?}",
        started.elapsed()
    );
    // Running means the listener is up: no waiting here.
    TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(500))
        .expect("php-cgi accepts connections once Running");
    let status = instance.status(QUICK_ASK_TIMEOUT).await.unwrap();
    assert_eq!(status.state, InstanceState::Running);
    let health = status.health.expect("health status");
    assert_eq!((health.probe.as_str(), health.passing), ("tcp", Some(true)));

    let mut samples = instance.subscribe_metrics(ASK).await.unwrap();
    let sample = tokio::time::timeout(Duration::from_secs(10), samples.wait_for(|m| m.is_some()))
        .await
        .expect("a metrics sample within 10 s")
        .unwrap()
        .clone()
        .unwrap();
    println!(
        "metrics: {} processes, rss {:.1} MiB, private {:.1} MiB, handles {}, cpu {:.2} %",
        sample.processes.len(),
        sample.rss_bytes as f64 / 1_048_576.0,
        sample.private_bytes as f64 / 1_048_576.0,
        sample.handles,
        sample.cpu_pct
    );
    assert_eq!(sample.pid, Some(pid));
    assert!(sample.processes.iter().any(|p| p.pid == pid));
    assert!(
        sample.rss_bytes > 1024 * 1024 && sample.handles > 0,
        "{sample:?}"
    );

    let report = instance.stop(Duration::from_secs(60)).await.unwrap();
    assert_eq!(report.strategy, Some(StopStrategy::CtrlBreak));
    assert!(!alive(pid));
}
