//! [`SystemAppearance`] for Windows (docs/07 §2.3, §7):
//!
//! * `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize`:
//!   `AppsUseLightTheme`, `SystemUsesLightTheme`, `EnableTransparency` (`REG_DWORD`);
//! * `HKCU\Software\Microsoft\Windows\DWM\AccentColor` (`0xAABBGGRR`), falling back to
//!   `DwmGetColorizationColor` (`0xAARRGGBB`);
//! * `SystemParametersInfoW(SPI_GETHIGHCONTRAST)` for high contrast;
//! * `GetSystemMetrics(SM_REMOTESESSION)` for RDP;
//! * `RtlGetVersion` (ntdll) for the build: unlike `GetVersionExW` it does not depend on
//!   the `supportedOS` entries of the executable's manifest.
//!
//! Every read is independent: a failure leaves that value `None` and
//! [`RawAppearance::into_snapshot`] applies the documented default. Blocking but fast
//! (in-process registry reads plus one call into DWM).

use std::ffi::c_void;

use ::windows::Win32::Foundation::{ERROR_SUCCESS, NTSTATUS, WIN32_ERROR};
use ::windows::Win32::Graphics::Dwm::DwmGetColorizationColor;
use ::windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use ::windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use ::windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_REMOTESESSION, SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SystemParametersInfoW,
};
use ::windows::core::{BOOL, PCWSTR, w};

use crate::platform::appearance::{AppearanceSnapshot, RawAppearance, SystemAppearance};

/// `RtlGetVersion` declared here rather than through the `Wdk_System_SystemServices`
/// feature of the `windows` crate (22k lines compiled for one function), the same trade
/// as the local `FILE_CS_FLAG_CASE_SENSITIVE_DIR` of `case_sensitive_fs.rs`.
mod ntdll {
    ::windows::core::link!("ntdll.dll" "system" fn RtlGetVersion(info: *mut ::windows::Win32::System::SystemInformation::OSVERSIONINFOW) -> ::windows::Win32::Foundation::NTSTATUS);
}

/// `HIGHCONTRASTW` (winuser.h), local to avoid the large `Win32_UI_Accessibility` feature
/// for one struct. Layout: `UINT cbSize; DWORD dwFlags; LPWSTR lpszDefaultScheme`.
#[repr(C)]
struct HighContrastW {
    cb_size: u32,
    flags: u32,
    default_scheme: *mut u16,
}

/// `HCF_HIGHCONTRASTON` (winuser.h).
const HCF_HIGHCONTRASTON: u32 = 0x0000_0001;

const PERSONALIZE_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
const DWM_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\DWM");

/// [`SystemAppearance`] for Windows.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsSystemAppearance;

impl SystemAppearance for WindowsSystemAppearance {
    fn snapshot(&self) -> AppearanceSnapshot {
        read_raw().into_snapshot()
    }
}

/// Every value the snapshot needs, as read.
fn read_raw() -> RawAppearance {
    RawAppearance {
        accent_abgr: hkcu_dword(DWM_KEY, w!("AccentColor")),
        colorization_argb: colorization_color(),
        apps_use_light_theme: hkcu_dword(PERSONALIZE_KEY, w!("AppsUseLightTheme")),
        system_uses_light_theme: hkcu_dword(PERSONALIZE_KEY, w!("SystemUsesLightTheme")),
        enable_transparency: hkcu_dword(PERSONALIZE_KEY, w!("EnableTransparency")),
        high_contrast: high_contrast(),
        remote_session: Some(remote_session()),
        os_build: os_build(),
    }
}

/// A `REG_DWORD` value under `HKEY_CURRENT_USER`; `None` when the key or value is missing
/// or has another type.
fn hkcu_dword(key: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut data = 0_u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: valid NUL-terminated strings; `data` holds `size` bytes.
    let status: WIN32_ERROR = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut data).cast::<c_void>()),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS).then_some(data)
}

/// `DwmGetColorizationColor`; `None` when DWM does not answer.
fn colorization_color() -> Option<u32> {
    let mut color = 0_u32;
    let mut opaque = BOOL::default();
    // SAFETY: both out-pointers are valid and writable.
    unsafe { DwmGetColorizationColor(&mut color, &mut opaque) }
        .ok()
        .map(|()| color)
}

/// Whether a high contrast theme is on; `None` when the query fails.
fn high_contrast() -> Option<bool> {
    let mut info = HighContrastW {
        cb_size: size_of::<HighContrastW>() as u32,
        flags: 0,
        default_scheme: std::ptr::null_mut(),
    };
    // SAFETY: SPI_GETHIGHCONTRAST writes a HIGHCONTRASTW of `uiParam` bytes into `info`,
    // whose layout matches winuser.h; `lpszDefaultScheme` points into system memory and
    // is not read nor freed here.
    unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            size_of::<HighContrastW>() as u32,
            Some((&raw mut info).cast::<c_void>()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .ok()
    .map(|()| info.flags & HCF_HIGHCONTRASTON != 0)
}

/// Whether the session is a Remote Desktop session.
fn remote_session() -> bool {
    // SAFETY: plain query.
    unsafe { GetSystemMetrics(SM_REMOTESESSION) != 0 }
}

/// Build number of the running Windows; `None` when `RtlGetVersion` fails.
fn os_build() -> Option<u32> {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is a writable OSVERSIONINFOW with its size set, as required.
    let status: NTSTATUS = unsafe { ntdll::RtlGetVersion(&mut info) };
    (status.is_ok() && info.dwBuildNumber > 0).then_some(info.dwBuildNumber)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_high_contrast_struct_matches_winuser_h() {
        // 4 + 4 + pointer, pointer-aligned: 16 bytes on x64.
        assert_eq!(
            size_of::<HighContrastW>(),
            8 + size_of::<*mut u16>(),
            "HIGHCONTRASTW layout"
        );
    }

    #[test]
    fn every_source_answers_on_a_desktop() {
        let raw = read_raw();
        assert!(raw.os_build.is_some_and(|b| b >= 10240), "{raw:?}");
        assert!(raw.high_contrast.is_some(), "{raw:?}");
        // DWM and the accent value may be absent in a service session (CI): not asserted.
    }

    #[test]
    fn missing_registry_values_read_as_none() {
        assert_eq!(hkcu_dword(DWM_KEY, w!("ForgeboxNoSuchValue")), None);
        assert_eq!(
            hkcu_dword(w!(r"Software\Forgebox\NoSuchKey"), w!("AccentColor")),
            None
        );
    }
}
