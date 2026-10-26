//! Forgebox domain base: IDs, errors, events, actors, jobs, SQLite store, runtime,
//! paths and platform traits.
//!
//! Crate of the Forgebox workspace, see docs/02-project-structure.md. Everything here
//! that touches the disk or Win32 is blocking; async code calls it from
//! `spawn_blocking` (docs/03 §7, ADR-0011). The concurrency skeleton lives in
//! [`actor`] (actors, supervisor, lock order), [`jobs`] (long operations as `job_id` +
//! `job.*` events), [`store`] (dedicated SQLite writer thread + read pool) and
//! [`runtime`] (tokio runtime configuration, opt-in tokio-console).

pub mod actor;
pub mod config;
pub mod error;
pub mod event;
pub mod ids;
pub mod paths;
pub mod platform;
pub mod runtime;
pub mod store;
pub mod sync;

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
