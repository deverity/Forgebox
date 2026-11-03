//! Forgebox desktop shell: a thin Tauri host over the `forgeboxd` API (ADR-0002, ADR-0034).
//!
//! No domain logic lives here; this crate may depend on `forge-ipc` only (see
//! `cargo xtask arch-guard`) and has no Win32 of its own (ADR-0035). The webview reaches
//! daemons through the generic commands of [`bridge`]; every command handler is `async`
//! (docs/03 §7.5). The window itself (chrome, materials, placement, first frame, single
//! instance) is [`window`]; GUI-only files live in the [`data_dir`].

pub mod autostart;
pub mod bridge;
pub mod commands;
pub mod data_dir;
pub mod i18n;
pub mod perf;
pub mod prefs;
pub mod shortcuts;
pub mod tray;
pub mod updater;
pub mod window;
pub mod window_state;

use std::sync::Arc;

use tauri::webview::PageLoadEvent;
use tauri::{Manager, RunEvent};

use crate::bridge::commands::TauriConnectionSink;
use crate::bridge::{Bridge, BridgeOptions};
use crate::perf::StartupClock;
use crate::window::{Boot, EFFECT_OVERRIDE_ENV, Effect, LaunchArgs, MAIN_LABEL, WindowShell};

/// Starts the Tauri application and blocks until it exits.
pub fn run() {
    let clock = StartupClock::for_current_process();
    let launch = LaunchArgs::from_env();
    let forced = std::env::var(EFFECT_OVERRIDE_ENV)
        .ok()
        .and_then(|value| Effect::parse(&value));
    // Data dir, preferences, placement and the appearance snapshot are blocking reads: they
    // run on a helper thread while Tauri builds its runtime.
    let appearance = forge_ipc::base::platform::system_appearance();
    let boot_source = appearance.clone();
    let boot = std::thread::Builder::new()
        .name("forgebox-boot".to_owned())
        .spawn(move || Boot::load(boot_source.as_ref()))
        .expect("spawning the boot thread");

    let context = tauri::generate_context!();
    // Release builds stay inactive without a production key and endpoint (ADR-0038); the
    // environment override only exists in debug builds.
    let activation = updater::decide(
        &updater::ConfSettings::from_value(context.config().plugins.0.get("updater")),
        std::env::var(updater::ENDPOINT_ENV).ok().as_deref(),
        cfg!(debug_assertions),
    );
    let mut builder = tauri::Builder::default()
        // First plugin, so a second launch exits before anything else starts.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            window::on_second_instance(app, &argv);
        }))
        .plugin(autostart::plugin())
        // M2-E1-T13: "Copiar comando" and the native dialog of PathField (only offered when the
        // daemon is local; capabilities/main.json grants just these two commands).
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        // M2-E1-T14: "open site in the browser" and the global terminal shortcuts (registered
        // only when the webview asks, see shortcuts.rs).
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build());
    if let updater::Activation::Active { pubkey, .. } = &activation {
        builder = builder.plugin(
            tauri_plugin_updater::Builder::new()
                .pubkey(pubkey.clone())
                .build(),
        );
    }
    let app = builder
        .setup(move |app| {
            // The bridge's actor and the forge-ipc clients live on Tauri's tokio runtime.
            // Nothing connects here: a context opens on the first command naming it,
            // after the shell's first paint.
            let runtime = tauri::async_runtime::handle();
            let _guard = runtime.inner().enter();
            let sink = Arc::new(TauriConnectionSink(app.handle().clone()));
            let bridge = Bridge::new(sink, BridgeOptions::default());
            app.manage(bridge.clone());

            let boot = boot
                .join()
                .unwrap_or_else(|_| Boot::load(appearance.as_ref()));
            let shell = Arc::new(WindowShell::new(launch, clock, boot, appearance, forced));
            app.manage(shell.clone());
            window::create_main_window(app.handle(), &shell)?;
            // The tray (docs/07 §7) exists from the start; it connects after the first paint.
            // `--perf-startup` measures the window alone.
            if !shell.launch().perf_startup {
                if let Err(err) = tray::menu::install(app.handle(), &shell, &bridge) {
                    tracing::warn!(%err, "tray icon not created; closing the window exits");
                }
            }
            window::spawn_ready_watchdog(app.handle());
            let updater = updater::build_service(app.handle(), &activation, shell.prefs().clone());
            app.manage(updater.clone());
            // First check after the first paint, then every 24 h (inactive builds skip it).
            updater::spawn_schedule(updater);
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
            window::ready,
            window::appearance_get,
            perf::perf_startup,
            prefs::gui_prefs_get,
            prefs::gui_prefs_set,
            autostart::autostart_get,
            autostart::autostart_set,
            updater::updater_status,
            updater::updater_check,
            updater::updater_apply,
            shortcuts::shortcuts_sync,
            tray::menu::tray_motion,
        ])
        .build(context)
        .expect("error while building the Forgebox desktop application");

    app.run(|app, event| {
        if let RunEvent::Exit = event {
            // Closing already saved it; this covers exits without a close (tray "Sair").
            if let (Some(shell), Some(window)) = (
                app.try_state::<Arc<WindowShell>>(),
                app.get_webview_window(MAIN_LABEL),
            ) {
                shell.save_placement(&window);
            }
        }
    });
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
        assert!(found.len() >= 13, "{found:?}");
        for (file, signature) in &found {
            assert!(
                signature.starts_with("pub async fn") || signature.starts_with("async fn"),
                "{file}: `{signature}` is not async"
            );
        }
    }
}
