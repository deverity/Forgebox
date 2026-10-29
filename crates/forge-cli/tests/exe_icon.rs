//! `forge.exe` carries the Forgebox icon (build.rs, docs/design/brand/README.md): icon group
//! 32512 holds exactly the images of `apps/desktop/src-tauri/icons/icon.ico`.
#![cfg(windows)]

use std::path::Path;

use forge_ipc::base::platform::windows::pe_resources::{APP_ICON_ID, ico_file_images, icon_group};

const ICON: &[u8] = include_bytes!("../../../apps/desktop/src-tauri/icons/icon.ico");

#[test]
fn exe_embeds_the_forgebox_icon() {
    let exe = Path::new(env!("CARGO_BIN_EXE_forge"));
    let embedded = icon_group(exe, APP_ICON_ID)
        .unwrap()
        .expect("icon group 32512 is embedded");
    let expected = ico_file_images(ICON).unwrap();
    let sizes: Vec<u32> = embedded.iter().map(|i| i.width).collect();
    assert_eq!(sizes, [16, 20, 24, 32, 40, 48, 64, 256]);
    assert_eq!(embedded, expected);
}
