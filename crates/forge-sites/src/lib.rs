//! Forgebox sites: site model, framework drivers, parked dirs and vhosts.
//!
//! Crate of the Forgebox workspace, see docs/02-project-structure.md and
//! docs/06-sites-vhosts-multi-php.md.
//!
//! - [`site`]: `Site` (stored by `forge-core`), host/alias validation, [`site::SiteDraft`].
//! - [`service`]: CRUD over the store with `site.created|updated|removed` events.
//! - [`drivers`]: `Driver` trait and the built-in drivers (detection, root, vhost rules).
//! - [`vhost`]: `Site` to the server-neutral `VhostModel` of `forge-render`.
//! - [`parked`]: parked directories, reconcile and the debounced `notify` watcher.
//! - [`project_file`]: `forgebox.yaml` loader producing a [`project_file::ProjectPlan`].
//!
//! Proxies and Procfile apps arrive with M1-E4-T5.

pub mod drivers;
pub mod messages;
pub mod parked;
pub mod project_file;
pub mod service;
pub mod site;
pub mod vhost;

pub use drivers::{Confidence, Detection, Driver, VhostContext};
pub use parked::{ParkedConfig, ParkedDir, ParkedSync, ParkedWatcher};
pub use project_file::ProjectPlan;
pub use service::SiteService;
pub use site::{Site, SiteDraft, SiteKind, SiteRules};

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-sites");
    }
}
