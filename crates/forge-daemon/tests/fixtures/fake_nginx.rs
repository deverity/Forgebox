//! Fake `nginx.exe` of the log targets test (M2-E3-T8), compiled with `rustc` by the test
//! (no dependencies) and copied to `bin/nginx/1.30.0/nginx.exe`. The daemon finds it with
//! the manifest's `discover` (`nginx.exe -v`) and runs it as `nginx/default` with the real
//! manifest:
//!
//! * `-v` → `nginx version: nginx/1.30.0` on stderr (like nginx);
//! * `-t ...` (pre-check) → exit 0;
//! * `-s <signal> ...` (`quit`, `reload`, `reopen`) → exit 0; `quit` leaves a `quit`
//!   file next to the executable, which the running process takes as its stop request;
//! * otherwise → listens on the first `listen 127.0.0.1:<port>` of the `-c` file, prints
//!   nothing and serves until the `quit` file appears (or CTRL_BREAK).

use std::net::TcpListener;
use std::time::Duration;

fn value_after(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exe = std::env::current_exe().expect("current exe");
    let quit = exe.with_file_name("quit");
    if args.iter().any(|a| a == "-v") {
        eprintln!("nginx version: nginx/1.30.0");
        return;
    }
    if args.iter().any(|a| a == "-t") {
        return;
    }
    if let Some(signal) = value_after(&args, "-s") {
        if signal == "quit" {
            let _ = std::fs::write(&quit, b"");
        }
        return;
    }
    let conf = value_after(&args, "-c").expect("-c <nginx.conf>");
    let text = std::fs::read_to_string(&conf).expect("reading nginx.conf");
    let port = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("listen 127.0.0.1:"))
        .find_map(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u16>().ok()
        })
        .expect("a listen directive");
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("binding the port");
    listener.set_nonblocking(true).expect("non-blocking");
    let _ = std::fs::remove_file(&quit);
    loop {
        while let Ok((stream, _)) = listener.accept() {
            drop(stream);
        }
        if quit.exists() {
            let _ = std::fs::remove_file(&quit);
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
