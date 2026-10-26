//! Error type shared by the domain crates, with stable codes for the API and CLI.
//!
//! [`ForgeError::code`] is part of the public API contract (ADR-0009): clients match on
//! it, so codes are never renamed. [`ForgeError::exit_code`] follows docs/09-cli.md:
//! `0` ok · `1` generic · `2` invalid usage · `3` not found · `4` conflict ·
//! `5` needs elevation · `6` verification failed.

use std::io;
use std::path::Path;
use std::time::Duration;

/// Result alias used across Forgebox crates.
pub type Result<T, E = ForgeError> = std::result::Result<T, E>;

/// Every error the domain can report.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ForgeError {
    /// An entity, file or version does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// The operation collides with existing state (host taken, link already there...).
    #[error("conflict: {0}")]
    Conflict(String),

    /// A TCP/UDP port is taken, optionally by a known owner (`mysqld.exe (PID 4412)`).
    #[error("port {port} is already in use{}", owner_suffix(owner.as_deref()))]
    PortInUse { port: u16, owner: Option<String> },

    /// The operation must go through `forgebox-helper` (docs/11).
    #[error("operation needs elevation: {0}")]
    NeedsElevation(String),

    /// Checksum, signature or size verification failed.
    #[error("verification failed: {0}")]
    VerificationFailed(String),

    /// File system or other I/O failure, with a human readable context.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },

    /// SQLite failure.
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    /// The database schema cannot be used by this build (newer version, broken migration).
    #[error("database schema error: {0}")]
    Schema(String),

    /// `forgebox.toml`, `root.toml` or another configuration value is invalid.
    #[error("invalid configuration: {0}")]
    ConfigInvalid(String),

    /// An argument is malformed (bad id, bad path component, bad host...).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// The OS, file system or volume does not support the operation.
    #[error("unsupported on this platform: {0}")]
    UnsupportedPlatform(String),

    /// An operation exceeded its time budget (ADR-0011: every message has a timeout).
    #[error("timed out after {after:?}: {operation}")]
    Timeout { operation: String, after: Duration },

    /// The feature exists in the contracts but is not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(String),
}

fn owner_suffix(owner: Option<&str>) -> String {
    owner.map(|o| format!(" by {o}")).unwrap_or_default()
}

/// Every value [`ForgeError::code`] can return. Stable; only additions are allowed.
pub const ERROR_CODES: &[&str] = &[
    "not_found",
    "conflict",
    "port_in_use",
    "needs_elevation",
    "verification_failed",
    "io",
    "db",
    "config_invalid",
    "invalid_input",
    "unsupported_platform",
    "timeout",
    "not_implemented",
];

impl ForgeError {
    /// Stable machine-readable code, used by the API and by `forge --json`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::PortInUse { .. } => "port_in_use",
            Self::NeedsElevation(_) => "needs_elevation",
            Self::VerificationFailed(_) => "verification_failed",
            Self::Io { .. } => "io",
            Self::Db(_) | Self::Schema(_) => "db",
            Self::ConfigInvalid(_) => "config_invalid",
            Self::InvalidInput(_) => "invalid_input",
            Self::UnsupportedPlatform(_) => "unsupported_platform",
            Self::Timeout { .. } => "timeout",
            Self::NotImplemented(_) => "not_implemented",
        }
    }

    /// Process exit code for the CLI (docs/09-cli.md).
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::ConfigInvalid(_) | Self::InvalidInput(_) => 2,
            Self::NotFound(_) => 3,
            Self::Conflict(_) | Self::PortInUse { .. } => 4,
            Self::NeedsElevation(_) => 5,
            Self::VerificationFailed(_) => 6,
            Self::Io { .. }
            | Self::Db(_)
            | Self::Schema(_)
            | Self::UnsupportedPlatform(_)
            | Self::Timeout { .. }
            | Self::NotImplemented(_) => 1,
        }
    }

    /// I/O error with context.
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// I/O error about `path`, mapping `NotFound` to [`ForgeError::NotFound`].
    pub fn io_at(action: &str, path: &Path, source: io::Error) -> Self {
        if source.kind() == io::ErrorKind::NotFound {
            return Self::NotFound(format!("{} ({action})", path.display()));
        }
        Self::io(format!("{action} {}", path.display()), source)
    }
}

impl From<io::Error> for ForgeError {
    fn from(source: io::Error) -> Self {
        Self::io("I/O error", source)
    }
}

/// Adds context to `std::io::Result` values.
pub trait IoResultExt<T> {
    /// Wraps the error as [`ForgeError::Io`] (or `NotFound`) mentioning `action` and `path`.
    fn at(self, action: &str, path: &Path) -> Result<T>;
}

impl<T> IoResultExt<T> for io::Result<T> {
    fn at(self, action: &str, path: &Path) -> Result<T> {
        self.map_err(|e| ForgeError::io_at(action, path, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<ForgeError> {
        vec![
            ForgeError::NotFound("x".into()),
            ForgeError::Conflict("x".into()),
            ForgeError::PortInUse {
                port: 80,
                owner: None,
            },
            ForgeError::NeedsElevation("nrpt.add".into()),
            ForgeError::VerificationFailed("sha256".into()),
            ForgeError::io("reading", io::Error::other("boom")),
            ForgeError::Db(rusqlite::Error::InvalidQuery),
            ForgeError::Schema("x".into()),
            ForgeError::ConfigInvalid("x".into()),
            ForgeError::InvalidInput("x".into()),
            ForgeError::UnsupportedPlatform("x".into()),
            ForgeError::Timeout {
                operation: "stop".into(),
                after: Duration::from_secs(1),
            },
            ForgeError::NotImplemented("x".into()),
        ]
    }

    #[test]
    fn codes_are_stable_and_listed() {
        for err in samples() {
            assert!(ERROR_CODES.contains(&err.code()), "{}", err.code());
        }
        let mut seen: Vec<_> = samples().iter().map(ForgeError::code).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), ERROR_CODES.len(), "every code has a variant");
    }

    #[test]
    fn exit_codes_follow_cli_doc() {
        for err in samples() {
            let expected = match err.code() {
                "config_invalid" | "invalid_input" => 2,
                "not_found" => 3,
                "conflict" | "port_in_use" => 4,
                "needs_elevation" => 5,
                "verification_failed" => 6,
                _ => 1,
            };
            assert_eq!(err.exit_code(), expected, "{}", err.code());
        }
    }

    #[test]
    fn port_in_use_message_mentions_owner() {
        let err = ForgeError::PortInUse {
            port: 3306,
            owner: Some("mysqld.exe (PID 4412)".into()),
        };
        assert_eq!(
            err.to_string(),
            "port 3306 is already in use by mysqld.exe (PID 4412)"
        );
        let err = ForgeError::PortInUse {
            port: 80,
            owner: None,
        };
        assert_eq!(err.to_string(), "port 80 is already in use");
    }

    #[test]
    fn io_at_maps_not_found() {
        let path = Path::new("x.toml");
        let err = ForgeError::io_at("reading", path, io::ErrorKind::NotFound.into());
        assert_eq!(err.code(), "not_found");
        let err = ForgeError::io_at("reading", path, io::ErrorKind::PermissionDenied.into());
        assert_eq!(err.code(), "io");
        assert!(err.to_string().starts_with("reading x.toml"));
    }
}
