//! A stand-in for `nginx.exe`, shared by the log targets test (`tests/logs.rs`, M2-E3-T8)
//! and the metrics tests (`tests/metrics.rs`, M2-E5-T8). Compiled with `rustc` by the tests
//! (no dependencies) and copied to `bin/nginx/<version>/nginx.exe`; the daemon finds it with
//! the manifest's `discover` (`nginx.exe -v`) and runs it with the real nginx manifest, with
//! no HTTP at all.
//!
//! * `-v` / `-V` → `nginx version: nginx/<version>` on stderr (like nginx) and stdout, where
//!   `<version>` is the name of the folder holding the executable (`1.30.0` otherwise);
//! * `-t ...` (pre-check) → exit 0;
//! * `-s quit|stop` → leaves a `fake-nginx.quit` file next to the executable, which the
//!   running process takes as its stop request; `-s reload|reopen` → nothing to do;
//! * otherwise → reads `-c <conf>`, holds every `listen 127.0.0.1:<port>` socket (the
//!   manifest's TCP health probe; incoming connections are accepted and dropped), creates
//!   the `access_log "<path>"` file like nginx opens it, prints nothing and waits for the
//!   quit file (at most ten minutes). The tests append the access lines themselves.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_VERSION: &str = "1.30.0";

fn quit_file() -> PathBuf {
    std::env::current_exe()
        .expect("current exe")
        .with_file_name("fake-nginx.quit")
}

/// The version is the install folder's name (`bin/nginx/<version>/nginx.exe`).
fn version() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.parent()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
        })
        .filter(|name| {
            let parts: Vec<&str> = name.split('.').collect();
            parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
        .unwrap_or_else(|| DEFAULT_VERSION.to_owned())
}

fn arg_after(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

/// `listen 127.0.0.1:<port>` ports and the `access_log "<path>"` of the configuration.
fn read_conf(path: &Path) -> (Vec<u16>, Option<PathBuf>) {
    let text = std::fs::read_to_string(path).expect("reading nginx.conf");
    let mut ports = Vec::new();
    let mut access = None;
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("listen 127.0.0.1:") {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(port) = digits.parse::<u16>() {
                if !ports.contains(&port) {
                    ports.push(port);
                }
            }
        }
        if access.is_none() {
            if let Some(rest) = line.strip_prefix("access_log \"") {
                if let Some(end) = rest.find('"') {
                    access = Some(PathBuf::from(&rest[..end]));
                }
            }
        }
    }
    (ports, access)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-v" || a == "-V") {
        let version = version();
        eprintln!("nginx version: nginx/{version}");
        println!("nginx version: nginx/{version}");
        return;
    }
    if args.iter().any(|a| a == "-t") {
        eprintln!("nginx: the configuration file syntax is ok");
        return;
    }
    if let Some(signal) = arg_after(&args, "-s") {
        if signal == "quit" || signal == "stop" {
            let _ = std::fs::write(quit_file(), signal);
        }
        return;
    }
    let _ = std::fs::remove_file(quit_file());
    let conf = arg_after(&args, "-c").expect("-c <nginx.conf>");
    let (ports, access) = read_conf(Path::new(&conf));
    assert!(!ports.is_empty(), "a listen directive");
    let listeners: Vec<TcpListener> = ports
        .iter()
        .map(|p| {
            let listener = TcpListener::bind(("127.0.0.1", *p)).expect("binding the port");
            listener.set_nonblocking(true).expect("non-blocking");
            listener
        })
        .collect();
    if let Some(access) = access {
        if let Some(dir) = access.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&access);
    }
    for _ in 0..12_000 {
        for listener in &listeners {
            while let Ok((stream, _)) = listener.accept() {
                drop(stream);
            }
        }
        if quit_file().exists() {
            let _ = std::fs::remove_file(quit_file());
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
