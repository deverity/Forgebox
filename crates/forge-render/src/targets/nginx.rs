//! nginx files and where they go (docs/03 §3.2, docs/08 §2).
//!
//! No per-tool logic: this only maps the neutral model to templates.

use super::{FileSpec, Ownership, Scope, Target};
use crate::managed::CommentStyle;

/// Files rendered for nginx.
pub static TARGET: Target = Target {
    id: "nginx",
    dirs: &[
        "nginx/conf.d",
        "nginx/snippets",
        "nginx/upstreams",
        "nginx/sites",
    ],
    files: &[
        FileSpec {
            scope: Scope::Global,
            template: "nginx/nginx.conf.j2",
            out: "nginx/nginx.conf",
            ownership: Ownership::Managed("main"),
            style: CommentStyle::Hash,
            when: None,
        },
        FileSpec {
            scope: Scope::Global,
            template: "nginx/fastcgi_params.j2",
            out: "nginx/fastcgi_params",
            ownership: Ownership::Generated,
            style: CommentStyle::Hash,
            when: None,
        },
        FileSpec {
            scope: Scope::Global,
            template: "nginx/snippets/php-common.conf.j2",
            out: "nginx/snippets/php-common.conf",
            ownership: Ownership::Generated,
            style: CommentStyle::Hash,
            when: None,
        },
        FileSpec {
            scope: Scope::SiteSsl,
            template: "nginx/snippets/ssl.conf.j2",
            out: "nginx/snippets/ssl-{host}.conf",
            ownership: Ownership::Generated,
            style: CommentStyle::Hash,
            when: None,
        },
        FileSpec {
            scope: Scope::Pool,
            template: "nginx/upstream.conf.j2",
            out: "nginx/upstreams/{pool}.conf",
            ownership: Ownership::Generated,
            style: CommentStyle::Hash,
            when: None,
        },
        FileSpec {
            scope: Scope::Site,
            template: "nginx/site-{kind}.conf.j2",
            out: "nginx/sites/{host}.conf",
            ownership: Ownership::Generated,
            style: CommentStyle::Hash,
            when: None,
        },
    ],
};
