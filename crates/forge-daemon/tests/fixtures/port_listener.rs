//! A process that holds a TCP port, for the `services.kill_port_holder` tests (M2-E3-T11):
//! listens on `127.0.0.1:0`, prints `listening <port>` and sleeps for two minutes (the
//! test ends it long before). Compiled with `rustc` by the tests (no dependencies).

use std::io::Write as _;
use std::net::TcpListener;
use std::time::Duration;

fn main() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("address").port();
    let mut out = std::io::stdout();
    writeln!(out, "listening {port}").expect("stdout");
    out.flush().expect("flush");
    std::thread::sleep(Duration::from_secs(120));
    drop(listener);
}
