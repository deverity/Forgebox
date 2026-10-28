//! A minimal DNS client that asks one server directly (UDP or TCP), bypassing the OS
//! resolver and its cache: the "our server" half of the diagnosis and the tests.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use hickory_server::proto::op::{Message, Query, ResponseCode};
use hickory_server::proto::rr::{Name, Record, RecordType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

/// How to reach the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Udp,
    Tcp,
}

/// What the server answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsReply {
    pub code: ResponseCode,
    /// AA bit.
    pub authoritative: bool,
    pub answers: Vec<Record>,
    /// Records of the authority section (the SOA of negative answers).
    pub authorities: Vec<Record>,
}

impl DnsReply {
    /// Addresses of the `A`/`AAAA` answers, in order.
    pub fn addresses(&self) -> Vec<IpAddr> {
        self.answers
            .iter()
            .filter_map(|r| r.data.ip_addr())
            .collect()
    }
}

fn proto_err(what: &str, err: impl std::fmt::Display) -> ForgeError {
    ForgeError::InvalidInput(format!("{what}: {err}"))
}

/// Asks `server` for `name`/`record_type` once, within `timeout`.
pub async fn query(
    server: SocketAddr,
    name: &str,
    record_type: RecordType,
    transport: Transport,
    timeout: Duration,
) -> Result<DnsReply> {
    let fqdn = if name.ends_with('.') {
        name.to_owned()
    } else {
        format!("{name}.")
    };
    let name = Name::from_ascii(&fqdn).map_err(|e| proto_err("invalid DNS name", e))?;
    let mut request = Message::query();
    request.metadata.recursion_desired = true;
    request.add_query(Query::query(name, record_type));
    let id = request.metadata.id;
    let bytes = request
        .to_vec()
        .map_err(|e| ForgeError::Internal(format!("encoding a DNS query: {e}")))?;
    let exchange = async {
        match transport {
            Transport::Udp => udp_exchange(server, id, &bytes).await,
            Transport::Tcp => tcp_exchange(server, &bytes).await,
        }
    };
    let response = tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| ForgeError::Timeout {
            operation: format!("DNS query {fqdn} {record_type} to {server}"),
            after: timeout,
        })??;
    if response.metadata.id != id {
        return Err(ForgeError::Internal(format!(
            "DNS answer from {server} has id {} instead of {id}",
            response.metadata.id
        )));
    }
    Ok(DnsReply {
        code: response.metadata.response_code,
        authoritative: response.metadata.authoritative,
        answers: response.answers,
        authorities: response.authorities,
    })
}

/// `A` then `AAAA` for `name` over UDP; the addresses of both (NXDOMAIN gives none).
pub async fn resolve(server: SocketAddr, name: &str, timeout: Duration) -> Result<Vec<IpAddr>> {
    let mut out = Vec::new();
    for record_type in [RecordType::A, RecordType::AAAA] {
        let reply = query(server, name, record_type, Transport::Udp, timeout).await?;
        match reply.code {
            ResponseCode::NoError | ResponseCode::NXDomain => out.extend(reply.addresses()),
            code => {
                return Err(ForgeError::Internal(format!(
                    "{server} answered {code} for {name} {record_type}"
                )));
            }
        }
    }
    Ok(out)
}

async fn udp_exchange(server: SocketAddr, id: u16, bytes: &[u8]) -> Result<Message> {
    let local: SocketAddr = match server {
        SocketAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        SocketAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let io = |e| ForgeError::io(format!("DNS over UDP to {server}"), e);
    let socket = UdpSocket::bind(local).await.map_err(io)?;
    socket.connect(server).await.map_err(io)?;
    socket.send(bytes).await.map_err(io)?;
    let mut buf = vec![0_u8; 4096];
    loop {
        let len = socket.recv(&mut buf).await.map_err(io)?;
        match Message::from_vec(&buf[..len]) {
            // Late answers to earlier queries on a reused port are skipped.
            Ok(message) if message.metadata.id == id => return Ok(message),
            Ok(_) => continue,
            Err(err) => return Err(proto_err("invalid DNS answer", err)),
        }
    }
}

async fn tcp_exchange(server: SocketAddr, bytes: &[u8]) -> Result<Message> {
    let io = |e| ForgeError::io(format!("DNS over TCP to {server}"), e);
    let mut stream = TcpStream::connect(server).await.map_err(io)?;
    let len = u16::try_from(bytes.len())
        .map_err(|_| ForgeError::InvalidInput("DNS query too long".to_owned()))?;
    let mut framed = len.to_be_bytes().to_vec();
    framed.extend_from_slice(bytes);
    stream.write_all(&framed).await.map_err(io)?;
    let mut len = [0_u8; 2];
    stream.read_exact(&mut len).await.map_err(io)?;
    let mut buf = vec![0_u8; usize::from(u16::from_be_bytes(len))];
    stream.read_exact(&mut buf).await.map_err(io)?;
    Message::from_vec(&buf).map_err(|e| proto_err("invalid DNS answer", e))
}
