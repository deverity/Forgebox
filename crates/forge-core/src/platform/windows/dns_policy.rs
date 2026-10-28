//! [`DnsPolicy`] for Windows (docs/06 §5, ADR-0004).
//!
//! * NRPT rules are read without elevation from the registry keys behind
//!   `Get-DnsClientNrptRule`: the local table
//!   `HKLM\SYSTEM\CurrentControlSet\Services\Dnscache\Parameters\DnsPolicyConfig` and the
//!   Group Policy one `HKLM\SOFTWARE\Policies\Microsoft\Windows NT\DNSClient\DnsPolicyConfig`.
//!   Each rule is a subkey with `Name` (`REG_MULTI_SZ`, the namespaces),
//!   `GenericDNSServers` (`REG_SZ`, `;`-separated) and optional `Comment`/`DisplayName`.
//!   While the Group Policy key exists, **even with no rule in it**, Windows ignores the
//!   local ones ([`crate::platform::NrptTable::group_policy_table`], ADR-0033).
//! * The OS resolver is asked with `DnsQuery_W` (type `A`, through the DNS Client service:
//!   NRPT, cache and `hosts` apply as for any application), to verify that a rule is
//!   really followed.
//! * `hosts` lives in `DataBasePath` of `HKLM\SYSTEM\CurrentControlSet\Services\Tcpip\Parameters`
//!   (normally `%SystemRoot%\System32\drivers\etc`).
//! * Changes go through `forgebox-helper` ([`HelperClient`]): `nrpt.add`, `nrpt.remove`,
//!   `hosts.set_block`.
//! * The resolver cache is flushed with `DnsFlushResolverCache` (exported by `dnsapi.dll`,
//!   what `ipconfig /flushdns` calls, no elevation), loaded from System32 at run time.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use ::windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, FreeLibrary,
    WIN32_ERROR,
};
use ::windows::Win32::NetworkManagement::Dns::{
    DNS_QUERY_STANDARD, DNS_QUERY_TREAT_AS_FQDN, DNS_RECORDA, DNS_TYPE_A, DnsFree,
    DnsFreeRecordList, DnsQuery_W,
};
use ::windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use ::windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_ROUTINE_FLAGS, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ,
    RRF_RT_REG_MULTI_SZ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
};
use ::windows::core::{BOOL, PCWSTR, PWSTR, s, w};

use super::process::{os_error, system_directory};
use crate::error::{ForgeError, Result};
use crate::helper::{HelperClient, HelperOp, HostsEntry};
use crate::platform::{DnsPolicy, NrptApplied, NrptRule, NrptSource, NrptTable};

const LOCAL_RULES: PCWSTR =
    w!(r"SYSTEM\CurrentControlSet\Services\Dnscache\Parameters\DnsPolicyConfig");
const GPO_RULES: PCWSTR = w!(r"SOFTWARE\Policies\Microsoft\Windows NT\DNSClient\DnsPolicyConfig");
const TCPIP_PARAMETERS: PCWSTR = w!(r"SYSTEM\CurrentControlSet\Services\Tcpip\Parameters");

/// [`DnsPolicy`] backed by the registry, `hosts` and the elevated helper.
#[derive(Debug, Clone)]
pub struct WindowsDnsPolicy {
    helper: HelperClient,
}

impl WindowsDnsPolicy {
    pub fn new(helper: HelperClient) -> Self {
        Self { helper }
    }
}

/// An open registry key, closed on drop.
struct Key(HKEY);

impl Key {
    /// Opens `HKLM\<path>` for reading; `None` when it does not exist.
    fn open(path: PCWSTR) -> Result<Option<Self>> {
        let mut key = HKEY::default();
        // SAFETY: valid NUL-terminated path; `key` receives the handle.
        let status = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, path, None, KEY_READ, &mut key) };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(status, "opening an NRPT registry key")?;
        Ok(Some(Self(key)))
    }

    /// Names of the direct subkeys.
    fn subkeys(&self) -> Result<Vec<Vec<u16>>> {
        let mut out = Vec::new();
        for index in 0.. {
            // Registry key names have at most 255 characters.
            let mut name = [0_u16; 256];
            let mut len = name.len() as u32;
            // SAFETY: `name` holds `len` u16s; the other outputs are not requested.
            let status = unsafe {
                RegEnumKeyExW(
                    self.0,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            check(status, "listing NRPT rules")?;
            let mut key: Vec<u16> = name[..len as usize].to_vec();
            key.push(0);
            out.push(key);
        }
        Ok(out)
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from RegOpenKeyExW and is closed once.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn check(status: WIN32_ERROR, what: &str) -> Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(ForgeError::io(
            what,
            std::io::Error::from_raw_os_error(status.0 as i32),
        ))
    }
}

/// Raw UTF-16 data (without the final NUL) of a string value; `None` when missing or of
/// another type.
fn string_value(
    key: HKEY,
    subkey: PCWSTR,
    value: PCWSTR,
    flags: REG_ROUTINE_FLAGS,
) -> Option<Vec<u16>> {
    let mut size = 0_u32;
    loop {
        let mut buffer = vec![0_u16; (size as usize).div_ceil(2).max(1)];
        let mut bytes = (buffer.len() * 2) as u32;
        // SAFETY: `buffer` holds `bytes` bytes; strings are NUL-terminated.
        let status = unsafe {
            RegGetValueW(
                key,
                subkey,
                value,
                flags,
                None,
                Some(buffer.as_mut_ptr().cast()),
                Some(&mut bytes),
            )
        };
        if status == ERROR_MORE_DATA && bytes > size {
            size = bytes;
            continue;
        }
        if status != ERROR_SUCCESS {
            return None;
        }
        buffer.truncate(bytes as usize / 2);
        while buffer.last() == Some(&0) {
            buffer.pop();
        }
        return Some(buffer);
    }
}

fn read_sz(key: HKEY, subkey: PCWSTR, value: PCWSTR) -> Option<String> {
    string_value(key, subkey, value, RRF_RT_REG_SZ)
        .map(|w| String::from_utf16_lossy(&w))
        .filter(|s| !s.trim().is_empty())
}

fn read_multi_sz(key: HKEY, subkey: PCWSTR, value: PCWSTR) -> Vec<String> {
    string_value(key, subkey, value, RRF_RT_REG_MULTI_SZ)
        .map(|w| {
            w.split(|c| *c == 0)
                .map(String::from_utf16_lossy)
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// `1.1.1.1;8.8.8.8` (also tolerates spaces and commas).
fn parse_servers(text: &str) -> Vec<IpAddr> {
    text.split([';', ',', ' '])
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

/// The rules under `HKLM\<path>`; `None` when the key does not exist.
fn read_rules(path: PCWSTR, source: NrptSource) -> Result<Option<Vec<NrptRule>>> {
    let Some(key) = Key::open(path)? else {
        return Ok(None);
    };
    let mut rules = Vec::new();
    for subkey in key.subkeys()? {
        let sub = PCWSTR(subkey.as_ptr());
        let namespaces = read_multi_sz(key.0, sub, w!("Name"))
            .into_iter()
            .map(|n| n.trim().to_ascii_lowercase())
            .collect();
        let nameservers = read_sz(key.0, sub, w!("GenericDNSServers"))
            .map(|s| parse_servers(&s))
            .unwrap_or_default();
        let comment =
            read_sz(key.0, sub, w!("Comment")).or_else(|| read_sz(key.0, sub, w!("DisplayName")));
        rules.push(NrptRule {
            id: String::from_utf16_lossy(&subkey[..subkey.len() - 1]),
            namespaces,
            nameservers,
            source,
            comment,
        });
    }
    Ok(Some(rules))
}

/// Expands `%VAR%` with the process environment (unknown variables are kept).
fn expand_env(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(value) if !name.is_empty() => out.push_str(&value),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// `DnsFlushResolverCache` signature (dnsapi.dll, undocumented but stable since XP).
type FlushFn = unsafe extern "system" fn() -> BOOL;

/// The NRPT table: the local rules, then the Group Policy ones, and whether the Group
/// Policy key exists (an empty one still disables the local table). No elevation.
pub(crate) fn read_nrpt_table() -> Result<NrptTable> {
    let mut rules = read_rules(LOCAL_RULES, NrptSource::Local)?.unwrap_or_default();
    let group_policy = read_rules(GPO_RULES, NrptSource::GroupPolicy)?;
    let group_policy_table = group_policy.is_some();
    rules.extend(group_policy.unwrap_or_default());
    Ok(NrptTable {
        rules,
        group_policy_table,
    })
}

/// IPv4 addresses of `host` from `DnsQuery_W` (type `A`, standard options: the DNS Client
/// service, NRPT, cache and `hosts`). `host` is taken as a full name (no suffix search).
/// Errors keep the Windows code (`9003` = `DNS_ERROR_RCODE_NAME_ERROR`, NXDOMAIN).
pub(crate) fn query_a(host: &str) -> Result<Vec<IpAddr>> {
    let name: Vec<u16> = host.encode_utf16().chain(std::iter::once(0)).collect();
    let mut records: *mut DNS_RECORDA = std::ptr::null_mut();
    // SAFETY: NUL-terminated name; `records` receives a list freed below with DnsFree.
    let status = unsafe {
        DnsQuery_W(
            PCWSTR(name.as_ptr()),
            DNS_TYPE_A,
            DNS_QUERY_STANDARD | DNS_QUERY_TREAT_AS_FQDN,
            None,
            &mut records,
            None,
        )
    };
    let mut out = Vec::new();
    let mut current = records;
    while !current.is_null() {
        // SAFETY: a node of the list returned by DnsQuery_W, alive until DnsFree.
        let record = unsafe { &*current };
        if record.wType == DNS_TYPE_A.0 {
            // SAFETY: type A records carry DNS_A_DATA; the address is in network order.
            let ip = unsafe { record.Data.A.IpAddress };
            let ip = IpAddr::V4(Ipv4Addr::from(ip.to_ne_bytes()));
            if !out.contains(&ip) {
                out.push(ip);
            }
        }
        current = record.pNext;
    }
    if !records.is_null() {
        // SAFETY: frees the whole list once.
        unsafe { DnsFree(Some(records as *const _), DnsFreeRecordList) };
    }
    if status != ERROR_SUCCESS {
        return Err(ForgeError::io(
            format!("resolving {host} through the OS resolver"),
            std::io::Error::from_raw_os_error(status.0 as i32),
        ));
    }
    Ok(out)
}

/// The system `hosts` file: `DataBasePath` of `Tcpip\Parameters` (normally
/// `%SystemRoot%\System32\drivers\etc`) + `hosts`.
pub(crate) fn hosts_file_path() -> PathBuf {
    let configured = string_value(
        HKEY_LOCAL_MACHINE,
        TCPIP_PARAMETERS,
        w!("DataBasePath"),
        RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
    )
    .map(|w| expand_env(&String::from_utf16_lossy(&w)))
    .filter(|p| !p.trim().is_empty());
    configured
        .map(PathBuf::from)
        .unwrap_or_else(|| system_directory().join("drivers").join("etc"))
        .join("hosts")
}

/// `DnsFlushResolverCache` (what `ipconfig /flushdns` calls). No elevation.
pub(crate) fn flush_resolver_cache() -> Result<()> {
    // SAFETY: loads a system DLL from System32 only (no DLL planting).
    let module = unsafe { LoadLibraryExW(w!("dnsapi.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
        .map_err(|e| ForgeError::io("loading dnsapi.dll", os_error(&e)))?;
    // SAFETY: valid module; NUL-terminated ANSI name.
    let proc = unsafe { GetProcAddress(module, s!("DnsFlushResolverCache")) };
    let result = match proc {
        // SAFETY: the export has this signature (no arguments, returns BOOL).
        Some(proc) => {
            let flush: FlushFn = unsafe { std::mem::transmute(proc) };
            // SAFETY: plain call into dnsapi.dll while the module is loaded.
            if unsafe { flush() }.as_bool() {
                Ok(())
            } else {
                Err(ForgeError::io(
                    "flushing the DNS resolver cache",
                    std::io::Error::last_os_error(),
                ))
            }
        }
        None => Err(ForgeError::UnsupportedPlatform(
            "dnsapi.dll has no DnsFlushResolverCache".to_owned(),
        )),
    };
    // SAFETY: balances LoadLibraryExW.
    unsafe {
        let _ = FreeLibrary(module);
    }
    result
}

impl DnsPolicy for WindowsDnsPolicy {
    fn nrpt_table(&self) -> Result<NrptTable> {
        read_nrpt_table()
    }

    fn query_system(&self, host: &str) -> Result<Vec<IpAddr>> {
        query_a(host)
    }

    fn apply(&self, tld: &str, nameserver: IpAddr) -> Result<NrptApplied> {
        self.helper
            .run(HelperOp::NrptAdd {
                namespace: tld.to_owned(),
                nameserver,
            })
            .map(|response| NrptApplied::from_details(&response.details))
    }

    fn remove(&self, tld: &str) -> Result<()> {
        self.helper
            .run(HelperOp::NrptRemove {
                namespace: tld.to_owned(),
            })
            .map(drop)
    }

    fn hosts_path(&self) -> PathBuf {
        hosts_file_path()
    }

    fn set_hosts_block(&self, entries: &[HostsEntry]) -> Result<()> {
        self.helper
            .run(HelperOp::HostsSetBlock {
                entries: entries.to_vec(),
            })
            .map(drop)
    }

    fn flush_cache(&self) -> Result<()> {
        flush_resolver_cache()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::helper::{FakeElevation, HelperPolicy};
    use crate::platform::DnsPolicyStatus;

    fn policy_with(fake: Arc<FakeElevation>) -> WindowsDnsPolicy {
        let policy = HelperPolicy::new(r"C:\Forgebox", [".test"]).unwrap();
        WindowsDnsPolicy::new(HelperClient::new(
            fake,
            r"C:\Forgebox\forgebox-helper.exe",
            policy,
        ))
    }

    #[test]
    fn reads_the_real_tables_without_elevation() {
        let policy = policy_with(Arc::new(FakeElevation::accepting()));
        let table = policy.nrpt_table().unwrap();
        for rule in &table.rules {
            assert!(!rule.id.is_empty());
            assert!(
                rule.namespaces.iter().all(|n| *n == n.to_ascii_lowercase()),
                "{rule:?}"
            );
        }
        // Whatever the machine has, the status call works.
        let status = policy.status(".forgebox-never-configured").unwrap();
        assert_eq!(status, DnsPolicyStatus::NotApplied);
        // The table flag agrees with the registry, whatever this machine has (empty
        // corporate key on the M1 gate machine, nothing on a home PC).
        let key_exists = Key::open(GPO_RULES).unwrap().is_some();
        assert_eq!(table.group_policy_table, key_exists);
        if key_exists {
            assert!(table.local_rules_ignored().is_some());
        }
    }

    #[test]
    fn asks_the_os_resolver() {
        // `localhost` is answered by the DNS Client itself on every Windows.
        let ips = query_a("localhost").unwrap();
        assert!(ips.iter().all(IpAddr::is_loopback), "{ips:?}");
        // A reserved name that no server answers (RFC 6761 `.invalid`): an error that
        // names the host; never a panic or an empty success.
        let err = query_a("forgebox-probe.invalid").unwrap_err();
        assert!(err.to_string().contains("forgebox-probe.invalid"), "{err}");
    }

    #[test]
    fn hosts_path_is_the_system_one() {
        let policy = policy_with(Arc::new(FakeElevation::accepting()));
        let path = policy.hosts_path();
        assert!(path.ends_with(r"drivers\etc\hosts"), "{}", path.display());
        assert!(!path.to_string_lossy().contains('%'), "expanded");
        assert!(path.is_file(), "{}", path.display());
        policy.read_hosts().unwrap();
    }

    #[test]
    fn flushes_the_resolver_cache() {
        policy_with(Arc::new(FakeElevation::accepting()))
            .flush_cache()
            .unwrap();
    }

    #[test]
    fn changes_go_through_the_helper() {
        let fake = Arc::new(FakeElevation::accepting());
        let policy = policy_with(fake.clone());
        policy
            .apply(".test", "127.0.0.53".parse().unwrap())
            .unwrap();
        policy.remove(".test").unwrap();
        let entries = vec![HostsEntry::new("127.0.0.1".parse().unwrap(), "loja.test")];
        policy.set_hosts_block(&entries).unwrap();
        policy.set_hosts_block(&[]).unwrap();
        assert_eq!(
            fake.ops(),
            vec![
                HelperOp::NrptAdd {
                    namespace: ".test".into(),
                    nameserver: "127.0.0.53".parse().unwrap()
                },
                HelperOp::NrptRemove {
                    namespace: ".test".into()
                },
                HelperOp::HostsSetBlock { entries },
                HelperOp::HostsSetBlock { entries: vec![] },
            ]
        );
        // Refused before any prompt.
        let err = policy
            .apply(".com", "127.0.0.1".parse().unwrap())
            .unwrap_err();
        assert_eq!(err.code(), "forbidden");
        assert_eq!(fake.requests().len(), 4);

        let declined = policy_with(Arc::new(FakeElevation::declining()));
        assert_eq!(
            declined
                .apply(".test", "127.0.0.1".parse().unwrap())
                .unwrap_err()
                .code(),
            "needs_elevation"
        );
    }

    #[test]
    fn env_expansion_and_server_lists() {
        // SAFETY: test-only variable read back in this thread.
        let root = std::env::var("SystemRoot").unwrap();
        assert_eq!(
            expand_env(r"%SystemRoot%\System32\drivers\etc"),
            format!(r"{root}\System32\drivers\etc")
        );
        assert_eq!(
            expand_env("%NO_SUCH_VAR_FORGEBOX%x"),
            "%NO_SUCH_VAR_FORGEBOX%x"
        );
        assert_eq!(expand_env("50%"), "50%");
        assert_eq!(
            parse_servers("127.0.0.1; ::1,bad 10.0.0.1"),
            vec![
                "127.0.0.1".parse::<IpAddr>().unwrap(),
                "::1".parse().unwrap(),
                "10.0.0.1".parse().unwrap()
            ]
        );
    }
}
