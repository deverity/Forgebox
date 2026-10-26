//! PostgreSQL instance configuration (docs/03 §3.2). `postgresql.conf` is a managed block
//! appended after initdb's own file (the last assignment wins); `pg_hba.conf` is fully
//! generated because its security posture (trust only on loopback) is not negotiable.

use super::{FileSpec, Ownership, Scope, Target};
use crate::managed::CommentStyle;

/// Files rendered for PostgreSQL.
pub static TARGET: Target = Target {
    id: "postgresql",
    dirs: &[],
    files: &[
        FileSpec {
            scope: Scope::Global,
            template: "postgresql/postgresql.conf.j2",
            out: "postgresql/{instance}/postgresql.conf",
            ownership: Ownership::Managed("main"),
            style: CommentStyle::Hash,
            when: None,
        },
        FileSpec {
            scope: Scope::Global,
            template: "postgresql/pg_hba.conf.j2",
            out: "postgresql/{instance}/pg_hba.conf",
            ownership: Ownership::Generated,
            style: CommentStyle::Hash,
            when: None,
        },
    ],
};
