//! Authenticode verification (docs/05 §3 item 4, docs/11 §2).
//!
//! Trust decision: `WinVerifyTrust` with `WINTRUST_ACTION_GENERIC_VERIFY_V2`, first on the
//! file itself (embedded signature) and, when it has none, through the system catalogs
//! (`CryptCATAdmin*`), which is how most binaries shipped with Windows are signed
//! (`notepad.exe`, for example). Publisher: `CryptQueryObject` on the signed object (the
//! file or its catalog), signer certificate looked up by issuer + serial number, and
//! `CertGetNameStringW(CERT_NAME_SIMPLE_DISPLAY_TYPE)`.
//!
//! Policy: no UI, no revocation checks (`WTD_REVOKE_NONE`): verification must work offline
//! and never wait on CRL/OCSP servers; the file is pinned by its sha256 before this runs.
//! Chain building may still use Windows root auto-update. Everything here is blocking:
//! callers run it in `spawn_blocking`.

use std::ffi::c_void;
use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use ::windows::Win32::Foundation::{HANDLE, HWND, INVALID_HANDLE_VALUE};
use ::windows::Win32::Security::Cryptography::Catalog::{
    CATALOG_INFO, CryptCATAdminAcquireContext2, CryptCATAdminCalcHashFromFileHandle2,
    CryptCATAdminEnumCatalogFromHash, CryptCATAdminReleaseCatalogContext,
    CryptCATAdminReleaseContext, CryptCATCatalogInfoFromContext,
};
use ::windows::Win32::Security::Cryptography::{
    BCRYPT_SHA256_ALGORITHM, CERT_CONTEXT, CERT_FIND_SUBJECT_CERT, CERT_INFO,
    CERT_NAME_SIMPLE_DISPLAY_TYPE, CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED,
    CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED, CERT_QUERY_CONTENT_TYPE_FLAGS,
    CERT_QUERY_ENCODING_TYPE, CERT_QUERY_FORMAT_FLAG_BINARY, CERT_QUERY_OBJECT_FILE,
    CMSG_SIGNER_INFO, CMSG_SIGNER_INFO_PARAM, CertCloseStore, CertFindCertificateInStore,
    CertFreeCertificateContext, CertGetNameStringW, CryptMsgClose, CryptMsgGetParam,
    CryptQueryObject, HCERTSTORE, PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
};
use ::windows::Win32::Security::WinTrust::{
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_CATALOG_INFO, WINTRUST_DATA, WINTRUST_DATA_0,
    WINTRUST_DATA_UNION_CHOICE, WINTRUST_FILE_INFO, WTD_CHOICE_CATALOG, WTD_CHOICE_FILE,
    WTD_REVOCATION_CHECK_NONE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
    WTD_UI_NONE, WTD_UICONTEXT_EXECUTE, WinVerifyTrust,
};
use ::windows::core::PCWSTR;

use crate::error::{ForgeError, IoResultExt, Result};
use crate::platform::{CodeSign, CodeSignInfo, CodeSignKind};

/// `TRUST_E_NOSIGNATURE`: the subject has no signature of the requested kind.
const TRUST_E_NOSIGNATURE: u32 = 0x800B_0100;
/// `TRUST_E_SUBJECT_FORM_UNKNOWN`: not a format the SIPs know (not a PE, script...).
const TRUST_E_SUBJECT_FORM_UNKNOWN: u32 = 0x800B_0003;
/// `TRUST_E_PROVIDER_UNKNOWN`.
const TRUST_E_PROVIDER_UNKNOWN: u32 = 0x800B_0001;

/// Largest digest a catalog hash can have (SHA-512), in bytes.
const MAX_HASH_LEN: usize = 64;

/// [`CodeSign`] backed by `WinVerifyTrust` and the catalog database.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsCodeSign;

impl CodeSign for WindowsCodeSign {
    fn verify(&self, path: &Path) -> Result<CodeSignInfo> {
        let absolute = std::path::absolute(path).at("resolving", path)?;
        let meta = std::fs::metadata(&absolute).at("inspecting", path)?;
        if !meta.is_file() {
            return Err(ForgeError::InvalidInput(format!(
                "{} is not a file",
                path.display()
            )));
        }
        let mut file = File::open(&absolute).at("opening", path)?;
        let wide = wide_nul(&absolute);

        let embedded = verify_embedded(&wide, &file);
        if embedded == 0 {
            return Ok(CodeSignInfo {
                signed: true,
                trusted: true,
                publisher: signer_name(&wide, CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED),
                kind: Some(CodeSignKind::Embedded),
                status: None,
            });
        }
        if !is_unsigned(embedded) {
            // Signed, but broken or not trusted: still show who claims to have signed it.
            return Ok(CodeSignInfo {
                signed: true,
                trusted: false,
                publisher: signer_name(&wide, CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED),
                kind: Some(CodeSignKind::Embedded),
                status: Some(describe(embedded)),
            });
        }

        file.seek(SeekFrom::Start(0)).at("reading", path)?;
        if let Some((status, catalog)) = verify_by_catalog(&wide, &file) {
            return Ok(CodeSignInfo {
                signed: true,
                trusted: status == 0,
                publisher: signer_name(&catalog, CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED),
                kind: Some(CodeSignKind::Catalog),
                status: (status != 0).then(|| describe(status)),
            });
        }

        Ok(CodeSignInfo {
            signed: false,
            trusted: false,
            publisher: None,
            kind: None,
            status: Some(describe(embedded)),
        })
    }
}

/// `path` as a NUL-terminated UTF-16 string.
fn wide_nul(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn raw_handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

fn is_unsigned(status: i32) -> bool {
    matches!(
        status as u32,
        TRUST_E_NOSIGNATURE | TRUST_E_SUBJECT_FORM_UNKNOWN | TRUST_E_PROVIDER_UNKNOWN
    )
}

/// Human readable name of a `WinVerifyTrust` result.
fn describe(status: i32) -> String {
    let code = status as u32;
    let name = match code {
        TRUST_E_NOSIGNATURE => "TRUST_E_NOSIGNATURE",
        TRUST_E_SUBJECT_FORM_UNKNOWN => "TRUST_E_SUBJECT_FORM_UNKNOWN",
        TRUST_E_PROVIDER_UNKNOWN => "TRUST_E_PROVIDER_UNKNOWN",
        0x800B_0004 => "TRUST_E_SUBJECT_NOT_TRUSTED",
        0x800B_0101 => "CERT_E_EXPIRED",
        0x800B_0109 => "CERT_E_UNTRUSTEDROOT",
        0x800B_010A => "CERT_E_CHAINING",
        0x800B_010C => "CERT_E_REVOKED",
        0x800B_0111 => "TRUST_E_EXPLICIT_DISTRUST",
        0x8009_6004 => "TRUST_E_CERT_SIGNATURE",
        0x8009_6010 => "TRUST_E_BAD_DIGEST",
        0x8009_2026 => "CRYPT_E_SECURITY_SETTINGS",
        _ => "WinVerifyTrust error",
    };
    format!("{name} (0x{code:08X})")
}

/// Runs `WinVerifyTrust` (verify, then close the state data) on `union`.
///
/// # Safety
/// The pointers inside `union` must stay valid for the whole call.
unsafe fn win_verify_trust(choice: WINTRUST_DATA_UNION_CHOICE, union: WINTRUST_DATA_0) -> i32 {
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: choice,
        Anonymous: union,
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_REVOCATION_CHECK_NONE,
        dwUIContext: WTD_UICONTEXT_EXECUTE,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // INVALID_HANDLE_VALUE as the window means "no UI at all".
    let hwnd = HWND(INVALID_HANDLE_VALUE.0);
    // SAFETY: `data` and `action` live for both calls; the caller guarantees the pointers
    // in `union`; CLOSE releases the state data allocated by VERIFY.
    unsafe {
        let status = WinVerifyTrust(hwnd, &mut action, (&raw mut data).cast());
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(hwnd, &mut action, (&raw mut data).cast());
        status
    }
}

/// `WinVerifyTrust` on the embedded signature of `file`.
fn verify_embedded(wide: &[u16], file: &File) -> i32 {
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(wide.as_ptr()),
        hFile: raw_handle(file),
        pgKnownSubject: std::ptr::null_mut(),
    };
    // SAFETY: `info`, `wide` and `file` outlive the call.
    unsafe {
        win_verify_trust(
            WTD_CHOICE_FILE,
            WINTRUST_DATA_0 {
                pFile: &raw mut info,
            },
        )
    }
}

/// Catalog admin context, released on drop.
struct CatAdmin(isize);

impl Drop for CatAdmin {
    fn drop(&mut self) {
        // SAFETY: the handle came from CryptCATAdminAcquireContext2 and is released once.
        unsafe {
            let _ = CryptCATAdminReleaseContext(self.0, 0);
        }
    }
}

/// Catalog found for a hash, released on drop (before its admin context).
struct CatInfo<'a> {
    admin: &'a CatAdmin,
    handle: isize,
}

impl Drop for CatInfo<'_> {
    fn drop(&mut self) {
        // SAFETY: the handle came from CryptCATAdminEnumCatalogFromHash on `admin`.
        unsafe {
            let _ = CryptCATAdminReleaseCatalogContext(self.admin.0, self.handle, 0);
        }
    }
}

/// Looks `file` up in the system catalogs (SHA-256 catalogs, then legacy SHA-1 ones) and
/// verifies the catalog that lists it. Returns the `WinVerifyTrust` status and the
/// NUL-terminated path of the catalog, or `None` when no catalog lists the file.
fn verify_by_catalog(wide: &[u16], file: &File) -> Option<(i32, Vec<u16>)> {
    for algorithm in [BCRYPT_SHA256_ALGORITHM, PCWSTR::null()] {
        let mut admin = 0isize;
        // SAFETY: `admin` is a valid out pointer; the other arguments are optional.
        if unsafe { CryptCATAdminAcquireContext2(&mut admin, None, algorithm, None, None) }.is_err()
        {
            continue;
        }
        let admin = CatAdmin(admin);

        let mut hash = [0u8; MAX_HASH_LEN];
        let mut hash_len = hash.len() as u32;
        // SAFETY: `hash` has `hash_len` writable bytes; the file handle is valid.
        let hashed = unsafe {
            CryptCATAdminCalcHashFromFileHandle2(
                admin.0,
                raw_handle(file),
                &mut hash_len,
                Some(hash.as_mut_ptr()),
                None,
            )
        };
        if hashed.is_err() || hash_len as usize > hash.len() {
            continue;
        }
        let hash = &mut hash[..hash_len as usize];

        // SAFETY: `hash` is a valid slice; no previous catalog context is passed.
        let handle = unsafe { CryptCATAdminEnumCatalogFromHash(admin.0, hash, None, None) };
        if handle == 0 {
            continue;
        }
        let catalog = CatInfo {
            admin: &admin,
            handle,
        };
        let mut info = CATALOG_INFO {
            cbStruct: size_of::<CATALOG_INFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `info` is writable and sized as declared in cbStruct.
        if unsafe { CryptCATCatalogInfoFromContext(catalog.handle, &mut info, 0) }.is_err() {
            continue;
        }
        let len = info
            .wszCatalogFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info.wszCatalogFile.len());
        let mut catalog_path = info.wszCatalogFile[..len].to_vec();
        catalog_path.push(0);

        // Member tag: the file hash in upper-case hex, as written by makecat.
        let mut tag: Vec<u16> = hash
            .iter()
            .flat_map(|b| format!("{b:02X}").encode_utf16().collect::<Vec<_>>())
            .collect();
        tag.push(0);

        let mut catalog_info = WINTRUST_CATALOG_INFO {
            cbStruct: size_of::<WINTRUST_CATALOG_INFO>() as u32,
            dwCatalogVersion: 0,
            pcwszCatalogFilePath: PCWSTR(catalog_path.as_ptr()),
            pcwszMemberTag: PCWSTR(tag.as_ptr()),
            pcwszMemberFilePath: PCWSTR(wide.as_ptr()),
            hMemberFile: raw_handle(file),
            pbCalculatedFileHash: hash.as_mut_ptr(),
            cbCalculatedFileHash: hash_len,
            pcCatalogContext: std::ptr::null_mut(),
            hCatAdmin: admin.0,
        };
        // SAFETY: every pointer in `catalog_info` points into locals that outlive the call.
        let status = unsafe {
            win_verify_trust(
                WTD_CHOICE_CATALOG,
                WINTRUST_DATA_0 {
                    pCatalog: &raw mut catalog_info,
                },
            )
        };
        drop(catalog);
        return Some((status, catalog_path));
    }
    None
}

/// Store and message returned by `CryptQueryObject`, closed on drop.
struct QueriedObject {
    store: HCERTSTORE,
    msg: *mut c_void,
}

impl Drop for QueriedObject {
    fn drop(&mut self) {
        // SAFETY: both handles came from CryptQueryObject and are closed once.
        unsafe {
            if !self.msg.is_null() {
                let _ = CryptMsgClose(Some(self.msg));
            }
            if !self.store.is_invalid() {
                let _ = CertCloseStore(Some(self.store), 0);
            }
        }
    }
}

/// Certificate context, freed on drop.
struct CertContext(*mut CERT_CONTEXT);

impl Drop for CertContext {
    fn drop(&mut self) {
        // SAFETY: the context came from CertFindCertificateInStore and is freed once.
        unsafe {
            let _ = CertFreeCertificateContext(Some(self.0));
        }
    }
}

/// Display name of the first signer of the PKCS#7 object in the file `wide` (a signed PE
/// with `content = ..._PKCS7_SIGNED_EMBED`, a catalog with `..._PKCS7_SIGNED`).
fn signer_name(wide: &[u16], content: CERT_QUERY_CONTENT_TYPE_FLAGS) -> Option<String> {
    let mut object = QueriedObject {
        store: HCERTSTORE::default(),
        msg: std::ptr::null_mut(),
    };
    let mut encoding = CERT_QUERY_ENCODING_TYPE(0);
    // SAFETY: `wide` is NUL-terminated; the out pointers are valid locals.
    unsafe {
        CryptQueryObject(
            CERT_QUERY_OBJECT_FILE,
            wide.as_ptr().cast(),
            content,
            CERT_QUERY_FORMAT_FLAG_BINARY,
            0,
            Some(&mut encoding),
            None,
            None,
            Some(&mut object.store),
            Some(&mut object.msg),
            None,
        )
    }
    .ok()?;

    let mut size = 0u32;
    // SAFETY: size query with no buffer.
    unsafe { CryptMsgGetParam(object.msg, CMSG_SIGNER_INFO_PARAM, 0, None, &mut size) }.ok()?;
    // u64 elements keep the CMSG_SIGNER_INFO header aligned.
    let mut buffer = vec![0u64; (size as usize).div_ceil(size_of::<u64>())];
    // SAFETY: `buffer` holds at least `size` writable bytes.
    unsafe {
        CryptMsgGetParam(
            object.msg,
            CMSG_SIGNER_INFO_PARAM,
            0,
            Some(buffer.as_mut_ptr().cast()),
            &mut size,
        )
    }
    .ok()?;
    if (size as usize) < size_of::<CMSG_SIGNER_INFO>() {
        return None;
    }
    // SAFETY: the API wrote a CMSG_SIGNER_INFO (plus the data it points to) at the start
    // of the aligned buffer, which outlives `signer`.
    let signer = unsafe { &*buffer.as_ptr().cast::<CMSG_SIGNER_INFO>() };
    let wanted = CERT_INFO {
        Issuer: signer.Issuer,
        SerialNumber: signer.SerialNumber,
        ..Default::default()
    };
    // SAFETY: the store is open; `wanted` outlives the call.
    let cert = unsafe {
        CertFindCertificateInStore(
            object.store,
            X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
            0,
            CERT_FIND_SUBJECT_CERT,
            Some((&raw const wanted).cast()),
            None,
        )
    };
    if cert.is_null() {
        return None;
    }
    let cert = CertContext(cert);
    // SAFETY: valid certificate context; the first call asks for the length.
    let len = unsafe { CertGetNameStringW(cert.0, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, None) };
    if len <= 1 {
        return None;
    }
    let mut name = vec![0u16; len as usize];
    // SAFETY: `name` has `len` writable characters.
    let written = unsafe {
        CertGetNameStringW(
            cert.0,
            CERT_NAME_SIMPLE_DISPLAY_TYPE,
            0,
            None,
            Some(&mut name),
        )
    };
    let end = (written as usize).saturating_sub(1).min(name.len());
    let name = String::from_utf16_lossy(&name[..end]);
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_are_named() {
        assert_eq!(
            describe(0x800B_0109_u32 as i32),
            "CERT_E_UNTRUSTEDROOT (0x800B0109)"
        );
        assert_eq!(describe(0x1234_i32), "WinVerifyTrust error (0x00001234)");
        assert!(is_unsigned(TRUST_E_NOSIGNATURE as i32));
        assert!(!is_unsigned(0x8009_6010_u32 as i32));
    }

    #[test]
    fn plain_text_file_is_unsigned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("readme.txt");
        std::fs::write(&path, "not a program").unwrap();
        let info = WindowsCodeSign.verify(&path).unwrap();
        assert!(!info.signed, "{info:?}");
        assert!(!info.trusted);
        assert_eq!(info.publisher, None);
        assert!(info.status.is_some());
    }

    #[test]
    fn missing_file_and_directory_are_typed_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = WindowsCodeSign
            .verify(&dir.path().join("missing.exe"))
            .unwrap_err();
        assert_eq!(err.code(), "not_found");
        let err = WindowsCodeSign.verify(dir.path()).unwrap_err();
        assert_eq!(err.code(), "invalid_input", "{err}");
    }
}
