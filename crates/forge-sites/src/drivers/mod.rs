//! Framework drivers (docs/06 §1.2): detection + document root + vhost rules.
//!
//! A [`Driver`] is plain Rust (not PHP as in Valet) so it is fast and works with any web
//! server: it produces the server-neutral `VhostModel` that `forge-render` translates.
//!
//! Detection: every driver looks at the project folder and answers with a [`Confidence`]
//! or nothing. [`detect`] picks the highest confidence; ties go to the driver listed
//! first in [`all`] (framework drivers before `generic-php`, `spa` before `static`).
//! A manual override (`forgebox.yaml spec.site.driver`, `forge site add --driver`) skips
//! detection. A folder nothing recognizes gets `generic-php` with the folder as root.
//!
//! All markers are matched case-sensitively as written (docs/15): `Artisan` is not
//! `artisan`.

mod php;
mod web;

use std::path::{Path, PathBuf};

use forge_core::error::{ForgeError, Result};
use forge_core::store::sites::{Site, SiteKind};
use forge_render::{ExtraModel, VhostModel};

use crate::vhost;

pub use php::{CakePhp, Drupal, GenericPhp, Laravel, Magento, Symfony, WordPress};
pub use web::{Spa, Static};

/// How sure a driver is that a folder is its kind of project. Ordered: `High` wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Confidence {
    /// Weak hint (some `.php` file).
    Low,
    /// A usable entry point without framework markers.
    Medium,
    /// Framework marker files and the expected layout are present.
    High,
}

/// What a driver needs from the environment to build a vhost.
#[derive(Debug, Clone)]
pub struct VhostContext {
    /// `certs/sites` directory (`certs/sites/<host>.crt|key`).
    pub certs_sites: PathBuf,
    /// PHP series used when the site pins none (`prefs`/default), e.g. `8.3`.
    pub default_php_series: String,
    /// Hand-written per-server snippets of the user, inserted verbatim.
    pub extra: ExtraModel,
}

/// Detection and rules of one kind of project.
pub trait Driver: Send + Sync {
    /// Stable id stored in `sites.driver` (`laravel`).
    fn id(&self) -> &'static str;

    /// `Php` (default) or `Static`.
    fn kind(&self) -> SiteKind {
        SiteKind::Php
    }

    /// Confidence that `dir` (the project folder) is this kind of project.
    fn detect(&self, dir: &Path) -> Option<Confidence>;

    /// Document root for the project at `dir`.
    fn root(&self, dir: &Path) -> PathBuf;

    /// Driver-specific changes on top of the common model (try_files, deny...).
    fn tune(&self, _model: &mut VhostModel) {}

    /// Neutral vhost of `site` (not yet normalized; see [`vhost::build`]).
    fn vhost(&self, site: &Site, ctx: &VhostContext) -> VhostModel {
        let mut model = vhost::base_model(site, ctx, self.kind());
        self.tune(&mut model);
        model
    }
}

static LARAVEL: Laravel = Laravel;
static SYMFONY: Symfony = Symfony;
static WORDPRESS: WordPress = WordPress;
static CAKEPHP: CakePhp = CakePhp;
static DRUPAL: Drupal = Drupal;
static MAGENTO: Magento = Magento;
static GENERIC_PHP: GenericPhp = GenericPhp;
static SPA: Spa = Spa;
static STATIC: Static = Static;

/// Every driver, in tie-break order.
pub fn all() -> &'static [&'static dyn Driver] {
    static ALL: [&dyn Driver; 9] = [
        &LARAVEL,
        &SYMFONY,
        &WORDPRESS,
        &CAKEPHP,
        &DRUPAL,
        &MAGENTO,
        &GENERIC_PHP,
        &SPA,
        &STATIC,
    ];
    &ALL
}

/// Driver with the given id.
pub fn by_id(id: &str) -> Option<&'static dyn Driver> {
    all().iter().copied().find(|d| d.id() == id)
}

/// Outcome of detection for one folder: driver, how it was chosen and the document root.
#[derive(Clone)]
pub struct Detection {
    pub driver: &'static dyn Driver,
    /// `None` when the driver was forced or nothing matched (fallback).
    pub confidence: Option<Confidence>,
    /// The user forced the driver.
    pub overridden: bool,
    /// Document root.
    pub root: PathBuf,
}

impl std::fmt::Debug for Detection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Detection")
            .field("driver", &self.driver.id())
            .field("confidence", &self.confidence)
            .field("overridden", &self.overridden)
            .field("root", &self.root)
            .finish()
    }
}

/// Chooses the driver of the project at `dir`. `driver_override` skips detection; an
/// unknown id is an error. Blocking (a few `stat`s).
pub fn detect(dir: &Path, driver_override: Option<&str>) -> Result<Detection> {
    if let Some(id) = driver_override {
        let driver = by_id(id)
            .ok_or_else(|| ForgeError::InvalidInput(format!("driver desconhecido: {id}")))?;
        return Ok(Detection {
            driver,
            confidence: None,
            overridden: true,
            root: driver.root(dir),
        });
    }
    let mut best: Option<(&'static dyn Driver, Confidence)> = None;
    for driver in all() {
        if let Some(c) = driver.detect(dir) {
            // Strictly greater: on a tie the driver listed first stays.
            if best.is_none_or(|(_, b)| c > b) {
                best = Some((*driver, c));
            }
        }
    }
    Ok(match best {
        Some((driver, c)) => Detection {
            driver,
            confidence: Some(c),
            overridden: false,
            root: driver.root(dir),
        },
        None => Detection {
            driver: &GENERIC_PHP,
            confidence: None,
            overridden: false,
            root: dir.to_path_buf(),
        },
    })
}

pub(crate) fn is_file(dir: &Path, rel: &str) -> bool {
    dir.join(rel).is_file()
}

pub(crate) fn is_dir(dir: &Path, rel: &str) -> bool {
    dir.join(rel).is_dir()
}

/// Whether `dir` directly contains a file ending in `.php`.
pub(crate) fn has_php_file(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".php") && e.path().is_file())
    })
}
