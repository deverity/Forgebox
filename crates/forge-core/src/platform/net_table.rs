//! Who listens on which TCP port (docs/03 §1.5 and §4): the OS connection table with the
//! owning process of each socket.
//!
//! [`PortTable::listeners`] answers "port 3306 is in use by `C:\xampp\mysql\bin\mysqld.exe`,
//! PID 4412" for the pre-checks of the supervisor (`forge_supervisor::precheck`) and the
//! conflict report of the port allocator (`forge_net::ports`);
//! [`PortTable::established_count`] feeds the scaling of the PHP pools (docs/03 §2.1);
//! [`PortTable::connections`] gives the pools every connection of their workers' ports,
//! `TIME_WAIT` included, from which they estimate how many requests each worker served.
//!
//! All methods are **blocking** (Windows: `GetExtendedTcpTable`, then
//! `QueryFullProcessImageNameW` per owning process) and belong in `spawn_blocking`
//! ([`crate::runtime::spawn_blocking_timeout`], ADR-0011). [`PortTable::udp_endpoints`]
//! lists bound UDP sockets (UDP has no listen state) for the DNS diagnosis: who holds port
//! 53 (docs/06 §5.2).

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::ForgeError;
use crate::error::Result;

/// Protocol and address family of an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortProto {
    /// TCP over IPv4.
    Tcp,
    /// TCP over IPv6 (dual-stack sockets listening on `[::]` appear here only).
    Tcp6,
    /// UDP over IPv4.
    Udp,
    /// UDP over IPv6.
    Udp6,
}

/// A TCP socket in the `LISTEN` state (or a bound UDP socket) and the process that owns
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortOwner {
    pub port: u16,
    /// Local address it listens on (`0.0.0.0`, `127.0.0.1`, `::`...).
    pub addr: IpAddr,
    pub pid: u32,
    /// Full path of the owning executable; `None` when the process cannot be opened
    /// (PID 0/4 — the kernel, which also owns `http.sys` listeners —, protected processes,
    /// a process that exited meanwhile).
    pub exe_path: Option<PathBuf>,
    pub proto: PortProto,
}

impl PortOwner {
    /// The executable for messages: its full path, or `None` when unknown.
    pub fn exe_display(&self) -> Option<String> {
        self.exe_path.as_ref().map(|p| p.display().to_string())
    }

    /// The file name of the executable (`mysqld.exe`).
    pub fn exe_name(&self) -> Option<String> {
        self.exe_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
    }
}

/// State of a TCP connection (RFC 793 names, as in `MIB_TCP_STATE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    /// The side that closed first keeps the connection here for 30–240 s (Windows
    /// `TcpTimedWaitDelay`, 120 s by default).
    TimeWait,
    DeleteTcb,
    /// A value this build does not know.
    Other,
}

/// One TCP connection (not a listener) and the process that owns its local end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TcpConnection {
    pub local: SocketAddr,
    pub remote: SocketAddr,
    pub state: TcpState,
    /// Owning process; 0 for connections no process owns any more (`TIME_WAIT`).
    pub pid: u32,
}

/// The OS TCP table.
pub trait PortTable: Send + Sync + fmt::Debug {
    /// Every TCP listener of the machine, IPv4 and IPv6, sorted by port then address.
    /// A port held on several addresses (or by several processes) appears once per
    /// socket. Blocking.
    fn listeners(&self) -> Result<Vec<PortOwner>>;

    /// Established TCP connections whose **local** port is `port`: the server side of
    /// connections accepted on it (the client side of a loopback connection has an
    /// ephemeral local port and is not counted). IPv4 and IPv6. Blocking.
    fn established_count(&self, port: u16) -> Result<u32>;

    /// Every TCP connection (listeners excluded) whose local **or** remote port is one of
    /// `ports`, in any state — `TIME_WAIT` included —, IPv4 and IPv6. On loopback both
    /// ends of a connection appear: the server side (local port in `ports`) and the
    /// client side (remote port in `ports`). Order unspecified. Blocking.
    ///
    /// The default implementation fails with `NotImplemented` (tables that only know
    /// listeners, such as test fakes).
    fn connections(&self, ports: &[u16]) -> Result<Vec<TcpConnection>> {
        let _ = ports;
        Err(ForgeError::NotImplemented(
            "listing TCP connections with this port table".to_owned(),
        ))
    }

    /// Every bound UDP socket of the machine with its owning process, IPv4 and IPv6,
    /// sorted by port then address. Blocking. Implementations without a UDP table answer
    /// `unsupported_platform`.
    fn udp_endpoints(&self) -> Result<Vec<PortOwner>> {
        Err(ForgeError::UnsupportedPlatform(
            "this port table has no UDP view".to_owned(),
        ))
    }
}

/// The TCP table of the running OS.
#[cfg(windows)]
pub fn port_table() -> Arc<dyn PortTable> {
    Arc::new(super::windows::WindowsPortTable)
}

/// The TCP table of the running OS (stub until the Linux/macOS ports).
#[cfg(not(windows))]
pub fn port_table() -> Arc<dyn PortTable> {
    Arc::new(unsupported::Unsupported)
}

#[cfg(not(windows))]
mod unsupported {
    use super::*;

    #[derive(Debug)]
    pub struct Unsupported;

    fn err<T>() -> Result<T> {
        Err(ForgeError::UnsupportedPlatform(format!(
            "the TCP table is not implemented on {}",
            std::env::consts::OS
        )))
    }

    impl PortTable for Unsupported {
        fn listeners(&self) -> Result<Vec<PortOwner>> {
            err()
        }

        fn established_count(&self, _: u16) -> Result<u32> {
            err()
        }

        fn connections(&self, _: &[u16]) -> Result<Vec<TcpConnection>> {
            err()
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, Ipv6Addr, TcpListener, TcpStream};

    use super::*;

    fn current_exe_name() -> String {
        std::env::current_exe()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn finds_our_own_listeners_with_pid_and_exe() {
        let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = v4.local_addr().unwrap().port();
        let table = port_table();
        let listeners = table.listeners().unwrap();
        let ours: Vec<_> = listeners.iter().filter(|l| l.port == port).collect();
        assert_eq!(ours.len(), 1, "{ours:?}");
        let owner = ours[0];
        assert_eq!(owner.pid, std::process::id());
        assert_eq!(owner.proto, PortProto::Tcp);
        assert_eq!(owner.addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
        let exe = owner.exe_display().expect("our own exe is readable");
        assert!(exe.contains(&current_exe_name()), "{exe}");
        assert_eq!(owner.exe_name().unwrap(), current_exe_name());
        let sorted = listeners
            .windows(2)
            .all(|w| (w[0].port, w[0].addr) <= (w[1].port, w[1].addr));
        assert!(sorted, "listeners are sorted by port");

        drop(v4);
        let after = table.listeners().unwrap();
        assert!(
            !after
                .iter()
                .any(|l| l.port == port && l.pid == std::process::id()),
            "a closed listener is gone"
        );
    }

    #[test]
    fn finds_ipv6_listeners() {
        let Ok(v6) = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)) else {
            eprintln!("IPv6 loopback unavailable; skipping");
            return;
        };
        let port = v6.local_addr().unwrap().port();
        let listeners = port_table().listeners().unwrap();
        let owner = listeners
            .iter()
            .find(|l| l.port == port && l.pid == std::process::id())
            .expect("our IPv6 listener");
        assert_eq!(owner.proto, PortProto::Tcp6);
        assert_eq!(owner.addr, IpAddr::V6(Ipv6Addr::LOCALHOST));
    }

    #[test]
    fn counts_established_connections_on_the_server_side() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let table = port_table();
        assert_eq!(table.established_count(port).unwrap(), 0);

        let mut clients = Vec::new();
        let mut accepted = Vec::new();
        for _ in 0..3 {
            let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            let (mut server, _) = listener.accept().unwrap();
            client.write_all(b"x").unwrap();
            let mut buf = [0_u8; 1];
            server.read_exact(&mut buf).unwrap();
            clients.push(client);
            accepted.push(server);
        }
        assert_eq!(table.established_count(port).unwrap(), 3);
        drop(clients);
        drop(accepted);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while table.established_count(port).unwrap() != 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "connections did not close"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn lists_both_ends_and_time_wait() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let server_addr = listener.local_addr().unwrap();
        let port = server_addr.port();
        let table = port_table();
        assert!(table.connections(&[port]).unwrap().is_empty());

        let mut client = TcpStream::connect(server_addr).unwrap();
        let client_addr = client.local_addr().unwrap();
        let (mut server, _) = listener.accept().unwrap();
        client.write_all(b"x").unwrap();
        let mut buf = [0_u8; 1];
        server.read_exact(&mut buf).unwrap();

        let open = table.connections(&[port]).unwrap();
        let server_side = open
            .iter()
            .find(|c| c.local == server_addr)
            .expect("server side");
        assert_eq!(server_side.remote, client_addr);
        assert_eq!(server_side.state, TcpState::Established);
        assert_eq!(server_side.pid, std::process::id());
        let client_side = open
            .iter()
            .find(|c| c.local == client_addr)
            .expect("client side");
        assert_eq!(client_side.remote, server_addr);
        assert_eq!(client_side.state, TcpState::Established);
        assert!(open.iter().all(|c| c.state != TcpState::Listen), "{open:?}");

        // The server closes first, like php-cgi after a FastCGI response: its end stays
        // in TIME_WAIT and is still listed.
        server.shutdown(std::net::Shutdown::Write).unwrap();
        let mut rest = Vec::new();
        client.read_to_end(&mut rest).unwrap();
        drop(client);
        drop(server);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let closed = table.connections(&[port]).unwrap();
            let time_wait = closed
                .iter()
                .find(|c| c.local == server_addr && c.remote == client_addr);
            if let Some(found) = time_wait {
                if found.state == TcpState::TimeWait {
                    assert_eq!(found.pid, 0, "nobody owns a TIME_WAIT entry");
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no TIME_WAIT entry for {server_addr} <- {client_addr}: {closed:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let unrelated = table.connections(&[1]).unwrap();
        assert!(
            unrelated
                .iter()
                .all(|c| c.local.port() == 1 || c.remote.port() == 1)
        );
    }
}
