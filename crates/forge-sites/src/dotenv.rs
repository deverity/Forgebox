//! The `.env` files of a site, read for `sites.env` (docs/07 §5.4 "Ambiente", docs/11 §6,
//! ADR-0045).
//!
//! * Only [`DOTENV_FILES`] in the site's project folder
//!   ([`crate::lint_case::project_dir_for_root`]) are read: the API takes a site, never a
//!   path, so nothing else on disk is reachable through it. Links and multiply linked
//!   files are refused ([`forge_core::platform::open_no_follow`]) and files above
//!   [`MAX_DOTENV_BYTES`] are skipped.
//! * [`parse`] follows the common dotenv grammar (phpdotenv, Node `dotenv`, Symfony):
//!   `KEY=value`, `export KEY=value`, `#` comments, `"double"` (with `\n`, `\"`, `\\`
//!   escapes, may span lines), `'single'` (literal, may span lines), and a ` #` comment
//!   after an unquoted value. Lines that are not assignments are ignored.
//! * [`is_secret`] decides what is masked: keys containing `PASS`, `SECRET`, `KEY`,
//!   `TOKEN`, `PRIVATE`, `CREDENTIAL` or `SALT`, and any value that is a URL/DSN carrying
//!   credentials (`mysql://user:pw@host/db`, `https://key@sentry.io/1`).

use std::io::Read as _;
use std::path::{Path, PathBuf};

use forge_core::error::{ForgeError, Result};
use forge_core::platform::{NoFollow, open_no_follow};

/// The files read, in the order they apply (the later wins).
pub const DOTENV_FILES: [&str; 2] = [".env", ".env.local"];

/// Larger files are not read (a `.env` is a few KiB).
pub const MAX_DOTENV_BYTES: u64 = 256 * 1024;

/// Key fragments that mark a secret (upper case).
pub const SECRET_KEY_PARTS: [&str; 7] = [
    "PASS",
    "SECRET",
    "KEY",
    "TOKEN",
    "PRIVATE",
    "CREDENTIAL",
    "SALT",
];

/// One assignment of a `.env` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DotenvVar {
    pub key: String,
    pub value: String,
    /// 1-based line where the assignment starts.
    pub line: u32,
}

impl DotenvVar {
    /// Whether the value must be masked ([`is_secret`]).
    pub fn secret(&self) -> bool {
        is_secret(&self.key, &self.value)
    }
}

/// A `.env` file found in a project folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DotenvFile {
    /// `.env`, `.env.local`.
    pub name: &'static str,
    pub path: PathBuf,
    pub vars: Vec<DotenvVar>,
}

/// Whether `key = value` holds a secret.
pub fn is_secret(key: &str, value: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SECRET_KEY_PARTS.iter().any(|part| upper.contains(part)) || has_credentials(value)
}

/// A URL or DSN with a user-info part (`scheme://user:pw@host`, `scheme://token@host`).
fn has_credentials(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    authority
        .rsplit_once('@')
        .is_some_and(|(userinfo, _)| !userinfo.is_empty())
}

fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

/// The assignments of a `.env` text, in file order.
pub fn parse(text: &str) -> Vec<DotenvVar> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let start = i;
        let line = lines[i].trim_end_matches('\r');
        i += 1;
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let assignment = trimmed
            .strip_prefix("export ")
            .map(str::trim_start)
            .unwrap_or(trimmed);
        let Some((key, rest)) = assignment.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !valid_key(key) {
            continue;
        }
        let rest = rest.trim_start();
        let value = match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                // Up to the closing quote, possibly on a later line.
                let mut body = rest[1..].to_owned();
                loop {
                    if let Some(end) = closing(&body, quote) {
                        body.truncate(end);
                        break;
                    }
                    match lines.get(i) {
                        Some(next) => {
                            body.push('\n');
                            body.push_str(next.trim_end_matches('\r'));
                            i += 1;
                        }
                        // Never closed: the rest of the line as written.
                        None => break,
                    }
                }
                if quote == '"' { unescape(&body) } else { body }
            }
            _ => {
                let end = rest
                    .find(" #")
                    .or_else(|| rest.find("\t#"))
                    .unwrap_or(rest.len());
                rest[..end].trim_end().to_owned()
            }
        };
        out.push(DotenvVar {
            key: key.to_owned(),
            value,
            line: u32::try_from(start + 1).unwrap_or(u32::MAX),
        });
    }
    out
}

/// Byte offset of the closing `quote` in `body` (a `\` escapes it in double quotes).
fn closing(body: &str, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (at, c) in body.char_indices() {
        if quote == '"' && escaped {
            escaped = false;
            continue;
        }
        if quote == '"' && c == '\\' {
            escaped = true;
        } else if c == quote {
            return Some(at);
        }
    }
    None
}

fn unescape(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other @ ('"' | '\\' | '$')) => out.push(other),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Reads one file: `Ok(None)` when it does not exist.
fn read_file(path: &Path) -> Result<Option<String>> {
    let mut opened = match open_no_follow(path, NoFollow::Read) {
        Ok(opened) => opened,
        Err(ForgeError::NotFound(_)) => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut bytes = Vec::new();
    (&mut opened.file)
        .take(MAX_DOTENV_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ForgeError::io_at("reading", path, e))?;
    if bytes.len() as u64 > MAX_DOTENV_BYTES {
        return Err(ForgeError::InvalidInput(format!(
            "{} is larger than {} KiB",
            path.display(),
            MAX_DOTENV_BYTES / 1024
        )));
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// The [`DOTENV_FILES`] of the project folder `dir` that exist, parsed. A file that is a
/// link, too large or unreadable is left out and logged (the others are still shown).
/// Blocking.
pub fn read_project(dir: &Path) -> Vec<DotenvFile> {
    let mut out = Vec::new();
    for name in DOTENV_FILES {
        let path = dir.join(name);
        match read_file(&path) {
            Ok(Some(text)) => out.push(DotenvFile {
                name,
                vars: parse(&text),
                path,
            }),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "sites.env: file left out");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(text: &str) -> Vec<(String, String, u32)> {
        parse(text)
            .into_iter()
            .map(|v| (v.key, v.value, v.line))
            .collect()
    }

    #[test]
    fn parses_the_laravel_example() {
        let text = "\u{feff}APP_NAME=Laravel\r\nAPP_KEY=base64:abc=\r\n\r\n# db\r\nDB_PASSWORD=\r\n\
                    export DB_HOST = 127.0.0.1 # local\r\nMAIL_FROM_NAME=\"${APP_NAME}\"\r\n";
        assert_eq!(
            kv(text),
            vec![
                ("APP_NAME".into(), "Laravel".into(), 1),
                ("APP_KEY".into(), "base64:abc=".into(), 2),
                ("DB_PASSWORD".into(), String::new(), 5),
                ("DB_HOST".into(), "127.0.0.1".into(), 6),
                ("MAIL_FROM_NAME".into(), "${APP_NAME}".into(), 7),
            ]
        );
    }

    #[test]
    fn quotes_escapes_and_multiline_values() {
        let text = "A=\"x \\\"y\\\" \\n z\" # c\nB='literal \\n # not a comment'\n\
                    KEY_PEM=\"-----BEGIN-----\nabc\n-----END-----\"\nC=after\nnot an assignment\n\
                    1BAD=x\nD=a#b\nOPEN=\"never closed";
        let vars = kv(text);
        assert_eq!(vars[0], ("A".into(), "x \"y\" \n z".into(), 1));
        assert_eq!(
            vars[1],
            ("B".into(), "literal \\n # not a comment".into(), 2)
        );
        assert_eq!(
            vars[2],
            (
                "KEY_PEM".into(),
                "-----BEGIN-----\nabc\n-----END-----".into(),
                3
            )
        );
        assert_eq!(vars[3], ("C".into(), "after".into(), 6));
        assert_eq!(vars[4], ("D".into(), "a#b".into(), 9));
        assert_eq!(vars[5], ("OPEN".into(), "never closed".into(), 10));
        assert_eq!(vars.len(), 6);
    }

    #[test]
    fn secrets_by_key_and_by_credentials_in_urls() {
        for key in [
            "DB_PASSWORD",
            "APP_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
            "MAIL_PASS",
            "app_key",
            "PASSPORT_PRIVATE_KEY",
            "GOOGLE_CREDENTIALS",
            "AUTH_SALT",
        ] {
            assert!(is_secret(key, "x"), "{key}");
        }
        for (key, value) in [
            ("DATABASE_URL", "mysql://root:secret@127.0.0.1:3306/loja"),
            (
                "SENTRY_LARAVEL_DSN",
                "https://abc123@o1.ingest.sentry.io/42",
            ),
            ("REDIS_URL", "redis://:pw@localhost:6379"),
        ] {
            assert!(is_secret(key, value), "{key}");
        }
        for (key, value) in [
            ("APP_URL", "https://loja.test"),
            ("DATABASE_URL", "postgres://localhost/loja?x=a@b"),
            ("DB_HOST", "127.0.0.1"),
            ("MAIL_FROM_ADDRESS", "hello@example.com"),
            ("DB_USERNAME", "root"),
            ("NOTE", "://user@host"),
        ] {
            assert!(!is_secret(key, value), "{key}={value}");
        }
    }

    #[test]
    fn reads_only_the_dotenv_files_of_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "A=1\nDB_PASSWORD=pw\n").unwrap();
        std::fs::write(dir.path().join(".env.local"), "A=2\n").unwrap();
        std::fs::write(dir.path().join(".env.production"), "A=3\n").unwrap();
        let files = read_project(dir.path());
        let names: Vec<_> = files.iter().map(|f| f.name).collect();
        assert_eq!(names, [".env", ".env.local"]);
        assert!(files[0].vars[1].secret());
        assert!(!files[0].vars[0].secret());

        std::fs::write(
            dir.path().join(".env"),
            vec![b'A'; usize::try_from(MAX_DOTENV_BYTES).unwrap() + 1],
        )
        .unwrap();
        let files = read_project(dir.path());
        assert_eq!(files.len(), 1, "too large: left out");
        assert_eq!(files[0].name, ".env.local");
        assert!(read_project(&dir.path().join("missing")).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn a_dotenv_link_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(outside.path().join("secrets")).unwrap();
        std::fs::write(outside.path().join("secrets/.env"), "TOKEN=x\n").unwrap();
        // A junction named `.env` (no privilege needed) pointing elsewhere.
        junction_or_skip(&outside.path().join("secrets"), &dir.path().join(".env"));
        assert!(read_project(dir.path()).is_empty());
    }

    #[cfg(windows)]
    fn junction_or_skip(target: &Path, link: &Path) {
        forge_core::platform::current()
            .link_fs()
            .create_junction(link, target)
            .unwrap();
    }
}
