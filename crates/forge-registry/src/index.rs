//! `index.yaml` of a registry: signature check, manifest hashes and policy (docs/04 §5,
//! docs/11 §2).
//!
//! The pipeline is the same for every source and for the cache (the cache is re-verified on
//! every load, so a tampered cache is never trusted):
//!
//! 1. [`verify_index`]: parse the index and check its minisign signature **before any
//!    manifest is read**;
//! 2. read the files listed in the index (from a folder, a git checkout or HTTP);
//! 3. [`build_loaded`]: the sha256 of every file must match the index (mismatch = the whole
//!    load fails), then each file is parsed and validated; a manifest that is invalid or that
//!    breaks the policy of a third-party registry (`post_install.run`) is *skipped* and
//!    reported, not fatal.
//!
//! What is signed (ADR-0018): the **raw bytes of `index.yaml`**, with a detached minisign
//! signature in `index.yaml.minisig` next to it (`minisign -S -m index.yaml`, or
//! `cargo xtask registry-sign`). No canonicalization and no dependency on a YAML emitter:
//! any minisign implementation can sign and verify. The bytes are never re-serialized, so
//! every source must deliver them untouched (git checkouts disable newline conversion).
//!
//! Blocking file I/O in [`read_tree`] and CPU work in [`build_loaded`]: call from
//! `spawn_blocking`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use forge_core::error::{ForgeError, Result};
use forge_core::ids::ToolId;
use forge_manifest::index::sha256_hex;
use forge_manifest::schema::{Kind, Manifest, RegistryManifest};
use forge_manifest::{ValidateOptions, validate_with};
use serde::Serialize;

/// Largest `index.yaml` accepted.
pub const MAX_INDEX_BYTES: u64 = 4 * 1024 * 1024;
/// Largest manifest file accepted.
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// Most entries (tools + stacks) one index may list.
pub const MAX_ENTRIES: usize = 4096;
/// Name of the index file in a registry folder and in the cache.
pub const INDEX_FILE: &str = "index.yaml";
/// Name of the detached minisign signature of [`INDEX_FILE`].
pub const SIGNATURE_FILE: &str = "index.yaml.minisig";
/// Largest signature file accepted.
pub const MAX_SIGNATURE_BYTES: u64 = 8 * 1024;

/// The development public key (`registry/keys/dev.pub`), committed with the repository.
/// Release builds embed the production key instead (docs/11 §7).
pub const DEV_PUBLIC_KEY: &str = include_str!("../../../registry/keys/dev.pub");

/// Whether the signature of an index was checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Verification {
    /// Valid minisign signature by the configured key.
    Verified { key_id: String },
    /// Loaded without a valid signature because the registry is `trusted_unsigned`.
    Unverified,
}

impl Verification {
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }
}

/// How an index must be verified.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyOptions {
    /// Minisign public key: base64 (`RW...`) or the text of a `.pub` file.
    pub public_key: Option<String>,
    /// Development only: an unsigned index may load (shown as "not verified"). A signature
    /// that is present but wrong is always refused.
    pub trusted_unsigned: bool,
}

/// Verification plus the policy for manifests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadOptions {
    pub verify: VerifyOptions,
    /// The official registry may declare `post_install.run`; third parties may not.
    pub official: bool,
}

/// A manifest the loader refused or could not use; the rest of the registry still loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedEntry {
    pub id: String,
    pub path: String,
    pub reason: String,
}

/// Which section of the index lists an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    /// `spec.tools` (Tool and Extension manifests).
    Tools,
    /// `spec.stacks`.
    Stacks,
}

/// A verified, parsed manifest of a registry.
#[derive(Debug, Clone)]
pub struct LoadedManifest {
    pub section: Section,
    pub id: ToolId,
    /// Path inside the registry (`tools/php.yaml`).
    pub path: String,
    pub sha256: String,
    /// `latest` and `categories` as the index says.
    pub latest: Option<String>,
    pub manifest: Arc<Manifest>,
}

/// Result of verifying and parsing a whole registry.
#[derive(Debug, Clone)]
pub struct LoadedRegistry {
    pub index: RegistryManifest,
    pub index_bytes: Vec<u8>,
    /// The detached signature as delivered (`None` for an unsigned, trusted index).
    pub signature_bytes: Option<Vec<u8>>,
    pub index_sha256: String,
    pub verification: Verification,
    /// Bytes of every file whose sha256 matched, by index path (what the cache stores).
    pub files: BTreeMap<String, Vec<u8>>,
    pub manifests: Vec<LoadedManifest>,
    pub skipped: Vec<SkippedEntry>,
}

/// An index whose signature has been checked, with its raw bytes.
#[derive(Debug, Clone)]
pub struct VerifiedIndex {
    pub bytes: Vec<u8>,
    /// Contents of `index.yaml.minisig`, if the source had one.
    pub signature: Option<Vec<u8>>,
    pub index: RegistryManifest,
    pub verification: Verification,
}

fn invalid(context: &str, detail: impl std::fmt::Display) -> ForgeError {
    ForgeError::InvalidInput(format!("{context}: {detail}"))
}

fn verification_failed(detail: impl std::fmt::Display) -> ForgeError {
    ForgeError::VerificationFailed(detail.to_string())
}

/// The base64 line of a minisign public key given as base64 or as a `.pub` file.
fn public_key_base64(public_key: &str) -> &str {
    public_key
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.starts_with("untrusted comment:"))
        .unwrap_or("")
}

/// The 16 hexadecimal digits minisign shows as key id, from a public key (base64 or `.pub`
/// text).
pub fn key_id_of_public_key(public_key: &str) -> Result<String> {
    let bytes = BASE64
        .decode(public_key_base64(public_key).as_bytes())
        .map_err(|e| invalid("minisign public key", e))?;
    if bytes.len() != 42 {
        return Err(invalid("minisign public key", "wrong length"));
    }
    let mut id = [0u8; 8];
    id.copy_from_slice(&bytes[2..10]);
    Ok(format!("{:016X}", u64::from_le_bytes(id)))
}

enum SignatureState {
    /// There is no `index.yaml.minisig`.
    Absent,
    /// A signature exists but there is no key to check it with.
    NoKey,
    Valid(String),
    Invalid(String),
}

/// Checks the detached `signature` over the raw index `bytes`.
fn check_signature(
    bytes: &[u8],
    signature: Option<&[u8]>,
    public_key: Option<&str>,
) -> SignatureState {
    let Some(signature) = signature else {
        return SignatureState::Absent;
    };
    let Some(public_key) = public_key else {
        return SignatureState::NoKey;
    };
    let key = match minisign_verify::PublicKey::from_base64(public_key_base64(public_key)) {
        Ok(key) => key,
        Err(e) => return SignatureState::Invalid(format!("configured public key: {e}")),
    };
    let expected_id = match key_id_of_public_key(public_key) {
        Ok(id) => id,
        Err(e) => return SignatureState::Invalid(e.to_string()),
    };
    let Ok(text) = std::str::from_utf8(signature) else {
        return SignatureState::Invalid("signature is not UTF-8 text".to_owned());
    };
    let signature = match minisign_verify::Signature::decode(text) {
        Ok(signature) => signature,
        Err(e) => return SignatureState::Invalid(format!("unreadable signature: {e}")),
    };
    // `verify` also refuses a signature made by another key id.
    match key.verify(bytes, &signature, false) {
        Ok(()) => SignatureState::Valid(expected_id),
        Err(e) => SignatureState::Invalid(e.to_string()),
    }
}

/// Checks the signature of the raw index `bytes` against `options`.
///
/// `Err(VerificationFailed)` unless the signature is valid, or it is absent and the registry
/// is `trusted_unsigned`. A wrong signature is never accepted.
pub fn verify_bytes(
    bytes: &[u8],
    signature: Option<&[u8]>,
    options: &VerifyOptions,
) -> Result<Verification> {
    match check_signature(bytes, signature, options.public_key.as_deref()) {
        SignatureState::Valid(key_id) => Ok(Verification::Verified { key_id }),
        SignatureState::Invalid(reason) => Err(verification_failed(format!(
            "invalid index signature: {reason}"
        ))),
        SignatureState::Absent if options.trusted_unsigned => Ok(Verification::Unverified),
        SignatureState::NoKey if options.trusted_unsigned => Ok(Verification::Unverified),
        SignatureState::Absent => Err(verification_failed(
            "the index is not signed (no index.yaml.minisig) and the registry is not marked              trusted_unsigned",
        )),
        SignatureState::NoKey => Err(verification_failed(
            "the index is signed but no public key is configured for this registry",
        )),
    }
}

/// Verifies the signature of `bytes` (the raw `index.yaml`) and then parses it (step 1 of
/// the pipeline). The signature is checked before the content is interpreted.
pub fn verify_index(
    bytes: Vec<u8>,
    signature: Option<Vec<u8>>,
    options: &VerifyOptions,
) -> Result<VerifiedIndex> {
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        return Err(invalid("index.yaml", "file is too large"));
    }
    let verification = verify_bytes(&bytes, signature.as_deref(), options)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid("index.yaml", "not UTF-8"))?;
    let manifest =
        forge_manifest::load_str(text).map_err(|e| invalid("index.yaml", e.to_string()))?;
    let Manifest::Registry(index) = manifest else {
        return Err(invalid("index.yaml", "kind is not Registry"));
    };
    let issues = validate_with(
        &Manifest::Registry(index.clone()),
        &ValidateOptions::default(),
    );
    if let Some(issue) = issues.first() {
        return Err(invalid("index.yaml", issue));
    }
    if index.spec.tools.len() + index.spec.stacks.len() > MAX_ENTRIES {
        return Err(invalid("index.yaml", "too many entries"));
    }
    Ok(VerifiedIndex {
        bytes,
        signature,
        index,
        verification,
    })
}

/// Checks a path taken from the index before it is joined to a folder or a URL: relative,
/// `/` separators, inside `tools/` or `stacks/` (as `section` says), `.yaml`, no `..`.
pub fn validate_entry_path(section: Section, path: &str) -> Result<()> {
    let prefix = match section {
        Section::Tools => "tools/",
        Section::Stacks => "stacks/",
    };
    let bad = |why: &str| invalid(&format!("index path `{path}`"), why);
    if path.len() > 200 {
        return Err(bad("too long"));
    }
    let Some(rest) = path.strip_prefix(prefix) else {
        return Err(bad(&format!("must start with `{prefix}`")));
    };
    if !rest.ends_with(".yaml") {
        return Err(bad("must end with `.yaml`"));
    }
    for part in rest.split('/') {
        let ok = !part.is_empty()
            && part != "."
            && part != ".."
            && !part.ends_with('.')
            && !part.contains(['\\', ':', '\0', '?', '*', '"', '<', '>', '|'])
            && !part.chars().any(char::is_control);
        if !ok {
            return Err(bad("contains a forbidden component"));
        }
    }
    Ok(())
}

/// Every `(section, entry)` of the index, in index order, with paths validated.
pub fn entries(
    index: &RegistryManifest,
) -> Result<Vec<(Section, &forge_manifest::schema::registry::RegistryEntry)>> {
    let mut out = Vec::new();
    for (section, list) in [
        (Section::Tools, &index.spec.tools),
        (Section::Stacks, &index.spec.stacks),
    ] {
        for entry in list {
            validate_entry_path(section, &entry.path)?;
            out.push((section, entry));
        }
    }
    Ok(out)
}

fn read_limited(path: &Path, max: u64) -> Result<Vec<u8>> {
    let io = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::NotFound => ForgeError::NotFound(format!("{}", path.display())),
        _ => ForgeError::Io {
            context: format!("reading {}", path.display()),
            source: e,
        },
    };
    let meta = std::fs::metadata(path).map_err(io)?;
    if meta.len() > max {
        return Err(invalid(&path.display().to_string(), "file is too large"));
    }
    std::fs::read(path).map_err(io)
}

/// Reads and verifies a registry laid out in a folder: a `Dir` source, a git checkout or
/// the cache. Blocking.
pub fn read_tree(
    root: &Path,
    options: &LoadOptions,
) -> Result<(VerifiedIndex, BTreeMap<String, Vec<u8>>)> {
    let bytes = read_limited(&root.join(INDEX_FILE), MAX_INDEX_BYTES)?;
    let signature = match read_limited(&root.join(SIGNATURE_FILE), MAX_SIGNATURE_BYTES) {
        Ok(signature) => Some(signature),
        Err(ForgeError::NotFound(_)) => None,
        Err(e) => return Err(e),
    };
    let verified = verify_index(bytes, signature, &options.verify)?;
    let mut files = BTreeMap::new();
    for (_, entry) in entries(&verified.index)? {
        let bytes = read_limited(&root.join(&entry.path), MAX_MANIFEST_BYTES)?;
        files.insert(entry.path.clone(), bytes);
    }
    Ok((verified, files))
}

/// Step 3 of the pipeline. `files` maps every index path to the bytes read from the source.
///
/// Fails when a file is missing or its sha256 differs from the index. Manifests that do
/// not parse, do not validate, disagree with their index entry or break the third-party
/// policy are returned in `skipped`.
pub fn build_loaded(
    verified: VerifiedIndex,
    mut files: BTreeMap<String, Vec<u8>>,
    options: &LoadOptions,
) -> Result<LoadedRegistry> {
    let VerifiedIndex {
        bytes: index_bytes,
        signature: signature_bytes,
        index,
        verification,
    } = verified;
    let validate_options = ValidateOptions {
        third_party: !options.official,
        ..ValidateOptions::default()
    };
    let mut manifests = Vec::new();
    let mut skipped = Vec::new();
    let mut verified_files = BTreeMap::new();
    for (section, entry) in entries(&index)? {
        let bytes = files.remove(&entry.path).ok_or_else(|| {
            ForgeError::NotFound(format!("manifest {} listed in the index", entry.path))
        })?;
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(entry.sha256.as_str()) {
            return Err(verification_failed(format!(
                "sha256 of {} is {actual}, the index says {}",
                entry.path,
                entry.sha256.as_str()
            )));
        }
        let skip = |reason: String| SkippedEntry {
            id: entry.id.to_string(),
            path: entry.path.clone(),
            reason,
        };
        let parsed = std::str::from_utf8(&bytes)
            .map_err(|_| "not UTF-8".to_owned())
            .and_then(|text| forge_manifest::load_str(text).map_err(|e| e.to_string()));
        verified_files.insert(entry.path.clone(), bytes);
        let manifest = match parsed {
            Ok(manifest) => manifest,
            Err(reason) => {
                skipped.push(skip(reason));
                continue;
            }
        };
        let kind_ok = match section {
            Section::Tools => matches!(manifest.kind(), Kind::Tool | Kind::Extension),
            Section::Stacks => manifest.kind() == Kind::Stack,
        };
        if !kind_ok {
            skipped.push(skip(format!(
                "kind {} does not belong in this section",
                manifest.kind()
            )));
            continue;
        }
        if manifest.id() != Some(&entry.id) {
            skipped.push(skip("the manifest id differs from the index".to_owned()));
            continue;
        }
        let issues = validate_with(&manifest, &validate_options);
        if !issues.is_empty() {
            let reason = issues
                .iter()
                .take(3)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            skipped.push(skip(reason));
            continue;
        }
        manifests.push(LoadedManifest {
            section,
            id: entry.id.clone(),
            path: entry.path.clone(),
            sha256: entry.sha256.as_str().to_owned(),
            latest: entry.latest.clone(),
            manifest: Arc::new(manifest),
        });
    }
    let index_sha256 = sha256_hex(&index_bytes);
    Ok(LoadedRegistry {
        index,
        index_bytes,
        signature_bytes,
        index_sha256,
        verification,
        files: verified_files,
        manifests,
        skipped,
    })
}

/// [`read_tree`] + [`build_loaded`]. Blocking.
pub fn load_tree(root: &Path, options: &LoadOptions) -> Result<LoadedRegistry> {
    let (verified, files) = read_tree(root, options)?;
    build_loaded(verified, files, options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_public_key_has_a_key_id() {
        let id = key_id_of_public_key(DEV_PUBLIC_KEY).unwrap();
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(DEV_PUBLIC_KEY.contains("untrusted comment"));
    }

    /// Golden test (ADR-0018): the signed bytes are exactly the bytes of the committed
    /// fixture, and the committed detached signature verifies over them with the committed
    /// development key. It fails if a dependency update changes what verification means,
    /// or if the fixture is regenerated without re-signing.
    #[test]
    fn golden_fixture_signature_verifies_over_the_raw_bytes() {
        const GOLDEN_SHA256: &str =
            "66a096e1852e9271dc4b9308a8382612147d070e3929ba8731bc08e682895137";
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/registry");
        let bytes = std::fs::read(dir.join(INDEX_FILE)).unwrap();
        let signature = std::fs::read(dir.join(SIGNATURE_FILE)).unwrap();
        assert_eq!(
            sha256_hex(&bytes),
            GOLDEN_SHA256,
            "index.yaml of the fixture changed: re-sign it and update the golden hash"
        );
        let options = VerifyOptions {
            public_key: Some(DEV_PUBLIC_KEY.to_owned()),
            trusted_unsigned: false,
        };
        let verified = verify_bytes(&bytes, Some(&signature), &options).unwrap();
        assert_eq!(
            verified,
            Verification::Verified {
                key_id: "6F03D19A26B6D573".to_owned()
            }
        );

        // There is no canonical form: any change to the bytes breaks the signature, even one
        // that keeps the YAML meaning (here: CRLF line endings, a re-emitter's typical edit).
        let crlf = String::from_utf8(bytes.clone())
            .unwrap()
            .replace("\n", "\r\n");
        assert!(verify_bytes(crlf.as_bytes(), Some(&signature), &options).is_err());
        let mut flipped = bytes;
        let last = flipped.len() - 1;
        flipped[last] ^= 1;
        assert!(verify_bytes(&flipped, Some(&signature), &options).is_err());
    }

    #[test]
    fn a_detached_signature_needs_a_key_and_an_absent_one_needs_trust() {
        let bytes = b"anything";
        let strict = VerifyOptions::default();
        assert!(verify_bytes(bytes, None, &strict).is_err());
        assert!(verify_bytes(bytes, Some(b"x"), &strict).is_err());
        let trusted = VerifyOptions {
            public_key: None,
            trusted_unsigned: true,
        };
        assert_eq!(
            verify_bytes(bytes, None, &trusted).unwrap(),
            Verification::Unverified
        );
        // A present but unreadable signature is refused even for a trusted registry.
        let keyed = VerifyOptions {
            public_key: Some(DEV_PUBLIC_KEY.to_owned()),
            trusted_unsigned: true,
        };
        assert!(verify_bytes(bytes, Some(b"garbage"), &keyed).is_err());
    }

    #[test]
    fn entry_paths_are_confined() {
        assert!(validate_entry_path(Section::Tools, "tools/php.yaml").is_ok());
        assert!(validate_entry_path(Section::Stacks, "stacks/laravel.yaml").is_ok());
        for bad in [
            "tools/../etc/x.yaml",
            "/tools/x.yaml",
            "tools/x.yml",
            "stacks/x.yaml",
            "tools\\x.yaml",
            "tools/C:x.yaml",
            "tools//x.yaml",
            "tools/./x.yaml",
            "../tools/x.yaml",
            "x.yaml",
        ] {
            assert!(
                validate_entry_path(Section::Tools, bad).is_err(),
                "{bad} must be refused"
            );
        }
        assert!(validate_entry_path(Section::Tools, "tools/ok/..yaml").is_ok());
    }
}
