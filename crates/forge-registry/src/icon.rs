//! Catalog icons: the check applied before an SVG leaves the daemon (docs/11 §2,
//! `tools.icon`).
//!
//! The registry lint (rule 7) already refuses scripts at authoring time; this is the
//! second line, run on the bytes the daemon holds, because a third-party registry must
//! never get an active document into a client. Clients show the result only as an `<img>`
//! data URI.
//!
//! The check is a rejection, not a rewrite: the SVG that is accepted is byte-identical to
//! the one the index hashed, so `CatalogEntryDto.icon_sha256` is the hash of what
//! `tools.icon` returns. Everything is decided by a small tag/attribute scanner (no XML
//! parser, no entity expansion: any `<!DOCTYPE`/`<!ENTITY` is refused).
//!
//! Refused: more than 64 KiB, non-UTF-8, a root that is not `<svg>`, `<script>`,
//! `<foreignObject>`, `<iframe>`/`<object>`/`<embed>` and friends, `on*` attributes,
//! `href`/`xlink:href` other than `#id` or `data:`, `url()` in styles or attributes other
//! than `#id` or `data:`, `@import`, `javascript:`, animations that target `href`,
//! processing instructions other than `<?xml`, DOCTYPE and entities.

use forge_core::error::{ForgeError, Result};

/// Largest icon served (the registry lint's 20 KB is the authoring limit).
pub const MAX_ICON_BYTES: u64 = 64 * 1024;

/// Elements that run code or load documents.
const FORBIDDEN_ELEMENTS: &[&str] = &[
    "script",
    "foreignobject",
    "iframe",
    "object",
    "embed",
    "applet",
    "audio",
    "video",
    "canvas",
    "link",
    "meta",
    "handler",
    "listener",
];

fn refuse(why: impl std::fmt::Display) -> ForgeError {
    ForgeError::VerificationFailed(format!("icon refused: {why}"))
}

/// Returns the SVG text when it passes every rule, else `verification_failed`.
pub fn sanitize_svg(bytes: &[u8]) -> Result<String> {
    if bytes.len() as u64 > MAX_ICON_BYTES {
        return Err(refuse(format!(
            "{} bytes, the limit is {MAX_ICON_BYTES}",
            bytes.len()
        )));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| refuse("not UTF-8"))?;
    scan(text.strip_prefix('\u{feff}').unwrap_or(text))?;
    Ok(text.to_owned())
}

fn scan(text: &str) -> Result<()> {
    let mut rest = text;
    let mut seen_root = false;
    let mut in_style = false;
    let mut style_text = String::new();
    while let Some(lt) = rest.find('<') {
        if in_style {
            style_text.push_str(&rest[..lt]);
        }
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            let end = after
                .find("-->")
                .ok_or_else(|| refuse("unterminated comment"))?;
            rest = &after[end + 3..];
        } else if let Some(after) = rest.strip_prefix("<![CDATA[") {
            let end = after
                .find("]]>")
                .ok_or_else(|| refuse("unterminated CDATA"))?;
            if in_style {
                style_text.push_str(&after[..end]);
            }
            rest = &after[end + 3..];
        } else if rest.starts_with("<!") {
            return Err(refuse("DOCTYPE and entity declarations are not allowed"));
        } else if let Some(after) = rest.strip_prefix("<?") {
            let end = after
                .find("?>")
                .ok_or_else(|| refuse("unterminated processing instruction"))?;
            let target = after[..end].split_whitespace().next().unwrap_or("");
            if target != "xml" {
                return Err(refuse(format!("processing instruction `{target}`")));
            }
            rest = &after[end + 2..];
        } else if let Some(after) = rest.strip_prefix("</") {
            let end = after.find('>').ok_or_else(|| refuse("unterminated tag"))?;
            if in_style && local_name(after[..end].trim()) == "style" {
                in_style = false;
                check_css(&style_text)?;
                style_text.clear();
            }
            rest = &after[end + 1..];
        } else {
            let (tag, after) = parse_open_tag(&rest[1..])?;
            let name = local_name(&tag.name);
            if !seen_root {
                if name != "svg" {
                    return Err(refuse("the root element is not <svg>"));
                }
                seen_root = true;
            }
            check_tag(&name, &tag.attrs)?;
            if name == "style" && !tag.self_closing {
                in_style = true;
            }
            rest = after;
        }
    }
    if in_style {
        return Err(refuse("unterminated <style>"));
    }
    if !seen_root {
        return Err(refuse("not an SVG document"));
    }
    Ok(())
}

struct OpenTag {
    name: String,
    attrs: Vec<(String, String)>,
    self_closing: bool,
}

/// `name attr="v" ...>` (after `<`). Returns the tag and the text after `>`.
fn parse_open_tag(s: &str) -> Result<(OpenTag, &str)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && !matches!(bytes[i], b'/' | b'>') {
        i += 1;
    }
    let name = s[..i].to_ascii_lowercase();
    if name.is_empty() {
        return Err(refuse("malformed tag"));
    }
    let mut attrs = Vec::new();
    let mut self_closing = false;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        match bytes.get(i) {
            None => return Err(refuse("unterminated tag")),
            Some(b'>') => {
                return Ok((
                    OpenTag {
                        name,
                        attrs,
                        self_closing,
                    },
                    &s[i + 1..],
                ));
            }
            Some(b'/') => {
                self_closing = true;
                i += 1;
                continue;
            }
            Some(_) => {}
        }
        self_closing = false;
        let start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && !matches!(bytes[i], b'=' | b'>' | b'/')
        {
            i += 1;
        }
        let attr = s[start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if bytes.get(i) == Some(&b'=') {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            match bytes.get(i) {
                Some(&q @ (b'"' | b'\'')) => {
                    let from = i + 1;
                    let len = s[from..]
                        .find(q as char)
                        .ok_or_else(|| refuse("unterminated attribute value"))?;
                    value = s[from..from + len].to_owned();
                    i = from + len + 1;
                }
                Some(_) => {
                    let from = i;
                    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                        i += 1;
                    }
                    value = s[from..i].to_owned();
                }
                None => return Err(refuse("unterminated tag")),
            }
        }
        if attr.is_empty() {
            return Err(refuse("malformed attribute"));
        }
        attrs.push((attr, value));
    }
}

/// `svg:script` -> `script`.
fn local_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    lower.rsplit(':').next().unwrap_or("").to_owned()
}

fn check_tag(name: &str, attrs: &[(String, String)]) -> Result<()> {
    if FORBIDDEN_ELEMENTS.contains(&name) {
        return Err(refuse(format!("<{name}> is not allowed")));
    }
    let animates_link = matches!(
        name,
        "set" | "animate" | "animatetransform" | "animatemotion"
    ) && attrs.iter().any(|(k, v)| {
        k == "attributename" && {
            let v = v.to_ascii_lowercase();
            v.contains("href") || v.starts_with("on")
        }
    });
    if animates_link {
        return Err(refuse(format!(
            "<{name}> targets a link or event attribute"
        )));
    }
    for (key, value) in attrs {
        let key = key.as_str();
        if key.starts_with("on") {
            return Err(refuse(format!("event attribute `{key}`")));
        }
        if key == "href" || key.ends_with(":href") {
            let v = value.trim();
            if !(v.starts_with('#') || v.to_ascii_lowercase().starts_with("data:")) {
                return Err(refuse(format!("`{key}` must be #id or data:")));
            }
        }
        if key == "style" {
            check_css(value)?;
        } else {
            check_value(value)?;
        }
    }
    Ok(())
}

/// Lowercase text without whitespace or control characters, so `java script:` and
/// `ur l(` tricks cannot hide a pattern.
fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Text that must not carry a script URL or an external `url()`.
fn check_value(value: &str) -> Result<()> {
    let squashed = squash(value);
    if squashed.contains("javascript:") || squashed.contains("vbscript:") {
        return Err(refuse("script URL"));
    }
    check_urls(&squashed)
}

fn check_css(css: &str) -> Result<()> {
    let squashed = squash(css);
    if squashed.contains("@import") || squashed.contains("expression(") {
        return Err(refuse("@import and expression() are not allowed in styles"));
    }
    check_value(css)
}

/// Every `url(...)` points at `#id` or `data:`.
fn check_urls(squashed: &str) -> Result<()> {
    let mut rest = squashed;
    while let Some(at) = rest.find("url(") {
        let after = rest[at + 4..].trim_start_matches(['"', '\'']);
        if !(after.starts_with('#') || after.starts_with("data:")) {
            return Err(refuse("url() must point at #id or data:"));
        }
        rest = &rest[at + 4..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 24 24">
  <defs><linearGradient id="g"><stop offset="0" stop-color="#777BB4"/></linearGradient></defs>
  <!-- a comment with <script> inside is only a comment -->
  <style>.a { fill: url(#g); }</style>
  <use xlink:href="#p" href="#p"/>
  <image href="data:image/png;base64,AAAA"/>
  <path id="p" class="a" fill="url(#g)" d="M0 0h24v24H0z"/>
</svg>"##;

    #[test]
    fn a_plain_icon_passes_unchanged() {
        assert_eq!(sanitize_svg(VALID.as_bytes()).unwrap(), VALID);
    }

    #[test]
    fn the_shipped_icons_pass() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/icons");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            sanitize_svg(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        }
    }

    fn assert_refused(svg: &str) {
        match sanitize_svg(svg.as_bytes()) {
            Err(ForgeError::VerificationFailed(_)) => {}
            other => panic!("expected verification_failed for {svg}, got {other:?}"),
        }
    }

    #[test]
    fn malicious_icons_are_refused() {
        for svg in [
            r#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><SVG:ForeignObject><div/></SVG:ForeignObject></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"/>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><a href="javascript:alert(1)"><rect/></a></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><image xlink:href="https://evil.example/x.png"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><style>@import url(https://evil.example/a.css);</style></svg>"#,
            r#"<!DOCTYPE svg [<!ENTITY x "y">]><svg xmlns="http://www.w3.org/2000/svg"/>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect style="fill:url(https://evil.example/a)"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><set attributeName="href" to="javascript:1"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect onclick = 'x()'/></svg>"#,
            r#"<html><svg xmlns="http://www.w3.org/2000/svg"/></html>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect"#,
        ] {
            assert_refused(svg);
        }
    }

    #[test]
    fn size_and_encoding_limits() {
        let big = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\">{}</svg>",
            " ".repeat(MAX_ICON_BYTES as usize)
        );
        assert_refused(&big);
        assert!(matches!(
            sanitize_svg(&[0xff, 0xfe, b'<']),
            Err(ForgeError::VerificationFailed(_))
        ));
    }
}
