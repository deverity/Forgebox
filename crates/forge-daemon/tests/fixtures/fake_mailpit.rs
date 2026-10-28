//! Fake `mailpit.exe` of the daemon concurrency tests (docs/03 §7.6), compiled with
//! `rustc` by the tests (no dependencies). It is installed as `bin/mailpit/1.32.0/` and
//! found by the manifest's `discover` (`mailpit.exe version` → `mailpit v1.32.0`), so the
//! daemon runs it as the `mailpit/default` service with the manifest's arguments:
//! `--smtp 127.0.0.1:<p> --listen 127.0.0.1:<p> --database <file>`.
//!
//! * Answers any HTTP request on `--listen` with `200 {}` (the manifest's health probe)
//!   and accepts (and drops) connections on `--smtp`.
//! * **Stubborn:** ignores CTRL_C / CTRL_BREAK / CTRL_CLOSE and never exits by itself, so
//!   a stop always runs the whole cascade up to the Job termination.
//! * `fake-mode.txt` next to the executable containing `flood` makes it print
//!   `line <n>` on stdout at ~10 000 lines/s and write the count it reached to
//!   `progress.txt` (next to the executable) every 500 lines: if the daemon stops reading
//!   its stdout, the count stalls. Otherwise it prints `tick <n>` every 200 ms.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32)
    -> i32;
}

unsafe extern "system" fn ignore(_event: u32) -> i32 {
    1
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("version") {
        println!("mailpit v1.32.0 compiled with go1.25.0 on windows/amd64");
        return;
    }
    // SAFETY: registers a handler that only returns TRUE.
    unsafe {
        SetConsoleCtrlHandler(Some(ignore), 1);
    }
    let listen = arg(&args, "--listen").expect("--listen");
    let smtp = arg(&args, "--smtp").expect("--smtp");
    let http = TcpListener::bind(&listen).expect("binding --listen");
    let mail = TcpListener::bind(&smtp).expect("binding --smtp");
    std::thread::spawn(move || {
        for stream in mail.incoming().flatten() {
            drop(stream);
        }
    });
    std::thread::spawn(move || {
        for mut stream in http.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = [0_u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
            );
        }
    });

    let exe = std::env::current_exe().expect("current exe");
    let flood = std::fs::read_to_string(exe.with_file_name("fake-mode.txt"))
        .is_ok_and(|m| m.trim() == "flood");
    let progress = exe.with_file_name("progress.txt");
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let started = Instant::now();
    let mut n: u64 = 0;
    loop {
        if flood {
            // Paced at 10 000 lines/s; `write` blocks when nobody drains the pipe.
            let due = (started.elapsed().as_secs_f64() * 10_000.0) as u64;
            if n >= due {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            n += 1;
            let _ = writeln!(out, "line {n} of the flood, padded to look like a real log line");
            if n % 500 == 0 {
                let _ = out.flush();
                let _ = std::fs::write(&progress, n.to_string());
            }
        } else {
            n += 1;
            let _ = writeln!(out, "tick {n}");
            let _ = out.flush();
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}
