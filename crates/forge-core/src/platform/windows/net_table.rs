//! TCP table with owning processes (docs/03 §1.5, §2.1, §4): `GetExtendedTcpTable` with
//! `TCP_TABLE_OWNER_PID_LISTENER` (listeners) or `TCP_TABLE_OWNER_PID_CONNECTIONS`
//! (connections), for `AF_INET` and `AF_INET6`, and `QueryFullProcessImageNameW` for the
//! executable of each owning PID (one `PROCESS_QUERY_LIMITED_INFORMATION` handle per
//! distinct PID, closed at once). UDP endpoints (DNS diagnosis, docs/06 §5.2) come from
//! `GetExtendedUdpTable` with `UDP_TABLE_OWNER_PID`.
//!
//! The table size is asked first and the call repeated while it returns
//! `ERROR_INSUFFICIENT_BUFFER` (connections come and go between the two calls). Ports
//! are stored in network byte order in the low 16 bits of `dwLocalPort`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::path::PathBuf;

use ::windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE, NO_ERROR};
use ::windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCP_STATE_ESTAB, MIB_TCP6ROW_OWNER_PID,
    MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, MIB_UDP6ROW_OWNER_PID,
    MIB_UDP6TABLE_OWNER_PID, MIB_UDPROW_OWNER_PID, MIB_UDPTABLE_OWNER_PID, TCP_TABLE_CLASS,
    TCP_TABLE_OWNER_PID_CONNECTIONS, TCP_TABLE_OWNER_PID_LISTENER, UDP_TABLE_OWNER_PID,
};
use ::windows::Win32::System::Threading::{
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use ::windows::core::PWSTR;

use super::process::{open_process, win32_code};
use crate::error::{ForgeError, Result};
use crate::platform::net_table::{PortOwner, PortProto, PortTable};

/// `AF_INET` / `AF_INET6` (ws2def.h), as the `ulAf` argument of `GetExtendedTcpTable`.
const AF_INET: u32 = 2;
const AF_INET6: u32 = 23;

/// Attempts when the table keeps growing between the size query and the copy.
const TABLE_ATTEMPTS: usize = 8;

/// [`PortTable`] for Windows.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsPortTable;

/// Raw bytes of one `GetExtendedTcpTable` answer, 4-byte aligned (`u32` storage).
fn tcp_table(family: u32, class: TCP_TABLE_CLASS) -> Result<Vec<u32>> {
    // SAFETY: `ptr` is `None` or points at a buffer of at least `size` bytes.
    read_table(
        "reading the TCP table (GetExtendedTcpTable)",
        |ptr, size| unsafe { GetExtendedTcpTable(ptr, size, false, family, class, 0) },
    )
}

/// Raw bytes of one `GetExtendedUdpTable(UDP_TABLE_OWNER_PID)` answer.
fn udp_table(family: u32) -> Result<Vec<u32>> {
    // SAFETY: `ptr` is `None` or points at a buffer of at least `size` bytes.
    read_table(
        "reading the UDP table (GetExtendedUdpTable)",
        |ptr, size| unsafe {
            GetExtendedUdpTable(ptr, size, false, family, UDP_TABLE_OWNER_PID, 0)
        },
    )
}

/// Calls a size-then-copy table API until the buffer is big enough. `call` receives the
/// buffer (or `None` to ask the size) and its size in bytes, and returns a Win32 status.
fn read_table(
    what: &str,
    mut call: impl FnMut(Option<*mut std::ffi::c_void>, &mut u32) -> u32,
) -> Result<Vec<u32>> {
    let mut size = 0_u32;
    let mut buffer: Vec<u32> = Vec::new();
    for _ in 0..TABLE_ATTEMPTS {
        let ptr = if buffer.is_empty() {
            None
        } else {
            Some(buffer.as_mut_ptr().cast::<std::ffi::c_void>())
        };
        let status = call(ptr, &mut size);
        if status == NO_ERROR.0 && !buffer.is_empty() {
            return Ok(buffer);
        }
        if status == NO_ERROR.0 || status == ERROR_INSUFFICIENT_BUFFER.0 {
            // Room for the reported size plus a little growth.
            let words = (size as usize).div_ceil(4) + 64;
            buffer = vec![0_u32; words];
            size = u32::try_from(words * 4).unwrap_or(u32::MAX);
            continue;
        }
        return Err(ForgeError::io(
            what,
            std::io::Error::from_raw_os_error(status as i32),
        ));
    }
    Err(ForgeError::io(
        what,
        std::io::Error::other("the table kept growing while it was read"),
    ))
}

/// The rows of a table buffer: a `u32` count, then rows of `T` from byte `offset`.
/// Never reads past the buffer, whatever the count says.
///
/// # Safety
/// `T` must be a plain-old-data row type (`MIB_TCPROW_OWNER_PID`,
/// `MIB_TCP6ROW_OWNER_PID`, `MIB_UDPROW_OWNER_PID`, `MIB_UDP6ROW_OWNER_PID`) and `buffer`
/// a table of such rows as written by `GetExtendedTcpTable`/`GetExtendedUdpTable` with
/// the matching family and class.
unsafe fn rows<T: Copy>(buffer: &[u32], offset: usize) -> Vec<T> {
    let count = buffer.first().copied().unwrap_or(0) as usize;
    let available = (buffer.len() * 4).saturating_sub(offset) / size_of::<T>();
    let base = buffer.as_ptr().cast::<u8>();
    (0..count.min(available))
        // SAFETY: row `i` lies inside `buffer` (bounded by `available`); rows are POD.
        .map(|i| unsafe {
            base.add(offset + i * size_of::<T>())
                .cast::<T>()
                .read_unaligned()
        })
        .collect()
}

fn v4_rows(class: TCP_TABLE_CLASS) -> Result<Vec<MIB_TCPROW_OWNER_PID>> {
    let buffer = tcp_table(AF_INET, class)?;
    // SAFETY: AF_INET + an OWNER_PID class writes an MIB_TCPTABLE_OWNER_PID.
    Ok(unsafe { rows(&buffer, std::mem::offset_of!(MIB_TCPTABLE_OWNER_PID, table)) })
}

fn v6_rows(class: TCP_TABLE_CLASS) -> Result<Vec<MIB_TCP6ROW_OWNER_PID>> {
    let buffer = tcp_table(AF_INET6, class)?;
    // SAFETY: AF_INET6 + an OWNER_PID class writes an MIB_TCP6TABLE_OWNER_PID.
    Ok(unsafe {
        rows(
            &buffer,
            std::mem::offset_of!(MIB_TCP6TABLE_OWNER_PID, table),
        )
    })
}

fn udp_v4_rows() -> Result<Vec<MIB_UDPROW_OWNER_PID>> {
    let buffer = udp_table(AF_INET)?;
    // SAFETY: AF_INET + UDP_TABLE_OWNER_PID writes an MIB_UDPTABLE_OWNER_PID.
    Ok(unsafe { rows(&buffer, std::mem::offset_of!(MIB_UDPTABLE_OWNER_PID, table)) })
}

fn udp_v6_rows() -> Result<Vec<MIB_UDP6ROW_OWNER_PID>> {
    let buffer = udp_table(AF_INET6)?;
    // SAFETY: AF_INET6 + UDP_TABLE_OWNER_PID writes an MIB_UDP6TABLE_OWNER_PID.
    Ok(unsafe {
        rows(
            &buffer,
            std::mem::offset_of!(MIB_UDP6TABLE_OWNER_PID, table),
        )
    })
}

/// Sorts raw endpoints and resolves the executable of each distinct PID once.
fn owners(mut found: Vec<(u16, IpAddr, u32, PortProto)>) -> Vec<PortOwner> {
    found.sort();
    let mut exes: HashMap<u32, Option<PathBuf>> = HashMap::new();
    found
        .into_iter()
        .map(|(port, addr, pid, proto)| PortOwner {
            port,
            addr,
            pid,
            exe_path: exes.entry(pid).or_insert_with(|| image_path(pid)).clone(),
            proto,
        })
        .collect()
}

/// Port from the network-byte-order low 16 bits of a `dwLocalPort`.
fn port(raw: u32) -> u16 {
    u16::from_be((raw & 0xFFFF) as u16)
}

/// IPv4 address stored in network byte order in a `u32`.
fn ipv4(raw: u32) -> Ipv4Addr {
    Ipv4Addr::from(raw.to_ne_bytes())
}

/// Full path of the executable of `pid`; `None` when it cannot be read.
fn image_path(pid: u32) -> Option<PathBuf> {
    // PID 0 is the idle process and 4 the kernel: they have no image file.
    if pid == 0 || pid == 4 {
        return None;
    }
    let process: OwnedHandle = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
    let mut buffer = vec![0_u16; 1024];
    loop {
        let mut len = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // SAFETY: valid handle with query access; `buffer` holds `len` u16s.
        let result = unsafe {
            QueryFullProcessImageNameW(
                HANDLE(process.as_raw_handle()),
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut len,
            )
        };
        match result {
            Ok(()) => {
                buffer.truncate(len as usize);
                return Some(PathBuf::from(OsString::from_wide(&buffer)));
            }
            Err(err)
                if win32_code(&err) == Some(ERROR_INSUFFICIENT_BUFFER.0)
                    && buffer.len() < 32 * 1024 =>
            {
                let bigger = buffer.len() * 2;
                buffer.resize(bigger, 0);
            }
            Err(_) => return None,
        }
    }
}

impl PortTable for WindowsPortTable {
    fn listeners(&self) -> Result<Vec<PortOwner>> {
        let mut found: Vec<(u16, IpAddr, u32, PortProto)> = Vec::new();
        for row in v4_rows(TCP_TABLE_OWNER_PID_LISTENER)? {
            found.push((
                port(row.dwLocalPort),
                IpAddr::V4(ipv4(row.dwLocalAddr)),
                row.dwOwningPid,
                PortProto::Tcp,
            ));
        }
        for row in v6_rows(TCP_TABLE_OWNER_PID_LISTENER)? {
            found.push((
                port(row.dwLocalPort),
                IpAddr::V6(Ipv6Addr::from(row.ucLocalAddr)),
                row.dwOwningPid,
                PortProto::Tcp6,
            ));
        }
        Ok(owners(found))
    }

    fn udp_endpoints(&self) -> Result<Vec<PortOwner>> {
        let mut found: Vec<(u16, IpAddr, u32, PortProto)> = Vec::new();
        for row in udp_v4_rows()? {
            found.push((
                port(row.dwLocalPort),
                IpAddr::V4(ipv4(row.dwLocalAddr)),
                row.dwOwningPid,
                PortProto::Udp,
            ));
        }
        for row in udp_v6_rows()? {
            found.push((
                port(row.dwLocalPort),
                IpAddr::V6(Ipv6Addr::from(row.ucLocalAddr)),
                row.dwOwningPid,
                PortProto::Udp6,
            ));
        }
        Ok(owners(found))
    }

    fn established_count(&self, wanted: u16) -> Result<u32> {
        let established = MIB_TCP_STATE_ESTAB.0 as u32;
        let v4 = v4_rows(TCP_TABLE_OWNER_PID_CONNECTIONS)?
            .into_iter()
            .filter(|r| r.dwState == established && port(r.dwLocalPort) == wanted)
            .count();
        let v6 = v6_rows(TCP_TABLE_OWNER_PID_CONNECTIONS)?
            .into_iter()
            .filter(|r| r.dwState == established && port(r.dwLocalPort) == wanted)
            .count();
        Ok(u32::try_from(v4 + v6).unwrap_or(u32::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_order_helpers() {
        // Port 3306 = 0x0CEA, stored big-endian in the low word.
        let raw = u32::from(u16::to_be(3306));
        assert_eq!(port(raw), 3306);
        assert_eq!(port(raw | 0xABCD_0000), 3306, "high word ignored");
        let loopback = u32::from_ne_bytes([127, 0, 0, 1]);
        assert_eq!(ipv4(loopback), Ipv4Addr::LOCALHOST);
    }

    #[test]
    fn finds_our_own_udp_sockets() {
        let v4 = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port4 = v4.local_addr().unwrap().port();
        let v6 = std::net::UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).ok();
        let endpoints = WindowsPortTable.udp_endpoints().unwrap();
        let own = endpoints
            .iter()
            .find(|e| e.port == port4 && e.pid == std::process::id())
            .expect("our UDP socket");
        assert_eq!(own.proto, PortProto::Udp);
        assert_eq!(own.addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(own.exe_path, std::env::current_exe().ok());
        if let Some(v6) = &v6 {
            let port6 = v6.local_addr().unwrap().port();
            let own6 = endpoints
                .iter()
                .find(|e| e.port == port6 && e.proto == PortProto::Udp6)
                .expect("our UDP6 socket");
            assert_eq!(own6.addr, IpAddr::V6(Ipv6Addr::LOCALHOST));
        }
        assert!(
            endpoints
                .windows(2)
                .all(|w| (w[0].port, w[0].addr) <= (w[1].port, w[1].addr)),
            "sorted"
        );
        drop(v4);
        let after = WindowsPortTable.udp_endpoints().unwrap();
        assert!(
            !after
                .iter()
                .any(|e| e.port == port4 && e.pid == std::process::id())
        );
    }

    #[test]
    fn kernel_pids_have_no_image() {
        assert_eq!(image_path(0), None);
        assert_eq!(image_path(4), None);
        let own = image_path(std::process::id()).unwrap();
        assert_eq!(own, std::env::current_exe().unwrap());
    }
}
