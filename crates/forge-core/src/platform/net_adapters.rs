//! Network adapters of the machine, for the DNS diagnosis (docs/06 §5.2): VPN and
//! virtualization adapters that override DNS are a common reason why `*.test` does not
//! resolve.
//!
//! [`NetAdapters::adapters`] is **blocking** (Windows: `GetAdaptersAddresses`) and
//! belongs in `spawn_blocking`.

use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[cfg(not(windows))]
use crate::error::ForgeError;
use crate::error::Result;

/// One network interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetAdapter {
    /// Name shown by the OS (`Wi-Fi`, `vEthernet (WSL)`).
    pub name: String,
    /// Driver description (`TAP-Windows Adapter V9`, `WireGuard Tunnel`).
    pub description: String,
    /// Operationally up.
    pub up: bool,
    /// DNS servers configured on the adapter.
    pub dns_servers: Vec<IpAddr>,
}

/// The adapters of the machine.
pub trait NetAdapters: Send + Sync + fmt::Debug {
    /// Every adapter (loopback pseudo-interfaces excluded). Blocking.
    fn adapters(&self) -> Result<Vec<NetAdapter>>;
}

/// The adapters of the running OS.
#[cfg(windows)]
pub fn net_adapters() -> Arc<dyn NetAdapters> {
    Arc::new(super::windows::WindowsNetAdapters)
}

/// The adapters of the running OS (stub until the Linux/macOS ports).
#[cfg(not(windows))]
pub fn net_adapters() -> Arc<dyn NetAdapters> {
    Arc::new(Unsupported)
}

#[cfg(not(windows))]
#[derive(Debug)]
struct Unsupported;

#[cfg(not(windows))]
impl NetAdapters for Unsupported {
    fn adapters(&self) -> Result<Vec<NetAdapter>> {
        Err(ForgeError::UnsupportedPlatform(format!(
            "network adapters are not listed on {}",
            std::env::consts::OS
        )))
    }
}
