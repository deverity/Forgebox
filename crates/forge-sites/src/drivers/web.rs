//! Drivers for sites without PHP: plain static files and built single-page apps.

use std::path::{Path, PathBuf};

use forge_core::store::sites::SiteKind;
use forge_render::VhostModel;

use super::{Confidence, Driver, has_php_file, is_file};

/// Output folders of front-end builds, in order of preference.
const BUILD_DIRS: [&str; 2] = ["dist", "build"];

fn index_html(dir: &Path) -> bool {
    is_file(dir, "index.html") || is_file(dir, "index.htm")
}

/// `index.html` at the root and no PHP at all.
#[derive(Debug, Clone, Copy)]
pub struct Static;

impl Driver for Static {
    fn id(&self) -> &'static str {
        "static"
    }

    fn kind(&self) -> SiteKind {
        SiteKind::Static
    }

    fn detect(&self, dir: &Path) -> Option<Confidence> {
        (index_html(dir) && !has_php_file(dir)).then_some(Confidence::Medium)
    }

    fn root(&self, dir: &Path) -> PathBuf {
        dir.to_path_buf()
    }
}

/// A built SPA: `dist/` or `build/` with `index.html`. Unknown paths fall back to
/// `/index.html` so client-side routing works. PHP projects that merely ship a build
/// folder (an `index.php` at the root or in `public/`) are not SPAs.
#[derive(Debug, Clone, Copy)]
pub struct Spa;

impl Spa {
    fn build_dir(dir: &Path) -> Option<&'static str> {
        BUILD_DIRS
            .into_iter()
            .find(|d| is_file(dir, &format!("{d}/index.html")))
    }
}

impl Driver for Spa {
    fn id(&self) -> &'static str {
        "spa"
    }

    fn kind(&self) -> SiteKind {
        SiteKind::Static
    }

    fn detect(&self, dir: &Path) -> Option<Confidence> {
        let php_entry = is_file(dir, "index.php") || is_file(dir, "public/index.php");
        (Self::build_dir(dir).is_some() && !php_entry).then_some(Confidence::High)
    }

    fn root(&self, dir: &Path) -> PathBuf {
        Self::build_dir(dir).map_or_else(|| dir.to_path_buf(), |d| dir.join(d))
    }

    fn tune(&self, model: &mut VhostModel) {
        model.try_files = ["$uri", "$uri/", "/index.html"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
    }
}
