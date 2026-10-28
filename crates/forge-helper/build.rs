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

use embed_manifest::manifest::ExecutionLevel;
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let manifest = new_manifest("Forgebox.Helper")
            .requested_execution_level(ExecutionLevel::RequireAdministrator)
            // A console program without UI: no Common Controls dependency.
            .remove_dependency("Microsoft.Windows.Common-Controls");
        embed_manifest(manifest).expect("unable to embed the forgebox-helper manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
