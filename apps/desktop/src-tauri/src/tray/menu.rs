//! The native side of the tray: the Tauri tray icon, the native menu built from a
//! [`MenuSpec`], the quit dialog, and the window shortcuts (docs/07 §7).
//!
//! Everything here runs on Tauri's main thread (posted with `run_on_main_thread`) or only
//! forwards to the [`Tray`] actor; nothing blocks. No Win32 of its own (ADR-0035): the
//! icon and menu go through Tauri (`tray-icon`/`muda`).
//!
//! Menu theme: Windows draws popup menus itself; Tauri (tao) lets the process follow the
//! system app mode ("AllowDark"), and neither Tauri nor muda can pin a tray menu to the
//! app's own light/dark choice. The menu therefore follows the Windows app mode, and the
//! glyphs are drawn for it (`TrayAppearance::dark_menu`). See docs/07 §7.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use futures::FutureExt;
use futures::future::BoxFuture;
use tauri::image::Image;
use tauri::menu::{
    CheckMenuItem, IconMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu,
};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime, Wry};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;

use forge_ipc::base::platform::AppearanceSnapshot;

use super::icons::{self, IconKey, MENU_GLYPH_SIZE, Rgba};
use super::model::{Glyph, MenuNode, MenuSpec, TrayModel};
use super::{Click, QuitChoice, Tray, TrayAppearance, TrayOptions, TrayRender, TrayView};
use crate::bridge::{Bridge, LOCAL_CONTEXT};
use crate::i18n::{self, Namespace};
use crate::notifications::{Notices, PluginNotifier, PrefsMutes};
use crate::window::{MAIN_LABEL, WindowShell};

/// Id of the app's tray icon.
pub const TRAY_ID: &str = "forgebox";

/// Event the webview listens to: the payload is the route to show (`/logs`).
pub const NAVIGATE_EVENT: &str = "forge://navigate";

/// Menus kept alive after being replaced: the one on screen must outlive its replacement
/// (a rebuild can land while it is still open).
const RETAINED_MENUS: usize = 2;

fn image(rgba: Rgba) -> Image<'static> {
    Image::new_owned(rgba.pixels, rgba.width, rgba.height)
}

/// The tray appearance from an OS snapshot and the taskbar's scale factor.
pub fn appearance_from(snapshot: &AppearanceSnapshot, scale: f64) -> TrayAppearance {
    TrayAppearance {
        taskbar_light: snapshot.taskbar_light,
        dark_menu: !snapshot.apps_light && !snapshot.high_contrast,
        scale,
        reduced_motion: false,
    }
}

fn primary_scale<R: Runtime>(app: &AppHandle<R>) -> f64 {
    app.primary_monitor()
        .ok()
        .flatten()
        .map_or(1.0, |monitor| monitor.scale_factor())
}

/// Builds the native menu of `spec`.
pub fn build<R: Runtime>(
    app: &AppHandle<R>,
    spec: &MenuSpec,
    dark_menu: bool,
) -> tauri::Result<Menu<R>> {
    let menu = Menu::new(app)?;
    for node in &spec.items {
        let item = build_node(app, node, dark_menu)?;
        menu.append(item.as_ref())?;
    }
    Ok(menu)
}

fn glyph_image(glyph: Glyph, dark_menu: bool) -> Image<'static> {
    image(icons::glyph(glyph, MENU_GLYPH_SIZE, dark_menu))
}

fn build_node<R: Runtime>(
    app: &AppHandle<R>,
    node: &MenuNode,
    dark_menu: bool,
) -> tauri::Result<Box<dyn IsMenuItem<R>>> {
    Ok(match node {
        MenuNode::Item {
            id,
            label,
            enabled,
            glyph: Some(glyph),
        } => Box::new(IconMenuItem::with_id(
            app,
            id.as_str(),
            label,
            *enabled,
            Some(glyph_image(*glyph, dark_menu)),
            None::<&str>,
        )?),
        MenuNode::Item {
            id,
            label,
            enabled,
            glyph: None,
        } => Box::new(MenuItem::with_id(
            app,
            id.as_str(),
            label,
            *enabled,
            None::<&str>,
        )?),
        MenuNode::Check {
            id,
            label,
            enabled,
            checked,
        } => Box::new(CheckMenuItem::with_id(
            app,
            id.as_str(),
            label,
            *enabled,
            *checked,
            None::<&str>,
        )?),
        MenuNode::Submenu {
            id,
            label,
            enabled,
            glyph,
            items,
        } => {
            let submenu = Submenu::with_id_and_icon(
                app,
                id.as_str(),
                label,
                *enabled,
                glyph.map(|g| glyph_image(g, dark_menu)),
            )?;
            for child in items {
                let item = build_node(app, child, dark_menu)?;
                submenu.append(item.as_ref())?;
            }
            Box::new(submenu)
        }
        MenuNode::Separator => Box::new(PredefinedMenuItem::separator(app)?),
    })
}

#[derive(Default)]
struct NativeState {
    menus: VecDeque<Menu<Wry>>,
    spec: Option<(MenuSpec, bool)>,
}

/// [`TrayView`] over the app's tray icon and window.
pub struct NativeView {
    app: AppHandle<Wry>,
    state: Arc<Mutex<NativeState>>,
}

impl NativeView {
    pub fn new(app: AppHandle<Wry>) -> Self {
        Self {
            app,
            state: Arc::new(Mutex::new(NativeState::default())),
        }
    }
}

fn set_icon(tray: &TrayIcon<Wry>, key: IconKey) {
    if let Err(err) = tray.set_icon(Some(image(icons::tray_icon(key)))) {
        tracing::warn!(%err, "tray: icon not set");
    }
}

impl TrayView for NativeView {
    fn render(&self, render: &TrayRender) {
        let (app, state, render) = (self.app.clone(), Arc::clone(&self.state), render.clone());
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            let Some(tray) = handle.tray_by_id(TRAY_ID) else {
                return;
            };
            let wanted = (render.menu.clone(), render.dark_menu);
            let rebuild = state
                .lock()
                .map(|s| s.spec.as_ref() != Some(&wanted))
                .unwrap_or(true);
            if rebuild {
                match build(&handle, &render.menu, render.dark_menu) {
                    Ok(menu) => {
                        if let Err(err) = tray.set_menu(Some(menu.clone())) {
                            tracing::warn!(%err, "tray: menu not set");
                        }
                        if let Ok(mut s) = state.lock() {
                            s.menus.push_back(menu);
                            while s.menus.len() > RETAINED_MENUS {
                                s.menus.pop_front();
                            }
                            s.spec = Some(wanted);
                        }
                    }
                    Err(err) => tracing::warn!(%err, "tray: menu not built"),
                }
            }
            if let Err(err) = tray.set_tooltip(Some(&render.tooltip)) {
                tracing::debug!(%err, "tray: tooltip not set");
            }
            set_icon(&tray, render.icon);
        });
    }

    fn set_icon(&self, icon: IconKey) {
        let handle = self.app.clone();
        let _ = self.app.run_on_main_thread(move || {
            if let Some(tray) = handle.tray_by_id(TRAY_ID) {
                set_icon(&tray, icon);
            }
        });
    }

    fn show_window(&self) {
        crate::window::show_main_window(&self.app);
    }

    fn navigate(&self, route: &str) {
        crate::window::show_main_window(&self.app);
        if let Err(err) = self.app.emit_to(MAIN_LABEL, NAVIGATE_EVENT, route) {
            tracing::debug!(%err, "tray: navigation not delivered");
        }
    }

    fn open_url(&self, url: &str) {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            tracing::warn!(url, "tray: not an http(s) URL; not opened");
            return;
        }
        if let Err(err) = self.app.opener().open_url(url, None::<&str>) {
            tracing::warn!(%err, url, "tray: URL not opened");
        }
    }

    fn open_terminal(&self) {
        // The same path as the global terminal shortcut: the webview runs `terminal.open`,
        // so the job and error handling stay in one place (shortcuts.rs).
        if let Err(err) = self
            .app
            .emit_to(MAIN_LABEL, crate::shortcuts::EVENT, "tray")
        {
            tracing::debug!(%err, "tray: terminal request not delivered");
        }
    }

    fn ask_quit(&self) -> BoxFuture<'static, QuitChoice> {
        let t = |key: &str| i18n::t(Namespace::Tray, key, &[]);
        let (stop, keep, cancel) = (
            t("quit_dialog.stop"),
            t("quit_dialog.keep"),
            t("quit_dialog.cancel"),
        );
        let (tx, rx) = tokio::sync::oneshot::channel();
        let labels = (stop.clone(), keep.clone());
        self.app
            .dialog()
            .message(t("quit_dialog.message"))
            .title(t("quit_dialog.title"))
            .kind(MessageDialogKind::Info)
            .buttons(MessageDialogButtons::YesNoCancelCustom(stop, keep, cancel))
            .show_with_result(move |result| {
                let _ = tx.send(quit_choice(&result, &labels.0, &labels.1));
            });
        async move { rx.await.unwrap_or(QuitChoice::Cancel) }.boxed()
    }

    fn exit(&self) {
        self.app.exit(0);
    }
}

/// Maps the dialog's answer (custom labels come back as `Custom(label)` on Windows).
pub fn quit_choice(
    result: &tauri_plugin_dialog::MessageDialogResult,
    stop_label: &str,
    keep_label: &str,
) -> QuitChoice {
    use tauri_plugin_dialog::MessageDialogResult as R;
    match result {
        R::Yes | R::Ok => QuitChoice::StopServices,
        R::No => QuitChoice::KeepServices,
        R::Custom(label) if label == stop_label => QuitChoice::StopServices,
        R::Custom(label) if label == keep_label => QuitChoice::KeepServices,
        _ => QuitChoice::Cancel,
    }
}

/// Maps a tray icon event to what the actor wants to know.
pub fn click_of(event: &TrayIconEvent) -> Option<Click> {
    match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } => Some(Click::Left),
        TrayIconEvent::Click {
            button: MouseButton::Middle,
            button_state: MouseButtonState::Up,
            ..
        } => Some(Click::Middle),
        // The menu shows on the button's release: defer rebuilds from the press on.
        TrayIconEvent::Click {
            button: MouseButton::Right,
            button_state: MouseButtonState::Down,
            ..
        } => Some(Click::MenuOpening),
        // Back on the icon: whatever menu was open is closed.
        TrayIconEvent::Enter { .. } | TrayIconEvent::DoubleClick { .. } => Some(Click::MenuClosed),
        _ => None,
    }
}

/// Creates the tray icon and its actor (setup, main thread). The actor stays idle until
/// [`activate`] (after the first paint).
pub fn install(app: &AppHandle<Wry>, shell: &WindowShell, bridge: &Bridge) -> tauri::Result<()> {
    let appearance = appearance_from(&shell.snapshot(), primary_scale(app));
    let initial = TrayModel::new(LOCAL_CONTEXT);
    let lang = i18n::current_language();
    let menu = build(app, &initial.menu(lang), appearance.dark_menu)?;
    let key = IconKey {
        state: initial.state(),
        frame: 0,
        taskbar_light: appearance.taskbar_light,
        remote: false,
        size: icons::icon_size_for_scale(appearance.scale),
    };
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(image(icons::tray_icon(key)))
        .tooltip(initial.tooltip(lang))
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            if let Some(tray) = app.try_state::<Tray>() {
                tray.menu_event(event.id().as_ref());
            }
        })
        .on_tray_icon_event(|icon, event| {
            if let (Some(click), Some(tray)) =
                (click_of(&event), icon.app_handle().try_state::<Tray>())
            {
                tray.click(click);
            }
        })
        .build(app)?;
    let view = Arc::new(NativeView::new(app.clone()));
    if let Ok(mut state) = view.state.lock() {
        state.menus.push_back(menu);
    }
    // Native toasts (M2-E1-T16): shown from here, so they work with the window hidden.
    let notices = Notices {
        notifier: Arc::new(PluginNotifier::new(app.clone())),
        mutes: Arc::new(PrefsMutes::new(Arc::clone(shell.prefs()))),
    };
    let tray = Tray::spawn_with_notices(
        Arc::new(bridge.clone()),
        view,
        LOCAL_CONTEXT,
        TrayOptions::default(),
        appearance,
        Some(notices),
    );
    app.manage(tray);
    shell.set_tray_available(true);
    Ok(())
}

/// The shell painted (or the ready watchdog fired): the tray connects.
pub fn activate<R: Runtime>(app: &AppHandle<R>) {
    if let Some(tray) = app.try_state::<Tray>() {
        tray.activate();
    }
}

/// The appearance snapshot was refreshed (theme, focus, preferences).
pub fn refresh_appearance<R: Runtime>(app: &AppHandle<R>) {
    let (Some(tray), Some(shell)) = (app.try_state::<Tray>(), app.try_state::<Arc<WindowShell>>())
    else {
        return;
    };
    tray.set_appearance(appearance_from(&shell.snapshot(), primary_scale(app)));
}

/// The webview reports `prefers-reduced-motion` (the `starting` ring then stays still).
#[tauri::command]
pub async fn tray_motion(app: AppHandle, reduced: bool) -> Result<(), ()> {
    if let Some(tray) = app.try_state::<Tray>() {
        tray.set_reduced_motion(reduced);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri_plugin_dialog::MessageDialogResult as R;

    #[test]
    fn dialog_answers_map_to_choices() {
        let (stop, keep) = ("Parar serviços e sair", "Manter serviços e sair");
        assert_eq!(
            quit_choice(&R::Custom(stop.into()), stop, keep),
            QuitChoice::StopServices
        );
        assert_eq!(
            quit_choice(&R::Custom(keep.into()), stop, keep),
            QuitChoice::KeepServices
        );
        assert_eq!(quit_choice(&R::Yes, stop, keep), QuitChoice::StopServices);
        assert_eq!(quit_choice(&R::No, stop, keep), QuitChoice::KeepServices);
        assert_eq!(quit_choice(&R::Cancel, stop, keep), QuitChoice::Cancel);
        assert_eq!(
            quit_choice(&R::Custom("Cancelar".into()), stop, keep),
            QuitChoice::Cancel
        );
    }

    #[test]
    fn the_menu_follows_the_windows_app_mode() {
        let snapshot = AppearanceSnapshot {
            apps_light: false,
            taskbar_light: true,
            ..AppearanceSnapshot::default()
        };
        let a = appearance_from(&snapshot, 1.5);
        assert!(a.dark_menu && a.taskbar_light);
        assert_eq!(icons::icon_size_for_scale(a.scale), 24);
        let contrast = AppearanceSnapshot {
            high_contrast: true,
            ..snapshot
        };
        assert!(!appearance_from(&contrast, 1.0).dark_menu);
    }
}
