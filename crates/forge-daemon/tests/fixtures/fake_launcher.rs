//! A fake editor / terminal for the `shell.open*` tests (M2-E3-T9): `fake_launcher <out>
//! <linger seconds> [args...]` writes its arguments, `PATH`, working folder and pid to
//! `<out>` (atomically: a temporary file renamed over it, ending in `END`), then sleeps
//! `linger` seconds so the test can check that it outlives the daemon. Compiled with
//! `rustc` by the tests (no dependencies).

use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = args.first().expect("output file").clone();
    let linger: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut text = String::new();
    for arg in args.iter().skip(2) {
        text.push_str(&format!("ARG={arg}\n"));
    }
    text.push_str(&format!(
        "PATH={}\n",
        std::env::var("PATH").unwrap_or_default()
    ));
    text.push_str(&format!(
        "CWD={}\n",
        std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default()
    ));
    text.push_str(&format!("PID={}\nEND\n", std::process::id()));
    let tmp = format!("{out}.tmp{}", std::process::id());
    std::fs::write(&tmp, text).expect("writing the record");
    std::fs::rename(&tmp, &out).expect("publishing the record");
    std::thread::sleep(Duration::from_secs(linger));
}
