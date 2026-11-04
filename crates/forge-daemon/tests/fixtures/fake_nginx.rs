//! A stand-in for `nginx.exe` (the metrics tests, M2-E5-T8): enough of nginx's command line
//! for the daemon to discover, check, start, reload and stop it through the real nginx
//! manifest, with no HTTP at all. Compiled with `rustc` by the tests (no dependencies).
//!
//! * `-v`: prints the version banner (discovery);
//! * `-t`: the configuration check, always fine;
//! * `-s quit|stop`: asks the running fake to exit (a `fake-nginx.quit` file next to the
//!   executable); `-s reload|reopen`: nothing to do;
//! * otherwise it runs: reads `-c <conf>`, holds the `listen 127.0.0.1:<port>` sockets (the
//!   manifest's TCP health probe), creates the `access_log "<path>"` file like nginx opens it,
//!   and waits for the quit file. The test appends the access lines itself.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

const VERSION: &str = "1.30.6";

fn quit_file() -> PathBuf {
    std::env::current_exe()
        .expect("exe")
        .with_file_name("fake-nginx.quit")
}

fn arg_after(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

/// `listen 127.0.0.1:<port>` ports and the `access_log "<path>"` of the configuration.
fn read_conf(path: &Path) -> (Vec<u16>, Option<PathBuf>) {
    let text = std::fs::read_to_string(path).unwrap_or_default();
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
        if let Some(rest) = line.strip_prefix("access_log \"") {
            if let Some(end) = rest.find('"') {
                access = Some(PathBuf::from(&rest[..end]));
            }
        }
    }
    (ports, access)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-v" || a == "-V") {
        eprintln!("nginx version: nginx/{VERSION}");
        println!("nginx version: nginx/{VERSION}");
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
    let conf = arg_after(&args, "-c").map(PathBuf::from);
    let (ports, access) = conf.as_deref().map(read_conf).unwrap_or_default();
    let listeners: Vec<TcpListener> = ports
        .iter()
        .filter_map(|p| TcpListener::bind(("127.0.0.1", *p)).ok())
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
    // At most ten minutes, whatever happens to the test.
    for _ in 0..12_000 {
        if quit_file().exists() {
            let _ = std::fs::remove_file(quit_file());
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(listeners);
}
