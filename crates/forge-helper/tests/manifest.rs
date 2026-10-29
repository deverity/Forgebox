//! The UAC manifest embedded by `build.rs` (docs/11 §1): `forgebox-helper.exe` asks for
//! `requireAdministrator`; the other Forgebox executables built by this workspace never
//! do (they run as the user).
#![cfg(windows)]

use std::path::Path;

use forge_core::platform::windows::pe_resources::{
    APP_ICON_ID, application_manifest, ico_file_images, icon_group, requested_execution_level,
};

/// The brand icon (build.rs, docs/design/brand/README.md), embedded next to the manifest.
const ICON: &[u8] = include_bytes!("../../../apps/desktop/src-tauri/icons/icon.ico");

#[test]
fn forgebox_helper_carries_the_forgebox_icon() {
    let exe = Path::new(env!("CARGO_BIN_EXE_forgebox-helper"));
    let embedded = icon_group(exe, APP_ICON_ID)
        .unwrap()
        .expect("icon group 32512 is embedded");
    assert_eq!(embedded, ico_file_images(ICON).unwrap());
}

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
