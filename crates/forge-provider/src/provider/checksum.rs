//! Reading hashes published next to the archives (`SHASUMS256.txt`, `checksums.txt`,
//! `*.sha256`, Apache Lounge `.txt`) and filling them into [`Release`]s.

use std::collections::HashMap;
use std::sync::OnceLock;

use forge_core::error::Result;
use futures::StreamExt;
use regex::Regex;

use super::Release;
use super::fetch::{FetchRequest, Fetcher};

/// Hashes found for one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Digests {
    /// Lowercase hex.
    pub sha256: Option<String>,
    pub sha512: Option<String>,
}

fn sum_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^([0-9a-fA-F]{64}|[0-9a-fA-F]{128})\s+\*?(\S.*)$").expect("static regex")
    })
}

fn bsd_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^SHA(256|512) \((.+)\) = ([0-9a-fA-F]{64}|[0-9a-fA-F]{128})$")
            .expect("static regex")
    })
}

fn labeled_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^SHA(256|512)-Checksum for:\s*(.+?):?\s*$").expect("static regex")
    })
}

fn same_file(listed: &str, wanted: &str) -> bool {
    let listed = listed.trim().trim_end_matches(':');
    let base = listed.rsplit(['/', '\\']).next().unwrap_or(listed);
    base.eq_ignore_ascii_case(wanted)
}

fn assign(digests: &mut Digests, hex: &str) {
    let hex = hex.to_ascii_lowercase();
    match hex.len() {
        64 => digests.sha256 = Some(hex),
        128 => digests.sha512 = Some(hex),
        _ => {}
    }
}

fn is_hex_of(text: &str, lens: &[usize]) -> bool {
    lens.contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Extracts the sha256/sha512 of `file_name` from a checksum document. Understands
/// `<hash>  <file>` (coreutils, Node, GitHub `checksums.txt`), `SHA256 (<file>) = <hash>`,
/// Apache Lounge's `SHA256-Checksum for: <file>:` blocks and a file holding only the hash
/// (`<asset>.sha256`).
pub fn parse_checksums(body: &str, file_name: &str) -> Digests {
    let mut found = Digests::default();
    let mut bare = Digests::default();
    let mut pending_label = false;
    for raw in body.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        if let Some(caps) = labeled_line().captures(line) {
            pending_label = same_file(&caps[2], file_name);
            continue;
        }
        if pending_label && is_hex_of(line, &[64, 128]) {
            assign(&mut found, line);
            pending_label = false;
            continue;
        }
        pending_label = false;
        if let Some(caps) = sum_line().captures(line) {
            if same_file(&caps[2], file_name) {
                assign(&mut found, &caps[1]);
            }
        } else if let Some(caps) = bsd_line().captures(line) {
            if same_file(&caps[2], file_name) {
                assign(&mut found, &caps[3]);
            }
        } else if is_hex_of(line, &[64, 128]) {
            assign(&mut bare, line);
        }
    }
    if found.sha256.is_none() {
        found.sha256 = bare.sha256;
    }
    if found.sha512.is_none() {
        found.sha512 = bare.sha512;
    }
    found
}

/// Fetches `release.checksum_url` and fills the hashes; returns whether a sha256 was
/// found. For releases that [`super::ProviderRegistry::resolve`] left with a
/// `checksum_url` (beyond `max_checksum_fetches`).
pub async fn resolve_checksum(
    fetcher: &Fetcher,
    provider: &str,
    release: &mut Release,
) -> Result<bool> {
    let Some(url) = release.checksum_url.clone() else {
        return Ok(release.sha256.is_some());
    };
    let body = fetcher
        .get(FetchRequest {
            provider,
            url: &url,
            ..FetchRequest::default()
        })
        .await?
        .text();
    let digests = parse_checksums(&body, &release.file_name);
    apply(release, digests);
    Ok(release.sha256.is_some())
}

fn apply(release: &mut Release, digests: Digests) {
    if release.sha256.is_none() {
        release.sha256 = digests.sha256;
    }
    if release.sha512.is_none() {
        release.sha512 = digests.sha512;
    }
    if release.sha256.is_some() || release.sha512.is_some() {
        release.checksum_url = None;
    }
}

/// Resolves the checksum documents of the first `max` releases that lack a sha256
/// (several in parallel). Failures are logged and leave the release with its
/// `checksum_url`.
pub(crate) async fn fill_checksums(
    fetcher: &Fetcher,
    provider: &str,
    releases: &mut [Release],
    max: usize,
) {
    let mut urls: Vec<String> = Vec::new();
    for release in releases.iter().filter(|r| r.sha256.is_none()) {
        if let Some(url) = &release.checksum_url {
            if !urls.contains(url) {
                urls.push(url.clone());
            }
        }
        if urls.len() >= max {
            break;
        }
    }
    let bodies: HashMap<String, String> = futures::stream::iter(urls)
        .map(|url| async move {
            let result = fetcher
                .get(FetchRequest {
                    provider,
                    url: &url,
                    ..FetchRequest::default()
                })
                .await;
            (url, result)
        })
        .buffer_unordered(6)
        .filter_map(|(url, result)| async move {
            match result {
                Ok(fetched) => Some((url, fetched.text())),
                Err(err) => {
                    tracing::warn!(url, error = %err, "could not read the checksum document");
                    None
                }
            }
        })
        .collect()
        .await;
    for release in releases.iter_mut() {
        if release.sha256.is_some() {
            continue;
        }
        if let Some(body) = release.checksum_url.as_ref().and_then(|u| bodies.get(u)) {
            let digests = parse_checksums(body, &release.file_name);
            apply(release, digests);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H256: &str = "b3efd5c70cba3308ed46e7e699a9b8aac075bbc6b027fcb39c84c31acc25e2b4";

    #[test]
    fn coreutils_lines_match_by_file_name() {
        let body = format!(
            "{H256}  node-v1-win-x64.zip\n{}  node-v1-win-x64.7z\n",
            "0".repeat(64)
        );
        let d = parse_checksums(&body, "node-v1-win-x64.zip");
        assert_eq!(d.sha256.as_deref(), Some(H256));
        assert_eq!(d.sha512, None);
    }

    #[test]
    fn binary_marker_and_paths_are_ignored() {
        let body = format!("{H256} *dist/app.zip\n");
        assert_eq!(
            parse_checksums(&body, "app.zip").sha256.as_deref(),
            Some(H256)
        );
    }

    #[test]
    fn bsd_style() {
        let body = format!("SHA256 (app.zip) = {H256}\n");
        assert_eq!(
            parse_checksums(&body, "app.zip").sha256.as_deref(),
            Some(H256)
        );
    }

    #[test]
    fn apache_lounge_blocks() {
        let upper = H256.to_ascii_uppercase();
        let body = format!(
            "\u{feff}Checksums created with GPGHash\n\nSHA1-Checksum for: httpd-1-Win64-VS17.zip:\n{}\n\nSHA256-Checksum for: httpd-1-Win64-VS17.zip:\n{upper}\n\nSHA512-Checksum for: httpd-1-Win64-VS17.zip:\n{}\n",
            "A".repeat(40),
            "B".repeat(128)
        );
        let d = parse_checksums(&body, "httpd-1-Win64-VS17.zip");
        assert_eq!(d.sha256.as_deref(), Some(H256));
        assert_eq!(d.sha512, Some("b".repeat(128)));
    }

    #[test]
    fn a_file_with_only_the_hash() {
        assert_eq!(
            parse_checksums(&format!("{H256}\n"), "whatever.zip")
                .sha256
                .as_deref(),
            Some(H256)
        );
    }

    #[test]
    fn other_files_do_not_match() {
        let body = format!("{H256}  other.zip\n{H256}  third.zip\n");
        assert_eq!(parse_checksums(&body, "app.zip"), Digests::default());
    }
}
