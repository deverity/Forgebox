//! OS certificate trust store (docs/06 §6.1, docs/11 §3, ADR-0023).
//!
//! * Roots are identified by their SHA-1 [`CertThumbprint`], what `certmgr.msc`, `certutil`
//!   and `Cert:\` in PowerShell show, and what the helper operations
//!   `trust.add_machine_root` / `trust.remove_machine_root` take.
//! * [`TrustScope::CurrentUser`] needs no elevation. On Windows the OS shows a confirmation
//!   dialog when a root is added to (or removed from) `CurrentUser\Root`; the call blocks
//!   until the user answers, and a refusal is `cancelled`.
//! * [`TrustScope::LocalMachine`] is read without elevation; changes go through
//!   `forgebox-helper` (ADR-0022), or are written directly when the caller already is the
//!   elevated helper.
//! * [`TrustStore::verify_server`] asks the OS TLS verifier (Windows: CryptoAPI, what
//!   Schannel, curl and Chrome use) whether a server chain is valid for a host **if** a
//!   given anchor were trusted, without touching any store: diagnostics and tests of the
//!   CA name constraints.
//!
//! Every method is **blocking** (Win32 certificate APIs, UAC, a dialog waiting for a
//! human): async code calls them from `spawn_blocking` with a timeout (ADR-0011).

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha1::{Digest, Sha1};

use crate::error::{ForgeError, Result};
use crate::helper::HelperClient;

/// SHA-1 digest of a DER certificate. Displayed as 40 uppercase hex digits (as Windows
/// does); parsed from 40 hex digits in any case.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CertThumbprint([u8; 20]);

impl CertThumbprint {
    /// Thumbprint of the DER encoded certificate `der`.
    pub fn of_der(der: &[u8]) -> Self {
        Self(Sha1::digest(der).into())
    }

    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// 40 uppercase hex digits.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02X}")).collect()
    }
}

impl fmt::Display for CertThumbprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for CertThumbprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CertThumbprint({})", self.to_hex())
    }
}

impl FromStr for CertThumbprint {
    type Err = ForgeError;

    fn from_str(text: &str) -> Result<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 40 || !bytes.iter().all(u8::is_ascii_hexdigit) {
            return Err(ForgeError::InvalidInput(format!(
                "thumbprint (SHA-1) must be 40 hexadecimal digits, got {text:?}"
            )));
        }
        let mut out = [0_u8; 20];
        for (slot, pair) in out.iter_mut().zip(bytes.chunks_exact(2)) {
            let pair = std::str::from_utf8(pair).expect("ASCII hex digits");
            *slot = u8::from_str_radix(pair, 16).expect("validated hex digits");
        }
        Ok(Self(out))
    }
}

impl Serialize for CertThumbprint {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for CertThumbprint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// Where a root certificate is trusted (docs/06 §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustScope {
    /// `CurrentUser\Root`: this user only, no elevation (one OS confirmation dialog).
    CurrentUser,
    /// `LocalMachine\Root`: every user of the machine, through `forgebox-helper`.
    LocalMachine,
}

impl TrustScope {
    /// `current_user` / `local_machine`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CurrentUser => "current_user",
            Self::LocalMachine => "local_machine",
        }
    }
}

impl fmt::Display for TrustScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// OS certificate trust store (docs/06 §6.1, docs/11 §3).
pub trait TrustStore: Send + Sync + fmt::Debug {
    /// Whether a root with `thumbprint` is in the store of `scope` itself (for
    /// `CurrentUser`, the user's own roots, not the machine roots Windows also lists
    /// there). No elevation, no dialog.
    fn is_trusted(&self, thumbprint: &CertThumbprint, scope: TrustScope) -> Result<bool>;

    /// Adds the DER root `cert_der` to `scope`; already present is success.
    ///
    /// `CurrentUser`: the OS confirmation dialog blocks until answered; a refusal is
    /// `cancelled`. `LocalMachine`: `forgebox-helper` (one UAC prompt), which reads the
    /// certificate from `certs/ca/forgebox-ca.crt` and checks it has the thumbprint of
    /// `cert_der`; `needs_elevation` without a helper or when the prompt is declined.
    fn add_root(&self, cert_der: &[u8], scope: TrustScope) -> Result<()>;

    /// Removes the root with `thumbprint` from `scope`. `Ok(false)` when it was not there.
    /// Same dialogs and prompts as [`TrustStore::add_root`].
    fn remove_root(&self, thumbprint: &CertThumbprint, scope: TrustScope) -> Result<bool>;

    /// Checks with the OS TLS verifier that `chain` (DER, leaf first, then intermediates)
    /// is a valid server chain for `host` when `anchor_der` is the **only** trusted root.
    /// Name constraints, validity, key usages and signatures are checked. With
    /// `check_revocation` the leaf's revocation is checked too, fetching its CRL
    /// distribution point (what Windows `curl.exe` does by default); otherwise nothing is
    /// fetched. No store is read or written.
    ///
    /// Errors: `verification_failed` with the OS reason (name constraint violation, name
    /// mismatch, untrusted root, expired, revocation status unknown...).
    fn verify_server(
        &self,
        chain: &[Vec<u8>],
        anchor_der: &[u8],
        host: &str,
        check_revocation: bool,
    ) -> Result<()>;
}

/// The trust store of the running OS. `helper` runs the `LocalMachine` changes; without
/// it they fail with `needs_elevation`.
#[cfg(windows)]
pub fn trust_store(helper: Option<HelperClient>) -> Arc<dyn TrustStore> {
    Arc::new(super::windows::WindowsTrustStore::new(helper))
}

/// The trust store of the running OS (stub until the Linux/macOS ports:
/// `update-ca-certificates`, Keychain; docs/13 §4).
#[cfg(not(windows))]
pub fn trust_store(_helper: Option<HelperClient>) -> Arc<dyn TrustStore> {
    Arc::new(Unsupported)
}

#[cfg(not(windows))]
#[derive(Debug)]
struct Unsupported;

#[cfg(not(windows))]
impl Unsupported {
    fn err<T>() -> Result<T> {
        Err(ForgeError::UnsupportedPlatform(format!(
            "the certificate trust store is not implemented on {}",
            std::env::consts::OS
        )))
    }
}

#[cfg(not(windows))]
impl TrustStore for Unsupported {
    fn is_trusted(&self, _: &CertThumbprint, _: TrustScope) -> Result<bool> {
        Self::err()
    }
    fn add_root(&self, _: &[u8], _: TrustScope) -> Result<()> {
        Self::err()
    }
    fn remove_root(&self, _: &CertThumbprint, _: TrustScope) -> Result<bool> {
        Self::err()
    }
    fn verify_server(&self, _: &[Vec<u8>], _: &[u8], _: &str, _: bool) -> Result<()> {
        Self::err()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbprints_round_trip_as_uppercase_hex() {
        let t = CertThumbprint::of_der(b"abc");
        // SHA-1("abc") (FIPS 180-2 test vector).
        assert_eq!(t.to_hex(), "A9993E364706816ABA3E25717850C26C9CD0D89D");
        assert_eq!(t.to_string(), t.to_hex());
        let parsed: CertThumbprint = "a9993e364706816aba3e25717850c26c9cd0d89d".parse().unwrap();
        assert_eq!(parsed, t);
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"A9993E364706816ABA3E25717850C26C9CD0D89D\"");
        assert_eq!(serde_json::from_str::<CertThumbprint>(&json).unwrap(), t);
        for bad in ["", "A9993E36", &"g".repeat(40), &"a".repeat(41)] {
            let err = bad.parse::<CertThumbprint>().unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{bad:?}");
        }
    }

    #[test]
    fn scopes_have_wire_names() {
        assert_eq!(
            serde_json::to_string(&TrustScope::CurrentUser).unwrap(),
            "\"current_user\""
        );
        assert_eq!(TrustScope::LocalMachine.to_string(), "local_machine");
    }
}
