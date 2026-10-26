//! Forgebox API contracts: JSON-RPC 2.0 types, transport abstraction, event streams and client.
//!
//! Crate of the Forgebox workspace, see docs/02-project-structure.md.
//! Placeholder: the implementation starts in its roadmap task (see docs/10).

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-ipc");
    }
}
