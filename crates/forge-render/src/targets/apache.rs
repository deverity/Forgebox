//! apache files and where they go (docs/03 §3.2, docs/08 §2).
//!
//! No per-tool logic: this only maps the neutral model to templates.

use super::{FileSpec, Ownership, Scope, Target};
use crate::managed::CommentStyle;

/// Files rendered for apache.
pub static TARGET: Target = Target {
    id: "apache",
    dirs: &[
        "apache/conf.d",
        "apache/snippets",
        "apache/upstreams",
        "apache/sites",
    ],
    files: &[
        FileSpec {
            scope: Scope::Global,
            template: "apache/httpd.conf.j2",
            out: "apache/httpd.conf",
            ownership: Ownership::Managed("main"),
            style: CommentStyle::Hash,
            when: None,
        },
        FileSpec {
            scope: Scope::SiteSsl,
            template: "apache/snippets/ssl.conf.j2",
            out: "apache/snippets/ssl-{host}.conf",
            ownership: Ownership::Generated,
            style: CommentStyle::Hash,
            when: None,
        },
        FileSpec {
            scope: Scope::Pool,
            template: "apache/upstream.conf.j2",
            out: "apache/upstreams/{pool}.conf",
            ownership: Ownership::Generated,
            style: CommentStyle::Hash,
            when: None,
        },
        FileSpec {
            scope: Scope::Site,
            template: "apache/site-{kind}.conf.j2",
            out: "apache/sites/{host}.conf",
            ownership: Ownership::Generated,
            style: CommentStyle::Hash,
            when: None,
        },
    ],
};
