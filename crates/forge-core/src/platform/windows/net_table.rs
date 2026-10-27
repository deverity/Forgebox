//! TCP table with owning processes (docs/03 §1.5, §2.1, §4): `GetExtendedTcpTable` with
//! `TCP_TABLE_OWNER_PID_LISTENER` (listeners) or `TCP_TABLE_OWNER_PID_CONNECTIONS`
//! (connections), for `AF_INET` and `AF_INET6`, and `QueryFullProcessImageNameW` for the
//! executable of each owning PID (one `PROCESS_QUERY_LIMITED_INFORMATION` handle per
//! distinct PID, closed at once).
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
    GetExtendedTcpTable, MIB_TCP_STATE_ESTAB, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID,
    MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_CLASS, TCP_TABLE_OWNER_PID_CONNECTIONS,
    TCP_TABLE_OWNER_PID_LISTENER,
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
    let mut size = 0_u32;
    let mut buffer: Vec<u32> = Vec::new();
    for _ in 0..TABLE_ATTEMPTS {
        let ptr = if buffer.is_empty() {
            None
        } else {
            Some(buffer.as_mut_ptr().cast::<std::ffi::c_void>())
        };
        // SAFETY: `ptr` is `None` or points at `buffer`, whose byte length is >= `size`.
        let status = unsafe { GetExtendedTcpTable(ptr, &mut size, false, family, class, 0) };
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
            "reading the TCP table (GetExtendedTcpTable)",
            std::io::Error::from_raw_os_error(status as i32),
        ));
    }
    Err(ForgeError::io(
        "reading the TCP table (GetExtendedTcpTable)",
        std::io::Error::other("the table kept growing while it was read"),
    ))
}

/// The rows of a table buffer: a `u32` count, then rows of `T` from byte `offset`.
/// Never reads past the buffer, whatever the count says.
///
/// # Safety
/// `T` must be a plain-old-data row type (`MIB_TCPROW_OWNER_PID`,
/// `MIB_TCP6ROW_OWNER_PID`) and `buffer` a table of such rows as written by
/// `GetExtendedTcpTable` with the matching family and class.
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
        found.sort();
        let mut exes: HashMap<u32, Option<PathBuf>> = HashMap::new();
        Ok(found
            .into_iter()
            .map(|(port, addr, pid, proto)| PortOwner {
                port,
                addr,
                pid,
                exe_path: exes.entry(pid).or_insert_with(|| image_path(pid)).clone(),
                proto,
            })
            .collect())
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
    fn kernel_pids_have_no_image() {
        assert_eq!(image_path(0), None);
        assert_eq!(image_path(4), None);
        let own = image_path(std::process::id()).unwrap();
        assert_eq!(own, std::env::current_exe().unwrap());
    }
}
