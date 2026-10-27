//! Forgebox download providers, sha256 verification, extraction and versioned layout with current junction.
//!
//! Crate of the Forgebox workspace, see docs/02-project-structure.md. Implemented so
//! far: [`download`] (M1-E2-T1), [`verify`] and [`extract`] (M1-E2-T2), [`provider`]
//! (M1-E2-T3). The rest is
//! placeholder until its roadmap task (docs/10).

pub mod download;
pub mod extract;
mod messages;
pub mod provider;
pub mod verify;

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-provider");
    }
}
