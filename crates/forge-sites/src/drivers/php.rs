//! PHP framework drivers.

use std::path::{Path, PathBuf};

use forge_render::VhostModel;

use super::{Confidence, Driver, has_php_file, is_dir, is_file};

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// Laravel: `artisan` + `public/index.php`; document root `public/`.
#[derive(Debug, Clone, Copy)]
pub struct Laravel;

impl Driver for Laravel {
    fn id(&self) -> &'static str {
        "laravel"
    }

    fn detect(&self, dir: &Path) -> Option<Confidence> {
        (is_file(dir, "artisan") && is_file(dir, "public/index.php")).then_some(Confidence::High)
    }

    fn root(&self, dir: &Path) -> PathBuf {
        dir.join("public")
    }
}

/// Symfony: `bin/console` + `public/index.php`; document root `public/`. Every request
/// that is not a file goes to the front controller (no `$uri/`: directories are routes).
#[derive(Debug, Clone, Copy)]
pub struct Symfony;

impl Driver for Symfony {
    fn id(&self) -> &'static str {
        "symfony"
    }

    fn detect(&self, dir: &Path) -> Option<Confidence> {
        (is_file(dir, "bin/console") && is_file(dir, "public/index.php"))
            .then_some(Confidence::High)
    }

    fn root(&self, dir: &Path) -> PathBuf {
        dir.join("public")
    }

    fn tune(&self, model: &mut VhostModel) {
        model.try_files = strings(&["$uri", "/index.php?$query_string"]);
    }
}

/// WordPress: `wp-config.php`/`wp-load.php` at the root (classic) or under `web/`
/// (Bedrock). Pretty permalinks need the `/index.php?$args` fallback; PHP files inside
/// `wp-content/uploads` are never executed.
#[derive(Debug, Clone, Copy)]
pub struct WordPress;

impl WordPress {
    fn classic(dir: &Path) -> bool {
        is_file(dir, "wp-config.php") || is_file(dir, "wp-load.php")
    }
}

impl Driver for WordPress {
    fn id(&self) -> &'static str {
        "wordpress"
    }

    fn detect(&self, dir: &Path) -> Option<Confidence> {
        if Self::classic(dir) || is_file(dir, "web/wp-config.php") {
            Some(Confidence::High)
        } else if is_dir(dir, "wp-content") && is_file(dir, "index.php") {
            Some(Confidence::Medium)
        } else {
            None
        }
    }

    fn root(&self, dir: &Path) -> PathBuf {
        if Self::classic(dir) || !is_file(dir, "web/wp-config.php") {
            dir.to_path_buf()
        } else {
            dir.join("web")
        }
    }

    fn tune(&self, model: &mut VhostModel) {
        model.try_files = strings(&["$uri", "$uri/", "/index.php?$args"]);
        model.deny.push(r"/wp-content/uploads/.*\.php$".to_owned());
    }
}

/// CakePHP 3+: `bin/cake.php` (or `bin/cake`) + `webroot/`; CakePHP 2: `lib/Cake` +
/// `app/webroot/`.
#[derive(Debug, Clone, Copy)]
pub struct CakePhp;

impl Driver for CakePhp {
    fn id(&self) -> &'static str {
        "cakephp"
    }

    fn detect(&self, dir: &Path) -> Option<Confidence> {
        let modern = (is_file(dir, "bin/cake.php") || is_file(dir, "bin/cake"))
            && is_file(dir, "webroot/index.php");
        if modern {
            Some(Confidence::High)
        } else if is_dir(dir, "lib/Cake") && is_file(dir, "app/webroot/index.php") {
            Some(Confidence::Medium)
        } else {
            None
        }
    }

    fn root(&self, dir: &Path) -> PathBuf {
        if is_file(dir, "webroot/index.php") {
            dir.join("webroot")
        } else {
            dir.join("app").join("webroot")
        }
    }

    fn tune(&self, model: &mut VhostModel) {
        model.try_files = strings(&["$uri", "$uri/", "/index.php?$args"]);
    }
}

/// Drupal 8+: `core/lib/Drupal.php` at the root, or under `web/` / `docroot/`
/// (composer layouts). The files Drupal protects with `.htaccess` are denied.
#[derive(Debug, Clone, Copy)]
pub struct Drupal;

impl Drupal {
    const MARKER: &'static str = "core/lib/Drupal.php";

    fn docroot(dir: &Path) -> Option<&'static str> {
        ["", "web", "docroot"].into_iter().find(|sub| {
            let marker = if sub.is_empty() {
                Self::MARKER.to_owned()
            } else {
                format!("{sub}/{}", Self::MARKER)
            };
            is_file(dir, &marker)
        })
    }
}

impl Driver for Drupal {
    fn id(&self) -> &'static str {
        "drupal"
    }

    fn detect(&self, dir: &Path) -> Option<Confidence> {
        Self::docroot(dir).map(|_| Confidence::High)
    }

    fn root(&self, dir: &Path) -> PathBuf {
        match Self::docroot(dir) {
            Some(sub) if !sub.is_empty() => dir.join(sub),
            _ => dir.to_path_buf(),
        }
    }

    fn tune(&self, model: &mut VhostModel) {
        model.try_files = strings(&["$uri", "/index.php?$query_string"]);
        model.deny.extend(strings(&[
            r"/sites/[^/]+/files/.*\.php$",
            r"/composer\.(?:json|lock)$",
            r"\.(?:engine|inc|install|module|profile|sql|theme|twig)$",
        ]));
    }
}

/// Magento 2: `bin/magento` + `pub/`; document root `pub/`. Developer mode by default.
#[derive(Debug, Clone, Copy)]
pub struct Magento;

impl Driver for Magento {
    fn id(&self) -> &'static str {
        "magento"
    }

    fn detect(&self, dir: &Path) -> Option<Confidence> {
        (is_file(dir, "bin/magento") && is_dir(dir, "pub")).then_some(Confidence::High)
    }

    fn root(&self, dir: &Path) -> PathBuf {
        dir.join("pub")
    }

    fn tune(&self, model: &mut VhostModel) {
        model.try_files = strings(&["$uri", "$uri/", "/index.php?$query_string"]);
        model
            .env
            .entry("MAGE_MODE".to_owned())
            .or_insert_with(|| "developer".to_owned());
        model.deny.push(r"/\.user\.ini$".to_owned());
    }
}

/// Any PHP project: `index.php` at the root (Medium), or under `public/`,
/// `public_html/` or `web/` (Medium); other `.php` files at the root only give Low.
/// Also the fallback when nothing is recognized.
#[derive(Debug, Clone, Copy)]
pub struct GenericPhp;

impl GenericPhp {
    const SUBDIRS: [&'static str; 3] = ["public", "public_html", "web"];
}

impl Driver for GenericPhp {
    fn id(&self) -> &'static str {
        "generic-php"
    }

    fn detect(&self, dir: &Path) -> Option<Confidence> {
        if is_file(dir, "index.php")
            || Self::SUBDIRS
                .iter()
                .any(|s| is_file(dir, &format!("{s}/index.php")))
        {
            Some(Confidence::Medium)
        } else if has_php_file(dir) {
            Some(Confidence::Low)
        } else {
            None
        }
    }

    fn root(&self, dir: &Path) -> PathBuf {
        if is_file(dir, "index.php") {
            return dir.to_path_buf();
        }
        Self::SUBDIRS
            .iter()
            .find(|s| is_file(dir, &format!("{s}/index.php")))
            .map_or_else(|| dir.to_path_buf(), |s| dir.join(s))
    }

    fn tune(&self, model: &mut VhostModel) {
        // Plain PHP sites rely on files and directory indexes; unknown paths still reach
        // the front controller when there is one (the common `try_files` default).
        model.try_files = strings(&["$uri", "$uri/", "/index.php?$query_string"]);
    }
}
