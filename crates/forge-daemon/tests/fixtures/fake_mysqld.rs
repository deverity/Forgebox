//! Fake `mysqld.exe` / `mysqladmin.exe` of the quick settings tests (M2-E2-T7), compiled
//! with `rustc` by the tests (no dependencies) and copied as both names into
//! `bin/mysql/8.4.13/bin/`. The daemon finds it with the manifest's `discover`
//! (`mysqld.exe --version`) and runs it as `mysql/default` with the real manifest:
//!
//! * `--defaults-file=<my.ini> --validate-config` (pre-check) → exit 0;
//! * `--defaults-file=<my.ini> --initialize-insecure` (`data.init`) → creates the
//!   `datadir` of `[mysqld]`, exit 0;
//! * `--defaults-file=<my.ini> --console` → listens on `127.0.0.1:<port>`, the **last**
//!   `port` of `[mysqld]` (last wins, as in MySQL), prints `ready for connections port:
//!   <port>` and serves until `mysqladmin ... --port=<port> shutdown` (which leaves a
//!   `shutdown-<port>` file next to the executable) or CTRL_BREAK;
//! * run as `mysqladmin.exe`: writes that file and exits 0.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn value_of(args: &[String], prefix: &str) -> Option<String> {
    args.iter()
        .find_map(|a| a.strip_prefix(prefix).map(str::to_owned))
}

/// The last `key` of `[section]` in an ini text (quotes removed).
fn ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut current = String::new();
    let mut found = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(name) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current = name.trim().to_owned();
            continue;
        }
        if current != section || t.starts_with(['#', ';']) {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            if k.trim() == key {
                found = Some(v.trim().trim_matches('"').to_owned());
            }
        }
    }
    found
}

fn marker(exe: &Path, port: &str) -> PathBuf {
    exe.with_file_name(format!("shutdown-{port}"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exe = std::env::current_exe().expect("current exe");
    let name = exe
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name == "mysqladmin" {
        let port = value_of(&args, "--port=").unwrap_or_default();
        let _ = std::fs::write(marker(&exe, &port), b"");
        return;
    }
    if args.iter().any(|a| a == "--version") {
        println!("{} Ver 8.4.13 for Win64 on x86_64 (fake)", exe.display());
        return;
    }
    let defaults = value_of(&args, "--defaults-file=").expect("--defaults-file");
    let text = std::fs::read_to_string(&defaults).expect("reading the defaults file");
    if args.iter().any(|a| a == "--validate-config") {
        return;
    }
    if args.iter().any(|a| a == "--initialize-insecure") {
        let dir = ini_value(&text, "mysqld", "datadir").expect("datadir");
        std::fs::create_dir_all(dir).expect("creating the datadir");
        return;
    }
    let port = ini_value(&text, "mysqld", "port").expect("port");
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("binding the port");
    listener.set_nonblocking(true).expect("non-blocking");
    let stop = marker(&exe, &port);
    let _ = std::fs::remove_file(&stop);
    println!("ready for connections port: {port}");
    loop {
        while let Ok((stream, _)) = listener.accept() {
            drop(stream);
        }
        if stop.exists() {
            let _ = std::fs::remove_file(&stop);
            println!("shutdown complete");
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
