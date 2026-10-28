//! Domain entities shared by several crates (docs/01 §4). Each entity lands here with
//! the task that first needs it; the SQLite mapping lives in the matching repository of
//! [`crate::store`].
//!
//! * [`port`]: logical port reservations (`ports` table, docs/03 §4; M1-E3-T3) and the
//!   [`PortReservations`] trait the PHP pools allocate their worker ports through
//!   (M1-E3-T4).

pub mod port;

pub use port::{
    OwnerKind, PortFuture, PortHolder, PortProtocol, PortReservation, PortReservations, PortState,
    ReservationOwner,
};
