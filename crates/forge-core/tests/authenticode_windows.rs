//! Authenticode through `platform::current().code_sign()` against real files: a
//! catalog-signed Windows binary (trusted, Microsoft publisher), the unsigned test binary
//! itself and an embedded signature whose bytes were tampered with.
#![cfg(windows)]

use std::path::{Path, PathBuf};

use forge_core::platform::{self, CodeSignKind};

fn system32(file: &str) -> PathBuf {
    let windir = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    Path::new(&windir).join("System32").join(file)
}

#[test]
fn notepad_is_trusted_and_signed_by_microsoft() {
    let info = platform::current()
        .code_sign()
        .verify(&system32("notepad.exe"))
        .unwrap();
    assert!(info.signed, "{info:?}");
    assert!(info.trusted, "{info:?}");
    assert_eq!(info.status, None);
    let publisher = info.publisher.clone().unwrap_or_default();
    assert!(publisher.contains("Microsoft"), "{info:?}");
    assert!(info.kind.is_some());
}

#[test]
fn test_binary_is_unsigned() {
    let exe = std::env::current_exe().unwrap();
    let info = platform::current().code_sign().verify(&exe).unwrap();
    assert!(!info.signed, "{info:?}");
    assert!(!info.trusted);
    assert_eq!(info.publisher, None);
    assert_eq!(info.kind, None);
}

/// Finds a binary with an *embedded* signature among common System32 files (most are
/// catalog-signed; a few, depending on the Windows build, carry their own signature).
fn embedded_signed_sample() -> Option<PathBuf> {
    let code_sign = platform::current().code_sign().clone();
    [
        "ntoskrnl.exe",
        "msmpeng.exe",
        "OneDriveSetup.exe",
        "mrt.exe",
        "winload.exe",
        "smss.exe",
        "ci.dll",
        "hal.dll",
        "kernel32.dll",
        "ntdll.dll",
    ]
    .into_iter()
    .map(system32)
    .filter(|p| p.is_file())
    .find(|p| {
        code_sign
            .verify(p)
            .is_ok_and(|i| i.trusted && i.kind == Some(CodeSignKind::Embedded))
    })
}

#[test]
fn tampered_embedded_signature_is_not_trusted() {
    let Some(sample) = embedded_signed_sample() else {
        eprintln!("SKIPPED: no binary with an embedded signature found in System32");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("copy.bin");
    std::fs::copy(&sample, &copy).unwrap();
    let code_sign = platform::current().code_sign().clone();
    let original = code_sign.verify(&copy).unwrap();
    assert!(original.trusted, "{sample:?}: {original:?}");
    assert_eq!(original.kind, Some(CodeSignKind::Embedded));
    assert!(original.publisher.is_some());

    // Flip a byte in the middle of the image (covered by the Authenticode digest).
    let mut bytes = std::fs::read(&copy).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xFF;
    std::fs::write(&copy, &bytes).unwrap();
    let tampered = code_sign.verify(&copy).unwrap();
    assert!(tampered.signed, "{tampered:?}");
    assert!(!tampered.trusted, "{tampered:?}");
    assert!(tampered.status.is_some());
    assert_eq!(tampered.publisher, original.publisher);
}
