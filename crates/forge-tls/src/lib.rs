//! Forgebox local TLS: local CA, leaf certificates, user trust store and renewal.
//!
//! Crate of the Forgebox workspace, see docs/02-project-structure.md.
//! Placeholder: the implementation starts in its roadmap task (see docs/10).

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-tls");
    }
}
