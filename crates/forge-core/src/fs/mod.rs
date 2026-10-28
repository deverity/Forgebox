//! File system operations of the domain that are more than a platform call.
//!
//! - [`case`]: Linux parity (docs/15, ADR-0010): status of a folder, the reversible
//!   conversion of an existing tree to case-sensitive ("move into a new flagged
//!   folder"), its recovery after a crash, and the job wrapper that runs it off the
//!   async runtime with progress events.
//!
//! Everything here that touches the disk is blocking; the async entry points
//! ([`case::CaseConversions`]) run it in `spawn_blocking` (ADR-0011).

pub mod case;
