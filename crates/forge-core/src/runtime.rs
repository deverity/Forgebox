//! tokio runtime of the daemon (docs/03 §7.1, ADR-0011).
//!
//! Multi-thread scheduler with `worker_threads = max(4, cores)` so that one handler
//! blocking its thread never starves the others, and a blocking pool of `2 × cores` for
//! everything synchronous (`spawn_blocking`: file I/O, hashing, extraction, slow Win32,
//! SQLite reads). The SQLite writer is a dedicated `std::thread` outside this pool
//! ([`crate::store`]). All threads are named `<thread_name>-<n>`.
//!
//! # tokio-console (development only)
//!
//! Feature `console` (off by default) adds `init_console()` on top of
//! `console-subscriber`. tokio only emits task instrumentation when built with
//! `--cfg tokio_unstable`, so:
//!
//! ```text
//! RUSTFLAGS="--cfg tokio_unstable" cargo run -p forge-daemon --features forge-core/console
//! tokio-console            # in another terminal, connects to 127.0.0.1:6669
//! ```
//!
//! The binary calls `init_console()` at startup instead of installing its own subscriber
//! (wired into `forgeboxd` with its boot code, M1-E5-T2). The default build neither needs
//! nor enables `tokio_unstable`.

use std::io;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::runtime::{Builder, Runtime};

/// Lower bound of worker threads, whatever the machine.
pub const MIN_WORKER_THREADS: usize = 4;
/// Default prefix of runtime thread names.
pub const DEFAULT_THREAD_NAME: &str = "forgebox-rt";

/// Sizing of the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// Async worker threads.
    pub worker_threads: usize,
    /// Upper bound of the `spawn_blocking` pool (not counting workers).
    pub max_blocking_threads: usize,
    /// Prefix of thread names (`<prefix>-<n>`).
    pub thread_name: String,
}

impl RuntimeConfig {
    /// Sizing for a machine with `cores` logical CPUs: `max(4, cores)` workers and
    /// `2 × cores` blocking threads.
    pub fn for_cores(cores: usize) -> Self {
        let cores = cores.max(1);
        Self {
            worker_threads: cores.max(MIN_WORKER_THREADS),
            max_blocking_threads: 2 * cores,
            thread_name: DEFAULT_THREAD_NAME.to_owned(),
        }
    }

    /// Same sizing with another thread name prefix.
    pub fn thread_name(mut self, prefix: impl Into<String>) -> Self {
        self.thread_name = prefix.into();
        self
    }
}

impl Default for RuntimeConfig {
    /// [`RuntimeConfig::for_cores`] with [`available_cores`].
    fn default() -> Self {
        Self::for_cores(available_cores())
    }
}

/// Logical CPUs available to the process (1 if unknown).
pub fn available_cores() -> usize {
    std::thread::available_parallelism().map_or(1, NonZeroUsize::get)
}

/// Builds the multi-thread runtime described by `config` (I/O and time drivers on).
pub fn build_runtime(config: &RuntimeConfig) -> io::Result<Runtime> {
    let prefix = config.thread_name.clone();
    let next = Arc::new(AtomicUsize::new(0));
    Builder::new_multi_thread()
        .worker_threads(config.worker_threads.max(1))
        .max_blocking_threads(config.max_blocking_threads.max(1))
        .thread_name_fn(move || format!("{prefix}-{}", next.fetch_add(1, Ordering::Relaxed)))
        .enable_all()
        .build()
}

/// Whether this build has the `console` feature.
pub const fn console_enabled() -> bool {
    cfg!(feature = "console")
}

/// Installs a global `tracing` subscriber with the tokio-console layer (gRPC server on
/// `127.0.0.1:6669`, configurable through the `TOKIO_CONSOLE_*` variables) plus a fmt
/// layer filtered by `RUST_LOG`. Development only; needs `--cfg tokio_unstable`.
///
/// # Panics
/// If a global subscriber is already installed.
#[cfg(feature = "console")]
pub fn init_console() {
    console_subscriber::init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizing_follows_engine_doc() {
        let small = RuntimeConfig::for_cores(2);
        assert_eq!(small.worker_threads, 4, "at least 4 workers");
        assert_eq!(small.max_blocking_threads, 4);
        let big = RuntimeConfig::for_cores(16);
        assert_eq!(big.worker_threads, 16);
        assert_eq!(big.max_blocking_threads, 32);
        assert_eq!(RuntimeConfig::for_cores(0).worker_threads, 4);
        let default = RuntimeConfig::default();
        assert_eq!(default.worker_threads, available_cores().max(4));
        assert_eq!(default.max_blocking_threads, 2 * available_cores());
        assert_eq!(default.thread_name, DEFAULT_THREAD_NAME);
        assert!(!console_enabled() || cfg!(feature = "console"));
    }

    #[test]
    fn runtime_threads_are_named() {
        let config = RuntimeConfig::for_cores(1).thread_name("fb-test");
        let rt = build_runtime(&config).unwrap();
        let (worker, blocking) = rt.block_on(async {
            let worker = tokio::spawn(async { std::thread::current().name().map(str::to_owned) })
                .await
                .unwrap();
            let blocking =
                tokio::task::spawn_blocking(|| std::thread::current().name().map(str::to_owned))
                    .await
                    .unwrap();
            (worker, blocking)
        });
        for name in [worker, blocking] {
            let name = name.expect("named thread");
            assert!(name.starts_with("fb-test-"), "{name}");
        }
        assert_eq!(rt.metrics().num_workers(), 4);
    }
}
