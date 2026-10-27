//! Certificate parsing for the elevated helper's `trust.add_machine_root` /
//! `trust.remove_machine_root` (docs/11 §1, ADR-0024): thumbprint, subject, self-signature,
//! Basic Constraints and Name Constraints, read by CryptoAPI. The store itself is
//! [`super::trust_store`] (the same code the daemon's `TrustStore` uses). No elevation.

use std::ffi::c_void;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ::windows::Win32::Security::Cryptography::{
    CERT_BASIC_CONSTRAINTS2_INFO, CERT_CONTEXT, CERT_EXTENSION, CERT_NAME_CONSTRAINTS_INFO,
    CERT_NAME_SIMPLE_DISPLAY_TYPE, CRYPT_DECODE_ALLOC_FLAG, CRYPT_INTEGER_BLOB,
    CRYPT_STRING_BASE64HEADER, CRYPT_VERIFY_CERT_FLAGS, CRYPT_VERIFY_CERT_SIGN_ISSUER_CERT,
    CRYPT_VERIFY_CERT_SIGN_SUBJECT_CERT, CertCompareCertificateName, CertCreateCertificateContext,
    CertFindExtension, CertFreeCertificateContext, CertGetNameStringW, CryptDecodeObjectEx,
    CryptStringToBinaryA, CryptVerifyCertificateSignatureEx, X509_ASN_ENCODING,
    X509_BASIC_CONSTRAINTS2, X509_NAME_CONSTRAINTS, szOID_BASIC_CONSTRAINTS2,
    szOID_NAME_CONSTRAINTS,
};
use ::windows::core::PCSTR;

use super::security::LocalBox;
use crate::error::{ForgeError, Result};
use crate::platform::CertThumbprint;
use crate::platform::privileged::{CertificateInfo, IpPrefix, NameConstraints};

/// `CERT_ALT_NAME_DNS_NAME` (wincrypt.h).
const ALT_NAME_DNS: u32 = 3;
/// `CERT_ALT_NAME_IP_ADDRESS`.
const ALT_NAME_IP: u32 = 8;

/// Largest certificate file the helper reads.
const MAX_CERT_BYTES: usize = 64 * 1024;

/// A certificate context, freed on drop.
struct Cert(*mut CERT_CONTEXT);

impl Cert {
    fn from_der(der: &[u8]) -> Result<Self> {
        // SAFETY: `der` is a valid slice; the API copies it.
        let ctx = unsafe { CertCreateCertificateContext(X509_ASN_ENCODING, der) };
        if ctx.is_null() {
            return Err(ForgeError::InvalidInput(format!(
                "not an X.509 certificate: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(Self(ctx))
    }

    fn context(&self) -> &CERT_CONTEXT {
        // SAFETY: non-null context owned by `self`.
        unsafe { &*self.0 }
    }

    fn der(&self) -> Vec<u8> {
        let ctx = self.context();
        // SAFETY: the encoded bytes live as long as the context.
        unsafe { std::slice::from_raw_parts(ctx.pbCertEncoded, ctx.cbCertEncoded as usize) }
            .to_vec()
    }

    fn extensions(&self) -> &[CERT_EXTENSION] {
        // SAFETY: `pCertInfo` and its extension array live as long as the context.
        unsafe {
            let info = &*self.context().pCertInfo;
            if info.cExtension == 0 || info.rgExtension.is_null() {
                &[]
            } else {
                std::slice::from_raw_parts(info.rgExtension, info.cExtension as usize)
            }
        }
    }

    fn subject(&self) -> String {
        let mut buf = [0_u16; 512];
        // SAFETY: writable buffer whose length is passed by the slice.
        let len = unsafe {
            CertGetNameStringW(
                self.0,
                CERT_NAME_SIMPLE_DISPLAY_TYPE,
                0,
                None,
                Some(&mut buf),
            )
        } as usize;
        String::from_utf16_lossy(&buf[..len.saturating_sub(1).min(buf.len())])
    }

    fn self_signed(&self) -> bool {
        // SAFETY: `pCertInfo` lives as long as the context; both blobs belong to it.
        let same_name = unsafe {
            let info = &*self.context().pCertInfo;
            CertCompareCertificateName(X509_ASN_ENCODING, &info.Subject, &info.Issuer).as_bool()
        };
        if !same_name {
            return false;
        }
        // SAFETY: subject and issuer are the same valid context.
        unsafe {
            CryptVerifyCertificateSignatureEx(
                None,
                X509_ASN_ENCODING,
                CRYPT_VERIFY_CERT_SIGN_SUBJECT_CERT,
                self.0.cast::<c_void>(),
                CRYPT_VERIFY_CERT_SIGN_ISSUER_CERT,
                Some(self.0.cast::<c_void>()),
                CRYPT_VERIFY_CERT_FLAGS(0),
                None,
            )
        }
        .is_ok()
    }

    fn extension(&self, oid: PCSTR) -> Option<&CERT_EXTENSION> {
        // SAFETY: the extension array belongs to the context; the result points into it.
        let found = unsafe { CertFindExtension(oid, self.extensions()) };
        // SAFETY: null or a pointer into `self.extensions()`.
        unsafe { found.as_ref() }
    }

    fn is_ca(&self) -> Result<bool> {
        let Some(ext) = self.extension(szOID_BASIC_CONSTRAINTS2) else {
            return Ok(false);
        };
        let decoded = decode(X509_BASIC_CONSTRAINTS2, &ext.Value, "Basic Constraints")?;
        // SAFETY: the decoder allocated a CERT_BASIC_CONSTRAINTS2_INFO.
        let info = unsafe { &*decoded.as_ptr().cast::<CERT_BASIC_CONSTRAINTS2_INFO>() };
        Ok(info.fCA.as_bool())
    }

    fn name_constraints(&self) -> Result<Option<NameConstraints>> {
        let Some(ext) = self.extension(szOID_NAME_CONSTRAINTS) else {
            return Ok(None);
        };
        let decoded = decode(X509_NAME_CONSTRAINTS, &ext.Value, "Name Constraints")?;
        // SAFETY: the decoder allocated a CERT_NAME_CONSTRAINTS_INFO and every array and
        // string it points to, inside the same allocation.
        let info = unsafe { &*decoded.as_ptr().cast::<CERT_NAME_CONSTRAINTS_INFO>() };
        let mut out = NameConstraints {
            critical: ext.fCritical.as_bool(),
            excluded: info.cExcludedSubtree as usize,
            ..NameConstraints::default()
        };
        let permitted = if info.cPermittedSubtree == 0 || info.rgPermittedSubtree.is_null() {
            &[][..]
        } else {
            // SAFETY: array of `cPermittedSubtree` entries inside the decoded allocation.
            unsafe {
                std::slice::from_raw_parts(info.rgPermittedSubtree, info.cPermittedSubtree as usize)
            }
        };
        for subtree in permitted {
            let entry = &subtree.Base;
            match entry.dwAltNameChoice {
                ALT_NAME_DNS => {
                    // SAFETY: DNS choice: the union holds a NUL-terminated wide string.
                    let name =
                        unsafe { entry.Anonymous.pwszDNSName.to_string() }.unwrap_or_default();
                    out.permitted_dns.push(name);
                }
                ALT_NAME_IP => {
                    // SAFETY: IP choice: the union holds a blob (address followed by mask).
                    let blob = unsafe { entry.Anonymous.IPAddress };
                    // SAFETY: `cbData` bytes inside the decoded allocation.
                    let bytes = unsafe { blob_bytes(&blob) };
                    match ip_prefix(bytes) {
                        Some(prefix) => out.permitted_ip.push(prefix),
                        None => out.permitted_other += 1,
                    }
                }
                _ => out.permitted_other += 1,
            }
        }
        Ok(Some(out))
    }

    fn info(&self) -> Result<CertificateInfo> {
        let der = self.der();
        Ok(CertificateInfo {
            thumbprint: CertThumbprint::of_der(&der),
            der,
            subject: self.subject(),
            self_signed: self.self_signed(),
            is_ca: self.is_ca()?,
            name_constraints: self.name_constraints()?,
        })
    }
}

impl Drop for Cert {
    fn drop(&mut self) {
        // SAFETY: the context is owned by `self` and freed once.
        unsafe {
            let _ = CertFreeCertificateContext(Some(self.0));
        }
    }
}

/// # Safety
/// `blob` must describe `cbData` readable bytes (or be empty).
unsafe fn blob_bytes(blob: &CRYPT_INTEGER_BLOB) -> &[u8] {
    if blob.cbData == 0 || blob.pbData.is_null() {
        &[]
    } else {
        // SAFETY: guaranteed by the caller.
        unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }
    }
}

/// Address + mask (8 bytes for IPv4, 32 for IPv6) of a name constraint.
fn ip_prefix(bytes: &[u8]) -> Option<IpPrefix> {
    let (addr, mask): (IpAddr, &[u8]) = match bytes.len() {
        8 => (
            IpAddr::V4(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3])),
            &bytes[4..],
        ),
        32 => {
            let mut octets = [0_u8; 16];
            octets.copy_from_slice(&bytes[..16]);
            (IpAddr::V6(Ipv6Addr::from(octets)), &bytes[16..])
        }
        _ => return None,
    };
    let ones: u32 = mask.iter().map(|b| b.count_ones()).sum();
    let contiguous = mask
        .iter()
        .flat_map(|b| (0..8).rev().map(move |i| b >> i & 1 == 1))
        .skip_while(|bit| *bit)
        .all(|bit| !bit);
    Some(IpPrefix {
        addr,
        len: contiguous.then_some(ones as u8),
    })
}

/// `CryptDecodeObjectEx` with an allocated result (freed by `LocalBox`).
fn decode(struct_type: PCSTR, value: &CRYPT_INTEGER_BLOB, what: &str) -> Result<LocalBox> {
    let mut out: *mut c_void = std::ptr::null_mut();
    let mut len = 0_u32;
    // SAFETY: `value` points to the extension bytes; with CRYPT_DECODE_ALLOC_FLAG the API
    // stores a LocalAlloc'd pointer in `out`.
    unsafe {
        CryptDecodeObjectEx(
            X509_ASN_ENCODING,
            struct_type,
            blob_bytes(value),
            CRYPT_DECODE_ALLOC_FLAG,
            None,
            Some((&mut out as *mut *mut c_void).cast()),
            &mut len,
        )
    }
    .map_err(|e| ForgeError::InvalidInput(format!("certificate {what} extension: {e}")))?;
    Ok(LocalBox::new(out))
}

/// DER bytes of a PEM (`-----BEGIN CERTIFICATE-----`) or DER certificate file.
pub(crate) fn decode_certificate(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() > MAX_CERT_BYTES {
        return Err(ForgeError::InvalidInput(format!(
            "certificate file has {} bytes; at most {MAX_CERT_BYTES} are accepted",
            bytes.len()
        )));
    }
    if !bytes.windows(10).any(|w| w == b"-----BEGIN") {
        return Ok(bytes.to_vec());
    }
    let mut len = 0_u32;
    // SAFETY: sizing call over a valid slice.
    unsafe { CryptStringToBinaryA(bytes, CRYPT_STRING_BASE64HEADER, None, &mut len, None, None) }
        .map_err(|e| ForgeError::InvalidInput(format!("invalid PEM certificate: {e}")))?;
    let mut der = vec![0_u8; len as usize];
    // SAFETY: `der` holds `len` bytes.
    unsafe {
        CryptStringToBinaryA(
            bytes,
            CRYPT_STRING_BASE64HEADER,
            Some(der.as_mut_ptr()),
            &mut len,
            None,
            None,
        )
    }
    .map_err(|e| ForgeError::InvalidInput(format!("invalid PEM certificate: {e}")))?;
    der.truncate(len as usize);
    Ok(der)
}

/// Parses a PEM or DER certificate.
pub(crate) fn certificate_info(bytes: &[u8]) -> Result<CertificateInfo> {
    Cert::from_der(&decode_certificate(bytes)?)?.info()
}

/// The certificate with SHA-1 `thumbprint` in `LocalMachine\Root`, parsed.
pub(crate) fn machine_root_find(thumbprint: &CertThumbprint) -> Result<Option<CertificateInfo>> {
    super::trust_store::machine_root_der(thumbprint)?
        .map(|der| Cert::from_der(&der)?.info())
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_prefixes_from_constraint_bytes() {
        let v4 = ip_prefix(&[127, 0, 0, 0, 255, 0, 0, 0]).unwrap();
        assert_eq!(v4.to_string(), "127.0.0.0/8");
        let odd = ip_prefix(&[10, 0, 0, 0, 255, 0, 255, 0]).unwrap();
        assert_eq!(odd.len, None);
        let mut v6 = [0_u8; 32];
        v6[15] = 1;
        v6[16..].fill(0xff);
        assert_eq!(ip_prefix(&v6).unwrap().to_string(), "::1/128");
        assert!(ip_prefix(&[1, 2, 3]).is_none());
    }

    #[test]
    fn garbage_is_not_a_certificate() {
        assert_eq!(
            certificate_info(b"not a certificate").unwrap_err().code(),
            "invalid_input"
        );
        assert_eq!(
            certificate_info(b"-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n")
                .unwrap_err()
                .code(),
            "invalid_input"
        );
        assert_eq!(
            decode_certificate(&vec![0_u8; MAX_CERT_BYTES + 1])
                .unwrap_err()
                .code(),
            "invalid_input"
        );
    }

    #[test]
    fn reads_the_machine_root_store_without_elevation() {
        let nobody = CertThumbprint::from_bytes([0; 20]);
        assert!(machine_root_find(&nobody).unwrap().is_none());
        // "Microsoft Root Certificate Authority 2011" ships with every Windows 10/11.
        let thumbprint: CertThumbprint =
            "8f43288ad272f3103b6fb1428485ea3014c0bcfe".parse().unwrap();
        let info = machine_root_find(&thumbprint)
            .unwrap()
            .expect("Microsoft root");
        assert_eq!(info.thumbprint, thumbprint);
        assert_eq!(info.subject, "Microsoft Root Certificate Authority 2011");
        assert!(info.self_signed && info.is_ca, "{info:?}");
        assert_eq!(info.name_constraints, None);
        // Parsing the DER again gives the same answer, PEM too.
        assert_eq!(certificate_info(&info.der).unwrap(), info);
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            base64_lines(&info.der)
        );
        assert_eq!(certificate_info(pem.as_bytes()).unwrap(), info);
    }

    /// Minimal base64 for the PEM round trip (no dependency for one test).
    fn base64_lines(bytes: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out.as_bytes()
            .chunks(64)
            .map(|l| std::str::from_utf8(l).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }
}
