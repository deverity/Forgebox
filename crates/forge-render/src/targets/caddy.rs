//! Caddy files and where they go (docs/03 §3.2). TLS is inline in each site file, so
//! there are no snippets; the pool addresses are read from `pools[]` by the site template.

use super::{FileSpec, Ownership, Scope, Target};
use crate::managed::CommentStyle;

/// Files rendered for Caddy.
pub static TARGET: Target = Target {
    id: "caddy",
    dirs: &["caddy/sites"],
    files: &[
        FileSpec {
            scope: Scope::Global,
            template: "caddy/Caddyfile.j2",
            out: "caddy/Caddyfile",
            ownership: Ownership::Managed("main"),
            style: CommentStyle::Hash,
            when: None,
        },
        FileSpec {
            scope: Scope::Site,
            template: "caddy/site-{kind}.caddy.j2",
            out: "caddy/sites/{host}.caddy",
            ownership: Ownership::Generated,
            style: CommentStyle::Hash,
            when: None,
        },
    ],
};
