//! `Forgebox.exe` carries the brand icon (docs/design/brand/README.md): tauri-build embeds
//! `icons/icon.ico` as icon group 32512, which is also the window and taskbar icon
//! (`default_window_icon_from_app_icon_resource` picks the entry for the system icon size).
#![cfg(windows)]

use std::path::Path;

use forge_ipc::base::platform::windows::pe_resources::{APP_ICON_ID, ico_file_images, icon_group};

const ICON: &[u8] = include_bytes!("../icons/icon.ico");

#[test]
fn forgebox_exe_embeds_the_brand_icon() {
    let exe = Path::new(env!("CARGO_BIN_EXE_Forgebox"));
    let embedded = icon_group(exe, APP_ICON_ID)
        .unwrap()
        .expect("icon group 32512 is embedded");
    let sizes: Vec<u32> = embedded.iter().map(|i| i.width).collect();
    assert_eq!(sizes, [16, 20, 24, 32, 40, 48, 64, 256]);
    assert_eq!(embedded, ico_file_images(ICON).unwrap());
}
