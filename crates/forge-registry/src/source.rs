//! Where a registry comes from: a local folder, an HTTP(S) `index.yaml` or a git repository
//! (docs/04 §5, docs/05 §7).
//!
//! * `Dir`: a folder with `index.yaml` at its root (`file://` URLs and plain paths; also a
//!   network share, docs/05 §7). Read in place; only verified files are copied to the cache.
//! * `Http`: the URL of `index.yaml`; manifests are fetched relative to it. The index is
//!   downloaded with `If-None-Match` when a cache exists (HTTP 304 = nothing to do).
//! * `Git`: a repository with `index.yaml` at its root. We run the machine's `git` command
//!   (`git clone --depth 1` / `git fetch --depth 1` + `reset --hard`) in `spawn_blocking`.
//!   Decision: no libgit2 (C dependency, ADR-0017 keeps the tree pure Rust) and no special
//!   GitHub tarball path (one code path to maintain and test). Machines without `git` get a
//!   clear error and can use the `Http` source (`raw.githubusercontent.com/.../index.yaml`).
//!   The URL scheme is restricted (`https`, `http`, `ssh`, `git`, `file`) so `ext::`-style
//!   transports can never run commands, and the URL is passed after `--`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use forge_core::error::{ForgeError, Result};
use url::Url;

/// Default time budget of one `git` invocation.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// A registry source. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrySource {
    /// Folder containing `index.yaml`.
    Dir(PathBuf),
    /// URL of the `index.yaml` file.
    Http(Url),
    /// Repository URL; `index.yaml` is at the repository root.
    Git(Url),
}

impl RegistrySource {
    /// The `kind` column of the `registries` table.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Dir(_) => "dir",
            Self::Http(_) => "http",
            Self::Git(_) => "git",
        }
    }

    /// The `url` column: the path (with `/`) or the URL.
    pub fn location(&self) -> String {
        match self {
            Self::Dir(path) => forge_core::paths::posix_string(path),
            Self::Http(url) | Self::Git(url) => url.to_string(),
        }
    }

    /// Rebuilds a source from the `kind` and `url` columns.
    pub fn from_parts(kind: &str, location: &str) -> Result<Self> {
        match kind {
            "dir" => Ok(Self::Dir(PathBuf::from(location))),
            "http" => Ok(Self::Http(parse_url(location)?)),
            "git" => Ok(Self::Git(parse_url(location)?)),
            other => Err(ForgeError::InvalidInput(format!(
                "unknown registry kind `{other}`"
            ))),
        }
    }

    /// Understands what a user types in `forge registry add <what>`:
    /// `file:///C:/x` or an existing path → `Dir`; `git+https://...` or a URL ending in
    /// `.git` → `Git`; any other `http(s)` URL → `Http` (the `index.yaml` file).
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            return Err(ForgeError::InvalidInput("registry source is empty".into()));
        }
        if let Some(rest) = input.strip_prefix("git+") {
            return Ok(Self::Git(parse_url(rest)?));
        }
        match Url::parse(input) {
            Ok(url) if url.scheme() == "file" => {
                let path = url.to_file_path().map_err(|()| {
                    ForgeError::InvalidInput(format!("`{input}` is not a local file URL"))
                })?;
                Ok(Self::Dir(path))
            }
            Ok(url) if matches!(url.scheme(), "http" | "https") => {
                if url.path().ends_with(".git") {
                    Ok(Self::Git(url))
                } else {
                    Ok(Self::Http(url))
                }
            }
            // A Windows path such as `C:\x` parses as a URL with scheme `c`: treat as path.
            _ => Ok(Self::Dir(PathBuf::from(input))),
        }
    }
}

fn parse_url(text: &str) -> Result<Url> {
    Url::parse(text).map_err(|e| ForgeError::InvalidInput(format!("invalid URL `{text}`: {e}")))
}

/// `Some(reason)` when `url` must not be given to `git`.
fn unsafe_git_url(url: &Url) -> Option<String> {
    if !matches!(url.scheme(), "https" | "http" | "ssh" | "git" | "file") {
        return Some(format!("scheme `{}` is not allowed for git", url.scheme()));
    }
    None
}

/// Runs `git`, returning trimmed stdout. Killed after `timeout`.
fn run_git(args: &[&str], cwd: Option<&Path>, timeout: Duration) -> Result<String> {
    let verb = args.first().copied().unwrap_or("");
    let mut command = Command::new("git");
    command
        // Byte-exact checkout: the manifest hashes are over LF files, so no CRLF conversion
        // whatever the user's global git config says (core.autocrlf is common on Windows).
        .args(["-c", "core.autocrlf=false", "-c", "core.eol=lf"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ForgeError::UnsupportedPlatform(
                "the `git` command was not found in PATH (needed for git registries)".into(),
            )
        } else {
            ForgeError::Io {
                context: "running git".into(),
                source: e,
            }
        }
    })?;
    // Drain the pipes on helper threads so a chatty git cannot block on a full pipe.
    fn drain<R: std::io::Read + Send + 'static>(
        pipe: Option<R>,
    ) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut text);
            }
            text
        })
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ForgeError::Timeout {
                    operation: format!("git {verb}"),
                    after: timeout,
                });
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => {
                return Err(ForgeError::Io {
                    context: "waiting for git".into(),
                    source: e,
                });
            }
        }
    };
    let out = stdout.join().unwrap_or_default();
    let err = stderr.join().unwrap_or_default();
    if status.success() {
        Ok(out.trim().to_owned())
    } else {
        Err(ForgeError::Io {
            context: format!("git {verb} failed ({status})"),
            source: std::io::Error::other(err.trim().to_owned()),
        })
    }
}

/// Clones or updates the checkout at `dir` and returns the commit id of `HEAD`.
/// Blocking: call from `spawn_blocking`.
pub fn git_sync(url: &Url, dir: &Path, timeout: Duration) -> Result<String> {
    if let Some(reason) = unsafe_git_url(url) {
        return Err(ForgeError::InvalidInput(reason));
    }
    if dir.join(".git").is_dir() {
        run_git(
            &["fetch", "--depth", "1", "origin", "HEAD"],
            Some(dir),
            timeout,
        )?;
        run_git(&["reset", "--hard", "FETCH_HEAD"], Some(dir), timeout)?;
    } else {
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(|e| ForgeError::Io {
                context: format!("clearing {}", dir.display()),
                source: e,
            })?;
        }
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ForgeError::Io {
                context: format!("creating {}", parent.display()),
                source: e,
            })?;
        }
        let dir_text = dir.to_string_lossy();
        let cloned = run_git(
            &["clone", "--depth", "1", "--", url.as_str(), &dir_text],
            None,
            timeout,
        );
        if let Err(e) = cloned {
            let _ = std::fs::remove_dir_all(dir);
            return Err(e);
        }
    }
    run_git(&["rev-parse", "HEAD"], Some(dir), timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_understands_the_user_forms() {
        assert_eq!(
            RegistrySource::parse("https://example.org/r/index.yaml").unwrap(),
            RegistrySource::Http(Url::parse("https://example.org/r/index.yaml").unwrap())
        );
        assert!(matches!(
            RegistrySource::parse("https://github.com/forgebox/registry.git").unwrap(),
            RegistrySource::Git(_)
        ));
        assert!(matches!(
            RegistrySource::parse("git+ssh://git@example.org/r").unwrap(),
            RegistrySource::Git(_)
        ));
        assert!(matches!(
            RegistrySource::parse(r"C:\registries\mine").unwrap(),
            RegistrySource::Dir(_)
        ));
        assert!(matches!(
            RegistrySource::parse("file:///C:/registries/mine").unwrap(),
            RegistrySource::Dir(_)
        ));
        assert!(RegistrySource::parse("  ").is_err());
    }

    #[test]
    fn kind_and_location_round_trip() {
        for input in [
            "https://example.org/index.yaml",
            "git+https://example.org/r",
            "file:///C:/registries/mine",
        ] {
            let source = RegistrySource::parse(input).unwrap();
            let again = RegistrySource::from_parts(source.kind(), &source.location()).unwrap();
            assert_eq!(source, again, "{input}");
        }
        assert!(RegistrySource::from_parts("ftp", "x").is_err());
    }

    #[test]
    fn git_urls_are_restricted_to_safe_schemes() {
        assert!(unsafe_git_url(&Url::parse("ftp://example.org/r").unwrap()).is_some());
        assert!(unsafe_git_url(&Url::parse("https://example.org/r").unwrap()).is_none());
        let err = git_sync(
            &Url::parse("ftp://example.org/r").unwrap(),
            Path::new("unused"),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(err.code(), "invalid_input");
    }
}
