//! Forgebox daemon (`forgeboxd`): composes the domain crates and serves the API
//! (docs/01 §1 and §5.1, docs/03 §6-§7).
//!
//! * [`boot`]: the boot sequence of docs/01 §5.1, producing a [`daemon::Daemon`];
//! * [`api`]: one handler per method of `forge_ipc::api::METHODS`, delegating to the
//!   domain actors; methods of later roadmap tasks answer `not_implemented`;
//! * [`services`], [`pools`], [`web`]: instances from manifests, PHP pools, sites rendered
//!   into the web servers; [`settings`]: the quick settings of a manifest (docs/04 §1.3);
//! * [`scheduler`]: periodic work with jitter (updates, registry, `tmp/`, ports) and the
//!   reactions to site/TLS events;
//! * [`shutdown`]: graceful stop in cascade with a global budget;
//! * [`logging`]: `logs/forgeboxd.log.<date>` (JSON lines) and stdout in `--foreground`;
//! * [`instance_lock`]: the single-instance mutex and `state/locks/forgeboxd.lock`.
//!
//! Nothing here knows a tool by id: everything a service needs comes from its manifest.

pub mod api;
pub mod args;
pub mod boot;
pub mod catalog;
pub mod daemon;
pub mod event_log;
pub mod install;
pub mod instance_case;
pub mod instance_lock;
pub mod internal;
pub mod logging;
pub mod messages;
pub mod pools;
pub mod render;
pub mod scheduler;
pub mod services;
pub mod settings;
pub mod shutdown;
pub mod update_feed;
pub mod vars;
pub mod web;

/// Process exit codes of `forgeboxd` (docs/09 codes where they apply).
pub mod exit {
    /// Clean stop.
    pub const OK: i32 = 0;
    /// Boot or runtime failure.
    pub const FAILED: i32 = 1;
    /// Invalid command line or a flag of a later milestone.
    pub const USAGE: i32 = 2;
    /// Another daemon already serves this root.
    pub const ALREADY_RUNNING: i32 = 4;
}

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-daemon");
    }
}
