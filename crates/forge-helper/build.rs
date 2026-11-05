//! Embeds the application manifest of `forgebox-helper.exe` (docs/11 §1, ADR-0024):
//! `requestedExecutionLevel level="requireAdministrator"`, so Windows always starts it
//! elevated (UAC prompt through `ShellExecuteExW`, `ERROR_ELEVATION_REQUIRED` for a plain
//! `CreateProcess` from a non-elevated parent).
//!
//! On MSVC `embed-manifest` writes the XML to `OUT_DIR` and passes `/MANIFEST:EMBED`,
//! `/MANIFESTINPUT` and `/MANIFESTUAC:NO` (the UAC level comes from the XML) to the linker
//! of the **binaries** only: the library and the integration tests keep the default
//! manifest-less `asInvoker` behaviour. The bin target has `test = false` for the same
//! reason. Check with `mt.exe -inputresource:forgebox-helper.exe;#1 -out:con` or with the
//! `embedded_manifest` integration test.
//!
//! It also embeds the Forgebox icon (docs/design/brand/README.md): icon group 32512 from
//! `apps/desktop/src-tauri/icons/icon.ico`, compiled by rc.exe (Windows SDK) through
//! embed-resource, into the binaries only as well. That `.res` holds no manifest, so it does
//! not clash with the one the linker generates. Checked by `tests/manifest.rs`.

use std::path::PathBuf;

use embed_manifest::manifest::ExecutionLevel;
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    let dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ico = dir.join("../../apps/desktop/src-tauri/icons/icon.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let manifest = new_manifest("Forgebox.Helper")
            .requested_execution_level(ExecutionLevel::RequireAdministrator)
            // A console program without UI: no Common Controls dependency.
            .remove_dependency("Microsoft.Windows.Common-Controls");
        embed_manifest(manifest).expect("unable to embed the forgebox-helper manifest");

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
}
