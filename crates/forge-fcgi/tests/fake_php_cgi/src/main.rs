//! Fake `php-cgi.exe` for the forge-fcgi tests (no dependencies, no PHP).
//!
//! `fake_php_cgi -b <ip>:<port> [-c <ini>]...`: listens like `php-cgi -b` and serves one
//! FastCGI connection at a time, as php-cgi does on Windows:
//! * `FCGI_GET_VALUES` → `FCGI_MAX_CONNS=1`, `FCGI_MAX_REQS=1`, `FCGI_MPXS_CONNS=0`
//!   (management records do not count as requests);
//! * a `FCGI_RESPONDER` request whose `SCRIPT_FILENAME` exists → `200` with the body of
//!   the Forgebox health script, `forgebox-health <version> cgi-fcgi <pid>`; a missing
//!   script → `404 No input file specified.`; `QUERY_STRING` `sleep_ms=<n>` waits n ms
//!   before answering;
//! * after the answer the connection is shut down for writing, drained and closed (the
//!   server end goes to `TIME_WAIT`, as with php-cgi);
//! * `PHP_FCGI_MAX_REQUESTS` (default 500, `0` = no limit): exits 0 after that many
//!   requests.
//!
//! Test knobs (environment): `FAKE_PHP_VERSION` (default `8.3.99`),
//! `FAKE_LISTEN_DELAY_MS` (bind only after that), `FAKE_EXIT_CODE` (exit at once with
//! it, before binding), `FAKE_ALLOC_MB` (keep that much memory touched: a big working
//! set). Prints `listening <addr>` once bound and `PHPRC=<value>` at start.
//! CTRL_BREAK ends the process (default console handling).

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

const BEGIN_REQUEST: u8 = 1;
const END_REQUEST: u8 = 3;
const PARAMS: u8 = 4;
const STDIN: u8 = 5;
const STDOUT: u8 = 6;
const GET_VALUES: u8 = 9;
const GET_VALUES_RESULT: u8 = 10;

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

fn record(kind: u8, id: u16, content: &[u8]) -> Vec<u8> {
    let padding = (8 - content.len() % 8) % 8;
    let mut out = vec![1, kind];
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&(content.len() as u16).to_be_bytes());
    out.push(padding as u8);
    out.push(0);
    out.extend_from_slice(content);
    out.extend(std::iter::repeat_n(0, padding));
    out
}

fn pairs(mut bytes: &[u8]) -> Vec<(String, String)> {
    fn len(bytes: &mut &[u8]) -> Option<usize> {
        let first = *bytes.first()?;
        if first < 128 {
            *bytes = &bytes[1..];
            Some(first as usize)
        } else {
            let raw = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) & 0x7FFF_FFFF;
            *bytes = &bytes[4..];
            Some(raw as usize)
        }
    }
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let (Some(n), Some(v)) = (len(&mut bytes), len(&mut bytes)) else {
            break;
        };
        if bytes.len() < n + v {
            break;
        }
        out.push((
            String::from_utf8_lossy(&bytes[..n]).into_owned(),
            String::from_utf8_lossy(&bytes[n..n + v]).into_owned(),
        ));
        bytes = &bytes[n + v..];
    }
    out
}

fn encode_pairs(items: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (n, v) in items {
        out.push(n.len() as u8);
        out.push(v.len() as u8);
        out.extend_from_slice(n.as_bytes());
        out.extend_from_slice(v.as_bytes());
    }
    out
}

/// Reads one record: (type, request id, content).
fn read_record(stream: &mut TcpStream) -> Option<(u8, u16, Vec<u8>)> {
    let mut header = [0_u8; 8];
    stream.read_exact(&mut header).ok()?;
    let len = u16::from_be_bytes([header[4], header[5]]) as usize;
    let mut body = vec![0_u8; len + header[6] as usize];
    stream.read_exact(&mut body).ok()?;
    body.truncate(len);
    Some((header[1], u16::from_be_bytes([header[2], header[3]]), body))
}

/// Shuts the write side down, drains what the client still sends, closes.
fn close(mut stream: TcpStream) {
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut sink = [0_u8; 512];
    while matches!(stream.read(&mut sink), Ok(n) if n > 0) {}
}

/// Serves one connection; `true` when it was a request (counts towards the limit).
fn serve(mut stream: TcpStream, version: &str) -> bool {
    let Some((kind, id, content)) = read_record(&mut stream) else {
        return false; // probe connection closed without data
    };
    if kind == GET_VALUES {
        let answer = encode_pairs(&[
            ("FCGI_MAX_CONNS", "1"),
            ("FCGI_MAX_REQS", "1"),
            ("FCGI_MPXS_CONNS", "0"),
        ]);
        let _ = stream.write_all(&record(GET_VALUES_RESULT, 0, &answer));
        close(stream);
        return false;
    }
    if kind != BEGIN_REQUEST || content.len() != 8 {
        close(stream);
        return false;
    }
    let mut params = Vec::new();
    loop {
        let Some((kind, _, content)) = read_record(&mut stream) else {
            return true;
        };
        match kind {
            PARAMS if content.is_empty() => {}
            PARAMS => params.extend_from_slice(&content),
            STDIN if content.is_empty() => break,
            _ => {}
        }
    }
    let params = pairs(&params);
    let param = |name: &str| {
        params
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    };
    for part in param("QUERY_STRING").split('&') {
        if let Some(ms) = part.strip_prefix("sleep_ms=").and_then(|v| v.parse().ok()) {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
    let script = param("SCRIPT_FILENAME");
    let output = if !script.is_empty() && Path::new(script).is_file() {
        format!(
            "Content-Type: text/plain; charset=UTF-8\r\n\r\nforgebox-health {version} cgi-fcgi {}\n",
            std::process::id()
        )
    } else {
        "Status: 404 Not Found\r\nContent-type: text/html; charset=UTF-8\r\n\r\nNo input file specified.\n"
            .to_owned()
    };
    let mut answer = record(STDOUT, id, output.as_bytes());
    answer.extend(record(STDOUT, id, &[]));
    answer.extend(record(END_REQUEST, id, &[0, 0, 0, 0, 0, 0, 0, 0]));
    let _ = stream.write_all(&answer);
    close(stream);
    true
}

fn main() {
    if let Some(code) = env_u64("FAKE_EXIT_CODE") {
        eprintln!("fake php-cgi: exiting with {code}");
        std::process::exit(code as i32);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bind = args
        .iter()
        .position(|a| a == "-b")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| {
            eprintln!("usage: fake_php_cgi -b <ip>:<port>");
            std::process::exit(2);
        });
    println!(
        "PHPRC={}",
        std::env::var("PHPRC").unwrap_or_else(|_| "<unset>".into())
    );
    let version = std::env::var("FAKE_PHP_VERSION").unwrap_or_else(|_| "8.3.99".into());
    let max_requests = env_u64("PHP_FCGI_MAX_REQUESTS").unwrap_or(500);
    let _memory: Vec<u8> = match env_u64("FAKE_ALLOC_MB") {
        Some(mb) => vec![1_u8; (mb as usize) * 1024 * 1024],
        None => Vec::new(),
    };
    if let Some(ms) = env_u64("FAKE_LISTEN_DELAY_MS") {
        std::thread::sleep(Duration::from_millis(ms));
    }
    let listener = match TcpListener::bind(&bind) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("fake php-cgi: cannot bind {bind}: {err}");
            std::process::exit(255);
        }
    };
    println!("listening {bind}");
    let _ = std::io::stdout().flush();
    let mut served = 0_u64;
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if serve(stream, &version) {
            served += 1;
            if max_requests > 0 && served >= max_requests {
                println!("served {served} requests, exiting");
                return;
            }
        }
    }
}
