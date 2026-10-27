//! Forgebox networking: port allocation, *.test DNS server, NRPT/hosts policy and firewall.
//!
//! Crate of the Forgebox workspace, see docs/02-project-structure.md.
//!
//! * [`ports`]: the port allocator (named ranges of `forgebox.toml`, reservations in the
//!   `ports` table, conflicts with processes outside Forgebox through
//!   [`forge_core::platform::PortTable`]; docs/03 §4, M1-E3-T3).
//!
//! DNS server, resolver policy and firewall rules come with their roadmap tasks
//! (M1-E4-T3, docs/10).

pub mod ports;

pub use ports::{AllocatorOptions, PortAllocator, PortConflict};

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-net");
    }
}
