//! [`TrustStore`] for Windows (docs/06 §6.1, docs/11 §3, ADR-0023).
//!
//! * Stores are opened with `CERT_STORE_PROV_SYSTEM_REGISTRY_W`: only the registry part of a
//!   system store. For `CurrentUser\Root` that is the user's own roots
//!   (`HKCU\Software\Microsoft\SystemCertificates\Root`), without the machine roots the
//!   logical store also lists, so "trusted for this user" means "we can remove it". Writes
//!   to the user's `Root` still go through the protected root list: Windows shows its
//!   confirmation dialog (`CertAddEncodedCertificateToStore` /
//!   `CertDeleteCertificateFromStore` block until the user answers; "No" is
//!   `ERROR_CANCELLED` → `cancelled`).
//! * Certificates are found by SHA-1 thumbprint (`CERT_FIND_SHA1_HASH`).
//! * `LocalMachine\Root` is read without elevation. Changes go through `forgebox-helper`
//!   (`trust.add_machine_root` / `trust.remove_machine_root`, ADR-0022), except in
//!   [`WindowsTrustStore::elevated`], meant for the helper itself, which writes the store
//!   directly (through `PrivilegedOps`, ADR-0024; [`machine_root_der`] gives it the
//!   certificate it is asked to remove, to check it is a Forgebox CA first).
//! * [`TrustStore::verify_server`] builds the chain with a private chain engine whose
//!   **exclusive** root store holds only the given anchor (`hExclusiveRoot`, Windows 7+),
//!   then applies the SSL policy (`CERT_CHAIN_POLICY_SSL`) with the server name: the same
//!   checks Schannel makes for `curl` or Chrome, name constraints included, without
//!   touching any store. Issuer certificates are never fetched (`CERT_CHAIN_DISABLE_AIA`);
//!   without `check_revocation` nothing is fetched at all (cache-only URL retrieval), with
//!   it the leaf's CRL is (`CERT_CHAIN_REVOCATION_CHECK_END_CERT`, what Schannel does for
//!   `curl.exe`).
//!
//! Everything here is blocking (and `add_root`/`remove_root` on `CurrentUser\Root` wait
//! for a human): callers use `spawn_blocking` with a timeout.

use std::ffi::c_void;

use ::windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_CANCELLED, ERROR_FILE_NOT_FOUND};
use ::windows::Win32::Security::Cryptography::{
    AUTHTYPE_SERVER, CERT_CHAIN_CACHE_ONLY_URL_RETRIEVAL, CERT_CHAIN_CONTEXT,
    CERT_CHAIN_DISABLE_AIA, CERT_CHAIN_ENGINE_CONFIG, CERT_CHAIN_PARA, CERT_CHAIN_POLICY_PARA,
    CERT_CHAIN_POLICY_SSL, CERT_CHAIN_POLICY_STATUS, CERT_CHAIN_REVOCATION_CHECK_END_CERT,
    CERT_CONTEXT, CERT_FIND_SHA1_HASH, CERT_OPEN_STORE_FLAGS, CERT_STORE_ADD_USE_EXISTING,
    CERT_STORE_DELETE_FLAG, CERT_STORE_OPEN_EXISTING_FLAG, CERT_STORE_PROV_MEMORY,
    CERT_STORE_PROV_SYSTEM_REGISTRY_W, CERT_STORE_READONLY_FLAG, CERT_SYSTEM_STORE_CURRENT_USER,
    CERT_SYSTEM_STORE_LOCAL_MACHINE, CERT_USAGE_MATCH, CRYPT_INTEGER_BLOB, CTL_USAGE,
    CertAddEncodedCertificateToStore, CertCloseStore, CertCreateCertificateChainEngine,
    CertCreateCertificateContext, CertDeleteCertificateFromStore, CertFindCertificateInStore,
    CertFreeCertificateChain, CertFreeCertificateChainEngine, CertFreeCertificateContext,
    CertGetCertificateChain, CertOpenStore, CertUnregisterSystemStore,
    CertVerifyCertificateChainPolicy, HCERTCHAINENGINE, HCERTSTORE, HTTPSPolicyCallbackData,
    HTTPSPolicyCallbackData_0, PKCS_7_ASN_ENCODING, USAGE_MATCH_TYPE_AND, X509_ASN_ENCODING,
    szOID_PKIX_KP_SERVER_AUTH,
};
use ::windows::core::{HSTRING, PSTR, PWSTR};

use super::process::{os_error, win32_code};
use crate::error::{ForgeError, Result};
use crate::helper::{HelperClient, HelperOp};
use crate::platform::{CertThumbprint, TrustScope, TrustStore};

/// Name of the root system store.
const ROOT_STORE: &str = "Root";
/// Budget of one CRL fetch when revocation is checked.
const URL_TIMEOUT_MS: u32 = 5_000;

/// How `LocalMachine` changes are made.
#[derive(Debug, Clone)]
enum MachineAccess {
    /// Through `forgebox-helper` (the daemon); `None`: refused with `needs_elevation`.
    Helper(Option<HelperClient>),
    /// Directly (the elevated helper itself).
    Direct,
}

/// [`TrustStore`] backed by the CryptoAPI system stores.
#[derive(Debug, Clone)]
pub struct WindowsTrustStore {
    machine: MachineAccess,
    /// System store of `CurrentUser` used for [`TrustScope::CurrentUser`]: `Root`, or a
    /// scratch store in tests (no protected root list, so no dialog).
    user_store: String,
}

impl WindowsTrustStore {
    /// The daemon's store: `LocalMachine` changes through `helper`.
    pub fn new(helper: Option<HelperClient>) -> Self {
        Self {
            machine: MachineAccess::Helper(helper),
            user_store: ROOT_STORE.to_owned(),
        }
    }

    /// The elevated helper's store: `LocalMachine\Root` is written directly (needs an
    /// administrator token; `needs_elevation` otherwise).
    pub fn elevated() -> Self {
        Self {
            machine: MachineAccess::Direct,
            user_store: ROOT_STORE.to_owned(),
        }
    }

    /// Uses the `CurrentUser` system store `name` instead of `Root` for
    /// [`TrustScope::CurrentUser`]. Any other name is an ordinary registry store without
    /// the protected root list: no dialog, nothing trusted. Tests use it to exercise the
    /// same calls; remove the store afterwards with [`WindowsTrustStore::delete_user_store`].
    pub fn with_user_store(mut self, name: impl Into<String>) -> Self {
        self.user_store = name.into();
        self
    }

    /// Deletes the `CurrentUser` system store `name` (certificates and the store itself,
    /// `CertUnregisterSystemStore` with `CERT_STORE_DELETE_FLAG`; opening with that flag
    /// only empties it). Refuses `Root`. Missing store is success.
    pub fn delete_user_store(name: &str) -> Result<()> {
        if name.eq_ignore_ascii_case(ROOT_STORE) {
            return Err(ForgeError::Forbidden(
                "the Root store is never deleted".to_owned(),
            ));
        }
        let wide = HSTRING::from(name);
        // SAFETY: `wide` is a NUL-terminated store name alive for the call; with
        // CERT_SYSTEM_STORE_CURRENT_USER the parameter is that name.
        let deleted = unsafe {
            CertUnregisterSystemStore(
                wide.as_ptr().cast(),
                CERT_SYSTEM_STORE_CURRENT_USER | CERT_STORE_DELETE_FLAG.0,
            )
        };
        if deleted.as_bool() {
            return Ok(());
        }
        let err = ::windows::core::Error::from_thread();
        if err.code().is_ok() || win32_code(&err) == Some(ERROR_FILE_NOT_FOUND.0) {
            Ok(())
        } else {
            Err(cert_error(&format!("deleting the store {name}"), &err))
        }
    }

    /// Whether the `CurrentUser` system store `name` exists (tests).
    pub fn user_store_exists(name: &str) -> Result<bool> {
        Ok(Store::open_registry(CERT_SYSTEM_STORE_CURRENT_USER, name, true, true)?.is_some())
    }

    /// `(location flag, store name)` of `scope`.
    fn location(&self, scope: TrustScope) -> (u32, &str) {
        match scope {
            TrustScope::CurrentUser => (CERT_SYSTEM_STORE_CURRENT_USER, &self.user_store),
            TrustScope::LocalMachine => (CERT_SYSTEM_STORE_LOCAL_MACHINE, ROOT_STORE),
        }
    }

    /// Adds `der` to the store of `scope` with this process's rights.
    fn add_direct(&self, der: &[u8], scope: TrustScope) -> Result<()> {
        let (location, name) = self.location(scope);
        let store = Store::open_registry(location, name, false, false)?
            .ok_or_else(|| ForgeError::NotFound(format!("certificate store {name}")))?;
        // SAFETY: valid store handle and DER buffer; no context is returned.
        unsafe {
            CertAddEncodedCertificateToStore(
                Some(store.0),
                X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                der,
                CERT_STORE_ADD_USE_EXISTING,
                None,
            )
        }
        .map_err(|e| cert_error(&format!("adding a root certificate to {scope} {name}"), &e))
    }

    /// Removes the certificate `thumbprint` from the store of `scope` with this
    /// process's rights.
    fn remove_direct(&self, thumbprint: &CertThumbprint, scope: TrustScope) -> Result<bool> {
        let (location, name) = self.location(scope);
        let Some(store) = Store::open_registry(location, name, false, true)? else {
            return Ok(false);
        };
        let Some(context) = store.find(thumbprint) else {
            return Ok(false);
        };
        // CertDeleteCertificateFromStore always frees the context, even on failure.
        let raw = context.into_raw();
        // SAFETY: `raw` is a context found in `store`, still open; ownership passes to the
        // call.
        unsafe { CertDeleteCertificateFromStore(raw) }.map_err(|e| {
            cert_error(
                &format!("removing the root certificate {thumbprint} from {scope} {name}"),
                &e,
            )
        })?;
        Ok(true)
    }

    fn helper(&self, op: &str) -> Result<&HelperClient> {
        match &self.machine {
            MachineAccess::Helper(Some(helper)) => Ok(helper),
            _ => Err(ForgeError::NeedsElevation(format!(
                "{op}: LocalMachine\\Root changes need forgebox-helper"
            ))),
        }
    }
}

impl TrustStore for WindowsTrustStore {
    fn is_trusted(&self, thumbprint: &CertThumbprint, scope: TrustScope) -> Result<bool> {
        let (location, name) = self.location(scope);
        Ok(Store::open_registry(location, name, true, true)?
            .and_then(|store| store.find(thumbprint))
            .is_some())
    }

    fn add_root(&self, cert_der: &[u8], scope: TrustScope) -> Result<()> {
        let thumbprint = CertThumbprint::of_der(cert_der);
        if self.is_trusted(&thumbprint, scope)? {
            return Ok(());
        }
        match (scope, &self.machine) {
            (TrustScope::CurrentUser, _) | (TrustScope::LocalMachine, MachineAccess::Direct) => {
                self.add_direct(cert_der, scope)
            }
            (TrustScope::LocalMachine, MachineAccess::Helper(_)) => {
                let op = HelperOp::TrustAddMachineRoot {
                    thumbprint: thumbprint.to_hex(),
                };
                self.helper(op.name())?.run(op).map(|_| ())
            }
        }
    }

    fn remove_root(&self, thumbprint: &CertThumbprint, scope: TrustScope) -> Result<bool> {
        match (scope, &self.machine) {
            (TrustScope::CurrentUser, _) | (TrustScope::LocalMachine, MachineAccess::Direct) => {
                self.remove_direct(thumbprint, scope)
            }
            (TrustScope::LocalMachine, MachineAccess::Helper(_)) => {
                if !self.is_trusted(thumbprint, scope)? {
                    return Ok(false);
                }
                let op = HelperOp::TrustRemoveMachineRoot {
                    thumbprint: thumbprint.to_hex(),
                };
                self.helper(op.name())?.run(op).map(|_| true)
            }
        }
    }

    fn verify_server(
        &self,
        chain: &[Vec<u8>],
        anchor_der: &[u8],
        host: &str,
        check_revocation: bool,
    ) -> Result<()> {
        let Some((leaf, intermediates)) = chain.split_first() else {
            return Err(ForgeError::InvalidInput(
                "empty certificate chain".to_owned(),
            ));
        };
        let roots = Store::memory()?;
        roots.add(anchor_der, "the trust anchor")?;
        let extra = Store::memory()?;
        for der in intermediates {
            extra.add(der, "an intermediate certificate")?;
        }
        // Without revocation nothing may be fetched; with it, only the CRL of the leaf
        // (what Schannel checks for `curl.exe`), never issuer certificates (AIA).
        let fetch = if check_revocation {
            CERT_CHAIN_REVOCATION_CHECK_END_CERT
        } else {
            CERT_CHAIN_CACHE_ONLY_URL_RETRIEVAL
        };
        let engine = ChainEngine::exclusive(&roots, fetch & CERT_CHAIN_CACHE_ONLY_URL_RETRIEVAL)?;
        let leaf = CertContext::from_der(leaf)?;

        let mut usage = [PSTR(szOID_PKIX_KP_SERVER_AUTH.0.cast_mut())];
        let para = CERT_CHAIN_PARA {
            cbSize: size_of::<CERT_CHAIN_PARA>() as u32,
            RequestedUsage: CERT_USAGE_MATCH {
                dwType: USAGE_MATCH_TYPE_AND,
                Usage: CTL_USAGE {
                    cUsageIdentifier: 1,
                    rgpszUsageIdentifier: usage.as_mut_ptr(),
                },
            },
            dwUrlRetrievalTimeout: URL_TIMEOUT_MS,
            ..Default::default()
        };
        let mut chain_ctx: *mut CERT_CHAIN_CONTEXT = std::ptr::null_mut();
        // SAFETY: engine, leaf context, extra store and parameters are valid for the call;
        // `chain_ctx` receives a chain freed by `ChainContext`.
        unsafe {
            CertGetCertificateChain(
                Some(engine.0),
                leaf.0,
                None,
                Some(extra.0),
                &para,
                fetch | CERT_CHAIN_DISABLE_AIA,
                None,
                &mut chain_ctx,
            )
        }
        .map_err(|e| cert_error("building the certificate chain", &e))?;
        let chain_ctx = ChainContext(chain_ctx);
        // SAFETY: a successful call returned a valid chain context.
        let chain_status = unsafe { (*chain_ctx.0).TrustStatus.dwErrorStatus };

        let mut server_name: Vec<u16> = host.encode_utf16().chain(std::iter::once(0)).collect();
        let mut https = HTTPSPolicyCallbackData {
            Anonymous: HTTPSPolicyCallbackData_0 {
                cbStruct: size_of::<HTTPSPolicyCallbackData>() as u32,
            },
            dwAuthType: AUTHTYPE_SERVER,
            fdwChecks: 0,
            pwszServerName: PWSTR(server_name.as_mut_ptr()),
        };
        let policy = CERT_CHAIN_POLICY_PARA {
            cbSize: size_of::<CERT_CHAIN_POLICY_PARA>() as u32,
            pvExtraPolicyPara: (&raw mut https).cast::<c_void>(),
            ..Default::default()
        };
        let mut status = CERT_CHAIN_POLICY_STATUS {
            cbSize: size_of::<CERT_CHAIN_POLICY_STATUS>() as u32,
            ..Default::default()
        };
        // SAFETY: the chain context, policy parameters (with the server name buffer) and
        // status are valid for the call.
        let ok = unsafe {
            CertVerifyCertificateChainPolicy(
                CERT_CHAIN_POLICY_SSL,
                chain_ctx.0,
                &policy,
                &mut status,
            )
        };
        if !ok.as_bool() {
            return Err(ForgeError::Internal(
                "CertVerifyCertificateChainPolicy could not check the chain".to_owned(),
            ));
        }
        if status.dwError == 0 && chain_status == 0 {
            return Ok(());
        }
        Err(ForgeError::VerificationFailed(format!(
            "{host}: {} (0x{:08X}); chain: {}",
            policy_error_name(status.dwError),
            status.dwError,
            chain_flags(chain_status)
        )))
    }
}

/// DER of the certificate with SHA-1 `thumbprint` in the registry part of
/// `LocalMachine\Root` (what [`WindowsTrustStore`] adds and removes), if any. No elevation.
pub(crate) fn machine_root_der(thumbprint: &CertThumbprint) -> Result<Option<Vec<u8>>> {
    let Some(store) =
        Store::open_registry(CERT_SYSTEM_STORE_LOCAL_MACHINE, ROOT_STORE, true, true)?
    else {
        return Ok(None);
    };
    Ok(store.find(thumbprint).map(|context| context.der()))
}

/// An open certificate store, closed on drop.
struct Store(HCERTSTORE);

impl Store {
    /// The registry part of the system store `name` at `location`. `Ok(None)` when
    /// `existing` is set and the store does not exist.
    fn open_registry(
        location: u32,
        name: &str,
        read_only: bool,
        existing: bool,
    ) -> Result<Option<Self>> {
        let mut flags = location;
        if read_only {
            flags |= CERT_STORE_READONLY_FLAG.0;
        }
        if existing {
            flags |= CERT_STORE_OPEN_EXISTING_FLAG.0;
        }
        let wide = HSTRING::from(name);
        // SAFETY: `wide` is a NUL-terminated store name alive for the call.
        match unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_REGISTRY_W,
                Default::default(),
                None,
                CERT_OPEN_STORE_FLAGS(flags),
                Some(wide.as_ptr().cast()),
            )
        } {
            Ok(store) => Ok(Some(Self(store))),
            Err(err) if existing && win32_code(&err) == Some(ERROR_FILE_NOT_FOUND.0) => Ok(None),
            Err(err) => Err(cert_error(
                &format!("opening the certificate store {name}"),
                &err,
            )),
        }
    }

    /// An empty in-memory store.
    fn memory() -> Result<Self> {
        // SAFETY: the memory provider takes no parameter.
        let store = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_MEMORY,
                Default::default(),
                None,
                CERT_OPEN_STORE_FLAGS(0),
                None,
            )
        }
        .map_err(|e| cert_error("opening a memory certificate store", &e))?;
        Ok(Self(store))
    }

    fn add(&self, der: &[u8], what: &str) -> Result<()> {
        // SAFETY: valid store handle and DER buffer; no context is returned.
        unsafe {
            CertAddEncodedCertificateToStore(
                Some(self.0),
                X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                der,
                CERT_STORE_ADD_USE_EXISTING,
                None,
            )
        }
        .map_err(|e| cert_error(&format!("loading {what}"), &e))
    }

    /// The certificate with SHA-1 `thumbprint`, if any.
    fn find(&self, thumbprint: &CertThumbprint) -> Option<CertContext> {
        let mut hash = *thumbprint.as_bytes();
        let blob = CRYPT_INTEGER_BLOB {
            cbData: hash.len() as u32,
            pbData: hash.as_mut_ptr(),
        };
        // SAFETY: valid store; `blob` points to the 20 hash bytes alive for the call. The
        // returned context (if any) is owned by `CertContext`.
        let found = unsafe {
            CertFindCertificateInStore(
                self.0,
                X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                0,
                CERT_FIND_SHA1_HASH,
                Some((&raw const blob).cast()),
                None,
            )
        };
        (!found.is_null()).then(|| CertContext(found))
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // SAFETY: the handle came from CertOpenStore and is closed once.
        unsafe {
            let _ = CertCloseStore(Some(self.0), 0);
        }
    }
}

/// A certificate context, freed on drop.
struct CertContext(*const CERT_CONTEXT);

impl CertContext {
    fn from_der(der: &[u8]) -> Result<Self> {
        // SAFETY: `der` is a valid buffer; the context copies it.
        let ctx =
            unsafe { CertCreateCertificateContext(X509_ASN_ENCODING | PKCS_7_ASN_ENCODING, der) };
        if ctx.is_null() {
            return Err(ForgeError::InvalidInput(format!(
                "not a DER certificate: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(Self(ctx))
    }

    /// A copy of the DER encoding.
    fn der(&self) -> Vec<u8> {
        // SAFETY: a context owned by `self`; its encoded bytes live as long as it does.
        unsafe {
            let ctx = &*self.0;
            std::slice::from_raw_parts(ctx.pbCertEncoded, ctx.cbCertEncoded as usize).to_vec()
        }
    }

    /// Gives up ownership (for calls that free the context themselves).
    fn into_raw(self) -> *const CERT_CONTEXT {
        let raw = self.0;
        std::mem::forget(self);
        raw
    }
}

impl Drop for CertContext {
    fn drop(&mut self) {
        // SAFETY: the context came from a CryptoAPI call that transfers ownership and is
        // freed once.
        unsafe {
            let _ = CertFreeCertificateContext(Some(self.0));
        }
    }
}

/// A chain engine trusting only the certificates of one store.
struct ChainEngine(HCERTCHAINENGINE);

impl ChainEngine {
    /// `flags`: extra engine flags (`CERT_CHAIN_CACHE_ONLY_URL_RETRIEVAL` or none).
    fn exclusive(roots: &Store, flags: u32) -> Result<Self> {
        let config = CERT_CHAIN_ENGINE_CONFIG {
            cbSize: size_of::<CERT_CHAIN_ENGINE_CONFIG>() as u32,
            dwFlags: flags | CERT_CHAIN_DISABLE_AIA,
            dwUrlRetrievalTimeout: URL_TIMEOUT_MS,
            hExclusiveRoot: roots.0,
            ..Default::default()
        };
        let mut engine = HCERTCHAINENGINE::default();
        // SAFETY: `config` is fully initialized and `roots` outlives the engine (the
        // engine duplicates the store handle).
        unsafe { CertCreateCertificateChainEngine(&config, &mut engine) }
            .map_err(|e| cert_error("creating a certificate chain engine", &e))?;
        Ok(Self(engine))
    }
}

impl Drop for ChainEngine {
    fn drop(&mut self) {
        // SAFETY: the engine came from CertCreateCertificateChainEngine, freed once.
        unsafe { CertFreeCertificateChainEngine(Some(self.0)) }
    }
}

/// A chain context, freed on drop.
struct ChainContext(*mut CERT_CHAIN_CONTEXT);

impl Drop for ChainContext {
    fn drop(&mut self) {
        // SAFETY: the context came from CertGetCertificateChain, freed once.
        unsafe { CertFreeCertificateChain(self.0) }
    }
}

/// Maps a CryptoAPI failure: a declined confirmation dialog is `cancelled`, access denied
/// is `needs_elevation`, the rest is I/O.
fn cert_error(context: &str, err: &::windows::core::Error) -> ForgeError {
    match win32_code(err) {
        Some(code) if code == ERROR_CANCELLED.0 => ForgeError::Cancelled(format!(
            "{context}: the user declined the Windows confirmation dialog"
        )),
        Some(code) if code == ERROR_ACCESS_DENIED.0 => {
            ForgeError::NeedsElevation(format!("{context}: access denied"))
        }
        _ => ForgeError::io(context, os_error(err)),
    }
}

/// Name of a `CertVerifyCertificateChainPolicy` result.
fn policy_error_name(code: u32) -> &'static str {
    match code {
        0 => "OK",
        0x800B_0101 => "CERT_E_EXPIRED",
        0x800B_0109 => "CERT_E_UNTRUSTEDROOT",
        0x800B_010A => "CERT_E_CHAINING",
        0x800B_010C => "CERT_E_REVOKED",
        0x800B_010F => "CERT_E_CN_NO_MATCH",
        0x800B_0110 => "CERT_E_WRONG_USAGE",
        0x800B_0111 => "TRUST_E_EXPLICIT_DISTRUST",
        0x800B_0112 => "CERT_E_UNTRUSTEDCA",
        0x800B_0113 => "CERT_E_INVALID_POLICY",
        0x800B_0114 => "CERT_E_INVALID_NAME",
        0x8009_6004 => "TRUST_E_CERT_SIGNATURE",
        0x8009_6010 => "TRUST_E_BAD_DIGEST",
        0x800B_0100 => "TRUST_E_NOSIGNATURE",
        0x8009_2012 => "CRYPT_E_NO_REVOCATION_CHECK",
        0x8009_2013 => "CRYPT_E_REVOCATION_OFFLINE",
        0x8009_2010 => "CRYPT_E_REVOKED",
        _ => "certificate chain policy error",
    }
}

/// Names of the `CERT_TRUST_*` error bits of a chain (`none` when clean).
fn chain_flags(status: u32) -> String {
    const FLAGS: [(u32, &str); 16] = [
        (0x0000_0001, "IS_NOT_TIME_VALID"),
        (0x0000_0004, "IS_REVOKED"),
        (0x0000_0008, "IS_NOT_SIGNATURE_VALID"),
        (0x0000_0010, "IS_NOT_VALID_FOR_USAGE"),
        (0x0000_0020, "IS_UNTRUSTED_ROOT"),
        (0x0000_0040, "REVOCATION_STATUS_UNKNOWN"),
        (0x0000_0080, "IS_CYCLIC"),
        (0x0000_0100, "INVALID_EXTENSION"),
        (0x0000_0200, "INVALID_POLICY_CONSTRAINTS"),
        (0x0000_0400, "INVALID_BASIC_CONSTRAINTS"),
        (0x0000_0800, "INVALID_NAME_CONSTRAINTS"),
        (0x0000_1000, "HAS_NOT_SUPPORTED_NAME_CONSTRAINT"),
        (0x0000_2000, "HAS_NOT_DEFINED_NAME_CONSTRAINT"),
        (0x0000_4000, "HAS_NOT_PERMITTED_NAME_CONSTRAINT"),
        (0x0000_8000, "HAS_EXCLUDED_NAME_CONSTRAINT"),
        (0x0001_0000, "IS_PARTIAL_CHAIN"),
    ];
    if status == 0 {
        return "none".to_owned();
    }
    let mut names: Vec<String> = FLAGS
        .iter()
        .filter(|(bit, _)| status & bit != 0)
        .map(|(_, name)| format!("CERT_TRUST_{name}"))
        .collect();
    let known = FLAGS.iter().fold(0, |acc, (bit, _)| acc | bit);
    if status & !known != 0 {
        names.push(format!("0x{:08X}", status & !known));
    }
    names.join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_flags_are_named() {
        assert_eq!(chain_flags(0), "none");
        assert_eq!(
            chain_flags(0x0000_4000 | 0x0000_0020),
            "CERT_TRUST_IS_UNTRUSTED_ROOT | CERT_TRUST_HAS_NOT_PERMITTED_NAME_CONSTRAINT"
        );
        assert_eq!(chain_flags(0x0100_0000), "0x01000000");
        assert_eq!(policy_error_name(0x800B_010F), "CERT_E_CN_NO_MATCH");
    }

    #[test]
    fn the_root_store_is_never_deleted() {
        let err = WindowsTrustStore::delete_user_store("root").unwrap_err();
        assert_eq!(err.code(), "forbidden");
    }

    #[test]
    fn machine_changes_need_a_helper() {
        let store = WindowsTrustStore::new(None);
        // A thumbprint nobody has: is_trusted reads LocalMachine\Root without elevation.
        let nobody = CertThumbprint::from_bytes([0xA5; 20]);
        assert!(!store.is_trusted(&nobody, TrustScope::LocalMachine).unwrap());
        assert!(
            !store
                .remove_root(&nobody, TrustScope::LocalMachine)
                .unwrap()
        );
        let err = store
            .add_root(b"not a certificate", TrustScope::LocalMachine)
            .unwrap_err();
        assert_eq!(err.code(), "needs_elevation", "{err}");
    }

    #[test]
    fn machine_roots_are_read_without_elevation() {
        assert_eq!(
            machine_root_der(&CertThumbprint::from_bytes([0xA5; 20])).unwrap(),
            None
        );
        // "Microsoft Root Certificate Authority 2011" ships with every Windows 10/11.
        let microsoft: CertThumbprint = "8F43288AD272F3103B6FB1428485EA3014C0BCFE".parse().unwrap();
        let der = machine_root_der(&microsoft)
            .unwrap()
            .expect("Microsoft root");
        assert_eq!(CertThumbprint::of_der(&der), microsoft);
    }

    #[test]
    fn unknown_certificates_are_not_trusted_for_the_user() {
        let store = WindowsTrustStore::new(None);
        let nobody = CertThumbprint::from_bytes([0x5A; 20]);
        assert!(!store.is_trusted(&nobody, TrustScope::CurrentUser).unwrap());
        assert!(!store.remove_root(&nobody, TrustScope::CurrentUser).unwrap());
    }

    #[test]
    fn verify_server_rejects_garbage() {
        let store = WindowsTrustStore::new(None);
        let err = store
            .verify_server(&[], b"x", "loja.test", false)
            .unwrap_err();
        assert_eq!(err.code(), "invalid_input");
        let err = store
            .verify_server(
                &[b"not der".to_vec()],
                b"not der either",
                "loja.test",
                false,
            )
            .unwrap_err();
        assert!(matches!(err.code(), "io" | "invalid_input"), "{err}");
    }
}
