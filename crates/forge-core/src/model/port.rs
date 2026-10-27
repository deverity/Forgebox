//! Logical port reservations (docs/03 §4, docs/01 §4 `Port`): which Forgebox owner
//! (instance, pool, app, site, the daemon itself) holds which port, so a port is never
//! "guessed" twice. Rows of the `ports` table ([`crate::store::PortRepo`]); allocated by
//! `forge_net::ports::PortAllocator`.
//!
//! [`PortReservations`] is the allocator seen from crates that cannot depend on
//! `forge-net` (the PHP pools of `forge-fcgi` take one by injection; docs/01 §3).

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::Timestamp;
use crate::error::{ForgeError, Result};
use crate::ids::PortReservationId;

/// Transport protocol of a reservation (`ports.protocol`). Not to be confused with
/// [`crate::platform::PortProto`], the address family of an OS TCP socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortProtocol {
    Tcp,
    Udp,
}

/// Kind of owner of a reservation (`ports.owner_kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerKind {
    Instance,
    Pool,
    App,
    Site,
    Daemon,
}

/// State of a reservation (`ports.state`), refreshed by the conflict check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortState {
    /// Held for the owner; nobody listens on it.
    Reserved,
    /// A process of the owner listens on it.
    Bound,
    /// A process outside Forgebox (or another owner) listens on it.
    Conflict,
}

macro_rules! wire_enum {
    ($ty:ident { $($variant:ident => $name:literal),+ $(,)? }) => {
        impl $ty {
            /// Wire and SQLite name.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)+
                }
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $ty {
            type Err = ForgeError;

            fn from_str(s: &str) -> Result<Self> {
                match s {
                    $($name => Ok(Self::$variant),)+
                    other => Err(ForgeError::InvalidInput(format!(
                        concat!("invalid ", stringify!($ty), " {:?}"),
                        other
                    ))),
                }
            }
        }
    };
}

wire_enum!(PortProtocol { Tcp => "tcp", Udp => "udp" });
wire_enum!(OwnerKind {
    Instance => "instance",
    Pool => "pool",
    App => "app",
    Site => "site",
    Daemon => "daemon",
});
wire_enum!(PortState {
    Reserved => "reserved",
    Bound => "bound",
    Conflict => "conflict",
});

/// Who holds a reservation: one port per `(kind, id, port_name)` (`UNIQUE` in the
/// table). `id` is the ULID of the instance/app/site, the version id of a pool, or a
/// fixed name for the daemon (`dns`, `xdebug`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ReservationOwner {
    pub kind: OwnerKind,
    pub id: String,
    /// Port name in the manifest (`main`, `smtp`, `ui`) or a worker slot of a pool
    /// (`worker-3`).
    pub port_name: String,
}

impl ReservationOwner {
    /// Owner `kind`/`id` for its port `port_name`. Both texts must be non-empty, at most
    /// 128 bytes long and free of control characters (`InvalidInput` otherwise).
    pub fn new(
        kind: OwnerKind,
        id: impl Into<String>,
        port_name: impl Into<String>,
    ) -> Result<Self> {
        let owner = Self {
            kind,
            id: id.into(),
            port_name: port_name.into(),
        };
        owner.validate()?;
        Ok(owner)
    }

    /// Checks the texts (see [`ReservationOwner::new`]).
    pub fn validate(&self) -> Result<()> {
        for (what, text) in [("owner id", &self.id), ("port name", &self.port_name)] {
            if text.is_empty() || text.len() > 128 || text.chars().any(char::is_control) {
                return Err(ForgeError::InvalidInput(format!(
                    "{what} {text:?} must be 1-128 printable characters"
                )));
            }
        }
        Ok(())
    }
}

impl fmt::Display for ReservationOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}#{}", self.kind, self.id, self.port_name)
    }
}

/// A process seen listening on a reserved port that does not belong to its owner.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortHolder {
    pub pid: u32,
    /// Full path of its executable, when readable.
    pub exe: Option<String>,
}

/// A row of `ports`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortReservation {
    pub id: PortReservationId,
    pub port: u16,
    pub protocol: PortProtocol,
    /// Named range of `forgebox.toml` it was allocated from; `None` for an explicit port.
    pub range: Option<String>,
    pub owner: ReservationOwner,
    pub state: PortState,
    /// Set when `state` is [`PortState::Conflict`].
    pub conflict: Option<PortHolder>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl PortReservation {
    /// New `reserved` TCP reservation of `port` for `owner`, stamped now.
    pub fn new_tcp(port: u16, range: Option<String>, owner: ReservationOwner) -> Self {
        let now = Timestamp::now();
        Self {
            id: PortReservationId::new(),
            port,
            protocol: PortProtocol::Tcp,
            range,
            owner,
            state: PortState::Reserved,
            conflict: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Future returned by [`PortReservations`].
pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Hands out and takes back port reservations (implemented by
/// `forge_net::ports::PortAllocator`; fakes in tests). Every method is async and bounded
/// by the implementation (store writer and TCP table timeouts).
pub trait PortReservations: Send + Sync + fmt::Debug {
    /// The reservation of `owner`, allocating the first free port of the named `range`
    /// (`php-pools`) the first time; the same port on later calls for the same owner.
    fn allocate<'a>(
        &'a self,
        range: &'a str,
        owner: ReservationOwner,
    ) -> PortFuture<'a, PortReservation>;

    /// Drops the reservation of `owner`; returns it if there was one.
    fn release<'a>(
        &'a self,
        owner: &'a ReservationOwner,
    ) -> PortFuture<'a, Option<PortReservation>>;

    /// Drops every reservation of `kind`/`id` (all the workers of a pool); returns how
    /// many there were.
    fn release_owner<'a>(&'a self, kind: OwnerKind, id: &'a str) -> PortFuture<'a, usize>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_match_the_table_checks() {
        for (kind, name) in [
            (OwnerKind::Instance, "instance"),
            (OwnerKind::Pool, "pool"),
            (OwnerKind::App, "app"),
            (OwnerKind::Site, "site"),
            (OwnerKind::Daemon, "daemon"),
        ] {
            assert_eq!(kind.as_str(), name);
            assert_eq!(name.parse::<OwnerKind>().unwrap(), kind);
            assert_eq!(serde_json::to_value(kind).unwrap(), name);
        }
        for state in [PortState::Reserved, PortState::Bound, PortState::Conflict] {
            assert_eq!(state.as_str().parse::<PortState>().unwrap(), state);
        }
        assert_eq!("udp".parse::<PortProtocol>().unwrap(), PortProtocol::Udp);
        let err = "TCP".parse::<PortProtocol>().unwrap_err();
        assert_eq!(err.code(), "invalid_input");
    }

    #[test]
    fn owners_are_validated() {
        let owner = ReservationOwner::new(OwnerKind::Instance, "01J0000000000000000000000", "main")
            .unwrap();
        assert_eq!(owner.to_string(), "instance/01J0000000000000000000000#main");
        for (id, name) in [
            ("", "main"),
            ("x", ""),
            ("x", "a\nb"),
            (&*"y".repeat(129), "m"),
        ] {
            assert!(
                ReservationOwner::new(OwnerKind::App, id, name).is_err(),
                "{id:?} {name:?}"
            );
        }
        let r = PortReservation::new_tcp(3306, Some("databases".into()), owner.clone());
        assert_eq!(
            (r.state, r.protocol, r.conflict),
            (PortState::Reserved, PortProtocol::Tcp, None)
        );
        assert_eq!(r.owner, owner);
    }
}
