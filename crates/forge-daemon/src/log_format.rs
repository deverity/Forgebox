//! Line parsers of the log viewer (docs/09 `forge logs`, docs/07 §5.8): the timestamp and
//! the level of a line of a known `format` — the `service.daemon.logs[].format` of the
//! manifests, never the tool id — and the redaction of app output (docs/11 §6).
//!
//! Formats: `nginx-error`, `apache-error`, `php`, `mysql` (MySQL and MariaDB),
//! `postgresql`, `json` (the daemon's own lines). Anything else (and a line that does not
//! match its format, like the frames of a PHP stack trace) falls back to the generic rule:
//! a JSON `level`/`timestamp`, or a `[error]`-like marker. Levels are normalized to
//! `error`, `warn`, `info` and `debug`.
//!
//! Timestamps without a zone (nginx, Apache, MariaDB) are in the machine's local time,
//! which is where the daemon and the server run.

use forge_core::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::{Offset, TimeZone};

/// What a parser found in one line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    pub ts: Option<Timestamp>,
    pub level: Option<String>,
}

/// Parses one line of a log of `format` (`None` = plain text).
pub fn parse(format: Option<&str>, line: &str) -> Parsed {
    let line = line.trim_end_matches(['\r', '\n']);
    let found = match format {
        Some("nginx-error") => nginx_error(line),
        Some("apache-error") => apache_error(line),
        Some("php") => php(line),
        Some("mysql") => mysql(line),
        Some("postgresql") => postgresql(line),
        Some("json" | "daemon-json") => json(line),
        _ => None,
    };
    let mut parsed = found.unwrap_or_default();
    if parsed.level.is_none() || parsed.ts.is_none() {
        let generic = generic(line);
        parsed.level = parsed.level.or(generic.level);
        parsed.ts = parsed.ts.or(generic.ts);
    }
    parsed
}

fn generic(line: &str) -> Parsed {
    let mut parsed = json(line).unwrap_or_default();
    if parsed.level.is_none() {
        let lower = line.to_ascii_lowercase();
        parsed.level = ["error", "warn", "info", "debug"]
            .into_iter()
            .find(|level| lower.contains(&format!("[{level}")))
            .map(str::to_owned);
    }
    parsed
}

fn json(line: &str) -> Option<Parsed> {
    if !line.trim_start().starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    Some(Parsed {
        ts: value
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(|t| t.parse().ok()),
        level: value
            .get("level")
            .and_then(|l| l.as_str())
            .map(|l| normalize_level(&l.to_ascii_lowercase())),
    })
}

/// `warning` → `warn`, `crit` → `error`, `notice` → `info`...
fn normalize_level(level: &str) -> String {
    match level {
        "emerg" | "emergency" | "alert" | "crit" | "critical" | "fatal" | "panic" | "err"
        | "error" => "error",
        "warn" | "warning" => "warn",
        "notice" | "info" | "note" | "system" | "log" => "info",
        l if l.starts_with("debug") || l.starts_with("trace") => "debug",
        other => return other.to_owned(),
    }
    .to_owned()
}

fn local() -> TimeZone {
    TimeZone::system()
}

fn at(date: (i16, i8, i8), time: (i8, i8, i8), nanos: i32, tz: &TimeZone) -> Option<Timestamp> {
    let civil = DateTime::new(date.0, date.1, date.2, time.0, time.1, time.2, nanos).ok()?;
    civil.to_zoned(tz.clone()).ok().map(|z| z.timestamp())
}

/// `HH:MM:SS` or `HH:MM:SS.ffffff` → ((h, m, s), nanoseconds).
fn clock(text: &str) -> Option<((i8, i8, i8), i32)> {
    let (hms, frac) = match text.split_once('.') {
        Some((hms, frac)) => (hms, Some(frac)),
        None => (text, None),
    };
    let mut parts = hms.split(':');
    let h = parts.next()?.parse().ok()?;
    let m = parts.next()?.parse().ok()?;
    let s = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let nanos = match frac {
        Some(f) if !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()) => {
            let digits: String = f.chars().take(9).collect();
            digits.parse::<i32>().ok()? * 10_i32.pow(9 - digits.len() as u32)
        }
        Some(_) => return None,
        None => 0,
    };
    Some(((h, m, s), nanos))
}

fn month(name: &str) -> Option<i8> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let lower = name.to_ascii_lowercase();
    MONTHS.iter().position(|m| *m == lower).map(|i| i as i8 + 1)
}

/// `2026-10-27` → (2026, 10, 27); `/` also separates (nginx).
fn ymd(text: &str) -> Option<(i16, i8, i8)> {
    let mut parts = text.split(['-', '/']);
    let y = parts.next()?.parse().ok()?;
    let m = parts.next()?.parse().ok()?;
    let d = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((y, m, d))
}

/// The text between the first `[` and its `]` at the start of `line`, and the rest.
fn bracket(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('[')?;
    let end = rest.find(']')?;
    Some((&rest[..end], rest[end + 1..].trim_start()))
}

/// `2026/10/27 10:00:00 [error] 1234#5678: *1 CreateFile() failed`.
fn nginx_error(line: &str) -> Option<Parsed> {
    let mut words = line.splitn(3, ' ');
    let date = ymd(words.next()?)?;
    let (time, nanos) = clock(words.next()?)?;
    let (level, _) = bracket(words.next()?)?;
    Some(Parsed {
        ts: at(date, time, nanos, &local()),
        level: Some(normalize_level(&level.to_ascii_lowercase())),
    })
}

/// `[Tue Oct 27 10:00:00.123456 2026] [core:error] [pid 1:tid 2] AH00124: ...`.
fn apache_error(line: &str) -> Option<Parsed> {
    let (stamp, rest) = bracket(line)?;
    let words: Vec<&str> = stamp.split_whitespace().collect();
    // [dow, month, day, time, year]
    let [_, mon, day, time, year] = words.as_slice() else {
        return None;
    };
    let (time, nanos) = clock(time)?;
    let ts = at(
        (year.parse().ok()?, month(mon)?, day.parse().ok()?),
        time,
        nanos,
        &local(),
    );
    let level = bracket(rest).map(|(module_level, _)| {
        let level = module_level.rsplit(':').next().unwrap_or(module_level);
        normalize_level(&level.to_ascii_lowercase())
    });
    Some(Parsed { ts, level })
}

/// `[27-Oct-2026 10:00:00 UTC] PHP Fatal error:  Uncaught Exception: x in /a.php:3`. The
/// frames of a stack trace (`#0 ...`, `thrown in ...`) have neither.
fn php(line: &str) -> Option<Parsed> {
    let (ts, message) = match bracket(line) {
        Some((stamp, rest)) => {
            let mut words = stamp.split_whitespace();
            let date = words.next()?;
            let time = words.next()?;
            let zone = words.next();
            let mut parts = date.split('-');
            let day: i8 = parts.next()?.parse().ok()?;
            let mon = month(parts.next()?)?;
            let year: i16 = parts.next()?.parse().ok()?;
            let (time, nanos) = clock(time)?;
            let tz = match zone {
                Some("UTC") => TimeZone::UTC,
                Some(name) => TimeZone::get(name).unwrap_or_else(|_| local()),
                None => local(),
            };
            (at((year, mon, day), time, nanos, &tz), rest)
        }
        None => (None, line),
    };
    let message = message.strip_prefix("PHP ")?;
    let level = [
        ("Fatal error", "error"),
        ("Parse error", "error"),
        ("Recoverable fatal error", "error"),
        ("Warning", "warn"),
        ("Deprecated", "warn"),
        ("Notice", "info"),
        ("Strict Standards", "info"),
    ]
    .into_iter()
    .find(|(prefix, _)| {
        message
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with(':'))
    })
    .map(|(_, level)| level.to_owned());
    level.as_ref()?;
    Some(Parsed { ts, level })
}

/// MySQL 8: `2026-10-27T10:00:00.123456Z 0 [Warning] [MY-010068] [Server] ...`;
/// MariaDB: `2026-10-27 10:00:00 0 [Note] ...`.
fn mysql(line: &str) -> Option<Parsed> {
    let first = line.split_whitespace().next()?;
    let ts = if first.contains('T') {
        first.parse::<Timestamp>().ok()
    } else {
        let mut words = line.split_whitespace();
        let date = ymd(words.next()?)?;
        let (time, nanos) = clock(words.next()?)?;
        at(date, time, nanos, &local())
    };
    ts?;
    let level = line.match_indices('[').find_map(|(i, _)| {
        let (name, _) = bracket(&line[i..])?;
        let lower = name.to_ascii_lowercase();
        matches!(
            lower.as_str(),
            "system" | "note" | "info" | "warning" | "warn" | "error" | "debug"
        )
        .then(|| normalize_level(&lower))
    });
    Some(Parsed { ts, level })
}

/// `2026-10-27 10:00:00.123 UTC [1234] LOG:  database system is ready`.
fn postgresql(line: &str) -> Option<Parsed> {
    let mut words = line.split_whitespace();
    let date = ymd(words.next()?)?;
    let (time, nanos) = clock(words.next()?)?;
    let zone = words.next()?;
    let tz = match zone {
        "UTC" | "GMT" => TimeZone::UTC,
        z if z.starts_with(['+', '-']) => offset(z).map_or_else(local, Offset::to_time_zone),
        _ => local(),
    };
    let level = line.split_whitespace().skip(3).find_map(|word| {
        let name = word.strip_suffix(':')?;
        const LEVELS: [&str; 11] = [
            "LOG",
            "INFO",
            "NOTICE",
            "WARNING",
            "ERROR",
            "FATAL",
            "PANIC",
            "STATEMENT",
            "DETAIL",
            "HINT",
            "CONTEXT",
        ];
        (LEVELS.contains(&name)
            || name
                .strip_prefix("DEBUG")
                .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit())))
        .then(|| normalize_level(&name.to_ascii_lowercase()))
    });
    Some(Parsed {
        ts: at(date, time, nanos, &tz),
        level: level.map(|l| match l.as_str() {
            "statement" | "detail" | "hint" | "context" => "info".to_owned(),
            _ => l,
        }),
    })
}

/// `-03`, `+0530`, `+05:30`.
fn offset(text: &str) -> Option<Offset> {
    let sign = if text.starts_with('-') { -1 } else { 1 };
    let digits: String = text[1..].chars().filter(char::is_ascii_digit).collect();
    let (h, m) = match digits.len() {
        1 | 2 => (digits.parse::<i32>().ok()?, 0),
        4 => (digits[..2].parse().ok()?, digits[2..].parse().ok()?),
        _ => return None,
    };
    Offset::from_seconds(sign * (h * 3600 + m * 60)).ok()
}

/// Masks credentials in a line of app output (docs/11 §6): the value of an
/// `Authorization:` header (with its scheme) and of `password=` / `passwd=` /
/// `"password": "..."` pairs. Keys are matched without regard to case; the rest of the
/// line is kept.
pub fn redact(line: &str) -> String {
    let mut out = line.to_owned();
    out = mask_after(&out, "authorization:", true);
    for key in ["password", "passwd"] {
        out = mask_pairs(&out, key);
    }
    out
}

const MASK: &str = "***";

fn is_value_end(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '&' | ',' | ';')
}

/// Masks what follows each `marker` (ASCII, lowercase): one value, or two when the first
/// word is an authentication scheme.
fn mask_after(text: &str, marker: &str, scheme: bool) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some(found) = lower[pos..].find(marker) {
        let start = pos + found + marker.len();
        out.push_str(&text[pos..start]);
        let rest = &text[start..];
        let spaces = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        out.push_str(&rest[..spaces]);
        let value = &rest[spaces..];
        let mut end = value.find(is_value_end).unwrap_or(value.len());
        if scheme && end > 0 {
            let word = value[..end].to_ascii_lowercase();
            if matches!(
                word.as_str(),
                "bearer" | "basic" | "digest" | "token" | "negotiate"
            ) {
                let after = &value[end..];
                let gap = after.len() - after.trim_start_matches([' ', '\t']).len();
                let tail = &after[gap..];
                end += gap + tail.find(is_value_end).unwrap_or(tail.len());
            }
        }
        if end > 0 {
            out.push_str(MASK);
        }
        pos = start + spaces + end;
    }
    out.push_str(&text[pos..]);
    out
}

/// `key=value`, `key: value` and `"key": "value"` for one key.
fn mask_pairs(text: &str, key: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some(found) = lower[pos..].find(key) {
        let key_end = pos + found + key.len();
        let rest = &text[key_end..];
        let after_quote = rest.strip_prefix('"').unwrap_or(rest);
        let Some(sep) = after_quote.strip_prefix(['=', ':']) else {
            out.push_str(&text[pos..key_end]);
            pos = key_end;
            continue;
        };
        let value = sep.trim_start_matches([' ', '\t']);
        let value = value.strip_prefix('"').unwrap_or(value);
        let value_start = text.len() - value.len();
        let end = value.find(is_value_end).unwrap_or(value.len());
        out.push_str(&text[pos..value_start]);
        if end > 0 {
            out.push_str(MASK);
        }
        pos = value_start + end;
    }
    out.push_str(&text[pos..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The civil time of `ts` in the machine's zone.
    fn local_civil(ts: Timestamp) -> String {
        ts.to_zoned(TimeZone::system())
            .strftime("%Y-%m-%d %H:%M:%S")
            .to_string()
    }

    fn level(format: &str, line: &str) -> Option<String> {
        parse(Some(format), line).level
    }

    #[test]
    fn nginx_130_error_lines() {
        let line = "2026/10/27 10:15:42 [error] 8412#9120: *17 CreateFile() \"C:/forgebox/www/loja/missing.css\" failed (2: The system cannot find the file specified), client: 127.0.0.1, server: loja.test, request: \"GET /missing.css HTTP/1.1\", host: \"loja.test\"";
        let p = parse(Some("nginx-error"), line);
        assert_eq!(p.level.as_deref(), Some("error"));
        assert_eq!(local_civil(p.ts.unwrap()), "2026-10-27 10:15:42");
        let notice = "2026/10/27 10:15:40 [notice] 8412#9120: nginx/1.30.0";
        assert_eq!(level("nginx-error", notice).as_deref(), Some("info"));
        let crit = "2026/10/27 10:15:41 [crit] 8412#9120: *1 SSL_do_handshake() failed";
        assert_eq!(level("nginx-error", crit).as_deref(), Some("error"));
        let warn = "2026/10/27 10:15:41 [warn] 8412#9120: conflicting server name";
        assert_eq!(level("nginx-error", warn).as_deref(), Some("warn"));
    }

    #[test]
    fn apache_24_error_lines() {
        let line = "[Tue Oct 27 10:15:42.123456 2026] [core:error] [pid 4120:tid 1204] [client 127.0.0.1:50321] AH00124: Request exceeded the limit of 10 internal redirects";
        let p = parse(Some("apache-error"), line);
        assert_eq!(p.level.as_deref(), Some("error"));
        let ts = p.ts.unwrap();
        assert_eq!(local_civil(ts), "2026-10-27 10:15:42");
        assert_eq!(ts.subsec_nanosecond(), 123_456_000);
        let notice = "[Tue Oct 27 10:15:40.000001 2026] [mpm_winnt:notice] [pid 4120:tid 1204] AH00455: Apache/2.4.66 configured";
        assert_eq!(level("apache-error", notice).as_deref(), Some("info"));
        let warn = "[Tue Oct 27 10:15:40.000001 2026] [ssl:warn] [pid 4120] AH01909: x";
        assert_eq!(level("apache-error", warn).as_deref(), Some("warn"));
        let bare = "[Tue Oct 27 10:15:40.000001 2026] [error] oops";
        assert_eq!(level("apache-error", bare).as_deref(), Some("error"));
    }

    #[test]
    fn php_84_lines_with_a_stack_trace() {
        let first = "[27-Oct-2026 10:15:42 UTC] PHP Fatal error:  Uncaught RuntimeException: boom in C:\\forgebox\\www\\loja\\public\\index.php:12";
        let p = parse(Some("php"), first);
        assert_eq!(p.level.as_deref(), Some("error"));
        assert_eq!(p.ts, Some("2026-10-27T10:15:42Z".parse().unwrap()));
        // The frames carry neither (and are not errors by accident).
        for frame in [
            "Stack trace:",
            "#0 C:\\forgebox\\www\\loja\\public\\index.php(20): run()",
            "#1 {main}",
            "  thrown in C:\\forgebox\\www\\loja\\public\\index.php on line 12",
        ] {
            assert_eq!(parse(Some("php"), frame), Parsed::default(), "{frame}");
        }
        let warning =
            "[27-Oct-2026 10:15:43 UTC] PHP Warning:  Undefined variable $x in /a.php on line 3";
        assert_eq!(level("php", warning).as_deref(), Some("warn"));
        let deprecated =
            "[27-Oct-2026 10:15:43 UTC] PHP Deprecated:  Return type of X::y() in /a.php on line 3";
        assert_eq!(level("php", deprecated).as_deref(), Some("warn"));
        let notice = "[27-Oct-2026 10:15:43 UTC] PHP Notice:  A non well formed number in /a.php";
        assert_eq!(level("php", notice).as_deref(), Some("info"));
    }

    #[test]
    fn mysql_84_and_mariadb_lines() {
        let line = "2026-10-27T10:15:42.123456Z 0 [Warning] [MY-010068] [Server] CA certificate ca.pem is self signed.";
        let p = parse(Some("mysql"), line);
        assert_eq!(p.level.as_deref(), Some("warn"));
        assert_eq!(p.ts, Some("2026-10-27T10:15:42.123456Z".parse().unwrap()));
        let system = "2026-10-27T10:15:41.000000Z 0 [System] [MY-010931] [Server] C:\\mysqld.exe: ready for connections.";
        assert_eq!(level("mysql", system).as_deref(), Some("info"));
        let error = "2026-10-27T10:15:41.000000Z 0 [ERROR] [MY-010119] [Server] Aborting";
        assert_eq!(level("mysql", error).as_deref(), Some("error"));
        let maria = "2026-10-27 10:15:42 0 [Note] mariadbd: ready for connections.";
        let p = parse(Some("mysql"), maria);
        assert_eq!(p.level.as_deref(), Some("info"));
        assert_eq!(local_civil(p.ts.unwrap()), "2026-10-27 10:15:42");
    }

    #[test]
    fn postgresql_lines() {
        let line = "2026-10-27 10:15:42.123 UTC [4120] LOG:  database system is ready to accept connections";
        let p = parse(Some("postgresql"), line);
        assert_eq!(p.level.as_deref(), Some("info"));
        assert_eq!(p.ts, Some("2026-10-27T10:15:42.123Z".parse().unwrap()));
        let error = "2026-10-27 10:15:43.000 -03 [4188] forge@loja ERROR:  relation \"x\" does not exist at character 15";
        let p = parse(Some("postgresql"), error);
        assert_eq!(p.level.as_deref(), Some("error"));
        assert_eq!(p.ts, Some("2026-10-27T13:15:43Z".parse().unwrap()));
        let warning =
            "2026-10-27 10:15:43.000 UTC [4188] WARNING:  there is no transaction in progress";
        assert_eq!(level("postgresql", warning).as_deref(), Some("warn"));
        let fatal = "2026-10-27 10:15:43.000 UTC [4188] FATAL:  password authentication failed";
        assert_eq!(level("postgresql", fatal).as_deref(), Some("error"));
        let debug = "2026-10-27 10:15:43.000 UTC [4188] DEBUG2:  x";
        assert_eq!(level("postgresql", debug).as_deref(), Some("debug"));
    }

    #[test]
    fn daemon_json_lines_and_the_generic_fallback() {
        let line =
            r#"{"timestamp":"2026-10-27T10:00:00Z","level":"WARN","fields":{"message":"x"}}"#;
        let p = parse(Some("json"), line);
        assert_eq!(p.level.as_deref(), Some("warn"));
        assert_eq!(p.ts, Some("2026-10-27T10:00:00Z".parse().unwrap()));
        assert_eq!(
            parse(None, "2026/10/27 [error] 1#1: boom").level.as_deref(),
            Some("error")
        );
        assert_eq!(parse(Some("unknown"), "plain text"), Parsed::default());
        // A line that does not match its format still gets the generic level.
        assert_eq!(
            level("nginx-error", "garbage [warn] here").as_deref(),
            Some("warn")
        );
    }

    #[test]
    fn credentials_are_masked() {
        assert_eq!(
            redact("-> Authorization: Bearer abc.def.ghi next"),
            "-> Authorization: *** next"
        );
        assert_eq!(
            redact("authorization:Basic dXNlcjpwYXNz"),
            "authorization:***"
        );
        assert_eq!(
            redact("POST /login?user=a&password=hunter2&x=1"),
            "POST /login?user=a&password=***&x=1"
        );
        assert_eq!(
            redact(r#"{"user":"a","password":"hunter2","ok":true}"#),
            r#"{"user":"a","password":"***","ok":true}"#
        );
        assert_eq!(redact("DB_PASSWORD=s3cret done"), "DB_PASSWORD=*** done");
        assert_eq!(redact("nothing to hide"), "nothing to hide");
        assert_eq!(redact("password is required"), "password is required");
        assert_eq!(
            redact("héllo Authorization: X ünï"),
            "héllo Authorization: *** ünï"
        );
    }
}
