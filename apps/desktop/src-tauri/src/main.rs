// No console window in release builds (debug keeps it for `tauri dev` logs).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    forgebox_desktop::run();
}
