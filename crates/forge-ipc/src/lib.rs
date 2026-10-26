//! Forgebox API contracts: JSON-RPC 2.0 types, transport abstraction, event streams and client.
//!
//! Crate of the Forgebox workspace, see docs/02-project-structure.md.
//! Only `api::contracts` exists so far (M0-E1-T2); the rest starts in M1-E5-T1.

pub mod api;

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-ipc");
    }
}
