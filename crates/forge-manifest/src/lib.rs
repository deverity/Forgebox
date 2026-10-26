//! Forgebox manifest schema (forgebox.yaml), parser, semantic validation and version rules.
//!
//! Crate of the Forgebox workspace, see docs/02-project-structure.md.
//! Only the provisional schema stubs and lint exist so far (M0-E1-T2); the real ones
//! come in M1-E1-T1.

pub mod lint;
pub mod schema;

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-manifest");
    }
}
