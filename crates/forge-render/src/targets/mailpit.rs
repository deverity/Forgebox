//! Mailpit command line (docs/03 §3.2, docs/08 §2): one argument per line in
//! `etc/mailpit/<instance>.args`. The format has no comments, so no header is written;
//! the supervisor reads the file as an argv list.

use super::{FileSpec, Ownership, Scope, Target};
use crate::managed::CommentStyle;

/// Files rendered for Mailpit.
pub static TARGET: Target = Target {
    id: "mailpit",
    dirs: &[],
    files: &[FileSpec {
        scope: Scope::Global,
        template: "mailpit/args.j2",
        out: "mailpit/{instance}.args",
        ownership: Ownership::Generated,
        style: CommentStyle::None,
        when: None,
    }],
};
