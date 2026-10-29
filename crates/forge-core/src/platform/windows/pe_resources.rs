//! Reading resources embedded in an executable: the application manifest (`RT_MANIFEST`,
//! id 1), to check that `forgebox-helper.exe` asks for `requireAdministrator` (docs/11 §1)
//! and that no other binary does, and icon groups (`RT_GROUP_ICON` + `RT_ICON`), to check
//! that every Forgebox EXE carries the brand icon (docs/design/brand/README.md). The file is
//! mapped as an image resource, never executed.

use std::path::Path;

use ::windows::Win32::Foundation::{FreeLibrary, HMODULE};
use ::windows::Win32::System::LibraryLoader::{
    FindResourceW, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE, LoadLibraryExW,
    LoadResource, LockResource, SizeofResource,
};
use ::windows::Win32::UI::WindowsAndMessaging::{RT_GROUP_ICON, RT_ICON, RT_MANIFEST};
use ::windows::core::{HSTRING, PCWSTR};

use super::process::os_error;
use crate::error::{ForgeError, Result};

/// `CREATEPROCESS_MANIFEST_RESOURCE_ID`: the manifest the loader uses for an `.exe`.
const PROCESS_MANIFEST_ID: u16 = 1;

/// `IDI_APPLICATION`: the icon group id of the Forgebox EXEs (Tauri uses it for
/// `Forgebox.exe`; the `build.rs` of the CLI, daemon, helper, MCP server and ctrl use it too).
pub const APP_ICON_ID: u16 = 32512;

/// The UTF-8 text of the application manifest embedded in `exe`; `None` when there is
/// none.
pub fn application_manifest(exe: &Path) -> Result<Option<String>> {
    let bytes = with_module(exe, |module| {
        resource(module, PROCESS_MANIFEST_ID, RT_MANIFEST)
    })?;
    Ok(bytes.map(|bytes| {
        let text = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        String::from_utf8_lossy(text).into_owned()
    }))
}

/// One image of an icon: its size in pixels (a `0` in a directory means 256) and its bytes (a
/// `BITMAPINFOHEADER` DIB or a PNG), exactly as stored in an `.ico` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// The images of icon group `id` (`RT_GROUP_ICON`) embedded in `exe`, in directory order;
/// `None` when the group does not exist.
pub fn icon_group(exe: &Path, id: u16) -> Result<Option<Vec<IconImage>>> {
    with_module(exe, |module| {
        let Some(group) = resource(module, id, RT_GROUP_ICON) else {
            return Ok(None);
        };
        // GRPICONDIR (reserved, type 1, count) then 14-byte GRPICONDIRENTRY records whose
        // last field is the id of the RT_ICON resource holding the image.
        if read_u16(&group, 2)? != 1 {
            return Err(invalid("RT_GROUP_ICON is not an icon directory"));
        }
        let count = usize::from(read_u16(&group, 4)?);
        (0..count)
            .map(|i| {
                let entry = group
                    .get(6 + 14 * i..6 + 14 * (i + 1))
                    .ok_or_else(|| invalid("truncated RT_GROUP_ICON entry"))?;
                let image_id = u16::from_le_bytes([entry[12], entry[13]]);
                let data = resource(module, image_id, RT_ICON)
                    .ok_or_else(|| invalid(&format!("RT_ICON {image_id} is missing")))?;
                Ok(IconImage {
                    width: dimension(entry[0]),
                    height: dimension(entry[1]),
                    data,
                })
            })
            .collect::<Result<Vec<_>>>()
            .map(Some)
    })?
}

/// The images of an `.ico` file (ICONDIR then 16-byte ICONDIRENTRY records), in file order.
pub fn ico_file_images(ico: &[u8]) -> Result<Vec<IconImage>> {
    if read_u16(ico, 0)? != 0 || read_u16(ico, 2)? != 1 {
        return Err(invalid("not an .ico file"));
    }
    let count = usize::from(read_u16(ico, 4)?);
    (0..count)
        .map(|i| {
            let entry = ico
                .get(6 + 16 * i..6 + 16 * (i + 1))
                .ok_or_else(|| invalid("truncated ICONDIRENTRY"))?;
            let len = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as usize;
            let off = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]) as usize;
            let data = ico
                .get(off..off.saturating_add(len))
                .ok_or_else(|| invalid("icon image out of bounds"))?;
            Ok(IconImage {
                width: dimension(entry[0]),
                height: dimension(entry[1]),
                data: data.to_vec(),
            })
        })
        .collect()
}

fn dimension(byte: u8) -> u32 {
    if byte == 0 { 256 } else { u32::from(byte) }
}

fn read_u16(bytes: &[u8], at: usize) -> Result<u16> {
    bytes
        .get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| invalid("truncated icon directory"))
}

fn invalid(what: &str) -> ForgeError {
    ForgeError::InvalidInput(format!("icon resource: {what}"))
}

/// Maps `exe` as a data file for the duration of `f`.
fn with_module<T>(exe: &Path, f: impl FnOnce(HMODULE) -> T) -> Result<T> {
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
    let out = f(module);
    // SAFETY: balances LoadLibraryExW.
    unsafe {
        let _ = FreeLibrary(module);
    }
    Ok(out)
}

/// A copy of resource `id` of type `kind` (integer id and type, MAKEINTRESOURCE style).
fn resource(module: HMODULE, id: u16, kind: PCWSTR) -> Option<Vec<u8>> {
    // SAFETY: `module` is mapped by `with_module` for the whole call; integer id and type.
    unsafe {
        let found = FindResourceW(Some(module), PCWSTR(id as usize as *const u16), kind);
        if found.is_invalid() {
            return None;
        }
        let size = SizeofResource(Some(module), found) as usize;
        let data = LoadResource(Some(module), found).ok()?;
        let ptr = LockResource(data).cast::<u8>();
        // SAFETY: the resource has `size` bytes, mapped while `module` is.
        (!ptr.is_null()).then(|| std::slice::from_raw_parts(ptr, size).to_vec())
    }
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

    /// An `.ico` with a 16 px entry of 4 bytes and a 256 px ("0") entry of 3 bytes.
    fn sample_ico() -> Vec<u8> {
        let first = 6 + 2 * 16_u32;
        let mut ico = vec![0, 0, 1, 0, 2, 0];
        ico.extend_from_slice(&[16, 16, 0, 0, 1, 0, 32, 0, 4, 0, 0, 0]);
        ico.extend_from_slice(&first.to_le_bytes());
        ico.extend_from_slice(&[0, 0, 0, 0, 1, 0, 32, 0, 3, 0, 0, 0]);
        ico.extend_from_slice(&(first + 4).to_le_bytes());
        ico.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7]);
        ico
    }

    #[test]
    fn ico_files_are_parsed() {
        let images = ico_file_images(&sample_ico()).unwrap();
        assert_eq!(
            images,
            vec![
                IconImage {
                    width: 16,
                    height: 16,
                    data: vec![1, 2, 3, 4]
                },
                IconImage {
                    width: 256,
                    height: 256,
                    data: vec![5, 6, 7]
                },
            ]
        );
        // Truncated directory, image past the end, not an icon.
        assert_eq!(
            ico_file_images(&sample_ico()[..20]).unwrap_err().code(),
            "invalid_input"
        );
        assert_eq!(
            ico_file_images(&sample_ico()[..40]).unwrap_err().code(),
            "invalid_input"
        );
        assert_eq!(
            ico_file_images(b"\x89PNG\r\n").unwrap_err().code(),
            "invalid_input"
        );
    }

    #[test]
    fn icon_groups_are_read_from_system_executables() {
        let system = super::super::process::system_directory();
        // shell32.dll always carries icon group 1 (the generic document icon); every image of
        // the group resolves to an RT_ICON resource.
        let images = icon_group(&system.join("shell32.dll"), 1).unwrap().unwrap();
        assert!(!images.is_empty());
        assert!(images.iter().all(|i| i.width > 0 && !i.data.is_empty()));
        assert_eq!(
            icon_group(&system.join("notepad.exe"), 64_999).unwrap(),
            None
        );
        assert_eq!(
            icon_group(Path::new(r"C:\no\such\file.exe"), APP_ICON_ID)
                .unwrap_err()
                .code(),
            "not_found"
        );
    }
}
