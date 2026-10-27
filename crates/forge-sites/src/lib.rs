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
//! - [`lint_case`]: Case Mismatch Finder (docs/15 §6): references whose case differs from
//!   the files on disk, with fixes (rename or edit) and a dry-run preview.
//! - [`case_parity`]: case sensitivity per site (badge state, boot re-verification,
//!   `forge site case-sensitive` and `forge site lint-case` as jobs).
//! - [`parity`]: Git/`.gitattributes`/`.editorconfig` parity for `forge new`/`site add`.
//!
//! - [`proxy`]: reverse-proxy sites (`ProxyModel`: upstream, WebSocket, headers, TLS).
//! - [`apps`]: Procfile 2.0 processes (`$PORT`, `runtime:`, deterministic `PATH`,
//!   `autostart: on-visit`) supervised by `AppManager`.

pub mod apps;
pub mod case_parity;
pub mod drivers;
pub mod lint_case;
pub mod messages;
pub mod parity;
pub mod parked;
pub mod project_file;
pub mod proxy;
pub mod service;
pub mod site;
pub mod vhost;

pub use apps::{AppManager, AppSite, AppSpec};
pub use case_parity::{CaseParity, SiteCaseCheck};
pub use drivers::{Confidence, Detection, Driver, VhostContext};
pub use parity::{GitParityOptions, GitParityReport, git_parity_init};
pub use parked::{ParkedConfig, ParkedDir, ParkedSync, ParkedWatcher};
pub use project_file::ProjectPlan;
pub use proxy::{ProxyOptions, ProxyTarget};
pub use service::SiteService;
pub use site::{Site, SiteDraft, SiteKind, SiteRules};

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-sites");
    }
}
