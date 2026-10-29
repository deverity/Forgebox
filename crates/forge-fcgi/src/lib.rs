//! Forgebox PHP FastCGI pool manager (docs/03 §2, docs/06 §2, ADR-0003, ADR-0021): one
//! pool of `php-cgi.exe` workers per PHP version, one worker per port, scaling, recycle,
//! FastCGI warmup and blue/green swap.
//!
//! * [`probe`]: minimal FastCGI client — `FCGI_GET_VALUES`, one `GET` request, the
//!   `/_forgebox/health.php` check.
//! * [`worker`]: how a worker is spawned through `forge-supervisor` (env `PHPRC`,
//!   `PHP_INI_SCAN_DIR`, `PHP_FCGI_MAX_REQUESTS`, `FORGEBOX_SITE`), warmed up, counted
//!   (requests estimated from its TCP connections) and retired.
//! * [`pool`]: the [`PhpPool`] actor behind a [`PoolHandle`]: ports from an injected
//!   [`forge_core::model::port::PortReservations`], scaling by established connections,
//!   proactive recycle, drains, `pool.scaled` / `pool.worker.recycled` events.
//! * [`upstream`]: the [`UpstreamModel`] handed to the web server through the injected
//!   [`UpstreamListener`] (the daemon re-renders with `forge-render` and reloads).
//! * [`swap`]: blue/green between two pools ([`swap::swap`], [`Switcher`] by version id
//!   for the update engine).
//!
//! No Win32 here (`forge_core::platform`), no dependency on `forge-net`,
//! `forge-render` or `forge-provider` (docs/01 §3): ports, rendering and the update
//! engine are reached through traits the daemon implements.

pub mod events;
pub mod pool;
pub mod probe;
pub mod swap;
pub mod upstream;
pub mod worker;

pub use events::{PoolScaledEvent, RecycleReason, WorkerRecycledEvent};
pub use pool::{
    DrainReport, PhpPool, PoolDeps, PoolHandle, PoolMetrics, PoolMsg, PoolSpec, PoolState,
    PoolStatus, PoolTiming, Recycle, Scaling,
};
pub use probe::{FcgiRequest, FcgiResponse, FcgiValues, HealthReport, HealthScript, ProbeError};
pub use swap::{PoolDirectory, SwapOptions, SwapReport, Switcher};
pub use upstream::{
    LbPolicy, ScaleReason, UpstreamChange, UpstreamFuture, UpstreamListener, UpstreamModel,
    upstream_name,
};
pub use worker::{PORT_PLACEHOLDER, WorkerPhase, WorkerStatus};

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-fcgi");
    }
}
