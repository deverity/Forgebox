//! The daemon's internal HTTP endpoint (docs/06 §4.4 and §8), on `127.0.0.1` at the
//! port of the `internal` range (47481 by default): the web servers send there what
//! only the daemon can answer. Today:
//!
//! * `GET /_forgebox/starting?host=<host>` — the "site starting" page of
//!   `autostart: on-visit` processes (`AppManager::starting_page`: the first visit starts
//!   them, later ones only render; 503 + `Retry-After` while starting, 500 on failure),
//!   in pt-BR or en-US from `Accept-Language`;
//! * `GET /_forgebox/health` — `ok` (diagnostics).
//!
//! Minimal HTTP/1.x: one request per connection, request head of at most 8 KiB read
//! within 5 s, `Connection: close`. Every connection is its own task; nothing here
//! waits on a process.

use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::model::port::{OwnerKind, ReservationOwner};
use forge_sites::AppManager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// Named port range of the endpoint (`forgebox.toml [ports.ranges] internal`).
pub const INTERNAL_RANGE: &str = "internal";
const HEAD_LIMIT: usize = 8 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// Owner of the endpoint's port reservation.
pub fn owner() -> Result<ReservationOwner> {
    ReservationOwner::new(OwnerKind::Daemon, "forgeboxd", "internal")
}

/// What a request asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Starting { host: String, lang: &'static str },
    Health,
    NotFound,
}

/// Parses the request head (`GET /path?query HTTP/1.1` + headers).
pub fn route(head: &str) -> Route {
    let mut lines = head.lines();
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let (Some("GET"), Some(target)) = (parts.next(), parts.next()) else {
        return Route::NotFound;
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let accept = lines
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("accept-language")
                .then(|| value.trim().to_ascii_lowercase())
        })
        .unwrap_or_default();
    let lang = if accept.starts_with("en") {
        "en-US"
    } else {
        "pt-BR"
    };
    match path {
        "/_forgebox/health" => Route::Health,
        "/_forgebox/starting" => {
            let host = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("host="))
                .unwrap_or_default()
                .to_ascii_lowercase();
            let valid = !host.is_empty()
                && host.len() <= 253
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
            if valid {
                Route::Starting { host, lang }
            } else {
                Route::NotFound
            }
        }
        _ => Route::NotFound,
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

fn response(status: u16, content_type: &str, retry_after: u32, body: &str) -> String {
    let retry = if retry_after > 0 {
        format!("Retry-After: {retry_after}\r\n")
    } else {
        String::new()
    };
    format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nCache-Control: no-store\r\n{retry}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        reason(status),
        body.len()
    )
}

async fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() >= HEAD_LIMIT {
            break;
        }
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

async fn handle(mut stream: TcpStream, apps: AppManager) {
    let Ok(Some(head)) = tokio::time::timeout(READ_TIMEOUT, read_head(&mut stream)).await else {
        return;
    };
    let answer = match route(&head) {
        Route::Health => response(200, "text/plain; charset=utf-8", 0, "ok"),
        Route::NotFound => response(404, "text/plain; charset=utf-8", 0, "not found"),
        Route::Starting { host, lang } => {
            match tokio::time::timeout(ANSWER_TIMEOUT, apps.starting_page(&host, lang)).await {
                Ok(Ok(page)) => response(
                    page.status,
                    "text/html; charset=utf-8",
                    page.retry_after_secs,
                    &page.html,
                ),
                Ok(Err(err)) => {
                    let status = if err.code() == "not_found" { 404 } else { 500 };
                    response(status, "text/plain; charset=utf-8", 0, &err.to_string())
                }
                Err(_) => response(503, "text/plain; charset=utf-8", 2, "starting"),
            }
        }
    };
    let _ = tokio::time::timeout(ANSWER_TIMEOUT, stream.write_all(answer.as_bytes())).await;
    let _ = stream.shutdown().await;
}

/// Binds `127.0.0.1:<port>` and serves until `stop`. Returns the bound port.
pub async fn serve(port: u16, apps: AppManager, stop: CancellationToken) -> Result<u16> {
    let listener =
        TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| ForgeError::PortInUse {
                port,
                owner: Some(e.to_string()),
                pid: None,
                exe: None,
            })?;
    let bound = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                () = stop.cancelled() => break,
                accepted = listener.accept() => accepted,
            };
            match accepted {
                Ok((stream, _)) => {
                    tokio::spawn(handle(stream, apps.clone()));
                }
                Err(err) => {
                    tracing::debug!(%err, "internal endpoint: accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    });
    tracing::info!(port = bound, "internal endpoint listening");
    Ok(bound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_and_languages() {
        assert_eq!(
            route(
                "GET /_forgebox/starting?host=App.Test HTTP/1.1\r\nAccept-Language: en-US,en;q=0.9\r\n\r\n"
            ),
            Route::Starting {
                host: "app.test".into(),
                lang: "en-US"
            }
        );
        assert_eq!(
            route("GET /_forgebox/starting?x=1&host=a.test HTTP/1.0\r\n\r\n"),
            Route::Starting {
                host: "a.test".into(),
                lang: "pt-BR"
            }
        );
        assert_eq!(
            route("GET /_forgebox/health HTTP/1.1\r\n\r\n"),
            Route::Health
        );
        for bad in [
            "POST /_forgebox/health HTTP/1.1\r\n\r\n",
            "GET /_forgebox/starting?host=a/../b HTTP/1.1\r\n\r\n",
            "GET /_forgebox/starting HTTP/1.1\r\n\r\n",
            "GET / HTTP/1.1\r\n\r\n",
            "",
        ] {
            assert_eq!(route(bad), Route::NotFound, "{bad:?}");
        }
    }

    #[test]
    fn responses_carry_length_and_retry() {
        let r = response(503, "text/html", 2, "<p>x</p>");
        assert!(r.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
        assert!(r.contains("Retry-After: 2\r\n"));
        assert!(r.contains("Content-Length: 8\r\n"));
        assert!(r.ends_with("\r\n\r\n<p>x</p>"));
        assert!(!response(200, "text/plain", 0, "ok").contains("Retry-After"));
    }
}
