//! Fake `forgebox-helper.exe` for the elevation transport tests. No dependencies: the
//! JSON is read and written by hand.
//!
//! `fake_helper --request <file> --response <file>`: reads the request, then picks a
//! behaviour from the firewall rule name found in it:
//! * `Forgebox-fake-silent`: exits with code 3 without answering;
//! * `Forgebox-fake-garbage`: answers something that is not JSON;
//! * `Forgebox-fake-sleep`: sleeps 5 s before answering;
//! * `Forgebox-fake-refuse`: answers a `forbidden` refusal;
//! * anything else: answers success with the request size in `details`.
//!
//! Like the real helper, it never creates the response file: it must already exist
//! (exit code 4 otherwise).

use std::io::Write;
use std::time::Duration;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// The string value of `"key": "..."` in a flat JSON text.
fn string_field(text: &str, key: &str) -> Option<String> {
    let at = text.find(&format!("\"{key}\""))?;
    let rest = &text[at + key.len() + 2..];
    let rest = &rest[rest.find(':')? + 1..];
    let start = rest.find('"')? + 1;
    let len = rest[start..].find('"')?;
    Some(rest[start..start + len].to_owned())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(request), Some(response)) = (arg(&args, "--request"), arg(&args, "--response"))
    else {
        std::process::exit(2);
    };
    let Ok(text) = std::fs::read_to_string(&request) else {
        std::process::exit(5);
    };
    let id = string_field(&text, "request_id").unwrap_or_default();
    let body = if text.contains("Forgebox-fake-silent") {
        std::process::exit(3);
    } else if text.contains("Forgebox-fake-garbage") {
        "this is not json".to_owned()
    } else if text.contains("Forgebox-fake-refuse") {
        format!(
            "{{\"v\":1,\"request_id\":\"{id}\",\"ok\":false,\"code\":\"forbidden\",\"message\":\"fake helper refused\",\"details\":null}}"
        )
    } else {
        if text.contains("Forgebox-fake-sleep") {
            std::thread::sleep(Duration::from_secs(5));
        }
        format!(
            "{{\"v\":1,\"request_id\":\"{id}\",\"ok\":true,\"message\":\"fake helper ok\",\"details\":{{\"request_bytes\":{}}}}}",
            text.len()
        )
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&response)
    else {
        std::process::exit(4);
    };
    if file.write_all(body.as_bytes()).is_err() {
        std::process::exit(6);
    }
}
