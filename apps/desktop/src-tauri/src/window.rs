//! The main window ([ADR-0035], docs/07 §2.3, §4): undecorated and transparent with the
//! system shadow, a material chosen by [`choose_effect`], placement per monitor
//! ([`crate::window_state`]), hidden until the first frame (`ready`), close-to-tray and
//! focus on a second launch.
//!
//! **Win32 boundary.** This module has no Win32 of its own: OS facts come from
//! `forge_ipc::base::platform::SystemAppearance` (forge-core), the material is applied by
//! the `window-vibrancy` crate, whose `Result` decides the fallback — Tauri's
//! `set_effects` is never used, since it answers `Ok` on Windows 10 while doing nothing —
//! and the rest goes through Tauri's window API.
//!
//! **First frame.** The window is created hidden (no white flash) with an initialization
//! script that publishes the predicted appearance (`window.__FORGEBOX_SHELL__`), so
//! `data-theme`/`data-vibrancy` are on `<html>` before the first paint. The webview calls
//! [`ready`] after a double `requestAnimationFrame` following the first shell commit; the
//! window is shown then (or stays in the tray with `--minimized`). A watchdog shows it
//! anyway after [`READY_WATCHDOG`] so a broken page is never invisible.
//!
//! **Appearance.** Re-evaluated when the theme changes and when the window gains focus
//! (covers the transparency switch, high contrast and RDP reconnects, which have no event
//! of their own in Tauri) and when a preference changes; every change is emitted as
//! [`APPEARANCE_EVENT`] with an [`AppearanceState`].
//!
//! [ADR-0035]: ../../../../docs/adr/0035-window-chrome-materials.md

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Manager, Monitor, PhysicalPosition, PhysicalSize, Runtime, State,
    WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

use forge_ipc::base::platform::{AppearanceSnapshot, SystemAppearance};

use crate::data_dir::{GuiDataDir, GuiDataSource, GuiDirInputs};
use crate::perf::{PerfLine, StartupClock};
use crate::prefs::{GuiPrefs, PrefsStore, ThemePref, VibrancyPref};
use crate::window_state::{MonitorId, MonitorInfo, Placement, Rect, WindowStateFile};

/// Label of the only window (capabilities/main.json).
pub const MAIN_LABEL: &str = "main";
/// Event carrying every [`AppearanceState`] change.
pub const APPEARANCE_EVENT: &str = "forge://appearance";
/// Diagnostic override of the material (`mica`, `acrylic`, `none`): skips the policy, not
/// `window-vibrancy`'s answer. For QA and the frame-pacing measurement of ADR-0035.
pub const EFFECT_OVERRIDE_ENV: &str = "FORGE_GUI_EFFECT";
/// The window is shown after this long even if `ready` never arrives.
pub const READY_WATCHDOG: Duration = Duration::from_secs(5);
/// Default inner size (logical), capped by the work area.
pub const DEFAULT_SIZE: (f64, f64) = (1200.0, 800.0);
/// First Windows 10 build with Acrylic (1809).
pub const ACRYLIC_FIRST_BUILD: u32 = 17763;
/// What Windows 10 gets without the opt-in, decided by the frame-pacing measurement of
/// M2-E1-T7 (ADR-0035): Acrylic makes drags and resizes lag there.
pub const WIN10_DEFAULT: Effect = Effect::None;
/// Bound of one appearance snapshot (registry + DWM reads, normally < 5 ms).
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------------------
// Launch arguments
// ---------------------------------------------------------------------------------------

/// Flags of `Forgebox.exe`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LaunchArgs {
    /// `--minimized`: start in the tray, window hidden (autostart, docs/07 §7).
    pub minimized: bool,
    /// `--perf-startup`: print the first-frame JSON line and exit (see [`crate::perf`]).
    pub perf_startup: bool,
}

impl LaunchArgs {
    pub const MINIMIZED: &'static str = "--minimized";
    pub const PERF_STARTUP: &'static str = "--perf-startup";

    /// Parses the arguments after the program name; unknown ones are ignored.
    pub fn parse<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out = Self::default();
        for arg in args {
            match arg.as_ref() {
                Self::MINIMIZED => out.minimized = true,
                Self::PERF_STARTUP => out.perf_startup = true,
                _ => {}
            }
        }
        out
    }

    pub fn from_env() -> Self {
        Self::parse(std::env::args().skip(1))
    }
}

// ---------------------------------------------------------------------------------------
// Material policy (pure)
// ---------------------------------------------------------------------------------------

/// Window material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Mica,
    Acrylic,
    /// Opaque: the page paints `--bg-*-solid`.
    None,
}

impl Effect {
    pub fn as_str(self) -> &'static str {
        match self {
            Effect::Mica => "mica",
            Effect::Acrylic => "acrylic",
            Effect::None => "none",
        }
    }

    /// Value of [`EFFECT_OVERRIDE_ENV`].
    pub fn parse(value: &str) -> Option<Effect> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mica" => Some(Effect::Mica),
            "acrylic" => Some(Effect::Acrylic),
            "none" | "off" | "opaque" => Some(Effect::None),
            _ => None,
        }
    }
}

/// Why [`Effect`] is what it is (shown in diagnostics, `data-effect-reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectReason {
    /// Windows 11: Mica.
    Win11,
    /// Windows 10 with the Acrylic opt-in.
    Win10OptIn,
    /// Windows 10 without the opt-in: [`WIN10_DEFAULT`].
    Win10Default,
    /// The user switched materials off.
    UserOff,
    /// A high contrast theme is on.
    HighContrast,
    /// Remote Desktop session.
    RemoteSession,
    /// "Transparency effects" is off.
    TransparencyOff,
    /// OS without a material (unknown build, Windows 10 before 1809, other OSes).
    Unsupported,
    /// [`EFFECT_OVERRIDE_ENV`].
    Forced,
    /// `window-vibrancy` refused the effect.
    ApplyFailed,
    /// A material became possible but the window was created opaque (no material at
    /// start): it applies on the next start.
    RestartRequired,
}

/// The material preferences ([`GuiPrefs`] subset).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MaterialPrefs {
    pub vibrancy: VibrancyPref,
    pub win10_acrylic: bool,
}

impl From<&GuiPrefs> for MaterialPrefs {
    fn from(prefs: &GuiPrefs) -> Self {
        Self {
            vibrancy: prefs.vibrancy,
            win10_acrylic: prefs.win10_acrylic,
        }
    }
}

/// A decision of [`choose_effect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectChoice {
    pub effect: Effect,
    pub reason: EffectReason,
}

impl EffectChoice {
    const fn new(effect: Effect, reason: EffectReason) -> Self {
        Self { effect, reason }
    }
}

/// The material for `snapshot` and `prefs` (docs/07 §2.3, ADR-0035), in order:
/// the user's switch, high contrast, Remote Desktop and "Transparency effects" off mean
/// opaque; then Windows 11 → Mica; Windows 10 1809+ → Acrylic with the opt-in, otherwise
/// [`WIN10_DEFAULT`]; anything older or unknown → opaque. Pure: the caller applies it and
/// falls back to opaque if `window-vibrancy` refuses.
pub fn choose_effect(snapshot: &AppearanceSnapshot, prefs: &MaterialPrefs) -> EffectChoice {
    use EffectReason as R;
    if prefs.vibrancy == VibrancyPref::Off {
        return EffectChoice::new(Effect::None, R::UserOff);
    }
    if snapshot.high_contrast {
        return EffectChoice::new(Effect::None, R::HighContrast);
    }
    if snapshot.remote_session {
        return EffectChoice::new(Effect::None, R::RemoteSession);
    }
    if !snapshot.transparency {
        return EffectChoice::new(Effect::None, R::TransparencyOff);
    }
    if snapshot.is_win11 {
        return EffectChoice::new(Effect::Mica, R::Win11);
    }
    if snapshot.os_build >= ACRYLIC_FIRST_BUILD {
        return if prefs.win10_acrylic {
            EffectChoice::new(Effect::Acrylic, R::Win10OptIn)
        } else {
            EffectChoice::new(WIN10_DEFAULT, R::Win10Default)
        };
    }
    EffectChoice::new(Effect::None, R::Unsupported)
}

/// Light or dark, resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeName {
    Light,
    Dark,
}

impl ThemeName {
    pub fn is_dark(self) -> bool {
        self == ThemeName::Dark
    }
}

/// The theme for `pref` ("system" follows `apps_light`).
pub fn resolve_theme(pref: ThemePref, snapshot: &AppearanceSnapshot) -> ThemeName {
    match pref {
        ThemePref::Light => ThemeName::Light,
        ThemePref::Dark => ThemeName::Dark,
        ThemePref::System if snapshot.apps_light => ThemeName::Light,
        ThemePref::System => ThemeName::Dark,
    }
}

/// The native window theme: `None` follows Windows (and keeps Tauri's `ThemeChanged`
/// events coming), an explicit choice pins it.
fn window_theme(pref: ThemePref) -> Option<tauri::Theme> {
    match pref {
        ThemePref::System => None,
        ThemePref::Light => Some(tauri::Theme::Light),
        ThemePref::Dark => Some(tauri::Theme::Dark),
    }
}

/// What the webview applies to `<html>` (`src/app/appearance.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppearanceState {
    /// `data-theme`.
    pub theme: ThemeName,
    /// `data-vibrancy`: `on` exactly when a material is in effect.
    pub vibrancy: &'static str,
    /// The material in effect.
    pub effect: Effect,
    pub reason: EffectReason,
    /// `data-contrast`: `high` or `normal`.
    pub contrast: &'static str,
    /// The Windows accent, `#RRGGBB` (`--system-accent`).
    pub accent: String,
    pub remote_session: bool,
    pub os_build: u32,
}

/// The state for a snapshot, the preferences and the material actually in effect.
pub fn appearance_state(
    snapshot: &AppearanceSnapshot,
    prefs: &GuiPrefs,
    applied: EffectChoice,
) -> AppearanceState {
    AppearanceState {
        theme: resolve_theme(prefs.theme, snapshot),
        vibrancy: if applied.effect == Effect::None {
            "off"
        } else {
            "on"
        },
        effect: applied.effect,
        reason: applied.reason,
        contrast: if snapshot.high_contrast {
            "high"
        } else {
            "normal"
        },
        accent: snapshot.accent.clone(),
        remote_session: snapshot.remote_session,
        os_build: snapshot.os_build,
    }
}

/// Whether closing the window hides it instead of exiting: only with the preference on
/// and a tray icon to bring it back (M2-E1-T15 registers it); otherwise the GUI exits
/// (the daemon keeps running).
pub fn should_hide_on_close(close_to_tray: bool, tray_available: bool) -> bool {
    close_to_tray && tray_available
}

/// What a second launch does to the running instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecondInstance {
    /// `--minimized` (autostart racing an open GUI): nothing.
    Ignore,
    /// Show, restore and focus the window now.
    Focus,
    /// The first frame is not there yet: show it on `ready` (no white flash).
    FocusOnReady,
}

pub fn second_instance_action(args: LaunchArgs, ready: bool) -> SecondInstance {
    if args.minimized {
        SecondInstance::Ignore
    } else if ready {
        SecondInstance::Focus
    } else {
        SecondInstance::FocusOnReady
    }
}

/// Whether `ready` shows the window.
pub fn show_on_ready(launch: LaunchArgs, focus_requested: bool) -> bool {
    !launch.minimized || focus_requested
}

/// Whether the window is created transparent: only when a material is expected at start.
/// An opaque window paints the canvas colour from its first frame; a transparent one with
/// no material shows black until WebView2 presents (measured in M2-E1-T7: 3 of 8 launches
/// on this RDP machine showed a black first frame, ADR-0035).
pub fn transparent_at_start(predicted: Effect) -> bool {
    predicted != Effect::None
}

/// The choice actually possible in this window: an opaque window cannot show a material
/// until the next start.
pub fn effective_choice(wanted: EffectChoice, transparent_window: bool) -> EffectChoice {
    if wanted.effect != Effect::None && !transparent_window {
        EffectChoice::new(Effect::None, EffectReason::RestartRequired)
    } else {
        wanted
    }
}

/// The opaque canvas colour (`bg-canvas` of `docs/design/tokens.json`) the window paints
/// before and around the page when no material is in effect.
pub fn canvas_color(theme: ThemeName, high_contrast: bool) -> (u8, u8, u8) {
    match (high_contrast, theme) {
        (true, _) => (0x00, 0x00, 0x00),
        (false, ThemeName::Light) => (0xF5, 0xF5, 0xF7),
        (false, ThemeName::Dark) => (0x1C, 0x1C, 0x1E),
    }
}

fn opaque_background(theme: ThemeName, high_contrast: bool) -> tauri::window::Color {
    let (r, g, b) = canvas_color(theme, high_contrast);
    tauri::window::Color(r, g, b, 0xFF)
}

/// Acrylic tint (RGBA) per theme: the opaque sidebar colours of `docs/design/tokens.json`
/// (`bg-sidebar`) at ~63 % so the blur shows through.
fn acrylic_tint(theme: ThemeName) -> (u8, u8, u8, u8) {
    match theme {
        ThemeName::Light => (0xE8, 0xE8, 0xED, 0xA0),
        ThemeName::Dark => (0x2C, 0x2C, 0x2E, 0xA0),
    }
}

// ---------------------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------------------

/// Everything the window shell needs, prepared before Tauri starts (blocking reads).
#[derive(Debug)]
pub struct Boot {
    pub data_dir: GuiDataDir,
    pub prefs: PrefsStore,
    pub window_state: WindowStateFile,
    pub snapshot: AppearanceSnapshot,
}

impl Boot {
    /// Resolves the GUI data dir (creating it), reads the preferences and the window
    /// placement and takes the appearance snapshot. Blocking: run on a helper thread while
    /// Tauri starts.
    pub fn load(appearance: &dyn SystemAppearance) -> Self {
        let resolved = GuiDataDir::resolve(&GuiDirInputs::from_env());
        let data_dir = match resolved.ensure() {
            Ok(()) => resolved,
            Err(err) => {
                // The WebView2 profile needs a writable folder; never fail the start.
                let fallback = GuiDataDir::at(
                    std::env::temp_dir()
                        .join("Forgebox")
                        .join(crate::data_dir::GUI_DIR_NAME),
                    GuiDataSource::Fallback,
                );
                tracing::warn!(%err, dir = %resolved.path().display(), fallback = %fallback.path().display(), "GUI data dir not writable");
                let _ = fallback.ensure();
                fallback
            }
        };
        let prefs = PrefsStore::load(data_dir.prefs_file());
        let window_state = WindowStateFile::load(&data_dir.window_state_file());
        Self {
            data_dir,
            prefs,
            window_state,
            snapshot: appearance.snapshot(),
        }
    }
}

/// The window shell (managed state, `Arc<WindowShell>`).
#[derive(Debug)]
pub struct WindowShell {
    launch: LaunchArgs,
    clock: StartupClock,
    data_dir: GuiDataDir,
    prefs: Arc<PrefsStore>,
    appearance: Arc<dyn SystemAppearance>,
    forced: Option<Effect>,
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    snapshot: AppearanceSnapshot,
    /// Native material currently applied.
    applied: Effect,
    /// Theme the material was applied for (Mica/Acrylic tint).
    applied_theme: Option<ThemeName>,
    /// The window was created transparent ([`transparent_at_start`]).
    transparent: bool,
    /// Background painted by an opaque window: (theme, high contrast).
    background: Option<(ThemeName, bool)>,
    state: AppearanceState,
    generation: u64,
    focus_requested: bool,
    shown: bool,
    tray_available: bool,
    window_state: WindowStateFile,
    /// Last normal (not maximized/minimized) outer position + inner size.
    normal: Option<Rect>,
}

impl WindowShell {
    pub fn new(
        launch: LaunchArgs,
        clock: StartupClock,
        boot: Boot,
        appearance: Arc<dyn SystemAppearance>,
        forced: Option<Effect>,
    ) -> Self {
        let prefs = boot.prefs.get();
        let predicted = predicted_choice(&boot.snapshot, &prefs, forced);
        let state = appearance_state(&boot.snapshot, &prefs, predicted);
        Self {
            launch,
            clock,
            data_dir: boot.data_dir,
            prefs: Arc::new(boot.prefs),
            appearance,
            forced,
            inner: Mutex::new(Inner {
                snapshot: boot.snapshot,
                applied: Effect::None,
                applied_theme: None,
                transparent: true,
                background: None,
                state,
                generation: 0,
                focus_requested: false,
                shown: false,
                tray_available: false,
                window_state: boot.window_state,
                normal: None,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn launch(&self) -> LaunchArgs {
        self.launch
    }

    pub fn clock(&self) -> &StartupClock {
        &self.clock
    }

    pub fn data_dir(&self) -> &GuiDataDir {
        &self.data_dir
    }

    pub fn prefs(&self) -> &Arc<PrefsStore> {
        &self.prefs
    }

    /// The appearance last emitted.
    pub fn appearance(&self) -> AppearanceState {
        self.lock().state.clone()
    }

    /// Called by the tray (M2-E1-T15) once its icon exists: from then on closing the
    /// window may hide it (`close_to_tray`).
    pub fn set_tray_available(&self, available: bool) {
        self.lock().tray_available = available;
    }

    /// The `--perf-startup` line for the marks so far.
    pub fn perf_line(&self, error: Option<&'static str>) -> PerfLine {
        let inner = self.lock();
        PerfLine {
            report: self.clock.report(),
            session: if inner.snapshot.remote_session {
                "rdp"
            } else {
                "console"
            },
            os_build: inner.snapshot.os_build,
            effect: inner.state.effect.as_str(),
            version: env!("CARGO_PKG_VERSION"),
            error,
        }
    }

    /// The script run before the page's own scripts on every load.
    fn boot_script(&self) -> String {
        let payload = serde_json::json!({
            "appearance": self.appearance(),
            "minimized": self.launch.minimized,
        });
        format!(
            "Object.defineProperty(window,'__FORGEBOX_SHELL__',{{value:Object.freeze({payload}),configurable:true}});"
        )
    }

    /// Applies `snapshot` (on the main thread): picks and applies the material, updates
    /// the state and emits it when it changed. Results of an older refresh are dropped.
    /// The lock is never held across a native call (a Win32 call may dispatch window
    /// messages, and the event handlers take the same lock).
    fn apply_snapshot<R: Runtime>(
        &self,
        window: &WebviewWindow<R>,
        snapshot: AppearanceSnapshot,
        generation: u64,
    ) {
        let prefs = self.prefs.get();
        let (transparent, applied, applied_theme, background) = {
            let inner = self.lock();
            if generation < inner.generation {
                return;
            }
            (
                inner.transparent,
                inner.applied,
                inner.applied_theme,
                inner.background,
            )
        };
        let theme = resolve_theme(prefs.theme, &snapshot);
        let wanted = effective_choice(
            predicted_choice(&snapshot, &prefs, self.forced),
            transparent,
        );

        let wanted_background = (!transparent).then_some((theme, snapshot.high_contrast));
        if wanted_background.is_some() && wanted_background != background {
            let _ =
                window.set_background_color(Some(opaque_background(theme, snapshot.high_contrast)));
        }
        let changed = wanted.effect != applied
            || (wanted.effect != Effect::None && applied_theme != Some(theme));
        let result = if changed {
            match apply_native(window, applied, wanted.effect, theme) {
                Ok(()) => wanted,
                Err(err) => {
                    tracing::warn!(effect = wanted.effect.as_str(), %err, "window material refused; opaque");
                    let _ = apply_native(window, wanted.effect, Effect::None, theme);
                    EffectChoice::new(Effect::None, EffectReason::ApplyFailed)
                }
            }
        } else {
            EffectChoice::new(applied, wanted.reason)
        };

        let state = appearance_state(&snapshot, &prefs, result);
        let emit = {
            let mut inner = self.lock();
            inner.applied = result.effect;
            inner.applied_theme = Some(theme);
            if wanted_background.is_some() {
                inner.background = wanted_background;
            }
            inner.snapshot = snapshot;
            let emit = state != inner.state;
            if emit {
                inner.state = state.clone();
            }
            emit
        };
        if emit {
            if let Err(err) = window.emit_to(MAIN_LABEL, APPEARANCE_EVENT, &state) {
                tracing::debug!(%err, "appearance event not delivered");
            }
        }
    }

    fn next_generation(&self) -> u64 {
        let mut inner = self.lock();
        inner.generation += 1;
        inner.generation
    }

    /// Records the normal rectangle after a move/resize (main thread).
    fn track_normal_rect<R: Runtime>(&self, window: &WebviewWindow<R>) {
        if window.is_minimized().unwrap_or(true) || window.is_maximized().unwrap_or(true) {
            return;
        }
        let (Ok(pos), Ok(size)) = (window.outer_position(), window.inner_size()) else {
            return;
        };
        self.lock().normal = Some(Rect {
            x: pos.x,
            y: pos.y,
            width: size.width,
            height: size.height,
        });
    }

    /// The placement of the window now, recorded into the in-memory file; returns the
    /// file to save (`None` when nothing is known).
    fn capture_placement<R: Runtime>(&self, window: &WebviewWindow<R>) -> Option<WindowStateFile> {
        if !self.lock().shown {
            // Never shown (`--minimized`): keep what was saved.
            return None;
        }
        let monitor = window.current_monitor().ok().flatten()?;
        let maximized = window.is_maximized().unwrap_or(false);
        let current = match (window.outer_position(), window.inner_size()) {
            (Ok(pos), Ok(size)) if !maximized && !window.is_minimized().unwrap_or(false) => {
                Some(Rect {
                    x: pos.x,
                    y: pos.y,
                    width: size.width,
                    height: size.height,
                })
            }
            _ => None,
        };
        let info = monitor_info(&monitor);
        let mut inner = self.lock();
        let rect = current.or(inner.normal)?;
        inner.window_state.record(Placement {
            monitor: info.id,
            rect,
            scale: info.scale,
            maximized,
        });
        Some(inner.window_state.clone())
    }

    /// Saves the placement (blocking write of a few hundred bytes).
    pub fn save_placement<R: Runtime>(&self, window: &WebviewWindow<R>) {
        if self.launch.perf_startup {
            return;
        }
        if let Some(file) = self.capture_placement(window) {
            let path = self.data_dir.window_state_file();
            if let Err(err) = file.save(&path) {
                tracing::warn!(%err, path = %path.display(), "window state not saved");
            }
        }
    }

    /// Shows, restores and focuses the window.
    fn show<R: Runtime>(&self, window: &WebviewWindow<R>) {
        self.lock().shown = true;
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// The choice before `window-vibrancy` answers: the override or the policy.
fn predicted_choice(
    snapshot: &AppearanceSnapshot,
    prefs: &GuiPrefs,
    forced: Option<Effect>,
) -> EffectChoice {
    match forced {
        Some(effect) => EffectChoice::new(effect, EffectReason::Forced),
        None => choose_effect(snapshot, &MaterialPrefs::from(prefs)),
    }
}

/// Clears `previous` (when it changes) and applies `next` with `window-vibrancy`.
fn apply_native<R: Runtime>(
    window: &WebviewWindow<R>,
    previous: Effect,
    next: Effect,
    theme: ThemeName,
) -> Result<(), String> {
    if previous != next {
        // Clearing an effect that is not there is harmless; its error is not ours.
        let _ = match previous {
            Effect::Mica => window_vibrancy::clear_mica(window),
            Effect::Acrylic => window_vibrancy::clear_acrylic(window),
            Effect::None => Ok(()),
        };
    }
    match next {
        Effect::Mica => window_vibrancy::apply_mica(window, Some(theme.is_dark())),
        Effect::Acrylic => window_vibrancy::apply_acrylic(window, Some(acrylic_tint(theme))),
        Effect::None => Ok(()),
    }
    .map_err(|e| e.to_string())
}

fn monitor_info(monitor: &Monitor) -> MonitorInfo {
    let pos = monitor.position();
    let size = monitor.size();
    let area = monitor.work_area();
    MonitorInfo {
        id: MonitorId {
            name: monitor.name().cloned().unwrap_or_default(),
            bounds: Rect {
                x: pos.x,
                y: pos.y,
                width: size.width,
                height: size.height,
            },
        },
        work_area: Rect {
            x: area.position.x,
            y: area.position.y,
            width: area.size.width,
            height: area.size.height,
        },
        scale: monitor.scale_factor(),
    }
}

/// Applies the minimum size for the monitor the window is on (DPI or monitor change).
fn update_min_size<R: Runtime>(window: &WebviewWindow<R>) {
    if let Ok(Some(monitor)) = window.current_monitor() {
        let info = monitor_info(&monitor);
        let (w, h) = crate::window_state::min_logical_size(info.work_area, info.scale);
        let _ = window.set_min_size(Some(tauri::LogicalSize::new(w, h)));
    }
}

/// Creates the hidden main window (setup, main thread) and applies the material.
pub fn create_main_window<R: Runtime>(
    app: &AppHandle<R>,
    shell: &Arc<WindowShell>,
) -> tauri::Result<WebviewWindow<R>> {
    let monitors: Vec<MonitorInfo> = app
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(monitor_info)
        .collect();
    let primary = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| monitor_info(&m));
    let restore = {
        let inner = shell.lock();
        inner
            .window_state
            .restore(&monitors)
            .map(|(r, m)| (r, m.clone()))
    };
    let target = restore
        .as_ref()
        .map(|(_, m)| m.clone())
        .or(primary)
        .or_else(|| monitors.first().cloned());
    let (min_w, min_h, width, height) = match &target {
        Some(m) => {
            let (min_w, min_h) = crate::window_state::min_logical_size(m.work_area, m.scale);
            let area_w = f64::from(m.work_area.width) / m.scale.max(0.1);
            let area_h = f64::from(m.work_area.height) / m.scale.max(0.1);
            (
                min_w,
                min_h,
                DEFAULT_SIZE.0.min(area_w),
                DEFAULT_SIZE.1.min(area_h),
            )
        }
        None => (960.0, 600.0, DEFAULT_SIZE.0, DEFAULT_SIZE.1),
    };
    let prefs = shell.prefs.get();
    let (transparent, background) = {
        let mut inner = shell.lock();
        inner.transparent = transparent_at_start(inner.state.effect);
        let theme = inner.state.theme;
        let high_contrast = inner.snapshot.high_contrast;
        if !inner.transparent {
            inner.background = Some((theme, high_contrast));
        }
        (inner.transparent, opaque_background(theme, high_contrast))
    };

    let mut builder = WebviewWindowBuilder::new(app, MAIN_LABEL, WebviewUrl::default())
        .title("Forgebox")
        .inner_size(width, height)
        .min_inner_size(min_w, min_h)
        .decorations(false)
        .transparent(transparent)
        .shadow(true)
        .resizable(true)
        .visible(false)
        .theme(window_theme(prefs.theme))
        .data_directory(shell.data_dir.webview_dir())
        .initialization_script(shell.boot_script());
    if !transparent {
        // Painted from the first frame: no black until WebView2 presents.
        builder = builder.background_color(background);
    }
    if restore.is_none() {
        builder = builder.center();
    }
    let window = builder.build()?;

    if let Some((r, _)) = &restore {
        let _ = window.set_position(PhysicalPosition::new(r.rect.x, r.rect.y));
        let _ = window.set_size(PhysicalSize::new(r.rect.width, r.rect.height));
        shell.lock().normal = Some(r.rect);
        if r.maximized {
            // Hidden: tao only records the flag; `show` opens it maximized (no flash).
            let _ = window.maximize();
        }
    }

    // The material before the first show (we are on the main thread).
    let snapshot = shell.lock().snapshot.clone();
    let generation = shell.next_generation();
    shell.apply_snapshot(&window, snapshot, generation);

    let handle = app.clone();
    window.on_window_event(move |event| on_window_event(&handle, event));
    Ok(window)
}

/// Window events of the main window.
fn on_window_event<R: Runtime>(app: &AppHandle<R>, event: &WindowEvent) {
    let Some(window) = app.get_webview_window(MAIN_LABEL) else {
        return;
    };
    let Some(shell) = app.try_state::<Arc<WindowShell>>() else {
        return;
    };
    match event {
        WindowEvent::Focused(true) | WindowEvent::ThemeChanged(_) => refresh_appearance(app),
        WindowEvent::Moved(_) | WindowEvent::Resized(_) => shell.track_normal_rect(&window),
        WindowEvent::ScaleFactorChanged { .. } => update_min_size(&window),
        WindowEvent::CloseRequested { api, .. } => {
            shell.save_placement(&window);
            let (close_to_tray, tray) =
                (shell.prefs.get().close_to_tray, shell.lock().tray_available);
            if should_hide_on_close(close_to_tray, tray) {
                api.prevent_close();
                let _ = window.hide();
            }
        }
        _ => {}
    }
}

/// Takes a fresh snapshot off the main thread and applies it on the main thread.
pub fn refresh_appearance<R: Runtime>(app: &AppHandle<R>) {
    let Some(shell) = app
        .try_state::<Arc<WindowShell>>()
        .map(|s| s.inner().clone())
    else {
        return;
    };
    let generation = shell.next_generation();
    let source = shell.appearance.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let job = tokio::task::spawn_blocking(move || source.snapshot());
        let snapshot = match tokio::time::timeout(SNAPSHOT_TIMEOUT, job).await {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(err)) => {
                tracing::warn!(%err, "appearance snapshot failed");
                return;
            }
            Err(_) => {
                tracing::warn!("appearance snapshot timed out");
                return;
            }
        };
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            if let Some(window) = handle.get_webview_window(MAIN_LABEL) {
                shell.apply_snapshot(&window, snapshot, generation);
            }
        });
    });
}

/// A preference changed (`gui_prefs_set`): re-theme and re-evaluate the material.
pub fn on_prefs_changed<R: Runtime>(app: &AppHandle<R>, before: &GuiPrefs, after: &GuiPrefs) {
    if before.theme != after.theme {
        if let Some(window) = app.get_webview_window(MAIN_LABEL) {
            let _ = window.set_theme(window_theme(after.theme));
        }
    }
    if before.theme != after.theme
        || before.vibrancy != after.vibrancy
        || before.win10_acrylic != after.win10_acrylic
    {
        refresh_appearance(app);
    }
}

/// Single-instance callback: another `Forgebox.exe` was started with `argv`.
pub fn on_second_instance<R: Runtime>(app: &AppHandle<R>, argv: &[String]) {
    let Some(shell) = app
        .try_state::<Arc<WindowShell>>()
        .map(|s| s.inner().clone())
    else {
        return;
    };
    let args = LaunchArgs::parse(argv.iter().skip(1));
    if second_instance_action(args, shell.clock.is_ready()) != SecondInstance::Ignore {
        show_main_window(app);
    }
    tracing::info!(?args, "second launch handled by the running instance");
}

/// Shows, restores and focuses the main window — now, or on `ready` if the first frame is
/// not there yet (no white flash). For the second launch and the tray (M2-E1-T15).
pub fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    let Some(shell) = app
        .try_state::<Arc<WindowShell>>()
        .map(|s| s.inner().clone())
    else {
        return;
    };
    match second_instance_action(LaunchArgs::default(), shell.clock.is_ready()) {
        SecondInstance::Focus => {
            if let Some(window) = app.get_webview_window(MAIN_LABEL) {
                shell.show(&window);
            }
        }
        _ => shell.lock().focus_requested = true,
    }
}

/// Shows the window if `ready` never came (broken page, slow dev server).
pub fn spawn_ready_watchdog<R: Runtime>(app: &AppHandle<R>) {
    let Some(shell) = app
        .try_state::<Arc<WindowShell>>()
        .map(|s| s.inner().clone())
    else {
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let wait = if shell.launch.perf_startup {
            crate::perf::PERF_READY_TIMEOUT
        } else {
            READY_WATCHDOG
        };
        tokio::time::sleep(wait).await;
        if shell.clock.is_ready() {
            return;
        }
        if shell.launch.perf_startup {
            crate::perf::print_line(&shell.perf_line(Some("ready_timeout")));
            app.exit(1);
            return;
        }
        tracing::warn!(
            after_ms = wait.as_millis() as u64,
            "no ready from the webview; showing the window"
        );
        let focus = shell.lock().focus_requested;
        if show_on_ready(shell.launch, focus) {
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || {
                if let Some(window) = handle.get_webview_window(MAIN_LABEL) {
                    shell.show(&window);
                }
            });
        }
    });
}

/// Answer of [`ready`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadyAnswer {
    /// The first `ready` of this process (later ones come from reloads).
    pub first: bool,
    /// The window is (being) shown.
    pub shown: bool,
}

/// The webview painted its first frame (double rAF after the first shell commit): record
/// the mark, then show the window (unless `--minimized`), or print the perf line and exit
/// with `--perf-startup`.
#[tauri::command]
pub async fn ready(
    app: AppHandle,
    window: WebviewWindow,
    shell: State<'_, Arc<WindowShell>>,
) -> Result<ReadyAnswer, ()> {
    if window.label() != MAIN_LABEL {
        return Ok(ReadyAnswer {
            first: false,
            shown: false,
        });
    }
    let shell = shell.inner().clone();
    let first = shell.clock.mark_ready(SystemTime::now());
    if first && shell.launch.perf_startup {
        crate::perf::print_line(&shell.perf_line(None));
        app.exit(0);
        return Ok(ReadyAnswer {
            first,
            shown: false,
        });
    }
    let show = first && show_on_ready(shell.launch, shell.lock().focus_requested);
    if show {
        let handle = app.clone();
        let shell = shell.clone();
        let _ = app.run_on_main_thread(move || {
            if let Some(window) = handle.get_webview_window(MAIN_LABEL) {
                shell.show(&window);
            }
        });
    }
    let shown = show || shell.lock().shown;
    Ok(ReadyAnswer { first, shown })
}

/// The current appearance (the page asks once its listener is attached).
#[tauri::command]
pub async fn appearance_get(shell: State<'_, Arc<WindowShell>>) -> Result<AppearanceState, ()> {
    Ok(shell.appearance())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> AppearanceSnapshot {
        AppearanceSnapshot {
            accent: "#0078D7".to_owned(),
            apps_light: false,
            taskbar_light: false,
            transparency: true,
            high_contrast: false,
            remote_session: false,
            os_build: 19045,
            is_win11: false,
        }
    }

    fn win11() -> AppearanceSnapshot {
        AppearanceSnapshot {
            os_build: 22631,
            is_win11: true,
            ..snap()
        }
    }

    const AUTO: MaterialPrefs = MaterialPrefs {
        vibrancy: VibrancyPref::Auto,
        win10_acrylic: false,
    };
    const OPT_IN: MaterialPrefs = MaterialPrefs {
        vibrancy: VibrancyPref::Auto,
        win10_acrylic: true,
    };
    const OFF: MaterialPrefs = MaterialPrefs {
        vibrancy: VibrancyPref::Off,
        win10_acrylic: true,
    };

    #[test]
    fn windows_11_gets_mica() {
        let choice = choose_effect(&win11(), &AUTO);
        assert_eq!(choice, EffectChoice::new(Effect::Mica, EffectReason::Win11));
        // The Windows 10 opt-in does not turn Windows 11 into Acrylic.
        assert_eq!(choose_effect(&win11(), &OPT_IN).effect, Effect::Mica);
    }

    #[test]
    fn windows_10_follows_the_policy_and_the_opt_in() {
        assert_eq!(
            choose_effect(&snap(), &AUTO),
            EffectChoice::new(WIN10_DEFAULT, EffectReason::Win10Default)
        );
        assert_eq!(
            WIN10_DEFAULT,
            Effect::None,
            "ADR-0035: Windows 10 is opaque by default"
        );
        assert_eq!(
            choose_effect(&snap(), &OPT_IN),
            EffectChoice::new(Effect::Acrylic, EffectReason::Win10OptIn)
        );
        // 1809 is the first build with Acrylic; before it nothing.
        let s1809 = AppearanceSnapshot {
            os_build: ACRYLIC_FIRST_BUILD,
            ..snap()
        };
        assert_eq!(choose_effect(&s1809, &OPT_IN).effect, Effect::Acrylic);
        let s1803 = AppearanceSnapshot {
            os_build: 17134,
            ..snap()
        };
        assert_eq!(
            choose_effect(&s1803, &OPT_IN),
            EffectChoice::new(Effect::None, EffectReason::Unsupported)
        );
        let unknown = AppearanceSnapshot {
            os_build: 0,
            ..snap()
        };
        assert_eq!(
            choose_effect(&unknown, &OPT_IN).reason,
            EffectReason::Unsupported
        );
    }

    #[test]
    fn remote_transparency_off_high_contrast_and_user_off_are_opaque() {
        for base in [snap(), win11()] {
            let cases = [
                (
                    AppearanceSnapshot {
                        remote_session: true,
                        ..base.clone()
                    },
                    OPT_IN,
                    EffectReason::RemoteSession,
                ),
                (
                    AppearanceSnapshot {
                        transparency: false,
                        ..base.clone()
                    },
                    OPT_IN,
                    EffectReason::TransparencyOff,
                ),
                (
                    AppearanceSnapshot {
                        high_contrast: true,
                        ..base.clone()
                    },
                    OPT_IN,
                    EffectReason::HighContrast,
                ),
                (base.clone(), OFF, EffectReason::UserOff),
            ];
            for (snapshot, prefs, reason) in cases {
                assert_eq!(
                    choose_effect(&snapshot, &prefs),
                    EffectChoice::new(Effect::None, reason),
                    "{snapshot:?} {prefs:?}"
                );
            }
        }
        // Precedence: the user's switch, then high contrast, then RDP, then transparency.
        let everything = AppearanceSnapshot {
            remote_session: true,
            transparency: false,
            high_contrast: true,
            ..win11()
        };
        assert_eq!(
            choose_effect(&everything, &OFF).reason,
            EffectReason::UserOff
        );
        assert_eq!(
            choose_effect(&everything, &AUTO).reason,
            EffectReason::HighContrast
        );
    }

    /// Acceptance of M2-E1-T7: on this machine (an RDP session) the automatic state is
    /// `data-vibrancy="off"`. Opt-in check: `FORGEBOX_EXPECT_REMOTE_SESSION=1`.
    #[test]
    fn the_running_session_maps_to_its_vibrancy() {
        let snapshot = forge_ipc::base::platform::system_appearance().snapshot();
        let state = appearance_state(
            &snapshot,
            &GuiPrefs::default(),
            choose_effect(&snapshot, &AUTO),
        );
        println!(
            "session appearance: {}",
            serde_json::to_string(&state).unwrap()
        );
        if snapshot.remote_session {
            assert_eq!(state.vibrancy, "off");
            assert_eq!(state.reason, EffectReason::RemoteSession);
        }
        if std::env::var("FORGEBOX_EXPECT_REMOTE_SESSION").is_ok_and(|v| v == "1") {
            assert!(snapshot.remote_session, "{snapshot:?}");
            assert_eq!(state.vibrancy, "off");
        }
    }

    #[test]
    fn the_state_carries_theme_vibrancy_and_contrast() {
        let prefs = GuiPrefs::default();
        let state = appearance_state(&win11(), &prefs, choose_effect(&win11(), &AUTO));
        assert_eq!(state.theme, ThemeName::Dark, "apps_light=false");
        assert_eq!(state.vibrancy, "on");
        assert_eq!(state.contrast, "normal");
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "theme": "dark", "vibrancy": "on", "effect": "mica", "reason": "win11",
                "contrast": "normal", "accent": "#0078D7", "remote_session": false,
                "os_build": 22631
            })
        );

        let hc = AppearanceSnapshot {
            high_contrast: true,
            apps_light: true,
            ..snap()
        };
        let state = appearance_state(&hc, &prefs, choose_effect(&hc, &AUTO));
        assert_eq!(
            (state.theme, state.vibrancy, state.contrast),
            (ThemeName::Light, "off", "high")
        );

        // A refused effect is reported as opaque whatever the policy wanted.
        let failed = EffectChoice::new(Effect::None, EffectReason::ApplyFailed);
        let state = appearance_state(&win11(), &prefs, failed);
        assert_eq!(
            (state.vibrancy, state.reason),
            ("off", EffectReason::ApplyFailed)
        );
    }

    #[test]
    fn the_theme_preference_wins_over_the_system() {
        let light_os = AppearanceSnapshot {
            apps_light: true,
            ..snap()
        };
        assert_eq!(
            resolve_theme(ThemePref::System, &light_os),
            ThemeName::Light
        );
        assert_eq!(resolve_theme(ThemePref::System, &snap()), ThemeName::Dark);
        assert_eq!(resolve_theme(ThemePref::Dark, &light_os), ThemeName::Dark);
        assert_eq!(resolve_theme(ThemePref::Light, &snap()), ThemeName::Light);
        assert_eq!(window_theme(ThemePref::System), None);
        assert_eq!(window_theme(ThemePref::Dark), Some(tauri::Theme::Dark));
    }

    #[test]
    fn the_override_parses_and_wins() {
        assert_eq!(Effect::parse("Acrylic"), Some(Effect::Acrylic));
        assert_eq!(Effect::parse(" mica "), Some(Effect::Mica));
        assert_eq!(Effect::parse("off"), Some(Effect::None));
        assert_eq!(Effect::parse("blur"), None);
        let remote = AppearanceSnapshot {
            remote_session: true,
            ..snap()
        };
        let choice = predicted_choice(&remote, &GuiPrefs::default(), Some(Effect::Acrylic));
        assert_eq!(
            choice,
            EffectChoice::new(Effect::Acrylic, EffectReason::Forced)
        );
        assert_eq!(
            predicted_choice(&remote, &GuiPrefs::default(), None).reason,
            EffectReason::RemoteSession
        );
    }

    #[test]
    fn launch_flags_parse() {
        assert_eq!(
            LaunchArgs::parse(Vec::<String>::new()),
            LaunchArgs::default()
        );
        let args = LaunchArgs::parse(["--minimized", "--perf-startup", "--unknown"]);
        assert!(args.minimized && args.perf_startup);
        assert!(!LaunchArgs::parse(["--minimize"]).minimized);
    }

    #[test]
    fn close_hides_only_with_the_pref_and_a_tray() {
        assert!(should_hide_on_close(true, true));
        assert!(
            !should_hide_on_close(true, false),
            "no tray yet: closing exits"
        );
        assert!(!should_hide_on_close(false, true));
    }

    #[test]
    fn a_second_launch_focuses_unless_minimized() {
        let plain = LaunchArgs::default();
        let minimized = LaunchArgs {
            minimized: true,
            ..plain
        };
        assert_eq!(second_instance_action(plain, true), SecondInstance::Focus);
        assert_eq!(
            second_instance_action(plain, false),
            SecondInstance::FocusOnReady
        );
        assert_eq!(
            second_instance_action(minimized, true),
            SecondInstance::Ignore
        );

        // `ready` shows unless --minimized, and a focus request wins over --minimized.
        assert!(show_on_ready(plain, false));
        assert!(!show_on_ready(minimized, false));
        assert!(show_on_ready(minimized, true));
    }

    #[test]
    fn the_window_is_transparent_only_for_a_material() {
        assert!(!transparent_at_start(Effect::None));
        assert!(transparent_at_start(Effect::Mica));
        assert!(transparent_at_start(Effect::Acrylic));
        let mica = EffectChoice::new(Effect::Mica, EffectReason::Win11);
        assert_eq!(effective_choice(mica, true), mica);
        assert_eq!(
            effective_choice(mica, false),
            EffectChoice::new(Effect::None, EffectReason::RestartRequired)
        );
        let opaque = EffectChoice::new(Effect::None, EffectReason::RemoteSession);
        assert_eq!(effective_choice(opaque, false), opaque);
        assert_eq!(effective_choice(opaque, true), opaque);
    }

    /// The colours painted by an opaque window are the `bg-canvas` tokens.
    #[test]
    fn canvas_colors_match_the_design_tokens() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../docs/design/tokens.json");
        let tokens: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let hex = |group: &str| {
            let value = tokens["color"][group]["bg-canvas"]["$value"]
                .as_str()
                .unwrap();
            forge_ipc::base::platform::appearance::parse_hex_color(value).unwrap()
        };
        assert_eq!(canvas_color(ThemeName::Light, false), hex("light"));
        assert_eq!(canvas_color(ThemeName::Dark, false), hex("dark"));
        assert_eq!(canvas_color(ThemeName::Light, true), hex("high-contrast"));
        assert_eq!(canvas_color(ThemeName::Dark, true), hex("high-contrast"));
    }

    #[test]
    fn acrylic_tints_are_translucent() {
        for theme in [ThemeName::Light, ThemeName::Dark] {
            let (_, _, _, a) = acrylic_tint(theme);
            assert!(a > 0 && a < 0xFF);
        }
    }
}
