//! Access log line -> [`AccessRecord`].
//!
//! The mapping of fields is data ([`AccessFormat`]: JSON pointers and units), never code per
//! web server. The line is decoded once with `serde_json` straight into borrowed values: a
//! trie of the configured pointers drives the deserializer, so only the handful of mapped
//! members are materialized and everything else (headers, TLS info of Caddy...) is skipped
//! without allocating.

use std::borrow::Cow;
use std::fmt;

use jiff::Timestamp;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::record::{AccessFormat, AccessRecord, FormatError, Multi, Unit};
use crate::route::UriNormalizer;
use crate::router::normalize_host;

/// Longest line the parser looks at. Longer lines (a runaway log, a binary file) are rejected
/// without being decoded.
pub const MAX_LINE_BYTES: usize = 16 * 1024;

/// Why a line did not become a record. Cheap to build: no message is formatted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Blank line.
    #[error("empty line")]
    Empty,
    /// More than [`MAX_LINE_BYTES`] bytes.
    #[error("line of {len} bytes exceeds the {MAX_LINE_BYTES} byte limit")]
    TooLong {
        /// Length of the rejected line.
        len: usize,
    },
    /// Not a single JSON object.
    #[error("not a JSON object")]
    Malformed,
    /// A required field is not in the line.
    #[error("missing field `{0}`")]
    Missing(&'static str),
    /// A required field has an unusable value.
    #[error("invalid value for `{0}`")]
    Invalid(&'static str),
}

// Fields captured from the line, in the order of `Captured::vals`.
const TS: usize = 0;
const HOST: usize = 1;
const STATUS: usize = 2;
const DURATION: usize = 3;
const UPSTREAM: usize = 4;
const BYTES: usize = 5;
const METHOD: usize = 6;
const URI: usize = 7;
const FIELDS: usize = 8;

/// A parser for one [`AccessFormat`]. Immutable and cheap to share (`&AccessParser` across
/// threads); build it once per log source.
#[derive(Debug, Clone)]
pub struct AccessParser {
    /// Trie of the mapped pointers; node 0 is the root object.
    arena: Vec<Node>,
    ts_unit: Unit,
    ts_is_start: bool,
    duration_unit: Unit,
    upstream: Option<(Unit, Multi)>,
    lenient_escapes: bool,
    normalizer: UriNormalizer,
}

#[derive(Debug, Clone, Default)]
struct Node {
    /// Member name -> index of the child node.
    children: Vec<(Box<str>, usize)>,
    /// Fields whose value is this node.
    targets: Vec<usize>,
}

impl AccessParser {
    /// Compiles a format with the default [`UriNormalizer`].
    pub fn new(format: &AccessFormat) -> Result<Self, FormatError> {
        Self::with_normalizer(format, UriNormalizer::default())
    }

    /// Compiles a format with a custom [`UriNormalizer`].
    pub fn with_normalizer(
        format: &AccessFormat,
        normalizer: UriNormalizer,
    ) -> Result<Self, FormatError> {
        let mut arena = vec![Node::default()];
        add_pointer(&mut arena, &format.ts.pointer, TS)?;
        add_pointer(&mut arena, &format.host, HOST)?;
        add_pointer(&mut arena, &format.status, STATUS)?;
        add_pointer(&mut arena, &format.duration.pointer, DURATION)?;
        if let Some(up) = &format.upstream_duration {
            add_pointer(&mut arena, &up.field.pointer, UPSTREAM)?;
        }
        for (pointer, field) in [
            (&format.bytes, BYTES),
            (&format.method, METHOD),
            (&format.uri, URI),
        ] {
            if let Some(pointer) = pointer {
                add_pointer(&mut arena, pointer, field)?;
            }
        }
        Ok(Self {
            arena,
            ts_unit: format.ts.unit,
            ts_is_start: format.ts_is_start,
            duration_unit: format.duration.unit,
            upstream: format
                .upstream_duration
                .as_ref()
                .map(|u| (u.field.unit, u.multi)),
            lenient_escapes: format.lenient_escapes,
            normalizer,
        })
    }

    /// Parses one line (without its line terminator; surrounding whitespace is ignored).
    ///
    /// Never panics, whatever the bytes are.
    pub fn parse(&self, line: &[u8]) -> Result<AccessRecord, ParseError> {
        if line.len() > MAX_LINE_BYTES {
            return Err(ParseError::TooLong { len: line.len() });
        }
        let line = line.trim_ascii();
        if line.is_empty() {
            return Err(ParseError::Empty);
        }
        if line[0] != b'{' {
            return Err(ParseError::Malformed);
        }
        // nginx `escape=json` does not validate UTF-8 (a percent-decoded `$uri` can carry a raw
        // 0xFF) and serde_json rejects invalid UTF-8 in strings: replace it with U+FFFD, for
        // every format, so such requests are still counted. Allocates only in that case.
        let line: Cow<'_, [u8]> = match std::str::from_utf8(line) {
            Ok(_) => Cow::Borrowed(line),
            Err(_) => Cow::Owned(String::from_utf8_lossy(line).into_owned().into_bytes()),
        };
        let bytes: Cow<'_, [u8]> = if self.lenient_escapes {
            match fix_lenient_escapes(&line) {
                Cow::Borrowed(_) => line,
                Cow::Owned(fixed) => Cow::Owned(fixed),
            }
        } else {
            line
        };

        let mut captured = Captured::default();
        let mut de = serde_json::Deserializer::from_slice(&bytes);
        NodeSeed {
            arena: &self.arena,
            idx: 0,
            out: &mut captured,
        }
        .deserialize(&mut de)
        .map_err(|_| ParseError::Malformed)?;
        de.end().map_err(|_| ParseError::Malformed)?;

        self.build(&captured)
    }

    /// [`parse`](Self::parse) for a `&str`.
    pub fn parse_str(&self, line: &str) -> Result<AccessRecord, ParseError> {
        self.parse(line.as_bytes())
    }

    fn build(&self, c: &Captured<'_>) -> Result<AccessRecord, ParseError> {
        let ts_us = c
            .get(TS)
            .ok_or(ParseError::Missing("ts"))?
            .to_micros(self.ts_unit)
            .ok_or(ParseError::Invalid("ts"))?;
        let duration_us = c
            .get(DURATION)
            .ok_or(ParseError::Missing("duration"))?
            .to_micros(self.duration_unit)
            .ok_or(ParseError::Invalid("duration"))?;
        let end_us = if self.ts_is_start {
            ts_us.saturating_add(duration_us)
        } else {
            ts_us
        };
        let end_ts = i64::try_from(end_us)
            .ok()
            .and_then(|us| Timestamp::from_microsecond(us).ok())
            .ok_or(ParseError::Invalid("ts"))?;

        let host = match c.get(HOST).ok_or(ParseError::Missing("host"))? {
            Val::Str(s) if s.len() <= 255 => normalize_host(s).into_owned(),
            _ => return Err(ParseError::Invalid("host")),
        };

        let status = c
            .get(STATUS)
            .ok_or(ParseError::Missing("status"))?
            .to_status()
            .ok_or(ParseError::Invalid("status"))?;

        let upstream_us = match (c.get(UPSTREAM), self.upstream) {
            (Some(val), Some((unit, multi))) => val.to_upstream_micros(unit, multi),
            _ => None,
        };
        let bytes = c.get(BYTES).and_then(Val::to_bytes).unwrap_or(0);
        let method = match c.get(METHOD) {
            Some(Val::Str(s)) => sanitize_method(s),
            _ => String::new(),
        };
        let route = match c.get(URI) {
            Some(Val::Str(s)) => self.normalizer.normalize(s),
            _ => String::new(),
        };

        Ok(AccessRecord {
            end_ts,
            host,
            status,
            duration_us,
            upstream_us,
            bytes,
            method,
            route,
        })
    }
}

/// Adds `pointer` to the trie, with `field` as its target.
fn add_pointer(arena: &mut Vec<Node>, pointer: &str, field: usize) -> Result<(), FormatError> {
    let bad = || FormatError::BadPointer(pointer.to_owned());
    let rest = pointer.strip_prefix('/').ok_or_else(bad)?;
    let mut idx = 0;
    for token in rest.split('/') {
        if token.is_empty() {
            return Err(bad());
        }
        let name = unescape_pointer_token(token).ok_or_else(bad)?;
        // A value cannot be both a mapped scalar and a container of other mapped members.
        if !arena[idx].targets.is_empty() {
            return Err(FormatError::Overlap(pointer.to_owned()));
        }
        let existing = arena[idx]
            .children
            .iter()
            .find(|(n, _)| **n == *name)
            .map(|(_, i)| *i);
        idx = match existing {
            Some(i) => i,
            None => {
                arena.push(Node::default());
                let new = arena.len() - 1;
                arena[idx].children.push((name.into_boxed_str(), new));
                new
            }
        };
    }
    if !arena[idx].children.is_empty() {
        return Err(FormatError::Overlap(pointer.to_owned()));
    }
    arena[idx].targets.push(field);
    Ok(())
}

/// RFC 6901: `~1` is `/`, `~0` is `~`; any other `~` is an error.
fn unescape_pointer_token(token: &str) -> Option<String> {
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c == '~' {
            match chars.next()? {
                '0' => out.push('~'),
                '1' => out.push('/'),
                _ => return None,
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------
// Pointer-driven deserialization.
// ---------------------------------------------------------------------------------------

/// A mapped value, borrowed from the line when possible.
#[derive(Debug, Clone)]
enum Val<'de> {
    Str(Cow<'de, str>),
    U64(u64),
    I64(i64),
    F64(f64),
    /// null, bool, object or array where a scalar was expected.
    Other,
}

#[derive(Debug, Default)]
struct Captured<'de> {
    vals: [Option<Val<'de>>; FIELDS],
}

impl<'de> Captured<'de> {
    fn get(&self, field: usize) -> Option<&Val<'de>> {
        self.vals.get(field).and_then(Option::as_ref)
    }
}

/// Drives the deserializer along the trie: object members that are in the trie are descended
/// (or captured when they are mapped), all others are skipped.
struct NodeSeed<'a, 'de> {
    arena: &'a [Node],
    idx: usize,
    out: &'a mut Captured<'de>,
}

impl<'de> NodeSeed<'_, 'de> {
    fn store(&mut self, val: Val<'de>) {
        let Some(node) = self.arena.get(self.idx) else {
            return;
        };
        if let Some((last, rest)) = node.targets.split_last() {
            for t in rest {
                self.out.vals[*t] = Some(val.clone());
            }
            self.out.vals[*last] = Some(val);
        }
    }

    fn is_mapped(&self) -> bool {
        self.arena
            .get(self.idx)
            .is_some_and(|n| !n.targets.is_empty())
    }
}

impl<'de> DeserializeSeed<'de> for NodeSeed<'_, 'de> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for NodeSeed<'_, 'de> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(mut self, _: bool) -> Result<(), E> {
        if self.is_mapped() {
            self.store(Val::Other);
        }
        Ok(())
    }

    fn visit_i64<E>(mut self, v: i64) -> Result<(), E> {
        if self.is_mapped() {
            self.store(Val::I64(v));
        }
        Ok(())
    }

    fn visit_u64<E>(mut self, v: u64) -> Result<(), E> {
        if self.is_mapped() {
            self.store(Val::U64(v));
        }
        Ok(())
    }

    fn visit_f64<E>(mut self, v: f64) -> Result<(), E> {
        if self.is_mapped() {
            self.store(Val::F64(v));
        }
        Ok(())
    }

    fn visit_borrowed_str<E>(mut self, v: &'de str) -> Result<(), E> {
        if self.is_mapped() {
            self.store(Val::Str(Cow::Borrowed(v)));
        }
        Ok(())
    }

    fn visit_str<E>(mut self, v: &str) -> Result<(), E> {
        // Escaped strings are decoded into a scratch buffer: keep a copy.
        if self.is_mapped() {
            self.store(Val::Str(Cow::Owned(v.to_owned())));
        }
        Ok(())
    }

    fn visit_unit<E>(mut self) -> Result<(), E> {
        if self.is_mapped() {
            self.store(Val::Other);
        }
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<(), A::Error> {
        if self.is_mapped() {
            self.store(Val::Other);
        }
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(mut self, mut map: A) -> Result<(), A::Error> {
        let arena = self.arena;
        let children: &[(Box<str>, usize)] =
            arena.get(self.idx).map_or(&[], |n| n.children.as_slice());
        if children.is_empty() {
            // An object where a scalar (or nothing) is expected.
            if self.is_mapped() {
                self.store(Val::Other);
            }
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            return Ok(());
        }
        while let Some(child) = map.next_key_seed(KeySeed { children })? {
            match child {
                Some(idx) => map.next_value_seed(NodeSeed {
                    arena,
                    idx,
                    out: &mut *self.out,
                })?,
                None => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

/// Resolves an object key to the index of a child node without allocating.
struct KeySeed<'a> {
    children: &'a [(Box<str>, usize)],
}

impl<'de> DeserializeSeed<'de> for KeySeed<'_> {
    type Value = Option<usize>;

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for KeySeed<'_> {
    type Value = Option<usize>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an object key")
    }

    fn visit_str<E>(self, v: &str) -> Result<Option<usize>, E> {
        Ok(self
            .children
            .iter()
            .find(|(name, _)| **name == *v)
            .map(|(_, idx)| *idx))
    }
}

// ---------------------------------------------------------------------------------------
// Value conversion.
// ---------------------------------------------------------------------------------------

impl Val<'_> {
    /// The value as microseconds, from a number or a decimal string in `unit`.
    fn to_micros(&self, unit: Unit) -> Option<u64> {
        match self {
            Val::U64(n) => scale_u64(*n, unit),
            Val::I64(n) => scale_u64(u64::try_from(*n).ok()?, unit),
            Val::F64(f) => scale_f64(*f, unit),
            Val::Str(s) => decimal_to_micros(s.trim(), unit),
            Val::Other => None,
        }
    }

    fn to_status(&self) -> Option<u16> {
        let n = match self {
            Val::U64(n) => *n,
            Val::I64(n) => u64::try_from(*n).ok()?,
            Val::F64(f) if f.fract() == 0.0 && *f >= 0.0 && *f < 1000.0 => *f as u64,
            Val::Str(s) => s.trim().parse::<u64>().ok()?,
            _ => return None,
        };
        u16::try_from(n).ok().filter(|s| (100..=599).contains(s))
    }

    /// Bytes: a count, or a string where `-` means zero.
    fn to_bytes(&self) -> Option<u64> {
        match self {
            Val::U64(n) => Some(*n),
            Val::I64(n) => u64::try_from(*n).ok(),
            Val::F64(f) if f.is_finite() && *f >= 0.0 && *f < 9.0e18 => Some(*f as u64),
            Val::Str(s) => s.trim().parse::<u64>().ok(),
            _ => None,
        }
    }

    /// Upstream duration. nginx logs `-` (no upstream), `0.010`, `0.010, 0.020` (retries) and
    /// `0.010 : 0.020` (internal redirect): the numbers are combined with `multi`, anything
    /// else is skipped, and no number at all means no upstream.
    fn to_upstream_micros(&self, unit: Unit, multi: Multi) -> Option<u64> {
        let Val::Str(s) = self else {
            return self.to_micros(unit);
        };
        let mut acc: Option<u64> = None;
        for token in s.split(|c: char| c == ',' || c == ':' || c.is_whitespace()) {
            let Some(us) = decimal_to_micros(token, unit) else {
                continue;
            };
            acc = Some(match (multi, acc) {
                (Multi::Sum, Some(sum)) => sum.saturating_add(us),
                _ => us,
            });
        }
        acc
    }
}

/// Microseconds per unit as a fraction `(num, den)`.
fn unit_ratio(unit: Unit) -> (u128, u128) {
    match unit {
        Unit::Seconds => (1_000_000, 1),
        Unit::Millis => (1_000, 1),
        Unit::Micros => (1, 1),
        Unit::Nanos => (1, 1_000),
    }
}

fn scale_u64(n: u64, unit: Unit) -> Option<u64> {
    let (num, den) = unit_ratio(unit);
    u64::try_from(u128::from(n) * num / den).ok()
}

fn scale_f64(f: f64, unit: Unit) -> Option<u64> {
    if !f.is_finite() || f < 0.0 {
        return None;
    }
    let (num, den) = unit_ratio(unit);
    let us = f * num as f64 / den as f64;
    (us < 9.0e18).then(|| us.round() as u64)
}

/// Exact conversion of a plain decimal string (`12`, `0.012`, `1793167336.296`) without going
/// through floating point. Exponents, signs and anything else are rejected (`None`).
fn decimal_to_micros(s: &str, unit: Unit) -> Option<u64> {
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    // 18 digits of each part are plenty (nginx prints 3 decimals, Caddy about 7).
    if int.len() > 18 {
        return None;
    }
    // Validate BEFORE slicing: `frac` may hold multi-byte characters, and cutting it at an
    // arbitrary byte index would panic on a char boundary.
    if !int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Digits past the 18th are dropped (truncated, not rounded): ASCII only, so the cut is safe.
    let frac = &frac[..frac.len().min(18)];
    let mut mantissa: u128 = 0;
    for b in int.bytes().chain(frac.bytes()) {
        // At most 36 digits: fits in u128 (max ~3.4e38).
        mantissa = mantissa * 10 + u128::from(b - b'0');
    }
    let (num, den) = unit_ratio(unit);
    let pow10 = POW10.get(frac.len()).copied()?;
    // u64 arithmetic covers every realistic value (epoch microseconds are ~1.8e15); u128 is
    // the slow fallback for huge mantissas.
    if let (Ok(m), Ok(n)) = (u64::try_from(mantissa), u64::try_from(num)) {
        if let (Some(scaled), Some(divisor)) = (m.checked_mul(n), pow10.checked_mul(den as u64)) {
            return scaled.checked_add(divisor / 2).map(|v| v / divisor);
        }
    }
    // mantissa < 10^36 and num <= 10^6 can still overflow u128 (~3.4e38): checked, so such
    // values are rejected instead of panicking (debug) or wrapping (release).
    let divisor = u128::from(pow10) * den;
    let scaled = mantissa.checked_mul(num)?.checked_add(divisor / 2)?;
    u64::try_from(scaled / divisor).ok()
}

/// `10^n` for `n` in `0..=18`.
const POW10: [u64; 19] = {
    let mut table = [1u64; 19];
    let mut i = 1;
    while i < 19 {
        table[i] = table[i - 1] * 10;
        i += 1;
    }
    table
};

/// Method as a label: a short alphabetic token, uppercase; anything else is `OTHER`, so a
/// hostile client cannot create unbounded label values.
fn sanitize_method(s: &str) -> String {
    if (1..=16).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphabetic() || b == b'-') {
        s.to_ascii_uppercase()
    } else {
        "OTHER".to_owned()
    }
}

// ---------------------------------------------------------------------------------------
// Lenient escapes (Apache).
// ---------------------------------------------------------------------------------------

/// Rewrites what Apache's `ap_escape_logitem` emits and JSON does not allow: `\xhh` (one byte;
/// a run of them is decoded as UTF-8, invalid sequences become U+FFFD) and `\v`. Valid escapes
/// are copied as pairs, so `\\x41` (an escaped backslash followed by `x41`) is left alone.
/// Returns the input untouched (no allocation) when there is no backslash.
fn fix_lenient_escapes(line: &[u8]) -> Cow<'_, [u8]> {
    if !line.contains(&b'\\') {
        return Cow::Borrowed(line);
    }
    let mut out = Vec::with_capacity(line.len() + 16);
    let mut i = 0;
    while i < line.len() {
        let b = line[i];
        if b != b'\\' {
            out.push(b);
            i += 1;
            continue;
        }
        match line.get(i + 1) {
            Some(b'x') if hex_byte_at(line, i).is_some() => {
                let mut raw = Vec::new();
                let mut j = i;
                while let Some(byte) = hex_byte_at(line, j) {
                    raw.push(byte);
                    j += 4;
                }
                push_json_escaped(&mut out, &String::from_utf8_lossy(&raw));
                i = j;
            }
            Some(b'v') => {
                out.extend_from_slice(b"\\u000b");
                i += 2;
            }
            Some(&next) => {
                out.push(b'\\');
                out.push(next);
                i += 2;
            }
            None => {
                out.push(b'\\');
                i += 1;
            }
        }
    }
    Cow::Owned(out)
}

/// The byte of a `\xhh` sequence starting at `at`, if there is one.
fn hex_byte_at(line: &[u8], at: usize) -> Option<u8> {
    let seq = line.get(at..at + 4)?;
    if seq[0] != b'\\' || seq[1] != b'x' {
        return None;
    }
    let hi = (seq[2] as char).to_digit(16)?;
    let lo = (seq[3] as char).to_digit(16)?;
    u8::try_from(hi * 16 + lo).ok()
}

/// Appends `text` as the inside of a JSON string.
fn push_json_escaped(out: &mut Vec<u8>, text: &str) {
    for c in text.chars() {
        match c {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            c if c < ' ' => out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes()),
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_conversion_is_exact() {
        assert_eq!(decimal_to_micros("0.012", Unit::Seconds), Some(12_000));
        assert_eq!(decimal_to_micros("2.051", Unit::Seconds), Some(2_051_000));
        assert_eq!(decimal_to_micros("0.000", Unit::Seconds), Some(0));
        assert_eq!(
            decimal_to_micros("1793167336.296", Unit::Seconds),
            Some(1_793_167_336_296_000)
        );
        assert_eq!(
            decimal_to_micros("1793167414785", Unit::Millis),
            Some(1_793_167_414_785_000)
        );
        assert_eq!(decimal_to_micros("15638", Unit::Micros), Some(15_638));
        assert_eq!(decimal_to_micros("1500", Unit::Nanos), Some(2));
        assert_eq!(decimal_to_micros("1499", Unit::Nanos), Some(1));
        assert_eq!(decimal_to_micros("2.5", Unit::Millis), Some(2_500));
        assert_eq!(decimal_to_micros(".5", Unit::Seconds), Some(500_000));
        assert_eq!(decimal_to_micros("5.", Unit::Seconds), Some(5_000_000));
        for bad in [
            "", ".", "-", "-1", "+1", "1e3", "abc", "1.2.3", "0x10", " 1",
        ] {
            assert_eq!(decimal_to_micros(bad, Unit::Seconds), None, "bad={bad:?}");
        }
        assert_eq!(decimal_to_micros(&"9".repeat(19), Unit::Seconds), None);
    }

    #[test]
    fn decimal_conversion_never_panics_on_hostile_strings() {
        // A multi-byte character across byte 18 of the fraction (regression: char boundary).
        for frac in [
            "12345678901234567\u{20ac}",
            "1234567890123456\u{e9}9",
            "\u{20ac}",
        ] {
            for unit in [Unit::Seconds, Unit::Millis, Unit::Micros, Unit::Nanos] {
                assert_eq!(decimal_to_micros(&format!("1.{frac}"), unit), None);
                assert_eq!(decimal_to_micros(&format!("{frac}.5"), unit), None);
            }
        }
        // 18 integer + 18 fraction digits: the u128 product overflows (regression).
        let huge = format!("{}.{}", "123456789012345678", "123456789012345678");
        for unit in [Unit::Seconds, Unit::Millis] {
            assert_eq!(decimal_to_micros(&huge, unit), None, "{unit:?}");
        }
        // Past 18 fraction digits the rest is dropped.
        assert_eq!(
            decimal_to_micros("1.0000005000000000000099", Unit::Seconds),
            Some(1_000_001)
        );
    }

    #[test]
    fn upstream_tokens_sum_or_last() {
        let sum = |s: &str| Val::Str(s.into()).to_upstream_micros(Unit::Seconds, Multi::Sum);
        let last = |s: &str| Val::Str(s.into()).to_upstream_micros(Unit::Seconds, Multi::Last);
        assert_eq!(sum("0.010"), Some(10_000));
        assert_eq!(sum("-"), None);
        assert_eq!(sum(""), None);
        assert_eq!(sum("0.010, 0.020"), Some(30_000));
        assert_eq!(sum("0.010, 0.020 : 0.003"), Some(33_000));
        assert_eq!(sum("0.010, - : 0.003"), Some(13_000));
        assert_eq!(last("0.010, 0.020 : 0.003"), Some(3_000));
        assert_eq!(last("0.010, -"), Some(10_000));
        assert_eq!(sum("garbage"), None);
        assert_eq!(
            Val::F64(0.5).to_upstream_micros(Unit::Seconds, Multi::Sum),
            Some(500_000)
        );
    }

    #[test]
    fn lenient_escapes_are_rewritten_to_json() {
        let fix = |s: &str| String::from_utf8(fix_lenient_escapes(s.as_bytes()).into_owned());
        // No backslash: untouched, not copied.
        assert!(matches!(
            fix_lenient_escapes(b"{\"a\":1}"),
            Cow::Borrowed(_)
        ));
        assert_eq!(fix(r#"{"u":"/caf\xc3\xa9"}"#).unwrap(), "{\"u\":\"/café\"}");
        assert_eq!(fix(r#"{"u":"a\x22b"}"#).unwrap(), r#"{"u":"a\"b"}"#);
        assert_eq!(fix(r#"{"u":"a\x5cb"}"#).unwrap(), r#"{"u":"a\\b"}"#);
        assert_eq!(fix(r#"{"u":"a\x01b"}"#).unwrap(), r#"{"u":"a\u0001b"}"#);
        assert_eq!(fix(r#"{"u":"a\vb"}"#).unwrap(), r#"{"u":"a\u000bb"}"#);
        // Invalid UTF-8 becomes U+FFFD instead of breaking the line.
        assert_eq!(fix(r#"{"u":"\xff"}"#).unwrap(), "{\"u\":\"\u{fffd}\"}");
        // An escaped backslash followed by x41 is not an escape.
        assert_eq!(fix(r#"{"u":"a\\x41"}"#).unwrap(), r#"{"u":"a\\x41"}"#);
        // Valid JSON escapes pass through; truncated ones do not panic.
        assert_eq!(fix(r#"{"u":"\"\né"}"#).unwrap(), r#"{"u":"\"\né"}"#);
        assert_eq!(fix(r"\x4").unwrap(), r"\x4");
        assert_eq!(fix(r"\").unwrap(), r"\");
    }

    #[test]
    fn pointers_are_validated() {
        use crate::record::FieldMap;
        let mut f = AccessFormat {
            ts: FieldMap::new("/ts", Unit::Seconds),
            ts_is_start: false,
            host: "/request/host".into(),
            status: "/status".into(),
            duration: FieldMap::new("/duration", Unit::Seconds),
            upstream_duration: None,
            bytes: None,
            method: None,
            uri: None,
            lenient_escapes: false,
        };
        assert!(AccessParser::new(&f).is_ok());
        f.bytes = Some("/request".into());
        assert!(matches!(
            AccessParser::new(&f),
            Err(FormatError::Overlap(_))
        ));
        f.bytes = Some("request/x".into());
        assert!(matches!(
            AccessParser::new(&f),
            Err(FormatError::BadPointer(_))
        ));
        for bad in ["", "/", "/a//b", "/a~2", "/a~"] {
            f.bytes = Some(bad.into());
            assert!(
                matches!(AccessParser::new(&f), Err(FormatError::BadPointer(_))),
                "bad={bad:?}"
            );
        }
        f.bytes = Some("/a~1b".into());
        assert!(AccessParser::new(&f).is_ok());
        // The same pointer for two fields is fine.
        f.bytes = Some("/status".into());
        assert!(AccessParser::new(&f).is_ok());
    }
}
