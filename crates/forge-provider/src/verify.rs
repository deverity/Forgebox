//! File verification (docs/05 §3, docs/11 §2): size, SHA-256 or SHA-512 (mandatory unless
//! the caller explicitly allows an unverified file) and an optional signature:
//!
//! * detached OpenPGP with an embedded or armored keyring ([`PgpKeyring`], `pgp` crate);
//! * minisign (`minisign-verify`, prehashed signatures only, streamed);
//! * Authenticode of an executable through [`forge_core::platform::CodeSign`]
//!   (`WinVerifyTrust`), optionally pinned to a publisher name.
//!
//! [`Expect`] is plain data: building it never touches the disk. All work happens in
//! `spawn_blocking` ([`Verifier::verify`]) in 1 MiB buffers; the checksum and a minisign
//! signature are computed in the same pass over the file. Mismatches and bad signatures
//! are `verification_failed` (CLI exit 6); malformed expectations are `invalid_input`.

mod pgp;

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::jobs::{JobContext, JobProgress};
use forge_core::platform::{CodeSign, CodeSignInfo, Platform};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};

pub use self::pgp::PgpKeyring;
use crate::messages;

/// Read buffer of the hashing pass.
const BUFFER_SIZE: usize = 1024 * 1024;
/// Minimum interval between progress reports.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Expected digest of the file, lower- or upper-case hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Checksum {
    Sha256(String),
    Sha512(String),
}

impl Checksum {
    /// `sha256` or `sha512`.
    pub fn algorithm(&self) -> &'static str {
        match self {
            Self::Sha256(_) => "sha256",
            Self::Sha512(_) => "sha512",
        }
    }

    /// The expected digest, lower-case.
    fn normalized(&self) -> Result<String> {
        let (hex, len) = match self {
            Self::Sha256(hex) => (hex, 64),
            Self::Sha512(hex) => (hex, 128),
        };
        let hex = hex.trim();
        if hex.len() != len || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ForgeError::InvalidInput(format!(
                "expected {} must be {len} hex digits, got {hex:?}",
                self.algorithm()
            )));
        }
        Ok(hex.to_ascii_lowercase())
    }
}

/// Signature to check after the checksum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signature {
    /// Detached OpenPGP signature (`.asc` armored or `.sig` binary) at `sig_path`, made by
    /// a key of `keyring`.
    Pgp {
        sig_path: PathBuf,
        keyring: PgpKeyring,
    },
    /// Minisign signature: `sig` is the content of the `.minisig` file, `pubkey` the
    /// base64 key (`RW...`) or the content of a `minisign.pub` file.
    Minisign { sig: String, pubkey: String },
    /// Authenticode of the file itself (an `.exe`/`.dll`). When `expected_publisher` is
    /// set, the signer's display name must be exactly that.
    Authenticode { expected_publisher: Option<String> },
}

impl Signature {
    fn kind(&self) -> &'static str {
        match self {
            Self::Pgp { .. } => "PGP",
            Self::Minisign { .. } => "minisign",
            Self::Authenticode { .. } => "Authenticode",
        }
    }
}

/// What the file must look like. Build with [`Expect::sha256`] / [`Expect::sha512`]
/// (or [`Expect::unverified`] for `--allow-unverified`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Expect {
    pub size: Option<u64>,
    pub checksum: Option<Checksum>,
    pub signature: Option<Signature>,
    /// Accept a file without checksum. Off by default: no checksum is an error.
    pub allow_unverified: bool,
}

impl Expect {
    pub fn sha256(hex: impl Into<String>) -> Self {
        Self {
            checksum: Some(Checksum::Sha256(hex.into())),
            ..Self::default()
        }
    }

    pub fn sha512(hex: impl Into<String>) -> Self {
        Self {
            checksum: Some(Checksum::Sha512(hex.into())),
            ..Self::default()
        }
    }

    /// No checksum (`--allow-unverified`): the sha256 is still computed and reported, and
    /// the report says `unverified` so the UI keeps its warning.
    pub fn unverified() -> Self {
        Self {
            allow_unverified: true,
            ..Self::default()
        }
    }

    pub fn size(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }

    pub fn signature(mut self, signature: Signature) -> Self {
        self.signature = Some(signature);
        self
    }
}

/// Signature that was verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignatureReport {
    Pgp {
        keyring: String,
        /// Fingerprint of the key (primary or subkey) that signed.
        signer: String,
        /// Fingerprint of its primary key.
        primary: String,
    },
    Minisign {
        trusted_comment: String,
    },
    Authenticode(CodeSignInfo),
}

/// Result of a successful verification (`job.done` payload of verification steps).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyReport {
    pub path: PathBuf,
    pub size: u64,
    /// Computed sha256 (lower-case hex) when it was the checksum or none was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Computed sha512 when it was the checksum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha512: Option<String>,
    /// No checksum was checked (`--allow-unverified`).
    pub unverified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<SignatureReport>,
}

/// Step reported by [`Verifier::verify_blocking`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyProgress {
    /// Bytes read so far by the checksum pass.
    Hashing { done: u64, total: u64 },
    /// Checking the signature (`PGP`, `minisign`, `Authenticode`).
    Signature(&'static str),
}

/// Verifies files. Cheap to clone.
#[derive(Clone)]
pub struct Verifier {
    code_sign: Arc<dyn CodeSign>,
}

impl std::fmt::Debug for Verifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Verifier").finish_non_exhaustive()
    }
}

impl Verifier {
    /// Uses the code signing implementation of `platform`.
    pub fn new(platform: &Platform) -> Self {
        Self {
            code_sign: Arc::clone(platform.code_sign()),
        }
    }

    /// Verifies `path` against `expect` in `spawn_blocking`.
    pub async fn verify(&self, path: impl Into<PathBuf>, expect: Expect) -> Result<VerifyReport> {
        self.run(path.into(), expect, None).await
    }

    /// Like [`Verifier::verify`], as a step of a job: reports `job.progress` (bytes hashed)
    /// and stops with `cancelled` when the job is cancelled.
    pub async fn verify_in_job(
        &self,
        ctx: &JobContext,
        path: impl Into<PathBuf>,
        expect: Expect,
    ) -> Result<VerifyReport> {
        self.run(path.into(), expect, Some(ctx.clone())).await
    }

    async fn run(
        &self,
        path: PathBuf,
        expect: Expect,
        ctx: Option<JobContext>,
    ) -> Result<VerifyReport> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut last = None::<Instant>;
            let label = expect
                .checksum
                .as_ref()
                .map_or("sha256", Checksum::algorithm);
            let mut progress = |step: VerifyProgress| {
                let Some(ctx) = &ctx else { return };
                match step {
                    VerifyProgress::Hashing { done, total } => {
                        if last.is_some_and(|t| t.elapsed() < PROGRESS_INTERVAL) && done < total {
                            return;
                        }
                        last = Some(Instant::now());
                        let pct = if total == 0 {
                            100.0
                        } else {
                            done as f32 * 100.0 / total as f32
                        };
                        ctx.progress(
                            JobProgress::new()
                                .pct(pct)
                                .bytes(done, Some(total))
                                .message(messages::verifying(label)),
                        );
                    }
                    VerifyProgress::Signature(kind) => {
                        ctx.progress(
                            JobProgress::new().message(messages::verifying_signature(kind)),
                        );
                    }
                }
            };
            let cancelled = || ctx.as_ref().is_some_and(JobContext::is_cancelled);
            this.verify_blocking(&path, &expect, &mut progress, &cancelled)
        })
        .await
        .map_err(|e| ForgeError::Internal(format!("verification task failed: {e}")))?
    }

    /// The verification itself (blocking). `progress` is called as the file is read and
    /// before the signature check; `cancelled()` is polled between buffers.
    pub fn verify_blocking(
        &self,
        path: &Path,
        expect: &Expect,
        progress: &mut dyn FnMut(VerifyProgress),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<VerifyReport> {
        let meta = std::fs::metadata(path).at("inspecting", path)?;
        if !meta.is_file() {
            return Err(ForgeError::InvalidInput(format!(
                "{} is not a file",
                path.display()
            )));
        }
        let size = meta.len();
        if let Some(expected) = expect.size {
            if expected != size {
                return Err(ForgeError::VerificationFailed(format!(
                    "size of {}: expected {expected} bytes, got {size}",
                    path.display()
                )));
            }
        }

        let expected_digest = match &expect.checksum {
            Some(checksum) => Some(checksum.normalized()?),
            None if expect.allow_unverified => {
                tracing::warn!(path = %path.display(), "verifying without checksum (allow_unverified)");
                None
            }
            None => {
                return Err(ForgeError::VerificationFailed(format!(
                    "no sha256/sha512 to verify {} (installing without one needs \
                     --allow-unverified)",
                    path.display()
                )));
            }
        };
        let minisign = match &expect.signature {
            Some(Signature::Minisign { sig, pubkey }) => Some(parse_minisign(sig, pubkey)?),
            _ => None,
        };

        // One pass: checksum and minisign stream.
        let use_sha512 = matches!(expect.checksum, Some(Checksum::Sha512(_)));
        let mut sha256 = (!use_sha512).then(Sha256::new);
        let mut sha512 = use_sha512.then(Sha512::new);
        let mut stream = match &minisign {
            Some((key, sig)) => Some(key.verify_stream(sig).map_err(|e| {
                ForgeError::VerificationFailed(format!(
                    "minisign signature of {}: {e}",
                    path.display()
                ))
            })?),
            None => None,
        };
        let mut file = File::open(path).at("opening", path)?;
        let mut buffer = vec![0u8; BUFFER_SIZE];
        let mut done = 0u64;
        progress(VerifyProgress::Hashing {
            done: 0,
            total: size,
        });
        loop {
            if cancelled() {
                return Err(ForgeError::Cancelled(format!(
                    "verification of {}",
                    path.display()
                )));
            }
            let n = file.read(&mut buffer).at("reading", path)?;
            if n == 0 {
                break;
            }
            let chunk = &buffer[..n];
            if let Some(h) = sha256.as_mut() {
                h.update(chunk);
            }
            if let Some(h) = sha512.as_mut() {
                h.update(chunk);
            }
            if let Some(s) = stream.as_mut() {
                s.update(chunk);
            }
            done += n as u64;
            progress(VerifyProgress::Hashing {
                done,
                total: size.max(done),
            });
        }
        let sha256 = sha256.map(|h| hex_lower(&h.finalize()));
        let sha512 = sha512.map(|h| hex_lower(&h.finalize()));

        if let (Some(expected), Some(checksum)) = (&expected_digest, &expect.checksum) {
            let actual = sha256
                .as_ref()
                .or(sha512.as_ref())
                .cloned()
                .unwrap_or_default();
            if &actual != expected {
                return Err(ForgeError::VerificationFailed(format!(
                    "{} of {}: expected {expected}, got {actual}",
                    checksum.algorithm(),
                    path.display()
                )));
            }
        }

        if let Some(sig) = &expect.signature {
            progress(VerifyProgress::Signature(sig.kind()));
        }
        let signature = match &expect.signature {
            None => None,
            Some(Signature::Minisign { .. }) => {
                let (_, sig) = minisign.as_ref().expect("parsed above");
                stream
                    .as_mut()
                    .expect("created above")
                    .finalize()
                    .map_err(|e| {
                        ForgeError::VerificationFailed(format!(
                            "minisign signature of {}: {e}",
                            path.display()
                        ))
                    })?;
                Some(SignatureReport::Minisign {
                    trusted_comment: sig.trusted_comment().to_owned(),
                })
            }
            Some(Signature::Pgp { sig_path, keyring }) => {
                let signer = pgp::verify_detached(path, sig_path, keyring)?;
                Some(SignatureReport::Pgp {
                    keyring: keyring.name().to_owned(),
                    signer: signer.signer,
                    primary: signer.primary,
                })
            }
            Some(Signature::Authenticode { expected_publisher }) => {
                let info = self.code_sign.verify(path)?;
                check_authenticode(path, &info, expected_publisher.as_deref())?;
                Some(SignatureReport::Authenticode(info))
            }
        };
        if let Some(sig) = &expect.signature {
            tracing::debug!(path = %path.display(), kind = sig.kind(), "signature verified");
        }

        Ok(VerifyReport {
            path: path.to_path_buf(),
            size,
            sha256,
            sha512,
            unverified: expected_digest.is_none(),
            signature,
        })
    }
}

/// Verifies `path` with the platform of the running OS. See [`Verifier::verify`].
pub async fn verify_file(path: impl Into<PathBuf>, expect: Expect) -> Result<VerifyReport> {
    Verifier::new(&forge_core::platform::current())
        .verify(path, expect)
        .await
}

fn parse_minisign(
    sig: &str,
    pubkey: &str,
) -> Result<(minisign_verify::PublicKey, minisign_verify::Signature)> {
    let pubkey = pubkey.trim();
    let key = if pubkey.contains('\n') {
        minisign_verify::PublicKey::decode(pubkey)
    } else {
        minisign_verify::PublicKey::from_base64(pubkey)
    }
    .map_err(|e| ForgeError::InvalidInput(format!("minisign public key: {e}")))?;
    let sig = minisign_verify::Signature::decode(sig).map_err(|e| {
        ForgeError::VerificationFailed(format!("unreadable minisign signature: {e}"))
    })?;
    Ok((key, sig))
}

fn check_authenticode(path: &Path, info: &CodeSignInfo, expected: Option<&str>) -> Result<()> {
    let status = info.status.as_deref().unwrap_or("unknown");
    if !info.signed {
        return Err(ForgeError::VerificationFailed(format!(
            "{} has no Authenticode signature ({status})",
            path.display()
        )));
    }
    if !info.trusted {
        return Err(ForgeError::VerificationFailed(format!(
            "Authenticode signature of {} is not trusted: {status}",
            path.display()
        )));
    }
    if let Some(expected) = expected {
        let actual = info.publisher.as_deref().unwrap_or("");
        if actual.trim() != expected.trim() {
            return Err(ForgeError::VerificationFailed(format!(
                "{} is signed by {actual:?}, expected {expected:?}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_validation() {
        let ok = Checksum::Sha256("AB".repeat(32));
        assert_eq!(ok.normalized().unwrap(), "ab".repeat(32));
        for bad in [
            Checksum::Sha256("ab".repeat(31)),
            Checksum::Sha256("zz".repeat(32)),
            Checksum::Sha512("ab".repeat(32)),
        ] {
            assert_eq!(bad.normalized().unwrap_err().code(), "invalid_input");
        }
    }

    #[test]
    fn authenticode_rules() {
        let path = Path::new("tool.exe");
        let mut info = CodeSignInfo {
            signed: true,
            trusted: true,
            publisher: Some("Example, Inc.".into()),
            kind: None,
            status: None,
        };
        check_authenticode(path, &info, None).unwrap();
        check_authenticode(path, &info, Some("Example, Inc.")).unwrap();
        let err = check_authenticode(path, &info, Some("Other")).unwrap_err();
        assert_eq!(err.code(), "verification_failed");
        info.trusted = false;
        assert!(check_authenticode(path, &info, None).is_err());
        info.signed = false;
        assert!(check_authenticode(path, &info, None).is_err());
    }

    #[test]
    fn report_wire_format() {
        let report = VerifyReport {
            path: PathBuf::from("a.zip"),
            size: 3,
            sha256: Some("00".repeat(32)),
            sha512: None,
            unverified: false,
            signature: Some(SignatureReport::Pgp {
                keyring: "nginx".into(),
                signer: "AA".into(),
                primary: "BB".into(),
            }),
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["signature"]["type"], "pgp");
        assert!(json.get("sha512").is_none());
        let back: VerifyReport = serde_json::from_value(json).unwrap();
        assert_eq!(back, report);

        let authenticode = SignatureReport::Authenticode(CodeSignInfo {
            signed: true,
            trusted: true,
            publisher: Some("Microsoft Windows".into()),
            kind: Some(forge_core::platform::CodeSignKind::Catalog),
            status: None,
        });
        let json = serde_json::to_value(&authenticode).unwrap();
        assert_eq!(json["type"], "authenticode");
        assert_eq!(json["publisher"], "Microsoft Windows");
        assert_eq!(
            serde_json::from_value::<SignatureReport>(json).unwrap(),
            authenticode
        );
    }
}
