//! Forgebox networking: port allocation, *.test DNS server, NRPT/hosts policy and firewall.
//!
//! Crate of the Forgebox workspace, see docs/02-project-structure.md.
//!
//! * [`ports`]: the port allocator (named ranges of `forgebox.toml`, reservations in the
//!   `ports` table, conflicts with processes outside Forgebox through
//!   [`forge_core::platform::PortTable`]; docs/03 §4, M1-E3-T3).
//! * [`dns`]: the resolver for the local zones (hickory-server on `127.0.0.1:53`, or
//!   `127.0.0.53:53` when taken; remote zones; actor; diagnosis; docs/06 §5, M1-E4-T3).
//! * [`policy`]: how Windows reaches the resolver (NRPT rule through `forgebox-helper`,
//!   `hosts` managed block as fallback, cache flush; ADR-0004).
//!
//! Firewall rules come with LAN exposure (docs/06 §5.3).

pub mod dns;
pub mod messages;
pub mod policy;
pub mod ports;

pub use dns::{DnsDiagnostics, DnsServer, DnsServerConfig, DnsStats, ZoneSet};
pub use policy::{DnsPolicyManager, EnableRequest, PolicyState, TldPolicy};
pub use ports::{AllocatorOptions, PortAllocator, PortConflict};

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-net");
    }
}
