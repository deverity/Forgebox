//! The UAC manifest embedded by `build.rs` (docs/11 §1): `forgebox-helper.exe` asks for
//! `requireAdministrator`; the other Forgebox executables built by this workspace never
//! do (they run as the user).
#![cfg(windows)]

use std::path::Path;

use forge_core::platform::windows::pe_resources::{
    application_manifest, requested_execution_level,
};

#[test]
fn forgebox_helper_requires_administrator() {
    let exe = Path::new(env!("CARGO_BIN_EXE_forgebox-helper"));
    let manifest = application_manifest(exe)
        .unwrap()
        .expect("forgebox-helper.exe has an embedded manifest");
    assert_eq!(
        requested_execution_level(&manifest).as_deref(),
        Some("requireAdministrator"),
        "{manifest}"
    );
    assert!(manifest.contains(r#"uiAccess="false""#), "{manifest}");
    assert!(manifest.contains("Forgebox.Helper"), "{manifest}");
}

#[test]
fn test_binaries_are_not_elevated() {
    // The integration test itself is built without the manifest: `cargo test` runs it.
    let me = std::env::current_exe().unwrap();
    let level = application_manifest(&me)
        .unwrap()
        .and_then(|m| requested_execution_level(&m));
    assert_ne!(level.as_deref(), Some("requireAdministrator"));
}
