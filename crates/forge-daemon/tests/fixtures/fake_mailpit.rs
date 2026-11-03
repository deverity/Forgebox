//! Fake `mailpit.exe` of the daemon tests (docs/03 §7.6), compiled with `rustc` by the
//! tests (no dependencies). It is installed as `bin/mailpit/1.32.0/` and found by the
//! manifest's `discover` (`mailpit.exe version` → `mailpit v1.32.0`), so the daemon runs
//! it as the `mailpit/default` service with the manifest's arguments:
//! `--smtp 127.0.0.1:<p> --listen 127.0.0.1:<p> --database <file>`.
//!
//! * Answers HTTP on `--listen` with a minimal Mailpit API v1 over an in-memory mailbox
//!   (`/api/v1/info|messages|search|message/<id>[/headers|/part/1]`, `PUT`/`DELETE
//!   /api/v1/messages`; std only, one thread per connection) and `200 {}` for any other
//!   path (the manifest's health probe); accepts (and drops) connections on `--smtp`.
//!   Test controls: `POST /fake/inject?count=N[&subject=S][&attachment=BYTES]` adds N
//!   unread messages (attachment part `1` of that size, generated on download) and
//!   `POST /fake/hang?on=1|0` makes every other request sleep (a hanging Mailpit).
//! * **Stubborn:** ignores CTRL_C / CTRL_BREAK / CTRL_CLOSE and never exits by itself, so
//!   a stop always runs the whole cascade up to the Job termination.
//! * `fake-mode.txt` next to the executable containing `flood` makes it print
//!   `line <n>` on stdout at ~10 000 lines/s and write the count it reached to
//!   `progress.txt` (next to the executable) every 500 lines: if the daemon stops reading
//!   its stdout, the count stalls. Otherwise it prints `tick <n>` every 200 ms.
//! * `fake-mode.txt` containing `polite` (the log and mail tests): `tick <n>` every 50 ms,
//!   and console events are not ignored, so a stop (and a daemon restart) is quick.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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

// ------------------------------------------------------------------ mailbox

struct Msg {
    id: String,
    read: bool,
    subject: String,
    attachment: Option<u64>,
}

#[derive(Default)]
struct Mailbox {
    /// Oldest first.
    messages: Vec<Msg>,
    next: u32,
}

impl Mailbox {
    fn unread(&self) -> usize {
        self.messages.iter().filter(|m| !m.read).count()
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| percent_decode(v))
}

/// The strings of `"IDs":[...]` in a JSON body.
fn body_ids(body: &str) -> Vec<String> {
    let Some(start) = body.find("\"IDs\"") else {
        return Vec::new();
    };
    let rest = &body[start..];
    let (Some(open), Some(close)) = (rest.find('['), rest.find(']')) else {
        return Vec::new();
    };
    rest[open + 1..close]
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

fn json_str(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn summary(m: &Msg) -> String {
    format!(
        "{{\"ID\":\"{}\",\"Read\":{},\"From\":{{\"Name\":\"Loja\",\"Address\":\"loja@example.test\"}},\"To\":[{{\"Name\":\"\",\"Address\":\"ana@example.test\"}}],\"Subject\":\"{}\",\"Created\":\"2026-10-01T12:00:00Z\",\"Size\":100,\"Attachments\":{}}}",
        m.id,
        m.read,
        json_str(&m.subject),
        u32::from(m.attachment.is_some())
    )
}

fn page(mailbox: &Mailbox, filter: impl Fn(&Msg) -> bool, query: &str) -> String {
    let start: usize = query_param(query, "start")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let limit: usize = query_param(query, "limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let matching: Vec<&Msg> = mailbox
        .messages
        .iter()
        .rev()
        .filter(|m| filter(m))
        .collect();
    let shown: Vec<String> = matching
        .iter()
        .skip(start)
        .take(limit)
        .map(|m| summary(m))
        .collect();
    format!(
        "{{\"total\":{},\"unread\":{},\"count\":{},\"messages_count\":{},\"start\":{start},\"messages\":[{}]}}",
        mailbox.messages.len(),
        mailbox.unread(),
        shown.len(),
        matching.len(),
        shown.join(",")
    )
}

struct Response {
    status: u16,
    content_type: &'static str,
    extra: String,
    body: Vec<u8>,
}

fn json(body: String) -> Response {
    Response {
        status: 200,
        content_type: "application/json",
        extra: String::new(),
        body: body.into_bytes(),
    }
}

fn not_found() -> Response {
    Response {
        status: 404,
        content_type: "text/plain",
        extra: String::new(),
        body: b"not found".to_vec(),
    }
}

fn detail(m: &Msg) -> String {
    let attachments = match m.attachment {
        Some(size) => format!(
            "[{{\"PartID\":\"1\",\"FileName\":\"a.bin\",\"ContentType\":\"application/octet-stream\",\"ContentID\":\"\",\"Size\":{size}}}]"
        ),
        None => "[]".to_owned(),
    };
    format!(
        "{{\"ID\":\"{}\",\"From\":{{\"Name\":\"Loja\",\"Address\":\"loja@example.test\"}},\"To\":[{{\"Name\":\"\",\"Address\":\"ana@example.test\"}}],\"Cc\":[],\"Bcc\":[],\"ReplyTo\":[],\"Subject\":\"{}\",\"Date\":\"2026-10-01T12:00:00Z\",\"Text\":\"body of {}\",\"HTML\":\"\",\"Size\":100,\"Attachments\":{attachments},\"Inline\":[]}}",
        m.id,
        json_str(&m.subject),
        m.id
    )
}

fn route(state: &Mutex<Mailbox>, method: &str, target: &str, body: &str) -> Response {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut mailbox = state.lock().unwrap();
    match (method, path) {
        ("POST", "/fake/inject") => {
            let count: u32 = query_param(query, "count")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            let subject = query_param(query, "subject").unwrap_or_else(|| "Hello".to_owned());
            let attachment = query_param(query, "attachment").and_then(|v| v.parse().ok());
            for _ in 0..count {
                mailbox.next += 1;
                let next = mailbox.next;
                mailbox.messages.push(Msg {
                    id: format!("msg{next}"),
                    read: false,
                    subject: format!("{subject} {next}"),
                    attachment,
                });
            }
            json("{}".to_owned())
        }
        ("GET", "/api/v1/info") => json(format!(
            "{{\"Version\":\"v1.32.0\",\"Messages\":{},\"Unread\":{}}}",
            mailbox.messages.len(),
            mailbox.unread()
        )),
        ("GET", "/api/v1/messages") => json(page(&mailbox, |_| true, query)),
        ("GET", "/api/v1/search") => {
            let q = query_param(query, "query").unwrap_or_default();
            let mut unread = false;
            let mut words = Vec::new();
            for word in q.split_whitespace() {
                if word == "is:unread" {
                    unread = true;
                } else {
                    words.push(word.to_lowercase());
                }
            }
            json(page(
                &mailbox,
                |m| {
                    (!unread || !m.read)
                        && words.iter().all(|w| m.subject.to_lowercase().contains(w))
                },
                query,
            ))
        }
        ("PUT", "/api/v1/messages") => {
            let read = !body.contains("\"Read\":false");
            for id in body_ids(body) {
                if let Some(m) = mailbox.messages.iter_mut().find(|m| m.id == id) {
                    m.read = read;
                }
            }
            json("\"ok\"".to_owned())
        }
        ("DELETE", "/api/v1/messages") => {
            let ids = body_ids(body);
            if ids.is_empty() {
                mailbox.messages.clear();
            } else {
                mailbox.messages.retain(|m| !ids.contains(&m.id));
            }
            json("\"ok\"".to_owned())
        }
        ("GET", p) if p.starts_with("/api/v1/message/") => {
            let rest: Vec<&str> = p["/api/v1/message/".len()..].split('/').collect();
            let Some(m) = mailbox.messages.iter_mut().find(|m| m.id == rest[0]) else {
                return not_found();
            };
            match rest.as_slice() {
                [_] => {
                    // Opening a message marks it read, like the real thing.
                    m.read = true;
                    json(detail(m))
                }
                [_, "headers"] => json(format!("{{\"Subject\":[\"{}\"]}}", json_str(&m.subject))),
                [_, "part", "1"] if m.attachment.is_some() => Response {
                    status: 200,
                    content_type: "application/octet-stream",
                    extra: "Content-Disposition: attachment; filename=\"a.bin\"\r\n".to_owned(),
                    body: vec![b'x'; usize::try_from(m.attachment.unwrap_or(0)).unwrap_or(0)],
                },
                _ => not_found(),
            }
        }
        _ => json("{}".to_owned()),
    }
}

/// Reads one request (headers, then `Content-Length` bytes of body).
fn read_request(stream: &mut TcpStream) -> Option<(String, String, String)> {
    let mut data = Vec::new();
    let mut buf = [0_u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..header_end]).into_owned();
    let length: usize = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0);
    while data.len() < header_end + length {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }
    let mut first = head.lines().next()?.split_whitespace();
    let (method, target) = (first.next()?.to_owned(), first.next()?.to_owned());
    let body = String::from_utf8_lossy(&data[header_end..]).into_owned();
    Some((method, target, body))
}

fn serve(mut stream: TcpStream, state: &Mutex<Mailbox>, hang: &AtomicBool) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let Some((method, target, body)) = read_request(&mut stream) else {
        return;
    };
    if hang.load(Ordering::SeqCst) && !target.starts_with("/fake/") {
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    let response = if target.starts_with("/fake/hang") {
        hang.store(target.contains("on=1"), Ordering::SeqCst);
        json("{}".to_owned())
    } else {
        route(state, &method, &target, &body)
    };
    let reason = if response.status == 200 {
        "OK"
    } else {
        "Not Found"
    };
    let head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.extra,
        response.body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&response.body);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("version") {
        println!("mailpit v1.32.0 compiled with go1.25.0 on windows/amd64");
        return;
    }
    let exe = std::env::current_exe().expect("current exe");
    let mode = std::fs::read_to_string(exe.with_file_name("fake-mode.txt")).unwrap_or_default();
    let (flood, polite) = (mode.trim() == "flood", mode.trim() == "polite");
    if !polite {
        // SAFETY: registers a handler that only returns TRUE.
        unsafe {
            SetConsoleCtrlHandler(Some(ignore), 1);
        }
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
    let state = Arc::new(Mutex::new(Mailbox::default()));
    let hang = Arc::new(AtomicBool::new(false));
    std::thread::spawn(move || {
        for stream in http.incoming().flatten() {
            let (state, hang) = (Arc::clone(&state), Arc::clone(&hang));
            std::thread::spawn(move || serve(stream, &state, &hang));
        }
    });

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
            std::thread::sleep(Duration::from_millis(if polite { 50 } else { 200 }));
        }
    }
}
