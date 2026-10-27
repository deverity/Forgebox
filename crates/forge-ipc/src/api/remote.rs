//! `remote.*`: the daemon side of remote mode (docs/13 §3). Pairing over WSS/mTLS is v2
//! (M6); the contracts exist so that clients and the GUI can be written against them.
//!
//! `forge remote add|list|use|remove` and `--context` manage connection profiles on the
//! **client** machine (`state/remotes/*.toml`): they are not daemon methods.

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::Role;
use forge_core::Timestamp;
use forge_core::ids::ClientId;

/// Result of `remote.pair` (`forge remote pair`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PairingCodeDto {
    /// `1234-5678`.
    pub code: String,
    /// Fingerprint of the daemon CA to pin.
    pub ca_fingerprint: String,
    pub expires_at: Timestamp,
}

/// TLS mode of the remote listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum RemoteTlsMode {
    /// Certificate from the daemon CA.
    #[default]
    Auto,
    /// ACME certificate (needs a domain).
    Acme,
}

/// Params of `remote.listen`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RemoteListenParams {
    /// `0.0.0.0:7788`.
    pub addr: String,
    #[serde(default)]
    pub tls: RemoteTlsMode,
}

/// A paired client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RemoteClientDto {
    pub id: ClientId,
    pub name: String,
    pub role: Role,
    pub fingerprint: String,
    pub paired_at: Timestamp,
    pub last_seen_at: Option<Timestamp>,
}

/// Result of `remote.clients`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RemoteClientListDto {
    pub clients: Vec<RemoteClientDto>,
}

/// Params of `remote.revoke`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RemoteRevokeParams {
    pub client_id: ClientId,
}
