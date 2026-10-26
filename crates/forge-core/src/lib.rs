//! Forgebox domain base: IDs, errors, events, SQLite store, paths and platform traits.
//!
//! Crate of the Forgebox workspace, see docs/02-project-structure.md. Everything here
//! that touches the disk or Win32 is blocking; async code calls it from
//! `spawn_blocking` (docs/03 §7, ADR-0011).

pub mod error;
pub mod event;
pub mod ids;

pub use error::{ForgeError, Result};
/// UTC instant used by every timestamp of the domain (RFC 3339 on the wire, ADR-0013).
pub use jiff::Timestamp;

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-core");
    }
}
