//! COM plumbing shared by the WMI client ([`super::wmi`]) and the firewall API
//! ([`super::firewall`]): per-thread initialization, owned `VARIANT`s and error mapping.

use std::ffi::c_void;
use std::mem::ManuallyDrop;

use ::windows::Win32::Foundation::{
    E_ACCESSDENIED, RPC_E_CHANGED_MODE, VARIANT_FALSE, VARIANT_TRUE,
};
use ::windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use ::windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
use ::windows::Win32::System::Variant::{
    VARENUM, VARIANT, VARIANT_0_0_0, VT_ARRAY, VT_BOOL, VT_BSTR, VariantClear,
};
use ::windows::Win32::System::Wmi::{
    IWbemClassObject, WBEM_E_ACCESS_DENIED, WBEM_E_INVALID_CLASS, WBEM_E_INVALID_METHOD,
    WBEM_E_INVALID_NAMESPACE,
};
use ::windows::core::{BSTR, HRESULT, HSTRING, IUnknown, Interface};

use crate::error::{ForgeError, Result};

/// `REGDB_E_CLASSNOTREG`: the COM class is not registered (component missing).
const REGDB_E_CLASSNOTREG: HRESULT = HRESULT(0x8004_0154_u32 as i32);

/// COM initialized (multithreaded) on this thread; uninitialized on drop when we did it.
pub(crate) struct ComGuard(bool);

impl ComGuard {
    pub(crate) fn init() -> Self {
        // SAFETY: plain initialization of the calling thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // S_FALSE (already initialized) needs a matching uninitialize; RPC_E_CHANGED_MODE
        // (already apartment threaded) does not, and COM calls still work there.
        Self(hr.is_ok() && hr != RPC_E_CHANGED_MODE)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx of `init` on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// A value passed to a COM method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComArg {
    Str(String),
    StrArray(Vec<String>),
    Bool(bool),
}

/// An owned `VARIANT`, cleared on drop (`VariantClear` frees the BSTR, the SAFEARRAY or
/// releases the interface it holds).
pub(crate) struct Variant(pub(crate) VARIANT);

impl Variant {
    fn with(vt: VARENUM, set: impl FnOnce(&mut VARIANT_0_0_0)) -> Self {
        let mut v = VARIANT::default();
        // SAFETY: `v` is a zeroed VARIANT; the tag and the matching union member are set
        // together, so the value is always consistent for VariantClear.
        unsafe {
            let inner = &mut *v.Anonymous.Anonymous;
            inner.vt = vt;
            set(&mut inner.Anonymous);
        }
        Self(v)
    }

    pub(crate) fn from_arg(arg: &ComArg) -> Result<Self> {
        Ok(match arg {
            ComArg::Str(text) => Self::with(VT_BSTR, |u| {
                u.bstrVal = ManuallyDrop::new(BSTR::from(text.as_str()));
            }),
            ComArg::Bool(value) => Self::with(VT_BOOL, |u| {
                u.boolVal = if *value { VARIANT_TRUE } else { VARIANT_FALSE };
            }),
            ComArg::StrArray(items) => {
                let len = u32::try_from(items.len())
                    .map_err(|_| ForgeError::InvalidInput("too many array items".to_owned()))?;
                // SAFETY: plain allocation; checked for null below.
                let array = unsafe { SafeArrayCreateVector(VT_BSTR, 0, len) };
                if array.is_null() {
                    return Err(ForgeError::Internal(
                        "SafeArrayCreateVector failed (out of memory)".to_owned(),
                    ));
                }
                // The array belongs to the VARIANT from here on (freed by VariantClear).
                let variant = Self::with(VARENUM(VT_ARRAY.0 | VT_BSTR.0), |u| u.parray = array);
                for (index, item) in items.iter().enumerate() {
                    let text = BSTR::from(item.as_str());
                    let index = index as i32;
                    // SAFETY: `array` is a valid one-dimensional VT_BSTR array of `len`
                    // elements; for VT_BSTR the API stores a copy of the string.
                    unsafe { SafeArrayPutElement(array, &index, text.as_ptr().cast::<c_void>()) }
                        .map_err(|e| com_error("filling a string array", &e))?;
                }
                variant
            }
        })
    }

    /// The type tag.
    pub(crate) fn vt(&self) -> VARENUM {
        // SAFETY: every VARIANT starts with its tag.
        unsafe { self.0.Anonymous.Anonymous.vt }
    }
}

impl Drop for Variant {
    fn drop(&mut self) {
        // SAFETY: the VARIANT is either empty, built by `with` with a consistent tag, or
        // filled by a COM call that hands its content to us.
        unsafe {
            let _ = VariantClear(&mut self.0);
        }
    }
}

/// Maps a COM/WMI failure to a [`ForgeError`]: access denied → `needs_elevation`, missing
/// namespace/class/method/component → `unsupported_platform`, anything else → `io` with
/// the HRESULT and the provider's message.
pub(crate) fn com_error(context: &str, err: &::windows::core::Error) -> ForgeError {
    let code = err.code();
    let message = error_text(err);
    let detail = if message.is_empty() {
        format!("HRESULT 0x{:08X}", code.0 as u32)
    } else {
        format!("{message} (HRESULT 0x{:08X})", code.0 as u32)
    };
    if code == E_ACCESSDENIED || code == HRESULT(WBEM_E_ACCESS_DENIED.0) {
        ForgeError::NeedsElevation(format!("{context}: access denied: {detail}"))
    } else if [
        HRESULT(WBEM_E_INVALID_NAMESPACE.0),
        HRESULT(WBEM_E_INVALID_CLASS.0),
        HRESULT(WBEM_E_INVALID_METHOD.0),
        REGDB_E_CLASSNOTREG,
    ]
    .contains(&code)
    {
        ForgeError::UnsupportedPlatform(format!("{context}: {detail}"))
    } else {
        ForgeError::io(context, std::io::Error::other(detail))
    }
}

/// Human readable text of a COM failure: the WMI extended error (`Message` of an
/// `MSFT_WmiError`, `Description` of an `__ExtendedStatus`) when the error object is a
/// WMI one, else its `IErrorInfo` description, else the system text of the HRESULT.
fn error_text(err: &::windows::core::Error) -> String {
    let raw = err.as_ptr();
    // SAFETY: `raw` is null or the error object kept alive by `err`.
    if let Some(object) = unsafe { IUnknown::from_raw_borrowed(&raw) } {
        if let Ok(wmi) = object.cast::<IWbemClassObject>() {
            for property in ["Message", "Description"] {
                let mut value = Variant(VARIANT::default());
                // SAFETY: valid object and property name; `value` receives a copy.
                let got = unsafe { wmi.Get(&HSTRING::from(property), 0, &mut value.0, None, None) };
                if got.is_ok() && value.vt() == VT_BSTR {
                    // SAFETY: VT_BSTR: the union holds a BSTR owned by `value`.
                    let text =
                        unsafe { (*value.0.Anonymous.Anonymous).Anonymous.bstrVal.to_string() };
                    let text = text.trim();
                    if !text.is_empty() {
                        return text.to_owned();
                    }
                }
            }
        }
    }
    let message = err.message();
    let message = message.trim();
    if !message.is_empty() {
        return message.to_owned();
    }
    err.code().message().trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_are_built_and_cleared() {
        for arg in [
            ComArg::Str("x".into()),
            ComArg::Bool(true),
            ComArg::StrArray(vec![".test".into(), "ação".into()]),
            ComArg::StrArray(vec![]),
        ] {
            let v = Variant::from_arg(&arg).unwrap();
            match arg {
                ComArg::Str(_) => assert_eq!(v.vt(), VT_BSTR),
                ComArg::Bool(_) => assert_eq!(v.vt(), VT_BOOL),
                ComArg::StrArray(_) => assert_eq!(v.vt().0, VT_ARRAY.0 | VT_BSTR.0),
            }
        }
        assert_eq!(Variant(VARIANT::default()).vt().0, 0);
    }

    #[test]
    fn errors_are_classified() {
        let denied = ::windows::core::Error::from_hresult(E_ACCESSDENIED);
        assert_eq!(com_error("x", &denied).code(), "needs_elevation");
        let wbem_denied = ::windows::core::Error::from_hresult(HRESULT(WBEM_E_ACCESS_DENIED.0));
        assert_eq!(com_error("x", &wbem_denied).code(), "needs_elevation");
        let missing = ::windows::core::Error::from_hresult(REGDB_E_CLASSNOTREG);
        assert_eq!(com_error("x", &missing).code(), "unsupported_platform");
        let other = ::windows::core::Error::from_hresult(HRESULT(0x8004_1001_u32 as i32));
        let err = com_error("calling", &other);
        assert_eq!(err.code(), "io");
        assert!(err.to_string().contains("0x80041001"), "{err}");
    }
}
