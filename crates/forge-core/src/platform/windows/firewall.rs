//! Windows Defender Firewall rules through the COM API (`INetFwPolicy2`), for the elevated
//! helper's `firewall.add`/`firewall.remove` (docs/11 §1, ADR-0023). No `netsh`.
//!
//! Reading the rules works without elevation; adding or removing fails with
//! `E_ACCESSDENIED` (→ `needs_elevation`) unless the process is elevated.

use ::windows::Win32::Foundation::VARIANT_TRUE;
use ::windows::Win32::NetworkManagement::WindowsFirewall::{
    INetFwPolicy2, INetFwRule, INetFwRules, NET_FW_ACTION_ALLOW, NET_FW_IP_PROTOCOL_TCP,
    NET_FW_PROFILE2_DOMAIN, NET_FW_PROFILE2_PRIVATE, NET_FW_RULE_DIR_IN, NetFwPolicy2, NetFwRule,
};
use ::windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IDispatch};
use ::windows::Win32::System::Ole::IEnumVARIANT;
use ::windows::Win32::System::Variant::{VARIANT, VT_DISPATCH};
use ::windows::core::{BSTR, Interface};

use super::com::{ComGuard, Variant, com_error};
use crate::error::{ForgeError, Result};
use crate::platform::privileged::FirewallRuleSpec;

/// Profiles the helper's rules apply to: private and domain networks only. A LAN exposure
/// never opens the port on networks Windows classifies as public (ADR-0023).
pub(crate) const RULE_PROFILES: i32 = NET_FW_PROFILE2_PRIVATE.0 | NET_FW_PROFILE2_DOMAIN.0;

/// Upper bound of removals for one name (rules are only ever created one per name).
const MAX_SAME_NAME: usize = 256;

struct Firewall {
    rules: INetFwRules,
    // Dropped last.
    _com: ComGuard,
}

impl Firewall {
    fn open() -> Result<Self> {
        let com = ComGuard::init();
        // SAFETY: COM initialized on this thread; in-process activation of the policy.
        let policy: INetFwPolicy2 =
            unsafe { CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| com_error("opening the firewall policy", &e))?;
        // SAFETY: valid interface pointer.
        let rules =
            unsafe { policy.Rules() }.map_err(|e| com_error("listing the firewall rules", &e))?;
        Ok(Self { rules, _com: com })
    }

    /// Number of rules named exactly `name`.
    fn count(&self, name: &str) -> Result<usize> {
        let context = "listing the firewall rules";
        // SAFETY: valid collection; `_NewEnum` returns a fresh enumerator.
        let unknown = unsafe { self.rules._NewEnum() }.map_err(|e| com_error(context, &e))?;
        let items: IEnumVARIANT = unknown.cast().map_err(|e| com_error(context, &e))?;
        let mut count = 0;
        loop {
            let mut slot = [VARIANT::default()];
            let mut fetched = 0_u32;
            // SAFETY: one writable VARIANT slot; `fetched` receives how many were filled.
            let hr = unsafe { items.Next(&mut slot, &mut fetched) };
            let [raw] = slot;
            // Owns whatever Next stored (cleared on drop).
            let item = Variant(raw);
            if hr.is_err() {
                return Err(com_error(
                    context,
                    &::windows::core::Error::from_hresult(hr),
                ));
            }
            if fetched == 0 {
                break;
            }
            if item.vt() != VT_DISPATCH {
                continue;
            }
            // SAFETY: VT_DISPATCH: the union holds an IDispatch reference owned by `item`.
            let dispatch: Option<IDispatch> =
                unsafe { Option::clone(&(*item.0.Anonymous.Anonymous).Anonymous.pdispVal) };
            let Some(rule) = dispatch.and_then(|d| d.cast::<INetFwRule>().ok()) else {
                continue;
            };
            // SAFETY: valid rule interface.
            if unsafe { rule.Name() }.is_ok_and(|n| n == name) {
                count += 1;
            }
        }
        Ok(count)
    }

    fn remove_all(&self, name: &str) -> Result<usize> {
        let bstr = BSTR::from(name);
        let mut removed = 0;
        // `Item` finds one rule by name; `Remove` deletes one. Loop until none is left.
        // SAFETY (both calls): valid collection and BSTR.
        while unsafe { self.rules.Item(&bstr) }.is_ok() {
            if removed == MAX_SAME_NAME {
                return Err(ForgeError::Internal(format!(
                    "firewall rule {name:?} is still present after {MAX_SAME_NAME} removals"
                )));
            }
            unsafe { self.rules.Remove(&bstr) }
                .map_err(|e| com_error(&format!("removing firewall rule {name:?}"), &e))?;
            removed += 1;
        }
        Ok(removed)
    }

    fn add(&self, spec: &FirewallRuleSpec) -> Result<()> {
        let context = format!("adding firewall rule {:?}", spec.name);
        let exe = spec.exe.to_str().ok_or_else(|| {
            ForgeError::InvalidInput(format!("{} is not valid Unicode", spec.exe.display()))
        })?;
        // SAFETY: COM initialized by `open`; in-process activation of a rule object.
        let rule: INetFwRule = unsafe { CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| com_error(&context, &e))?;
        let configure = || -> ::windows::core::Result<()> {
            // SAFETY: valid rule object; every setter takes plain values or BSTRs. The
            // protocol is set before the ports, as the API requires.
            unsafe {
                rule.SetName(&BSTR::from(spec.name.as_str()))?;
                rule.SetDescription(&BSTR::from(spec.description.as_str()))?;
                rule.SetGrouping(&BSTR::from(spec.grouping.as_str()))?;
                rule.SetApplicationName(&BSTR::from(exe))?;
                rule.SetProtocol(NET_FW_IP_PROTOCOL_TCP.0)?;
                rule.SetLocalPorts(&BSTR::from(spec.port.to_string()))?;
                rule.SetDirection(NET_FW_RULE_DIR_IN)?;
                rule.SetAction(NET_FW_ACTION_ALLOW)?;
                rule.SetProfiles(RULE_PROFILES)?;
                rule.SetEnabled(VARIANT_TRUE)?;
            }
            Ok(())
        };
        configure().map_err(|e| com_error(&context, &e))?;
        // SAFETY: valid collection and rule.
        unsafe { self.rules.Add(&rule) }.map_err(|e| com_error(&context, &e))
    }
}

/// Number of firewall rules named `name`. No elevation needed.
pub(crate) fn rule_count(name: &str) -> Result<usize> {
    Firewall::open()?.count(name)
}

/// Replaces every rule named `spec.name` by one inbound TCP allow rule.
pub(crate) fn add_rule(spec: &FirewallRuleSpec) -> Result<usize> {
    let firewall = Firewall::open()?;
    let replaced = firewall.remove_all(&spec.name)?;
    firewall.add(spec)?;
    Ok(replaced)
}

/// Removes every rule named `name`; how many there were.
pub(crate) fn remove_rules(name: &str) -> Result<usize> {
    Firewall::open()?.remove_all(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rules_without_elevation() {
        assert_eq!(rule_count("Forgebox-test-never-created-0123").unwrap(), 0);
        // Windows ships many rules; at least one name is repeated or present. Just make
        // sure the enumeration itself walks the collection.
        let firewall = Firewall::open().unwrap();
        // SAFETY: valid collection.
        let total = unsafe { firewall.rules.Count() }.unwrap();
        assert!(total >= 0);
    }
}
