//! Forgebox desktop shell: a thin Tauri host over the `forgeboxd` API (ADR-0002).
//!
//! No domain logic lives here; this crate may depend on `forge-ipc` only (see
//! `cargo xtask arch-guard`).

pub mod commands;
pub mod i18n;

/// Starts the Tauri application and blocks until the last window closes.
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            i18n::set_language
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Forgebox desktop application");
}
