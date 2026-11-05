//! What the OS says about how the GUI should look (docs/07 §2.3, §7): the user's accent
//! color, light/dark for apps and for the taskbar (tray icons), the "Transparency effects"
//! switch, high contrast, whether the session is remote (RDP), and the OS build that
//! decides the window material (Windows 11, build [`WIN11_FIRST_BUILD`] and later, has
//! Mica; Windows 10 has Acrylic).
//!
//! The desktop shell (`Forgebox.exe`) reads it through `forge_ipc::base::platform` (the
//! same path as `spawn_detached`, ADR-0030), so `src-tauri` has no Win32 of its own
//! (CLAUDE.md). The CSS `AccentColor` keyword is not reliable in WebView2, so the native
//! value is the source of truth for `--accent`.
//!
//! [`SystemAppearance::snapshot`] is **blocking** (a few registry reads, one DWM call, two
//! system metrics; well under 5 ms) and never fails: anything that cannot be read falls
//! back to the documented default of that field (see [`AppearanceSnapshot`]). Async code
//! calls it from `tokio::task::spawn_blocking` (ADR-0011). It is a snapshot: the shell asks
//! again when the OS announces a change (`WM_SETTINGCHANGE`, theme or focus events).
//!
//! The parsing of raw OS values ([`RawAppearance::into_snapshot`], [`abgr_to_hex`],
//! [`argb_to_hex`]) is plain Rust shared by every OS, so it is unit tested everywhere.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Accent used when the OS gives none (docs/07 §2.3 `--accent` fallback).
pub const DEFAULT_ACCENT: &str = "#0A84FF";

/// First Windows 11 build (21H2). Builds at or above it have Mica.
pub const WIN11_FIRST_BUILD: u32 = 22000;

/// Registry key (under `HKEY_CURRENT_USER`) with the per-user theme switches:
/// `AppsUseLightTheme`, `SystemUsesLightTheme` and `EnableTransparency` (`REG_DWORD`, 0/1).
pub const PERSONALIZE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// Registry key (under `HKEY_CURRENT_USER`) with the accent color: `AccentColor`
/// (`REG_DWORD`, `0xAABBGGRR`).
pub const DWM_KEY: &str = r"Software\Microsoft\Windows\DWM";

/// The appearance facts of the OS at one instant. Serialized as is (snake_case) for the
/// desktop bridge.
///
/// Defaults, used field by field when the OS value is missing or unreadable (and by
/// [`AppearanceSnapshot::default`]) — they are what Windows itself assumes when the
/// registry values do not exist:
///
/// | field | default | source on Windows |
/// |---|---|---|
/// | `accent` | [`DEFAULT_ACCENT`] | `HKCU\…\DWM\AccentColor` (ABGR), else `DwmGetColorizationColor` (ARGB) |
/// | `apps_light` | `true` | `Personalize\AppsUseLightTheme` |
/// | `taskbar_light` | `false` | `Personalize\SystemUsesLightTheme` |
/// | `transparency` | `true` | `Personalize\EnableTransparency` |
/// | `high_contrast` | `false` | `SystemParametersInfoW(SPI_GETHIGHCONTRAST)` |
/// | `remote_session` | `false` | `GetSystemMetrics(SM_REMOTESESSION)` |
/// | `os_build` | `0` (unknown) | `RtlGetVersion` (not subject to manifest version lies) |
/// | `is_win11` | `false` | `os_build >= 22000` |
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AppearanceSnapshot {
    /// The user's accent color, `#RRGGBB` (uppercase hex).
    pub accent: String,
    /// Apps use the light theme ("Choose your default app mode: Light").
    pub apps_light: bool,
    /// The taskbar/Start/tray use the light theme ("Choose your default Windows mode").
    /// Picks the tray icon variant (docs/07 §7).
    pub taskbar_light: bool,
    /// "Transparency effects" is on. Off means an opaque window.
    pub transparency: bool,
    /// A high contrast theme is active. Means an opaque window and system colors.
    pub high_contrast: bool,
    /// The session is remote (RDP): materials render black or slowly there, so the
    /// window falls back to opaque (docs/07 §2.3).
    pub remote_session: bool,
    /// OS build number (Windows `19045`, `22631`...); `0` when unknown.
    pub os_build: u32,
    /// Windows 11 or later (`os_build >= 22000`).
    pub is_win11: bool,
}

impl Default for AppearanceSnapshot {
    fn default() -> Self {
        RawAppearance::default().into_snapshot()
    }
}

impl AppearanceSnapshot {
    /// What the stub of OSes without an implementation answers: the defaults, but with
    /// `transparency` off, since no window material exists there yet (docs/13 §4).
    pub fn unsupported() -> Self {
        Self {
            transparency: false,
            ..Self::default()
        }
    }

    /// The accent as `(r, g, b)`; `None` if it is not `#RRGGBB`.
    pub fn accent_rgb(&self) -> Option<(u8, u8, u8)> {
        parse_hex_color(&self.accent)
    }
}

/// Values as read from the OS, before defaults; `None` means missing or unreadable.
/// Platform implementations fill it and call [`RawAppearance::into_snapshot`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RawAppearance {
    /// `HKCU\…\DWM\AccentColor`, `0xAABBGGRR`.
    pub accent_abgr: Option<u32>,
    /// `DwmGetColorizationColor`, `0xAARRGGBB`. Used when `accent_abgr` is missing.
    pub colorization_argb: Option<u32>,
    /// `Personalize\AppsUseLightTheme`.
    pub apps_use_light_theme: Option<u32>,
    /// `Personalize\SystemUsesLightTheme`.
    pub system_uses_light_theme: Option<u32>,
    /// `Personalize\EnableTransparency`.
    pub enable_transparency: Option<u32>,
    pub high_contrast: Option<bool>,
    pub remote_session: Option<bool>,
    pub os_build: Option<u32>,
}

impl RawAppearance {
    /// Applies the documented defaults of [`AppearanceSnapshot`] to the missing values.
    pub fn into_snapshot(self) -> AppearanceSnapshot {
        let accent = self
            .accent_abgr
            .map(abgr_to_hex)
            .or_else(|| self.colorization_argb.map(argb_to_hex))
            .unwrap_or_else(|| DEFAULT_ACCENT.to_owned());
        let os_build = self.os_build.unwrap_or(0);
        AppearanceSnapshot {
            accent,
            apps_light: dword_flag(self.apps_use_light_theme, true),
            taskbar_light: dword_flag(self.system_uses_light_theme, false),
            transparency: dword_flag(self.enable_transparency, true),
            high_contrast: self.high_contrast.unwrap_or(false),
            remote_session: self.remote_session.unwrap_or(false),
            os_build,
            is_win11: is_win11_build(os_build),
        }
    }
}

/// A registry switch: non-zero is on, missing is `default`.
pub fn dword_flag(value: Option<u32>, default: bool) -> bool {
    value.map_or(default, |v| v != 0)
}

/// `#RRGGBB` of a color stored as `0xAABBGGRR` (the layout of `COLORREF` plus alpha, used
/// by `DWM\AccentColor` and `Explorer\Accent\AccentColorMenu`). Alpha is ignored.
pub fn abgr_to_hex(value: u32) -> String {
    let [r, g, b, _a] = value.to_le_bytes();
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// `#RRGGBB` of a color stored as `0xAARRGGBB` (`DwmGetColorizationColor`,
/// `DWM\ColorizationColor`). Alpha is ignored.
pub fn argb_to_hex(value: u32) -> String {
    let [b, g, r, _a] = value.to_le_bytes();
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// Whether `build` is Windows 11 or later.
pub fn is_win11_build(build: u32) -> bool {
    build >= WIN11_FIRST_BUILD
}

/// `(r, g, b)` of `#RRGGBB` (either case); `None` for anything else.
pub fn parse_hex_color(text: &str) -> Option<(u8, u8, u8)> {
    let hex = text.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some((channel(0)?, channel(2)?, channel(4)?))
}

/// Appearance facts of the OS.
pub trait SystemAppearance: Send + Sync + fmt::Debug {
    /// The current values. Blocking (call from `spawn_blocking`); never fails nor panics.
    fn snapshot(&self) -> AppearanceSnapshot;
}

/// [`SystemAppearance`] that always answers the same snapshot (tests, previews, OSes
/// without an implementation).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FixedAppearance(pub AppearanceSnapshot);

impl SystemAppearance for FixedAppearance {
    fn snapshot(&self) -> AppearanceSnapshot {
        self.0.clone()
    }
}

/// The appearance of the running OS.
#[cfg(windows)]
pub fn system_appearance() -> Arc<dyn SystemAppearance> {
    Arc::new(super::windows::WindowsSystemAppearance)
}

/// The appearance of the running OS: [`AppearanceSnapshot::unsupported`] until the
/// Linux/macOS ports (docs/13 §4).
#[cfg(not(windows))]
pub fn system_appearance() -> Arc<dyn SystemAppearance> {
    Arc::new(FixedAppearance(AppearanceSnapshot::unsupported()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abgr_dwords_become_rgb_hex() {
        // Default Windows 10 blue: AccentColor = 0xFFD77800 (ABGR) → #0078D7.
        assert_eq!(abgr_to_hex(0xFFD7_7800), "#0078D7");
        assert_eq!(abgr_to_hex(0x0000_00FF), "#FF0000");
        assert_eq!(abgr_to_hex(0x00FF_0000), "#0000FF");
        assert_eq!(abgr_to_hex(0xFF00_FF00), "#00FF00");
        // Alpha never leaks into the color.
        assert_eq!(abgr_to_hex(0x1234_5678), abgr_to_hex(0xFF34_5678));
    }

    #[test]
    fn argb_dwords_become_rgb_hex() {
        // DwmGetColorizationColor of the default blue: 0xC40078D7 (ARGB) → #0078D7.
        assert_eq!(argb_to_hex(0xC400_78D7), "#0078D7");
        assert_eq!(argb_to_hex(0x00FF_0000), "#FF0000");
        assert_eq!(argb_to_hex(0x0000_00FF), "#0000FF");
        assert_eq!(argb_to_hex(0), "#000000");
        assert_eq!(argb_to_hex(u32::MAX), "#FFFFFF");
    }

    #[test]
    fn missing_values_take_the_documented_defaults() {
        let snap = RawAppearance::default().into_snapshot();
        assert_eq!(
            snap,
            AppearanceSnapshot {
                accent: DEFAULT_ACCENT.to_owned(),
                apps_light: true,
                taskbar_light: false,
                transparency: true,
                high_contrast: false,
                remote_session: false,
                os_build: 0,
                is_win11: false,
            }
        );
        assert_eq!(snap, AppearanceSnapshot::default());
        assert_eq!(snap.accent_rgb(), Some((0x0A, 0x84, 0xFF)));
    }

    #[test]
    fn present_values_win_over_defaults() {
        let snap = RawAppearance {
            accent_abgr: Some(0xFF38_4A1F),
            colorization_argb: Some(0xC4FF_0000),
            apps_use_light_theme: Some(0),
            system_uses_light_theme: Some(1),
            enable_transparency: Some(0),
            high_contrast: Some(true),
            remote_session: Some(true),
            os_build: Some(19045),
        }
        .into_snapshot();
        assert_eq!(snap.accent, "#1F4A38");
        assert!(!snap.apps_light);
        assert!(snap.taskbar_light);
        assert!(!snap.transparency);
        assert!(snap.high_contrast);
        assert!(snap.remote_session);
        assert_eq!(snap.os_build, 19045);
        assert!(!snap.is_win11);
    }

    #[test]
    fn colorization_is_the_accent_fallback() {
        let snap = RawAppearance {
            colorization_argb: Some(0xC400_78D7),
            ..RawAppearance::default()
        }
        .into_snapshot();
        assert_eq!(snap.accent, "#0078D7");
    }

    #[test]
    fn any_non_zero_dword_is_on() {
        assert!(dword_flag(Some(1), false));
        assert!(dword_flag(Some(2), false));
        assert!(!dword_flag(Some(0), true));
        assert!(dword_flag(None, true));
        assert!(!dword_flag(None, false));
    }

    #[test]
    fn windows_11_starts_at_build_22000() {
        assert!(!is_win11_build(0));
        assert!(!is_win11_build(19045));
        assert!(!is_win11_build(21999));
        assert!(is_win11_build(22000));
        assert!(is_win11_build(26100));
        let snap = RawAppearance {
            os_build: Some(22631),
            ..RawAppearance::default()
        }
        .into_snapshot();
        assert!(snap.is_win11);
    }

    #[test]
    fn hex_colors_parse_strictly() {
        assert_eq!(parse_hex_color("#0078D7"), Some((0x00, 0x78, 0xD7)));
        assert_eq!(parse_hex_color("#0078d7"), Some((0x00, 0x78, 0xD7)));
        for bad in [
            "0078D7",
            "#078D7",
            "#0078D7FF",
            "#0078DZ",
            "",
            "#",
            "#+1+2+3",
        ] {
            assert_eq!(parse_hex_color(bad), None, "{bad}");
        }
        // Every DWORD formats to something the parser accepts.
        for value in [0, 1, 0xFFD7_7800, 0x8000_0001, u32::MAX] {
            assert!(parse_hex_color(&abgr_to_hex(value)).is_some());
            assert!(parse_hex_color(&argb_to_hex(value)).is_some());
        }
    }

    #[test]
    fn unsupported_is_opaque() {
        let snap = AppearanceSnapshot::unsupported();
        assert!(!snap.transparency);
        assert_eq!(snap.accent, DEFAULT_ACCENT);
        let fixed = FixedAppearance(snap.clone());
        assert_eq!(fixed.snapshot(), snap);
    }

    #[test]
    fn wire_format_is_snake_case() {
        let snap = RawAppearance {
            accent_abgr: Some(0xFFD7_7800),
            remote_session: Some(true),
            os_build: Some(19045),
            ..RawAppearance::default()
        }
        .into_snapshot();
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "accent": "#0078D7",
                "apps_light": true,
                "taskbar_light": false,
                "transparency": true,
                "high_contrast": false,
                "remote_session": true,
                "os_build": 19045,
                "is_win11": false,
            })
        );
        let back: AppearanceSnapshot = serde_json::from_value(json).unwrap();
        assert_eq!(back, snap);
    }

    #[test]
    fn the_running_os_answers_without_panicking() {
        let snap = system_appearance().snapshot();
        assert!(snap.accent_rgb().is_some(), "{snap:?}");
        assert_eq!(snap.is_win11, is_win11_build(snap.os_build));
    }
}
