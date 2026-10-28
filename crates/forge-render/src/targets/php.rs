//! PHP files and where they go (docs/03 §3.2): the overlay `10-forgebox.ini` and, when
//! Xdebug is installed, `20-xdebug.ini`, both in `conf.d` scanned through
//! `PHP_INI_SCAN_DIR`. INI files use `;` comments (`#` is a parse error in PHP 7+).
//! The user's own `php.ini` is never touched.

use super::{FileSpec, Ownership, Scope, Target};
use crate::managed::CommentStyle;

/// Files rendered for PHP.
pub static TARGET: Target = Target {
    id: "php",
    dirs: &[],
    files: &[
        FileSpec {
            scope: Scope::Global,
            template: "php/php.ini.j2",
            out: "php/{series}/conf.d/10-forgebox.ini",
            ownership: Ownership::Generated,
            style: CommentStyle::Semicolon,
            when: None,
        },
        FileSpec {
            scope: Scope::Global,
            template: "php/xdebug.ini.j2",
            out: "php/{series}/conf.d/20-xdebug.ini",
            ownership: Ownership::Generated,
            style: CommentStyle::Semicolon,
            when: Some("/php/xdebug"),
        },
    ],
};
