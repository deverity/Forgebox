//! Minimal WMI client for the elevated helper (ADR-0024): calls **static methods** of CIM
//! classes, the same entry points the PowerShell cmdlets use, without starting a shell.
//!
//! * `PS_DnsClientNrptRule` (`root/Microsoft/Windows/DNS`): `Add`/`Remove`, behind
//!   `Add-DnsClientNrptRule`/`Remove-DnsClientNrptRule`.
//! * `MSFT_MpPreference` (`root/Microsoft/Windows/Defender`): `Add`/`Remove`, behind
//!   `Add-MpPreference`/`Remove-MpPreference`.
//!
//! Only string, string array and boolean parameters are supported ([`ComArg`]): that is
//! all those methods need. Everything is blocking (COM calls into `WmiPrvSE.exe`); COM is
//! initialized on the calling thread for the lifetime of a [`Wmi`] value.

use ::windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, CoSetProxyBlanket, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
    RPC_C_IMP_LEVEL_IMPERSONATE,
};
use ::windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemLocator, IWbemServices, WBEM_E_NOT_FOUND,
    WBEM_FLAG_RETURN_WBEM_COMPLETE, WBEM_GENERIC_FLAG_TYPE, WbemLocator,
};
use ::windows::core::{BSTR, HRESULT, HSTRING, PCWSTR};

use super::com::{ComArg, ComGuard, Variant, com_error};
use crate::error::{ForgeError, Result};

/// `RPC_C_AUTHN_WINNT` (rpcdce.h): NTLM/Kerberos negotiation for the WMI proxy.
const RPC_C_AUTHN_WINNT: u32 = 10;
/// `RPC_C_AUTHZ_NONE`.
const RPC_C_AUTHZ_NONE: u32 = 0;

/// `WBEM_E_NOT_FOUND` while looking up a class or a method means this Windows has no such
/// provider (`unsupported_platform`); anything else goes through [`com_error`].
fn missing_or_error(context: &str, what: &str, err: &::windows::core::Error) -> ForgeError {
    if err.code() == HRESULT(WBEM_E_NOT_FOUND.0) {
        ForgeError::UnsupportedPlatform(format!("{context}: {what} not found"))
    } else {
        com_error(context, err)
    }
}

/// A connection to one WMI namespace.
pub(crate) struct Wmi {
    namespace: String,
    services: IWbemServices,
    // Dropped last: COM stays initialized while `services` is released.
    _com: ComGuard,
}

impl Wmi {
    /// Connects to `namespace` (`root/Microsoft/Windows/DNS`) as the current user, with
    /// impersonation (the providers act with the caller's rights).
    pub(crate) fn connect(namespace: &str) -> Result<Self> {
        let com = ComGuard::init();
        let context = format!("connecting to WMI namespace {namespace}");
        // SAFETY: COM is initialized on this thread; standard in-process activation.
        let locator: IWbemLocator =
            unsafe { CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| com_error(&context, &e))?;
        let empty = BSTR::new();
        // SAFETY: valid BSTRs; local connection with the caller's credentials.
        let services = unsafe {
            locator.ConnectServer(
                &BSTR::from(namespace),
                &empty,
                &empty,
                &empty,
                0,
                &empty,
                None,
            )
        }
        .map_err(|e| com_error(&context, &e))?;
        // SAFETY: `services` is a valid proxy; the blanket only sets its security options.
        unsafe {
            CoSetProxyBlanket(
                &services,
                RPC_C_AUTHN_WINNT,
                RPC_C_AUTHZ_NONE,
                PCWSTR::null(),
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
        }
        .map_err(|e| com_error(&context, &e))?;
        Ok(Self {
            namespace: namespace.to_owned(),
            services,
            _com: com,
        })
    }

    /// Calls the static method `class.method` with `args` (names of the method's input
    /// parameters). The method's own failure (the `Write-Error` of a cmdlet provider)
    /// comes back as an `Err` carrying the provider's message.
    pub(crate) fn call_static(
        &self,
        class: &str,
        method: &str,
        args: &[(&str, ComArg)],
    ) -> Result<()> {
        let context = format!("WMI {}:{class}.{method}", self.namespace);
        let class_name = BSTR::from(class);
        let mut class_object: Option<IWbemClassObject> = None;
        // SAFETY: valid BSTR and out pointer; synchronous call.
        unsafe {
            self.services.GetObject(
                &class_name,
                WBEM_FLAG_RETURN_WBEM_COMPLETE,
                None,
                Some(&mut class_object),
                None,
            )
        }
        .map_err(|e| missing_or_error(&context, "class", &e))?;
        let class_object = class_object.ok_or_else(|| {
            ForgeError::UnsupportedPlatform(format!("{context}: class not returned"))
        })?;
        let mut in_signature: Option<IWbemClassObject> = None;
        // SAFETY: valid method name; only the input signature is requested.
        unsafe {
            class_object.GetMethod(
                &HSTRING::from(method),
                0,
                &mut in_signature,
                std::ptr::null_mut(),
            )
        }
        .map_err(|e| missing_or_error(&context, "method", &e))?;
        let in_params = match in_signature {
            Some(signature) => {
                // SAFETY: a method signature is a class; SpawnInstance creates its value.
                let params =
                    unsafe { signature.SpawnInstance(0) }.map_err(|e| com_error(&context, &e))?;
                for (name, arg) in args {
                    let value = Variant::from_arg(arg)?;
                    // SAFETY: valid name and VARIANT; type 0 = the declared type.
                    unsafe { params.Put(&HSTRING::from(*name), 0, &value.0, 0) }
                        .map_err(|e| com_error(&format!("{context} parameter {name}"), &e))?;
                }
                Some(params)
            }
            None if args.is_empty() => None,
            None => {
                return Err(ForgeError::UnsupportedPlatform(format!(
                    "{context}: the method takes no parameters"
                )));
            }
        };
        let mut out: Option<IWbemClassObject> = None;
        // SAFETY: valid BSTRs and parameter object; synchronous call.
        unsafe {
            self.services.ExecMethod(
                &class_name,
                &BSTR::from(method),
                WBEM_GENERIC_FLAG_TYPE(0),
                None,
                in_params.as_ref(),
                Some(&mut out),
                None,
            )
        }
        .map_err(|e| com_error(&context, &e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::privileged::{NRPT_CLASS, NRPT_NAMESPACE};
    use super::*;

    #[test]
    fn missing_namespaces_are_unsupported() {
        let err = Wmi::connect("root/Forgebox/NoSuchNamespace")
            .err()
            .expect("namespace must not exist");
        assert_eq!(err.code(), "unsupported_platform", "{err}");
    }

    #[test]
    fn reads_through_a_static_method() {
        // `Get` of the NRPT provider is what `Get-DnsClientNrptRule` calls: no elevation.
        let wmi = Wmi::connect(NRPT_NAMESPACE).unwrap();
        wmi.call_static(NRPT_CLASS, "Get", &[]).unwrap();
        let err = wmi
            .call_static(NRPT_CLASS, "NoSuchMethod", &[])
            .unwrap_err();
        assert_eq!(err.code(), "unsupported_platform", "{err}");
        let err = wmi.call_static("PS_NoSuchClass", "Get", &[]).unwrap_err();
        assert_eq!(err.code(), "unsupported_platform", "{err}");
    }
}
