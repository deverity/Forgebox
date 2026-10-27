//! Forgebox API: contracts, JSON-RPC 2.0 protocol, transports, server and client.
//!
//! Crate of the Forgebox workspace (docs/02-project-structure.md). GUI, CLI and MCP
//! depend only on this crate (CLAUDE.md); the daemon serves it.
//!
//! * [`api`]: request/response DTOs of every method (one module per `forge` command
//!   group, docs/09), the method table ([`api::METHODS`]: role, `local_only`,
//!   long-running, CLI commands) and the TypeScript export ([`api::contracts`]).
//! * [`rpc`]: JSON-RPC 2.0 frames, with the mandatory `request_id` (ADR-0015).
//! * [`transport`]: the [`transport::Transport`] trait and its implementations (named
//!   pipe, unix socket, in-memory; WebSocket/TLS placeholder).
//! * [`auth`]: `state/ipc.token` and the `system.hello` check.
//! * [`server`]: dispatcher, per-connection tasks, streams with `seq` and
//!   `resume_from`, built-in `events.*`/`jobs.*`/`fs.*`.
//! * [`client`]: typed client with retries by `request_id` and resumable subscriptions.
//!
//! Remote-ready by construction (ADR-0009): identities are ULIDs, every operation has a
//! `request_id`, streams have `seq`, `local_only` and roles are in the contracts, and
//! file access goes through the restricted `fs.*` methods.

pub mod api;
pub mod auth;
pub mod client;
pub mod rpc;
pub mod server;
pub(crate) mod stream_queue;
pub mod transport;

/// The base crate (ids, paths, error codes, timestamps) re-exported so that thin clients
/// (CLI, MCP, desktop) depend on `forge-ipc` only (docs/01 §3): `forge_ipc::base::paths::ForgeRoot`.
pub use forge_core as base;

pub use api::{API_VERSION, ApiError, METHODS};
pub use client::{CallOptions, Client, ClientError, ClientOptions, Credentials};
pub use server::{Server, ServerConfig, ServerHandle};

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-ipc");
    }
}
