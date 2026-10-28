//! URI normalizer: request URI -> low-cardinality route (docs/14 section 4.1 and 7).

/// Reduces a request URI to a route that is safe to store and cheap to aggregate.
///
/// - The query string and the fragment are always removed: they are never stored
///   (docs/14 section 7).
/// - A segment that is all digits, a UUID (`8-4-4-4-12` hex) or hex with at least 16
///   characters becomes `{id}`.
/// - Empty segments collapse (`//a/` is `/a`), the case of the remaining segments is kept
///   (paths are case-sensitive, docs/15).
/// - At most `max_segments` segments (default 8) and `max_len` bytes (default 256) are kept;
///   the rest is cut, so an attacker cannot create unbounded route cardinality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UriNormalizer {
    max_segments: usize,
    max_len: usize,
}

/// Default limit of path segments of a route.
pub const MAX_SEGMENTS: usize = 8;
/// Default limit of bytes of a route.
pub const MAX_ROUTE_LEN: usize = 256;

impl Default for UriNormalizer {
    fn default() -> Self {
        Self {
            max_segments: MAX_SEGMENTS,
            max_len: MAX_ROUTE_LEN,
        }
    }
}

impl UriNormalizer {
    /// A normalizer with custom limits (both are clamped to at least 1).
    pub fn new(max_segments: usize, max_len: usize) -> Self {
        Self {
            max_segments: max_segments.max(1),
            max_len: max_len.max(1),
        }
    }

    /// Normalizes a request target (`/a/42?x=1`, or the absolute form `http://h/a`).
    ///
    /// Never fails and never returns an empty string: the root is `/`.
    pub fn normalize(&self, uri: &str) -> String {
        let mut path = uri.trim();
        // Absolute-form request target: drop scheme and authority.
        if let Some(rest) = path
            .strip_prefix("http://")
            .or_else(|| path.strip_prefix("https://"))
        {
            path = rest.find('/').map_or("/", |i| &rest[i..]);
        }
        if let Some(i) = path.find(['?', '#']) {
            path = &path[..i];
        }

        let mut out = String::with_capacity(path.len().min(self.max_len) + 1);
        let mut segments = 0;
        for segment in path.split('/') {
            if segment.is_empty() {
                continue;
            }
            if segments == self.max_segments {
                break;
            }
            // Bytes left after the "/" separator.
            let room = self.max_len.saturating_sub(out.len() + 1);
            if room == 0 {
                break;
            }
            out.push('/');
            segments += 1;
            if is_id_segment(segment) {
                push_truncated(&mut out, "{id}", room);
            } else {
                push_truncated(&mut out, segment, room);
            }
        }
        if out.is_empty() {
            out.push('/');
        }
        out
    }
}

/// Appends at most `room` bytes of `text` (on a char boundary), replacing control characters.
fn push_truncated(out: &mut String, text: &str, room: usize) {
    let mut used = 0;
    for c in text.chars() {
        let c = if c.is_control() { '_' } else { c };
        used += c.len_utf8();
        if used > room {
            break;
        }
        out.push(c);
    }
}

fn is_id_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    if bytes.iter().all(u8::is_ascii_digit) {
        return true;
    }
    if is_uuid(bytes) {
        return true;
    }
    bytes.len() >= 16 && bytes.iter().all(u8::is_ascii_hexdigit)
}

fn is_uuid(bytes: &[u8]) -> bool {
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(uri: &str) -> String {
        UriNormalizer::default().normalize(uri)
    }

    #[test]
    fn strips_query_and_fragment() {
        assert_eq!(n("/search?q=secret&token=abc"), "/search");
        assert_eq!(n("/a/b#frag?x"), "/a/b");
        assert_eq!(n("/?x=1"), "/");
        assert_eq!(n("?x=1"), "/");
        assert!(!n("/a?password=hunter2").contains("hunter2"));
    }

    #[test]
    fn replaces_numeric_uuid_and_long_hex_segments() {
        assert_eq!(n("/users/42/edit"), "/users/{id}/edit");
        assert_eq!(n("/p/550e8400-e29b-41d4-a716-446655440000"), "/p/{id}");
        assert_eq!(n("/p/550E8400-E29B-41D4-A716-446655440000/x"), "/p/{id}/x");
        assert_eq!(n("/o/0123456789abcdef"), "/o/{id}");
        assert_eq!(n("/o/DEADBEEFDEADBEEF0123"), "/o/{id}");
        // 15 hex chars, a word and a file name stay.
        assert_eq!(n("/o/0123456789abcde"), "/o/0123456789abcde");
        assert_eq!(n("/o/cafe"), "/o/cafe");
        assert_eq!(n("/img/42.png"), "/img/42.png");
        assert_eq!(n("/v2/items"), "/v2/items");
        // Not a UUID: wrong dash positions.
        assert_eq!(
            n("/p/550e8400e-29b-41d4-a716-446655440000"),
            "/p/550e8400e-29b-41d4-a716-446655440000"
        );
    }

    #[test]
    fn collapses_empty_segments_and_keeps_case() {
        assert_eq!(n("//a///B/"), "/a/B");
        assert_eq!(n(""), "/");
        assert_eq!(n("/"), "/");
        assert_eq!(n("/Admin/Users"), "/Admin/Users");
    }

    #[test]
    fn absolute_form_drops_scheme_and_authority() {
        assert_eq!(n("http://loja.test/a/1?x=1"), "/a/{id}");
        assert_eq!(n("https://loja.test"), "/");
    }

    #[test]
    fn caps_segments_and_length() {
        assert_eq!(n("/1/b/3/d/5/f/7/h/i/j"), "/{id}/b/{id}/d/{id}/f/{id}/h");
        let long = format!("/{}/tail", "x".repeat(400));
        let route = n(&long);
        assert_eq!(route.len(), 256);
        assert!(route.starts_with("/xxx"));
        let many = format!("/{}", vec!["abcdefghij"; 8].join("/"));
        assert!(n(&many).len() <= 256);
        // Cut on a char boundary.
        let wide = format!("/{}", "é".repeat(300));
        let route = n(&wide);
        assert!(route.len() <= 256);
        assert!(route.chars().all(|c| c == '/' || c == 'é'));
    }

    #[test]
    fn replaces_control_characters() {
        assert_eq!(n("/a\u{1}b/\u{2}"), "/a_b/_");
    }

    #[test]
    fn custom_limits_are_clamped() {
        let norm = UriNormalizer::new(0, 0);
        assert_eq!(norm.normalize("/a/b"), "/");
        let norm = UriNormalizer::new(2, 6);
        assert_eq!(norm.normalize("/ab/cd/ef"), "/ab/cd");
        assert_eq!(norm.normalize("/abcdefgh"), "/abcde");
    }
}
