//! The managed block of the system `hosts` file (fallback of the NRPT rule, docs/06 §5.1).
//!
//! ```text
//! # forgebox:begin hosts (hash=0123456789abcdef)
//! 127.0.0.1 loja.test
//! ::1 loja.test
//! # forgebox:end hosts
//! ```
//!
//! Same marker syntax and hash as the managed blocks of `forge-render` (first 16 hex
//! digits of the SHA-256 of the LF body), so a block edited by hand is detectable. The
//! helper writes it (`hosts.set_block`); the daemon reads it for the diagnosis. Outside
//! the block nothing changes, byte for byte. The `hosts` file is a Windows file outside the
//! root: the block keeps the file's own line endings (CRLF in a stock file).
//!
//! Everything here is pure text manipulation.

use std::fmt::Write as _;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{ForgeError, Result};

/// Name of the block (`# forgebox:begin hosts`).
pub const HOSTS_BLOCK_NAME: &str = "hosts";

/// One line of the block: `ip host`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostsEntry {
    pub ip: IpAddr,
    pub host: String,
}

impl HostsEntry {
    pub fn new(ip: IpAddr, host: impl Into<String>) -> Self {
        Self {
            ip,
            host: host.into(),
        }
    }
}

/// The block lines for `entries` (sorted by host, IPv4 first, duplicates dropped), LF,
/// without markers.
fn body(entries: &[HostsEntry]) -> String {
    let mut sorted: Vec<&HostsEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| {
        (a.host.as_str(), a.ip.is_ipv6(), a.ip).cmp(&(b.host.as_str(), b.ip.is_ipv6(), b.ip))
    });
    sorted.dedup();
    let mut out = String::new();
    for entry in sorted {
        let _ = writeln!(out, "{} {}", entry.ip, entry.host);
    }
    out
}

/// Hash recorded in the `begin` line: first 16 hex digits of the SHA-256 of the LF body
/// (identical to `forge_render::managed::hash_body` for a body that ends with one `\n`).
fn hash(body: &str) -> String {
    let digest = Sha256::digest(body.as_bytes());
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The whole block for `entries` with LF line endings (markers included, ending with a
/// newline). Empty for no entries.
pub fn render_hosts_block(entries: &[HostsEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let body = body(entries);
    format!(
        "# forgebox:begin {HOSTS_BLOCK_NAME} (hash={})\n{body}# forgebox:end {HOSTS_BLOCK_NAME}\n",
        hash(&body)
    )
}

fn is_begin(line: &str) -> bool {
    let line = line.trim();
    line.strip_prefix('#')
        .map(str::trim_start)
        .and_then(|rest| rest.strip_prefix("forgebox:begin "))
        .map(str::trim)
        .is_some_and(|rest| {
            rest == HOSTS_BLOCK_NAME
                || rest
                    .strip_prefix(HOSTS_BLOCK_NAME)
                    .is_some_and(|tail| tail.starts_with(' '))
        })
}

fn is_end(line: &str) -> bool {
    line.trim()
        .strip_prefix('#')
        .map(str::trim_start)
        .and_then(|rest| rest.strip_prefix("forgebox:end "))
        .is_some_and(|rest| rest.trim() == HOSTS_BLOCK_NAME)
}

/// Byte range of the block (begin line through end line and its newline) in `text`.
/// `config_invalid` for an unclosed or repeated block: a human must look at it.
fn find_block(text: &str) -> Result<Option<std::ops::Range<usize>>> {
    let mut found: Option<std::ops::Range<usize>> = None;
    let mut open: Option<usize> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        match open {
            None if is_begin(line) => {
                if found.is_some() {
                    return Err(ForgeError::ConfigInvalid(
                        "hosts: the forgebox block appears twice".to_owned(),
                    ));
                }
                open = Some(start);
            }
            None => {}
            Some(begin) => {
                if is_end(line) {
                    found = Some(begin..offset);
                    open = None;
                } else if is_begin(line) {
                    return Err(ForgeError::ConfigInvalid(
                        "hosts: the forgebox block is not closed before it starts again".to_owned(),
                    ));
                }
            }
        }
    }
    if open.is_some() {
        return Err(ForgeError::ConfigInvalid(
            "hosts: the forgebox block is not closed".to_owned(),
        ));
    }
    Ok(found)
}

/// Line ending of an existing file: CRLF when it has any, else LF; CRLF for an empty
/// file on Windows (the stock `hosts`).
fn line_ending(text: &str) -> &'static str {
    if text.contains("\r\n") || (text.is_empty() && cfg!(windows)) {
        "\r\n"
    } else {
        "\n"
    }
}

/// `existing` (the current `hosts` content) with the block replaced by one for
/// `entries`, appended when missing, or removed when `entries` is empty. Text outside the
/// block is kept byte for byte; the only addition is a line ending after a last line that
/// had none, before an appended block.
pub fn apply_hosts_block(existing: &str, entries: &[HostsEntry]) -> Result<String> {
    let eol = line_ending(existing);
    let block = render_hosts_block(entries).replace('\n', eol);
    match find_block(existing)? {
        Some(range) => Ok(format!(
            "{}{block}{}",
            &existing[..range.start],
            &existing[range.end..]
        )),
        None if block.is_empty() => Ok(existing.to_owned()),
        None => {
            let mut out = existing.to_owned();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push_str(eol);
            }
            out.push_str(&block);
            Ok(out)
        }
    }
}

/// The block found in a `hosts` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostsBlock {
    /// Valid `ip host` lines of the block (comments and garbage skipped).
    pub entries: Vec<HostsEntry>,
    /// The body no longer matches the hash written with it (edited by hand).
    pub drifted: bool,
}

/// Reads the block of `text`; `None` when there is none.
pub fn read_hosts_block(text: &str) -> Result<Option<HostsBlock>> {
    let Some(range) = find_block(text)? else {
        return Ok(None);
    };
    let block = &text[range];
    let mut lines = block.split_inclusive('\n');
    let begin = lines.next().unwrap_or_default();
    let recorded = begin
        .split_once("(hash=")
        .and_then(|(_, rest)| rest.trim_end().strip_suffix(')'))
        .unwrap_or_default()
        .to_owned();
    let inner: Vec<&str> = lines.collect();
    let inner = &inner[..inner.len().saturating_sub(1)];
    let mut body_lf = String::new();
    let mut entries = Vec::new();
    for line in inner {
        let line = line.trim_end_matches(['\r', '\n']);
        body_lf.push_str(line);
        body_lf.push('\n');
        entries.extend(parse_line(line));
    }
    Ok(Some(HostsBlock {
        entries,
        drifted: hash(&body_lf) != recorded,
    }))
}

/// `ip name...` entries of one `hosts` line (comment after `#` ignored).
fn parse_line(line: &str) -> Vec<HostsEntry> {
    let content = line.split('#').next().unwrap_or_default();
    let mut words = content.split_whitespace();
    let Some(ip) = words.next().and_then(|w| w.parse::<IpAddr>().ok()) else {
        return Vec::new();
    };
    words
        .map(|host| HostsEntry::new(ip, host.to_ascii_lowercase()))
        .collect()
}

/// Every address `hosts` maps `host` to, inside or outside the block, in file order.
pub fn lookup_hosts(text: &str, host: &str) -> Vec<IpAddr> {
    let host = host.to_ascii_lowercase();
    text.lines()
        .flat_map(parse_line)
        .filter(|e| e.host == host)
        .map(|e| e.ip)
        .collect()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn e(ip: &str, host: &str) -> HostsEntry {
        HostsEntry::new(ip.parse().unwrap(), host)
    }

    const STOCK: &str = "# Copyright (c) 1993-2009 Microsoft Corp.\r\n#\r\n# localhost name resolution is handled within DNS itself.\r\n#\t127.0.0.1       localhost\r\n#\t::1             localhost\r\n";

    #[test]
    fn block_is_sorted_deduplicated_and_hashed() {
        let block = render_hosts_block(&[
            e("::1", "loja.test"),
            e("127.0.0.1", "loja.test"),
            e("127.0.0.1", "api.test"),
            e("127.0.0.1", "loja.test"),
        ]);
        let body = "127.0.0.1 api.test\n127.0.0.1 loja.test\n::1 loja.test\n";
        assert_eq!(
            block,
            format!(
                "# forgebox:begin hosts (hash={})\n{body}# forgebox:end hosts\n",
                hash(body)
            )
        );
        assert_eq!(hash(body).len(), 16);
        assert_eq!(render_hosts_block(&[]), "");
    }

    #[test]
    fn append_replace_and_remove_keep_the_rest() {
        let entries = [e("127.0.0.1", "loja.test"), e("::1", "loja.test")];
        let added = apply_hosts_block(STOCK, &entries).unwrap();
        assert!(added.starts_with(STOCK), "outside text untouched");
        assert!(
            added[STOCK.len()..].starts_with("# forgebox:begin hosts (hash="),
            "{added:?}"
        );
        assert!(added.ends_with("::1 loja.test\r\n# forgebox:end hosts\r\n"));
        assert!(!added.replace("\r\n", "").contains('\n'), "CRLF everywhere");

        // Same entries: same text.
        assert_eq!(apply_hosts_block(&added, &entries).unwrap(), added);

        // Text added after the block by the user survives a rewrite.
        let with_tail = format!("{added}10.0.0.9 nas.lan\r\n");
        let replaced = apply_hosts_block(&with_tail, &[e("127.0.0.1", "outro.test")]).unwrap();
        assert!(replaced.starts_with(STOCK));
        assert!(replaced.ends_with("# forgebox:end hosts\r\n10.0.0.9 nas.lan\r\n"));
        assert!(replaced.contains("127.0.0.1 outro.test\r\n"));
        assert!(!replaced.contains("loja.test"));

        // Removing at the end of the file restores the original bytes.
        assert_eq!(apply_hosts_block(&added, &[]).unwrap(), STOCK);
        // Removing in the middle keeps what follows.
        assert_eq!(
            apply_hosts_block(&with_tail, &[]).unwrap(),
            format!("{STOCK}10.0.0.9 nas.lan\r\n")
        );
        // Nothing to remove.
        assert_eq!(apply_hosts_block(STOCK, &[]).unwrap(), STOCK);

        // Add/remove cycles leave no trace.
        let mut text = STOCK.to_owned();
        for _ in 0..3 {
            text = apply_hosts_block(&text, &entries).unwrap();
            text = apply_hosts_block(&text, &[]).unwrap();
        }
        assert_eq!(text, STOCK);
    }

    #[test]
    fn lf_files_and_files_without_final_newline() {
        let lf = "127.0.0.1 localhost\n";
        let out = apply_hosts_block(lf, &[e("127.0.0.1", "a.test")]).unwrap();
        assert!(!out.contains('\r'));
        assert!(out.starts_with("127.0.0.1 localhost\n# forgebox:begin hosts"));

        let no_newline = "127.0.0.1 localhost";
        let out = apply_hosts_block(no_newline, &[e("127.0.0.1", "a.test")]).unwrap();
        assert!(
            out.starts_with("127.0.0.1 localhost\n# forgebox:begin"),
            "{out:?}"
        );

        let empty = apply_hosts_block("", &[e("127.0.0.1", "a.test")]).unwrap();
        assert!(empty.starts_with("# forgebox:begin hosts"));
    }

    #[test]
    fn read_detects_entries_and_drift() {
        let text = apply_hosts_block(STOCK, &[e("127.0.0.1", "loja.test"), e("::1", "loja.test")])
            .unwrap();
        let block = read_hosts_block(&text).unwrap().unwrap();
        assert_eq!(
            block.entries,
            vec![e("127.0.0.1", "loja.test"), e("::1", "loja.test")]
        );
        assert!(!block.drifted);

        let edited = text.replace("::1 loja.test", "::1 loja.test extra.test # hand");
        let block = read_hosts_block(&edited).unwrap().unwrap();
        assert!(block.drifted);
        assert_eq!(block.entries.len(), 3);

        assert_eq!(read_hosts_block(STOCK).unwrap(), None);
    }

    #[test]
    fn broken_blocks_are_reported() {
        for broken in [
            "# forgebox:begin hosts (hash=x)\n127.0.0.1 a.test\n",
            "# forgebox:begin hosts\n# forgebox:begin hosts\n# forgebox:end hosts\n",
            "# forgebox:begin hosts\n# forgebox:end hosts\n# forgebox:begin hosts\n# forgebox:end hosts\n",
        ] {
            let err = apply_hosts_block(broken, &[]).unwrap_err();
            assert_eq!(err.code(), "config_invalid", "{broken:?}");
            assert!(read_hosts_block(broken).is_err());
        }
        // Blocks of other names (forge-render style) are not ours.
        let other = "# forgebox:begin hostsx\n# forgebox:end hostsx\n";
        assert_eq!(read_hosts_block(other).unwrap(), None);
    }

    #[test]
    fn lookup_scans_the_whole_file() {
        let text = format!(
            "{STOCK}10.0.0.9 Loja.test other # comment\r\n#127.0.0.2 loja.test\r\n{}",
            render_hosts_block(&[e("127.0.0.1", "loja.test")])
        );
        assert_eq!(
            lookup_hosts(&text, "LOJA.test"),
            vec![
                "10.0.0.9".parse::<IpAddr>().unwrap(),
                "127.0.0.1".parse().unwrap()
            ]
        );
        assert!(lookup_hosts(&text, "localhost").is_empty(), "commented out");
        assert!(lookup_hosts(&text, "nope.test").is_empty());
    }
}
