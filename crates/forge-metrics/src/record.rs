//! The access record and the format that describes how to read it from a log line.

use jiff::Timestamp;

/// One request, as seen by the web server, ready to be aggregated.
///
/// Built by [`crate::AccessParser`]. All fields are already normalized: `host` is lowercase
/// without port or trailing dot, `route` has no query string and no per-entity ids
/// (docs/14 section 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessRecord {
    /// When the request finished (the bucket key of the aggregator).
    pub end_ts: Timestamp,
    /// Normalized request host (see [`crate::normalize_host`]).
    pub host: String,
    /// HTTP status, `100..=599`.
    pub status: u16,
    /// Total time to serve the request, in microseconds.
    pub duration_us: u64,
    /// Time spent in the upstream (PHP-FPM/proxy), in microseconds; `None` when the request
    /// was served without an upstream.
    pub upstream_us: Option<u64>,
    /// Response body bytes (0 when the format does not log them).
    pub bytes: u64,
    /// Uppercase request method, or `OTHER` for anything that is not a plain token.
    pub method: String,
    /// Normalized route (see [`crate::UriNormalizer`]); empty when the format has no URI.
    pub route: String,
}

/// Unit of a numeric field of the log line. Everything is converted to microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unit {
    /// Seconds, possibly fractional (`0.012`, `1761652800.123`).
    Seconds,
    /// Milliseconds.
    Millis,
    /// Microseconds.
    Micros,
    /// Nanoseconds.
    Nanos,
}

/// How to combine a multi-valued upstream duration (`"0.010, 0.020 : 0.003"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Multi {
    /// Sum of all the values: the total time spent in upstreams (retries and redirects).
    #[default]
    Sum,
    /// Only the last value: the upstream that produced the response.
    Last,
}

/// A JSON pointer (RFC 6901) to a numeric field and its unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldMap {
    /// Pointer into the log line, e.g. `/rt` or `/request/host`. Only object members are
    /// supported (access logs have no arrays on these paths).
    pub pointer: String,
    /// Unit of the value found there.
    pub unit: Unit,
}

impl FieldMap {
    /// Convenience constructor.
    pub fn new(pointer: impl Into<String>, unit: Unit) -> Self {
        Self {
            pointer: pointer.into(),
            unit,
        }
    }
}

/// The upstream duration field: a [`FieldMap`] plus how to combine several values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamMap {
    /// Where the value is and its unit.
    pub field: FieldMap,
    /// `sum` or `last` when the value lists several upstreams.
    pub multi: Multi,
}

/// Crate-local mirror of the `logs[].access` mapping of the manifests (docs/04): which JSON
/// pointers hold each field of an [`AccessRecord`] and in which unit.
///
/// The daemon builds it from the manifest of the web server in use; this crate never looks at
/// the tool id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessFormat {
    /// Timestamp of the request (`/t`, `/ts`).
    pub ts: FieldMap,
    /// `true` when `ts` is the instant the request was *received* (Apache `%{msec}t`), so
    /// `end_ts = ts + duration`. `false` when it is the instant the response was logged
    /// (nginx `$msec`, Caddy `ts`).
    pub ts_is_start: bool,
    /// Host header (`/host`, `/request/host`); a string, may carry a port.
    pub host: String,
    /// HTTP status (`/s`, `/status`); a number or a numeric string.
    pub status: String,
    /// Total request duration.
    pub duration: FieldMap,
    /// Upstream duration, when the server logs it.
    pub upstream_duration: Option<UpstreamMap>,
    /// Response body bytes (`/b`, `/size`).
    pub bytes: Option<String>,
    /// Request method (`/m`, `/request/method`).
    pub method: Option<String>,
    /// Request URI (`/u`, `/request/uri`). The query string is always dropped.
    pub uri: Option<String>,
    /// Apache escapes values with `\xhh` and `\v`, which are not JSON: when set, a pre-pass
    /// rewrites them to valid JSON before parsing.
    pub lenient_escapes: bool,
}

/// An [`AccessFormat`] that cannot be compiled into a parser.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormatError {
    /// The pointer is empty, does not start with `/`, has an empty token or a bad `~` escape.
    #[error("invalid JSON pointer {0:?}")]
    BadPointer(String),
    /// One pointer is a prefix of another (`/request` and `/request/host`).
    #[error("JSON pointer {0:?} overlaps another mapped pointer")]
    Overlap(String),
}
