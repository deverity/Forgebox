//! Authentication of API connections (docs/11 §4, ADR-0009 rule 7).
//!
//! The daemon keeps a random token in `state/ipc.token` (256 bits, hex), readable only
//! by the current user ([`forge_core::platform::FileAcl`]). Every connection must start
//! with `system.hello {token, client, api_version}`. On the named pipe the DACL already
//! restricts access to the user, so the token is defence in depth; on remote transports
//! (M6) it will be one of the credentials. Local connections get the `admin` role.

use std::fmt;
use std::io::Write as _;
use std::path::Path;

use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::platform::FileAcl;

use crate::api::{HelloParams, Role};
use crate::transport::ConnectionInfo;

/// Bytes of entropy of a token.
const TOKEN_BYTES: usize = 32;

/// The API token. `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct IpcToken(String);

impl fmt::Debug for IpcToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IpcToken(<redacted>)")
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

fn looks_valid(token: &str) -> bool {
    token.len() == TOKEN_BYTES * 2 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

impl IpcToken {
    /// New random token from the OS CSPRNG.
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes)
            .map_err(|e| ForgeError::Internal(format!("OS random generator failed: {e}")))?;
        Ok(Self(hex(&bytes)))
    }

    /// Wraps a token received from elsewhere (tests, remote profiles).
    pub fn from_secret(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// The secret, for the `system.hello` of a client.
    pub fn secret(&self) -> &str {
        &self.0
    }

    /// Constant-time comparison with a presented token.
    pub fn verify(&self, presented: &str) -> bool {
        let a = self.0.as_bytes();
        let b = presented.as_bytes();
        let mut diff = a.len() ^ b.len();
        for i in 0..a.len().max(b.len()) {
            let x = a.get(i).copied().unwrap_or(0);
            let y = b.get(i).copied().unwrap_or(0xff);
            diff |= usize::from(x ^ y);
        }
        diff == 0
    }

    /// Reads the token file (client side). Blocking.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).at("reading the API token", path)?;
        let token = text.trim();
        if !looks_valid(token) {
            return Err(ForgeError::ConfigInvalid(format!(
                "{} does not contain a valid API token",
                path.display()
            )));
        }
        Ok(Self(token.to_owned()))
    }

    /// Server side: reads the token file, or creates it (random, owner-only ACL set
    /// before the secret is written) when missing or invalid. Blocking: call from
    /// `spawn_blocking`.
    pub fn load_or_create(path: &Path, acl: &dyn FileAcl) -> Result<Self> {
        if let Ok(token) = Self::load(path) {
            if !acl.is_restricted_to_current_user(path).unwrap_or(false) {
                acl.restrict_to_current_user(path)?;
            }
            return Ok(token);
        }
        let token = Self::generate()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).at("creating", dir)?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        let _ = std::fs::remove_file(&tmp);
        {
            // Empty file first, ACL second, secret last: the secret is never readable by
            // anyone else, not even for an instant.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .at("creating", &tmp)?;
            acl.restrict_to_current_user(&tmp)?;
            file.write_all(token.0.as_bytes()).at("writing", &tmp)?;
            file.sync_all().at("flushing", &tmp)?;
        }
        std::fs::rename(&tmp, path).at("replacing", path)?;
        Ok(token)
    }
}

/// Decides whether a `system.hello` is accepted and with which role.
pub trait Authenticator: Send + Sync + 'static {
    fn authenticate(&self, hello: &HelloParams, connection: &ConnectionInfo) -> Result<Role>;
}

/// Token check; local connections are `admin`, others `viewer` until remote profiles
/// exist (M6).
#[derive(Debug, Clone)]
pub struct TokenAuthenticator {
    token: IpcToken,
}

impl TokenAuthenticator {
    pub fn new(token: IpcToken) -> Self {
        Self { token }
    }
}

impl Authenticator for TokenAuthenticator {
    fn authenticate(&self, hello: &HelloParams, connection: &ConnectionInfo) -> Result<Role> {
        if !self.token.verify(&hello.token) {
            return Err(ForgeError::Unauthorized("invalid API token".into()));
        }
        Ok(if connection.local {
            Role::Admin
        } else {
            Role::Viewer
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{API_VERSION, ClientInfoDto};
    use crate::transport::TransportKind;

    #[test]
    fn tokens_are_random_hex_and_redacted() {
        let a = IpcToken::generate().unwrap();
        let b = IpcToken::generate().unwrap();
        assert_ne!(a, b);
        assert!(looks_valid(a.secret()));
        assert_eq!(format!("{a:?}"), "IpcToken(<redacted>)");
    }

    #[test]
    fn verify_is_exact() {
        let token = IpcToken::generate().unwrap();
        assert!(token.verify(token.secret()));
        assert!(!token.verify(""));
        assert!(!token.verify(&token.secret()[1..]));
        assert!(!token.verify(&format!("{}0", token.secret())));
        let mut other = token.secret().to_owned();
        other.replace_range(0..1, if other.starts_with('a') { "b" } else { "a" });
        assert!(!token.verify(&other));
    }

    #[test]
    fn load_or_create_persists_with_owner_only_acl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("ipc.token");
        let acl = forge_core::platform::current();
        let created = IpcToken::load_or_create(&path, acl.file_acl()).unwrap();
        assert_eq!(IpcToken::load(&path).unwrap(), created);
        assert_eq!(
            IpcToken::load_or_create(&path, acl.file_acl()).unwrap(),
            created,
            "existing token is kept"
        );
        #[cfg(windows)]
        assert!(acl.file_acl().is_restricted_to_current_user(&path).unwrap());

        std::fs::write(&path, "garbage").unwrap();
        assert!(IpcToken::load(&path).is_err());
        let replaced = IpcToken::load_or_create(&path, acl.file_acl()).unwrap();
        assert_ne!(replaced, created);
    }

    #[test]
    fn authenticator_checks_token_and_locality() {
        let token = IpcToken::generate().unwrap();
        let auth = TokenAuthenticator::new(token.clone());
        let hello = |secret: &str| HelloParams {
            token: secret.into(),
            client: ClientInfoDto {
                name: "test".into(),
                version: "0".into(),
            },
            api_version: API_VERSION,
        };
        let local = ConnectionInfo {
            transport: TransportKind::NamedPipe,
            peer: "p".into(),
            local: true,
        };
        let remote = ConnectionInfo {
            local: false,
            ..local.clone()
        };
        assert_eq!(
            auth.authenticate(&hello(token.secret()), &local).unwrap(),
            Role::Admin
        );
        assert_eq!(
            auth.authenticate(&hello(token.secret()), &remote).unwrap(),
            Role::Viewer
        );
        let err = auth.authenticate(&hello("nope"), &local).unwrap_err();
        assert_eq!(err.code(), "unauthorized");
    }
}
