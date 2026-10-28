//! Forgebox desktop shell: a thin Tauri host over the `forgeboxd` API (ADR-0002, ADR-0034).
//!
//! No domain logic lives here; this crate may depend on `forge-ipc` only (see
//! `cargo xtask arch-guard`). The webview reaches daemons through the generic commands
//! of [`bridge`]; every command handler is `async` (docs/03 §7.5).

pub mod bridge;
pub mod commands;
pub mod i18n;

use std::sync::Arc;

use tauri::Manager;
use tauri::webview::PageLoadEvent;

use crate::bridge::commands::TauriConnectionSink;
use crate::bridge::{Bridge, BridgeOptions};

/// Starts the Tauri application and blocks until the last window closes.
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // The bridge's actor and the forge-ipc clients live on Tauri's tokio runtime.
            // Nothing connects here: a context opens on the first command naming it,
            // after the shell's first paint.
            let runtime = tauri::async_runtime::handle();
            let _guard = runtime.inner().enter();
            let sink = Arc::new(TauriConnectionSink(app.handle().clone()));
            app.manage(Bridge::new(sink, BridgeOptions::default()));
            Ok(())
        })
        .on_page_load(|webview, payload| {
            // A (re)loading page cannot receive frames of its previous instance.
            if payload.event() == PageLoadEvent::Started {
                if let Some(bridge) = webview.try_state::<Bridge>() {
                    let closed = bridge.close_streams_of(webview.label());
                    if closed > 0 {
                        tracing::debug!(closed, "streams of a reloaded page closed");
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            i18n::set_language,
            bridge::commands::ipc_state,
            bridge::commands::ipc_call,
            bridge::commands::ipc_subscribe,
            bridge::commands::ipc_ack,
            bridge::commands::ipc_unsubscribe,
            bridge::commands::ipc_start,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Forgebox desktop application");
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// Review checklist item made executable (docs/03 §7.5, ADR-0034): every Tauri command
    /// of this crate is `async`, so none runs on the UI thread.
    #[test]
    fn every_tauri_command_is_async() {
        fn visit(dir: &Path, found: &mut Vec<(String, String)>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, found);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    let lines: Vec<&str> = text.lines().collect();
                    for (i, line) in lines.iter().enumerate() {
                        if line.trim_start().starts_with("#[tauri::command") {
                            let signature = lines[i + 1..]
                                .iter()
                                .find(|l| !l.trim_start().starts_with("#["))
                                .unwrap()
                                .trim()
                                .to_owned();
                            found.push((path.display().to_string(), signature));
                        }
                    }
                }
            }
        }
        let mut found = Vec::new();
        visit(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut found,
        );
        assert!(found.len() >= 8, "{found:?}");
        for (file, signature) in &found {
            assert!(
                signature.starts_with("pub async fn") || signature.starts_with("async fn"),
                "{file}: `{signature}` is not async"
            );
        }
    }
}
