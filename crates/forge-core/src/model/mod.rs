//! Domain entities shared by several crates (docs/01 §4). Each entity lands here with
//! the task that first needs it; the SQLite mapping lives in the matching repository of
//! [`crate::store`].
//!
//! * [`port`]: logical port reservations (`ports` table, docs/03 §4; M1-E3-T3).

pub mod port;

pub use port::{OwnerKind, PortHolder, PortProtocol, PortReservation, PortState, ReservationOwner};
