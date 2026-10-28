//! mysql instance configuration (docs/03 §3.2): `my.ini` in a managed block, so the
//! user's own sections survive regeneration.

use super::{FileSpec, Ownership, Scope, Target};
use crate::managed::CommentStyle;

/// Files rendered for mysql.
pub static TARGET: Target = Target {
    id: "mysql",
    dirs: &[],
    files: &[FileSpec {
        scope: Scope::Global,
        template: "mysql/my.ini.j2",
        out: "mysql/{instance}/my.ini",
        ownership: Ownership::Managed("main"),
        style: CommentStyle::Hash,
        when: None,
    }],
};
