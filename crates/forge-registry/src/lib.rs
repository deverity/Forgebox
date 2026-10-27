//! Forgebox manifest registry: sources, verification, cache, search and discovery of
//! folders in bin (docs/04 §5, docs/05 §7, docs/11 §2).
//!
//! * [`source`]: `Dir`, `Http` and `Git` registry sources (git through the machine's `git`).
//! * [`index`]: minisign verification of `index.yaml`, sha256 of every manifest, policy for
//!   third-party registries (no `post_install.run`).
//! * [`cache`]: `state/registry/<name>/`, re-verified on every read.
//! * [`search`]: id/name/tag search with a simple ranking.
//! * [`service`]: [`Registry`], the async service (`refresh` returns a `JobId`, `find`,
//!   `search`, `list`), with metadata in the `registries` SQLite table.
//! * [`messages`]: pt-BR strings for badges ("não verificado", "terceiro").
//!
//! This crate does not download tool binaries (that is `forge-provider`) and has no
//! tool-specific logic: everything is data from the manifests.

pub mod cache;
pub mod index;
pub mod messages;
pub mod search;
pub mod service;
pub mod source;

pub use index::{LoadOptions, Verification, VerifyOptions};
pub use search::{CatalogEntry, MatchKind, SearchHit};
pub use service::{LoadReport, LoadSource, Registry, RegistryConfig, RegistryInfo, ServiceOptions};
pub use source::RegistrySource;

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-registry");
    }
}
