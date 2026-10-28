//! Detached OpenPGP signatures (nginx `.zip.asc`, Node.js `SHASUMS256.txt.sig`) with the
//! pure Rust `pgp` crate (rpgp; sequoia needs nettle/cmake on Windows, ADR-0017).
//!
//! Keyrings are either embedded in the binary (`crates/forge-provider/keys/<name>.asc`,
//! selected by name from a manifest) or given as armored text (registry). A keyring file
//! may hold several armored blocks; text outside them (the fingerprint comments) is
//! ignored.
//!
//! Trust model: every primary key of the keyring is trusted as is (the keyring itself is
//! the trust anchor, its fingerprints are pinned by tests). A subkey is only used when its
//! binding signatures by the primary verify (including the back-signature of signing
//! subkeys) and it has no revocation. Primary keys carrying a revocation signature are
//! dropped. User ID certifications are not evaluated (third-party certifications are
//! normal in upstream keys). Key expiration is not evaluated (rpgp has no policy engine):
//! the embedded keyrings only hold keys the upstream lists as current, and the file is
//! pinned by its sha256 before the signature is checked (docs/11 §2).

use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::Path;
use std::sync::{Arc, OnceLock};

use forge_core::error::{ForgeError, IoResultExt, Result};
use pgp::composed::{Deserializable, DetachedSignature, SignedPublicKey};
use pgp::packet::{PublicKey, PublicSubkey, Signature as PgpSignature, SignatureType};
use pgp::types::{KeyDetails, VerifyingKey};

/// Where the trusted keys come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PgpKeyring {
    /// A keyring shipped in the binary, by name (`nginx`, `nodejs`): see
    /// [`PgpKeyring::EMBEDDED`].
    Embedded(String),
    /// Armored public keys (one or more blocks) from another trusted source, such as a
    /// signed registry. `name` only appears in reports and errors.
    Armored { name: String, armored: String },
}

impl PgpKeyring {
    /// Names accepted by [`PgpKeyring::Embedded`].
    pub const EMBEDDED: &'static [&'static str] = &["nginx", "nodejs"];

    /// Embedded keyring `name` (validated when the signature is checked).
    pub fn embedded(name: impl Into<String>) -> Self {
        Self::Embedded(name.into())
    }

    /// Name shown in reports.
    pub fn name(&self) -> &str {
        match self {
            Self::Embedded(name) => name,
            Self::Armored { name, .. } => name,
        }
    }

    /// Fingerprints (upper-case hex) of the primary keys, parsing the keyring. Blocking.
    pub fn fingerprints(&self) -> Result<Vec<String>> {
        Ok(load(self)?.iter().map(|k| k.fingerprint.clone()).collect())
    }
}

/// Armored text of each embedded keyring.
const EMBEDDED_TEXT: [(&str, &str); 2] = [
    ("nginx", include_str!("../../keys/nginx.asc")),
    ("nodejs", include_str!("../../keys/nodejs.asc")),
];

/// A primary key with the subkeys it validly binds.
#[derive(Debug)]
pub(crate) struct TrustedKey {
    primary: PublicKey,
    fingerprint: String,
    subkeys: Vec<PublicSubkey>,
}

impl TrustedKey {
    fn from_signed(key: SignedPublicKey) -> Option<Self> {
        let fingerprint = fingerprint_hex(&key.primary_key);
        if !key.details.revocation_signatures.is_empty() {
            tracing::warn!(%fingerprint, "PGP key carries a revocation signature: ignored");
            return None;
        }
        let mut subkeys = Vec::new();
        for subkey in key.public_subkeys {
            let revoked = subkey
                .signatures
                .iter()
                .any(|s| s.typ() == Some(SignatureType::SubkeyRevocation));
            if revoked {
                continue;
            }
            match subkey.verify_bindings(&key.primary_key) {
                Ok(()) => subkeys.push(subkey.key),
                Err(e) => tracing::warn!(
                    %fingerprint,
                    subkey = %fingerprint_hex(&subkey.key),
                    "PGP subkey binding does not verify, subkey ignored: {e}"
                ),
            }
        }
        Some(Self {
            primary: key.primary_key,
            fingerprint,
            subkeys,
        })
    }
}

type Keys = Arc<Vec<TrustedKey>>;

/// Parsed embedded keyrings, filled on first use.
static EMBEDDED_PARSED: [OnceLock<Keys>; 2] = [const { OnceLock::new() }; 2];

/// Keys of `keyring`, parsed and with verified bindings.
pub(crate) fn load(keyring: &PgpKeyring) -> Result<Keys> {
    match keyring {
        PgpKeyring::Embedded(name) => {
            let index = EMBEDDED_TEXT
                .iter()
                .position(|(n, _)| n == name)
                .ok_or_else(|| {
                    ForgeError::NotFound(format!(
                        "embedded PGP keyring {name:?} (known: {})",
                        PgpKeyring::EMBEDDED.join(", ")
                    ))
                })?;
            if let Some(keys) = EMBEDDED_PARSED[index].get() {
                return Ok(Arc::clone(keys));
            }
            let keys = Arc::new(parse_keyring(name, EMBEDDED_TEXT[index].1).map_err(|e| {
                ForgeError::Internal(format!("embedded PGP keyring {name} is broken: {e}"))
            })?);
            Ok(Arc::clone(EMBEDDED_PARSED[index].get_or_init(|| keys)))
        }
        PgpKeyring::Armored { name, armored } => Ok(Arc::new(parse_keyring(name, armored)?)),
    }
}

/// Every usable public key in the armored blocks of `text`.
fn parse_keyring(name: &str, text: &str) -> Result<Vec<TrustedKey>> {
    const BEGIN: &str = "-----BEGIN PGP PUBLIC KEY BLOCK-----";
    const END: &str = "-----END PGP PUBLIC KEY BLOCK-----";
    let invalid = |why: String| ForgeError::InvalidInput(format!("PGP keyring {name}: {why}"));
    let mut keys = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(BEGIN) {
        let block = &rest[start..];
        let end = block
            .find(END)
            .ok_or_else(|| invalid("armor block without END line".into()))?
            + END.len();
        let (parsed, _) = SignedPublicKey::from_armor_many(Cursor::new(&block.as_bytes()[..end]))
            .map_err(|e| invalid(format!("unreadable armor: {e}")))?;
        for key in parsed {
            let key = key.map_err(|e| invalid(format!("unreadable key: {e}")))?;
            keys.extend(TrustedKey::from_signed(key));
        }
        rest = &block[end..];
    }
    if keys.is_empty() {
        return Err(invalid("no public key found".into()));
    }
    Ok(keys)
}

fn fingerprint_hex(key: &impl KeyDetails) -> String {
    format!("{:X}", key.fingerprint())
}

fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// Who made a verified signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PgpSigner {
    /// Fingerprint of the key (primary or subkey) that made the signature.
    pub signer: String,
    /// Fingerprint of its primary key.
    pub primary: String,
}

/// Whether `sig` names `key` as issuer (or names nobody, so any key may have made it).
fn issued_by(sig: &PgpSignature, key: &impl KeyDetails) -> bool {
    let ids = sig.issuer_key_id();
    let fingerprints = sig.issuer_fingerprint();
    if ids.is_empty() && fingerprints.is_empty() {
        return true;
    }
    ids.iter().any(|id| **id == key.legacy_key_id())
        || fingerprints.iter().any(|fp| **fp == key.fingerprint())
}

fn issuer_description(sig: &PgpSignature) -> String {
    if let Some(fp) = sig.issuer_fingerprint().first() {
        return format!("{fp:X}");
    }
    sig.issuer_key_id()
        .first()
        .map(|id| hex_upper(id.as_ref()))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Verifies that `sig_path` holds a valid detached signature of `file` by a key of
/// `keyring`. Blocking (reads `file` once per candidate key, usually once).
pub(crate) fn verify_detached(
    file: &Path,
    sig_path: &Path,
    keyring: &PgpKeyring,
) -> Result<PgpSigner> {
    let failed = |why: String| {
        ForgeError::VerificationFailed(format!("PGP signature of {}: {why}", file.display()))
    };
    let bytes = std::fs::read(sig_path).at("reading", sig_path)?;
    let (parsed, _) = DetachedSignature::from_reader_many(Cursor::new(bytes))
        .map_err(|e| failed(format!("unreadable signature file: {e}")))?;
    let signatures: Vec<DetachedSignature> = parsed
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| failed(format!("unreadable signature file: {e}")))?;
    if signatures.is_empty() {
        return Err(failed("the signature file holds no signature".into()));
    }
    let keys = load(keyring)?;

    let mut last_error = None;
    let mut issuers = Vec::new();
    for detached in &signatures {
        let sig = &detached.signature;
        if !matches!(sig.typ(), Some(SignatureType::Binary | SignatureType::Text)) {
            last_error = Some(format!("not a document signature ({:?})", sig.typ()));
            continue;
        }
        issuers.push(issuer_description(sig));
        for key in keys.iter() {
            let primary = key.fingerprint.clone();
            if issued_by(sig, &key.primary) {
                match check(sig, &key.primary, file)? {
                    Ok(()) => {
                        return Ok(PgpSigner {
                            signer: primary.clone(),
                            primary,
                        });
                    }
                    Err(e) => last_error = Some(e),
                }
            }
            for subkey in &key.subkeys {
                if issued_by(sig, subkey) {
                    match check(sig, subkey, file)? {
                        Ok(()) => {
                            return Ok(PgpSigner {
                                signer: fingerprint_hex(subkey),
                                primary,
                            });
                        }
                        Err(e) => last_error = Some(e),
                    }
                }
            }
        }
    }
    Err(failed(match last_error {
        Some(e) => format!("invalid signature: {e}"),
        None => format!(
            "made by {}, which is not in the {} keyring",
            issuers.join(", "),
            keyring.name()
        ),
    }))
}

/// Cryptographic check of `sig` over the content of `file` with `key`. The outer `Err` is
/// an I/O problem with `file`; the inner one a signature that does not verify.
fn check(
    sig: &PgpSignature,
    key: &impl VerifyingKey,
    file: &Path,
) -> Result<std::result::Result<(), String>> {
    let reader = BufReader::with_capacity(1024 * 1024, File::open(file).at("opening", file)?);
    Ok(sig.verify(key, reader).map_err(|e| e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Primary key fingerprints of the embedded keyrings. Keep in sync with the comments
    /// at the top of `keys/*.asc`; a change here means the shipped trust changed.
    const NGINX: &[&str] = &[
        "43387825DDB1BB97EC36BA5D007C8D7C15D87369",
        "D6786CE303D9A9022998DC6CC8464D549AF75C0A",
        "13C82A63B603576156E30A4EA0EA981B66B0D967",
        "8540A6F18833A80E9C1653A42FD21310B49F6B46",
        "573BFD6B3D8FBC641079A6ABABF5BD827BD9BF62",
        "9E9BE90EACBCDE69FE9B204CBCDCD8A38D88A2B3",
    ];
    const NODEJS: &[&str] = &[
        "5BE8A3F6C8A5C01D106C0AD820B1A390B168D356",
        "DD792F5973C6DE52C432CBDAC77ABFA00DDBF2B7",
        "CC68F5A3106FF448322E48ED27F5E38D5B0A215F",
        "890C08DB8579162FEE0DF9DB8BEAB4DFCF555EF4",
        "C82FA3AE1CBEDC6BE46B9360C43CEC45C17AB93C",
        "108F52B48DB57BB0CC439B2997B01419BD92F80A",
        "655F3B5C1FB3FA8D1A0CA6BDE4A7D232B936D2FD",
        "A363A499291CBBC940DD62E41F10027AF002F8B0",
    ];

    #[test]
    fn embedded_keyrings_hold_the_documented_keys() {
        for (name, expected) in [("nginx", NGINX), ("nodejs", NODEJS)] {
            let found = PgpKeyring::embedded(name).fingerprints().unwrap();
            assert_eq!(found, expected, "{name}");
            let (_, text) = EMBEDDED_TEXT.iter().find(|(n, _)| *n == name).unwrap();
            for fp in expected {
                assert!(text.contains(fp), "{name}.asc comments must list {fp}");
            }
        }
        assert_eq!(
            PgpKeyring::EMBEDDED,
            EMBEDDED_TEXT.map(|(n, _)| n).as_slice()
        );
    }

    #[test]
    fn unknown_embedded_keyring_is_not_found() {
        let err = load(&PgpKeyring::embedded("openssl")).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[test]
    fn armored_keyring_must_contain_keys() {
        let keyring = PgpKeyring::Armored {
            name: "empty".into(),
            armored: "# nothing here\n".into(),
        };
        assert_eq!(load(&keyring).unwrap_err().code(), "invalid_input");
        let keyring = PgpKeyring::Armored {
            name: "cut".into(),
            armored: "-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nAAAA\n".into(),
        };
        assert_eq!(load(&keyring).unwrap_err().code(), "invalid_input");
    }
}
