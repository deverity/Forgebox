//! Embeds the Forgebox icon in `forge.exe` (docs/design/brand/README.md): icon group 32512
//! (`IDI_APPLICATION`, the id Tauri gives Forgebox.exe) from
//! `apps/desktop/src-tauri/icons/icon.ico`, compiled by rc.exe (Windows SDK) through
//! embed-resource and linked into the binaries only (`rustc-link-arg-bins`). Checked by
//! `tests/exe_icon.rs`. A no-op when the target is not Windows.

use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ico = dir.join("../../apps/desktop/src-tauri/icons/icon.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let rc = out.join("forgebox-icon.rc");
    let path = ico.to_str().expect("UTF-8 icon path").replace('\\', "\\\\");
    std::fs::write(&rc, format!("32512 ICON \"{path}\"\n")).expect("writing forgebox-icon.rc");
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_required()
        .unwrap_or_else(|e| {
            panic!(
                "embedding the Forgebox icon failed ({e}); rc.exe comes with the Windows SDK, \
                 see README \"Desenvolvimento\""
            )
        });
}
