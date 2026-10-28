//! [`NetAdapters`] for Windows: `GetAdaptersAddresses` (friendly name, description,
//! operational state and DNS servers of each adapter; unicast/anycast/multicast lists
//! skipped).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ::windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS};
use ::windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_MULTICAST, GAA_FLAG_SKIP_UNICAST, GetAdaptersAddresses,
    IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH,
};
use ::windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use ::windows::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, SOCKADDR_IN, SOCKADDR_IN6, SOCKET_ADDRESS,
};
use ::windows::core::PWSTR;

use crate::error::{ForgeError, Result};
use crate::platform::{NetAdapter, NetAdapters};

/// `AF_UNSPEC`: IPv4 and IPv6.
const AF_UNSPEC: u32 = 0;
/// Buffer size recommended by the documentation for a first call.
const FIRST_BUFFER: usize = 15 * 1024;
const ATTEMPTS: usize = 4;

/// [`NetAdapters`] for Windows.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsNetAdapters;

fn wide_to_string(text: PWSTR) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: the API returns NUL-terminated strings inside the buffer we own.
    unsafe { text.to_string() }.unwrap_or_default()
}

/// The address of a `SOCKET_ADDRESS`, when IPv4 or IPv6.
fn socket_ip(address: &SOCKET_ADDRESS) -> Option<IpAddr> {
    let sockaddr = address.lpSockaddr;
    if sockaddr.is_null() {
        return None;
    }
    // SAFETY: `lpSockaddr` points at a SOCKADDR of `iSockaddrLength` bytes in the buffer;
    // the family selects the concrete layout, and the length is checked before reading it.
    unsafe {
        let family = (*sockaddr).sa_family;
        let len = usize::try_from(address.iSockaddrLength).unwrap_or(0);
        if family == AF_INET && len >= size_of::<SOCKADDR_IN>() {
            let v4 = sockaddr.cast::<SOCKADDR_IN>().read_unaligned();
            Some(IpAddr::V4(Ipv4Addr::from(
                v4.sin_addr.S_un.S_addr.to_ne_bytes(),
            )))
        } else if family == AF_INET6 && len >= size_of::<SOCKADDR_IN6>() {
            let v6 = sockaddr.cast::<SOCKADDR_IN6>().read_unaligned();
            Some(IpAddr::V6(Ipv6Addr::from(v6.sin6_addr.u.Byte)))
        } else {
            None
        }
    }
}

impl NetAdapters for WindowsNetAdapters {
    fn adapters(&self) -> Result<Vec<NetAdapter>> {
        let flags = GAA_FLAG_SKIP_UNICAST | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST;
        // u64 storage keeps the buffer 8-byte aligned for IP_ADAPTER_ADDRESSES_LH.
        let mut buffer: Vec<u64> = vec![0; FIRST_BUFFER / 8];
        let mut ok = false;
        for _ in 0..ATTEMPTS {
            let mut size = u32::try_from(buffer.len() * 8).unwrap_or(u32::MAX);
            // SAFETY: `buffer` holds `size` bytes, aligned for the structure.
            let status = unsafe {
                GetAdaptersAddresses(
                    AF_UNSPEC,
                    flags,
                    None,
                    Some(buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>()),
                    &mut size,
                )
            };
            if status == ERROR_SUCCESS.0 {
                ok = true;
                break;
            }
            if status == ERROR_BUFFER_OVERFLOW.0 {
                buffer = vec![0; (size as usize).div_ceil(8) + 64];
                continue;
            }
            return Err(ForgeError::io(
                "listing network adapters (GetAdaptersAddresses)",
                std::io::Error::from_raw_os_error(status as i32),
            ));
        }
        if !ok {
            return Err(ForgeError::io(
                "listing network adapters (GetAdaptersAddresses)",
                std::io::Error::other("the adapter list kept growing"),
            ));
        }
        let mut out = Vec::new();
        let mut node = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        while !node.is_null() {
            // SAFETY: the list lives in `buffer`, filled by the successful call above.
            let adapter = unsafe { &*node };
            if adapter.IfType != IF_TYPE_SOFTWARE_LOOPBACK {
                let mut dns_servers = Vec::new();
                let mut server = adapter.FirstDnsServerAddress;
                while !server.is_null() {
                    // SAFETY: linked list inside `buffer`.
                    let entry = unsafe { &*server };
                    dns_servers.extend(socket_ip(&entry.Address));
                    server = entry.Next;
                }
                out.push(NetAdapter {
                    name: wide_to_string(adapter.FriendlyName),
                    description: wide_to_string(adapter.Description),
                    up: adapter.OperStatus == IfOperStatusUp,
                    dns_servers,
                });
            }
            node = adapter.Next;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_adapters_without_loopback() {
        let adapters = WindowsNetAdapters.adapters().unwrap();
        assert!(!adapters.is_empty(), "a Windows machine has adapters");
        for adapter in &adapters {
            assert!(!adapter.name.is_empty(), "{adapter:?}");
            assert!(
                !adapter.description.contains("Loopback Pseudo-Interface"),
                "{adapter:?}"
            );
        }
    }
}
