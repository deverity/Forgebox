//! Forgebox per-vhost metrics: access log tailing, latency histograms, CPU/RAM attribution.
//!
//! Crate of the Forgebox workspace, see docs/02-project-structure.md and docs/14 section 3.
//!
//! The parsing part of the crate is pure code (no I/O, no async, no Win32) shared by the
//! metrics pipeline and by the `site:` target of the daemon log viewer (M2-E3-T8):
//!
//! - [`AccessParser`] turns one access log line (nginx, Apache or Caddy JSON, described by an
//!   [`AccessFormat`]) into an [`AccessRecord`];
//! - [`HostRouter`] maps the host of a record to a site (exact host or alias first, then the
//!   longest `*.host` suffix, else unrouted);
//! - [`UriNormalizer`] reduces a request URI to a low-cardinality route and never keeps the
//!   query string (docs/14 section 7).
//!
//! [`tail`] follows an access log file by polling (std only, no `notify`, no Win32) across
//! rotations and truncations, with checkpoints and adaptive sampling, and hands the decoded
//! lines out in batches through a bounded channel (docs/14 section 3.2).

pub mod attribution;
mod parse;
mod record;
mod route;
mod router;
pub mod tail;

pub use attribution::{
    AppSample, Attribution, Bucket, Overhead, PoolMode, PoolRoute, PoolSample, SiteLoad,
    SiteResources, attribute,
};
pub use parse::{AccessParser, MAX_LINE_BYTES, ParseError};
pub use record::{AccessFormat, AccessRecord, FieldMap, FormatError, Multi, Unit, UpstreamMap};
pub use route::UriNormalizer;
pub use router::{HostRouter, Routed, SiteHosts, normalize_host};
pub use tail::{
    Checkpoint, LineDecoder, StartAt, StartOutcome, TailBatch, TailConfig, TailControl, TailSpec,
    TailStatus, Tailer, TailerHandle,
};
