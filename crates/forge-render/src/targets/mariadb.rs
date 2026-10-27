//! mariadb instance configuration (docs/03 §3.2): `my.ini` in a managed block, so the
//! user's own sections survive regeneration.

use super::{FileSpec, Ownership, Scope, Target};
use crate::managed::CommentStyle;

/// Files rendered for mariadb.
pub static TARGET: Target = Target {
    id: "mariadb",
    dirs: &[],
    files: &[FileSpec {
        scope: Scope::Global,
        template: "mariadb/my.ini.j2",
        out: "mariadb/{instance}/my.ini",
        ownership: Ownership::Managed("main"),
        style: CommentStyle::Hash,
        when: None,
    }],
};
