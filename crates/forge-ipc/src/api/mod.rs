//! API contracts (docs/09, docs/13 §2, ADR-0009): one module per group of `forge`
//! commands, with the request/response DTOs of each method. DTOs belong to the API and
//! are versioned with it ([`API_VERSION`]); the daemon converts its domain types into
//! them. Identities are ULIDs; paths are informative attributes.
//!
//! [`methods`] holds the method table ([`methods::METHODS`]) and one marker type per
//! method; [`contracts`] exports everything to TypeScript (`cargo xtask gen-ts`).
//! Every DTO is re-exported at this level (`forge_ipc::api::SiteDto`); names are unique
//! across groups because TypeScript has a single namespace.

pub mod cli_template;
pub mod common;
pub mod contracts;
pub mod daemon;
pub mod db;
pub mod events;
pub mod fs;
pub mod fs_parity;
pub mod jobs;
pub mod logs;
pub mod mail;
pub mod methods;
pub mod net;
pub mod php;
pub mod registry;
pub mod remote;
pub mod services;
pub mod shell;
pub mod sites;
pub mod stats;
pub mod system;
pub mod tls;
pub mod tools;

pub use common::*;
pub use daemon::*;
pub use db::*;
pub use events::*;
pub use fs::*;
pub use fs_parity::*;
pub use jobs::*;
pub use logs::*;
pub use mail::*;
pub use methods::{METHODS, method_info};
pub use net::*;
pub use php::*;
pub use registry::*;
pub use remote::*;
pub use services::*;
pub use shell::*;
pub use sites::*;
pub use stats::*;
pub use system::*;
pub use tls::*;
pub use tools::*;

pub use crate::transport::TransportKind;
pub use forge_core::ids::{
    ClientId, EventId, InstanceId, JobId, PortReservationId, RequestId, SessionId, SiteId,
    SubscriptionId, ToolId, VersionId,
};
