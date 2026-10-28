//! The Forgebox resolver for `*.test` (docs/06 §5, ADR-0004).
//!
//! * [`zones`]: what is answered ([`ZoneSet`]: local TLDs → `127.0.0.1`/`::1`, remote
//!   zones `*.hml.test` → the remote daemon, NXDOMAIN elsewhere, short TTLs).
//! * [`server`]: [`DnsServer`], the hickory-server UDP+TCP listener driven by the
//!   `dns-server` actor (start with port-53 fallback, stop, reload, stats).
//! * [`client`]: a direct DNS client (UDP/TCP) used by the diagnosis and the tests.
//! * [`diagnose`]: [`DnsDiagnostics`], why a host does or does not resolve.
//!
//! How Windows is told to ask this resolver (NRPT rule, `hosts` fallback) is
//! [`crate::policy`].

pub mod client;
pub mod diagnose;
pub mod server;
pub mod zones;

pub use client::{DnsReply, Transport, query, resolve};
pub use diagnose::{
    DnsDiagnosis, DnsDiagnostics, DnsHint, DnsHintCode, OsResolver, Severity, SystemResolver,
};
pub use server::{DnsServer, DnsServerConfig, DnsStats, STANDARD_LISTEN};
pub use zones::{Answer, DEFAULT_TTL, NEGATIVE_TTL, Zone, ZoneKind, ZoneSet};
