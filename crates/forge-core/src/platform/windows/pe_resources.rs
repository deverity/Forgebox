//! Reading the application manifest embedded in an executable (`RT_MANIFEST`, id 1), to
//! check that `forgebox-helper.exe` asks for `requireAdministrator` (docs/11 §1) and that
//! no other binary does. The file is mapped as an image resource, never executed.

use std::path::Path;

use ::windows::Win32::Foundation::FreeLibrary;
use ::windows::Win32::System::LibraryLoader::{
    FindResourceW, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE, LoadLibraryExW,
    LoadResource, LockResource, SizeofResource,
};
use ::windows::Win32::UI::WindowsAndMessaging::RT_MANIFEST;
use ::windows::core::{HSTRING, PCWSTR};

use super::process::os_error;
use crate::error::{ForgeError, Result};

/// `CREATEPROCESS_MANIFEST_RESOURCE_ID`: the manifest the loader uses for an `.exe`.
const PROCESS_MANIFEST_ID: u16 = 1;

/// The UTF-8 text of the application manifest embedded in `exe`; `None` when there is
/// none.
pub fn application_manifest(exe: &Path) -> Result<Option<String>> {
    if !exe.is_file() {
        return Err(ForgeError::NotFound(format!("{}", exe.display())));
    }
    // SAFETY: valid path; mapped as data/resources only (no code runs, no imports load).
    let module = unsafe {
        LoadLibraryExW(
            &HSTRING::from(exe),
            None,
            LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE,
        )
    }
    .map_err(|e| ForgeError::io(format!("mapping {}", exe.display()), os_error(&e)))?;
    // SAFETY: valid module; MAKEINTRESOURCE-style integer id and type.
    let text = unsafe {
        let found = FindResourceW(
            Some(module),
            PCWSTR(PROCESS_MANIFEST_ID as usize as *const u16),
            RT_MANIFEST,
        );
        if found.is_invalid() {
            None
        } else {
            let size = SizeofResource(Some(module), found) as usize;
            LoadResource(Some(module), found).ok().and_then(|data| {
                let ptr = LockResource(data).cast::<u8>();
                (!ptr.is_null()).then(|| {
                    // SAFETY: the resource has `size` bytes, mapped while `module` is.
                    let bytes = std::slice::from_raw_parts(ptr, size);
                    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
                    String::from_utf8_lossy(bytes).into_owned()
                })
            })
        }
    };
    // SAFETY: balances LoadLibraryExW.
    unsafe {
        let _ = FreeLibrary(module);
    }
    Ok(text)
}

/// The `level` of `requestedExecutionLevel` in a manifest (`asInvoker`,
/// `requireAdministrator`, `highestAvailable`), if declared.
pub fn requested_execution_level(manifest: &str) -> Option<String> {
    let at = manifest.find("requestedExecutionLevel")?;
    let rest = &manifest[at..];
    let end = rest.find('>').unwrap_or(rest.len());
    let tag = &rest[..end];
    let level = tag.find("level=")? + "level=".len();
    let quoted = &tag[level..];
    let quote = quoted.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let value = &quoted[1..];
    value.find(quote).map(|close| value[..close].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_extracted() {
        let m = r#"<trustInfo><security><requestedPrivileges>
            <requestedExecutionLevel level="requireAdministrator" uiAccess="false"/>
            </requestedPrivileges></security></trustInfo>"#;
        assert_eq!(
            requested_execution_level(m).as_deref(),
            Some("requireAdministrator")
        );
        assert_eq!(
            requested_execution_level(
                "<requestedExecutionLevel uiAccess='false' level='asInvoker'/>"
            )
            .as_deref(),
            Some("asInvoker")
        );
        assert_eq!(requested_execution_level("<assembly/>"), None);
    }

    #[test]
    fn system_executables_have_manifests() {
        let notepad = super::super::process::system_directory().join("notepad.exe");
        let manifest = application_manifest(&notepad).unwrap().unwrap();
        assert_eq!(
            requested_execution_level(&manifest).as_deref(),
            Some("asInvoker")
        );
        assert_eq!(
            application_manifest(Path::new(r"C:\no\such\file.exe"))
                .unwrap_err()
                .code(),
            "not_found"
        );
    }
}
