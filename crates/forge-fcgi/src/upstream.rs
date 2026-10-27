//! What the web server sees of a pool (docs/06 §2.1): an [`UpstreamModel`] (name, backends,
//! balancing policy) and the [`UpstreamListener`] that applies changes to it.
//!
//! The pool never renders web server configuration (that is `forge-render`, which this
//! crate cannot depend on, docs/01 §3). It hands every change of its backends to the
//! listener injected by the daemon, which maps the model to
//! `forge_render::model::UpstreamModel` (`servers` = the backends), re-renders
//! `etc/<server>/upstreams/<name>.conf` (and, for a swap, the vhosts) and reloads the web
//! server. The listener's future resolves once the web server runs the new
//! configuration: only then does the pool start draining the workers that left it.
//!
//! Every change is also published as a `pool.scaled` event and on the pool's `watch`
//! channel ([`crate::PoolHandle::subscribe_upstream`]).

use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;

use forge_core::{ForgeError, Result};
use serde::{Deserialize, Serialize};

/// How the web server spreads requests over the backends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LbPolicy {
    /// Fewest active connections (nginx `least_conn`, Apache `bybusyness`, Caddy
    /// `least_conn`): the default, since a php-cgi worker serves one connection at a time.
    #[default]
    LeastConn,
    /// Each backend in turn.
    RoundRobin,
}

impl LbPolicy {
    /// Wire name (`least_conn`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LeastConn => "least_conn",
            Self::RoundRobin => "round_robin",
        }
    }
}

impl fmt::Display for LbPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A pool as an upstream of the web server.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UpstreamModel {
    /// `php_8_3` ([`upstream_name`]).
    pub name: String,
    /// Warm workers, in slot order. Empty only while the pool is starting, draining or
    /// stopped (or every worker died).
    pub backends: Vec<SocketAddr>,
    pub policy: LbPolicy,
}

impl UpstreamModel {
    pub fn new(name: impl Into<String>, backends: Vec<SocketAddr>, policy: LbPolicy) -> Self {
        Self {
            name: name.into(),
            backends,
            policy,
        }
    }

    /// Model without backends.
    pub fn empty(name: impl Into<String>, policy: LbPolicy) -> Self {
        Self::new(name, Vec::new(), policy)
    }

    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }

    /// `127.0.0.1:19083`, ... (event payloads).
    pub fn backend_strings(&self) -> Vec<String> {
        self.backends.iter().map(ToString::to_string).collect()
    }

    /// `(host, port)` of each backend: the `servers` of
    /// `forge_render::model::UpstreamModel`, which renders the upstream block.
    pub fn servers(&self) -> Vec<(String, u16)> {
        self.backends
            .iter()
            .map(|b| (b.ip().to_string(), b.port()))
            .collect()
    }
}

/// `php_<major>_<minor>` for a series (`8.3` → `php_8_3`); dots and dashes become `_`.
/// The series must be non-empty `[0-9A-Za-z.-]` (case is kept: identifiers in the
/// configs are case-sensitive, docs/15).
pub fn upstream_name(series: &str) -> Result<String> {
    let valid = !series.is_empty()
        && series.len() <= 32
        && series
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    if !valid {
        return Err(ForgeError::InvalidInput(format!(
            "invalid PHP series {series:?} for an upstream name"
        )));
    }
    Ok(format!("php_{}", series.replace(['.', '-'], "_")))
}

/// Why the backends of a pool changed (`reason` of `pool.scaled`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleReason {
    /// The first workers became warm.
    Start,
    /// Every worker was busy: more were added.
    ScaleUp,
    /// Workers idle for `idle_timeout` above `min` were removed.
    ScaleDown,
    /// A worker was replaced (max requests, RSS or age).
    Recycle,
    /// A worker exited on its own and left the upstream.
    WorkerExit,
    /// Rolling replacement of every worker (`php.ini` changed, docs/03 §2.3).
    Reload,
    /// The pool left the web server (blue/green, docs/03 §2.2).
    Drain,
    /// The pool was stopped.
    Stop,
}

impl ScaleReason {
    /// Wire name (`scale_up`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::ScaleUp => "scale_up",
            Self::ScaleDown => "scale_down",
            Self::Recycle => "recycle",
            Self::WorkerExit => "worker_exit",
            Self::Reload => "reload",
            Self::Drain => "drain",
            Self::Stop => "stop",
        }
    }
}

/// A change for the web server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamChange {
    /// The backends of a serving pool changed: re-render the upstream `model.name` with
    /// `model.backends` and reload. Never sent with empty backends.
    Scaled {
        model: UpstreamModel,
        reason: ScaleReason,
    },
    /// Blue/green ([`crate::swap`]): the vhosts (or the upstream, when both pools share
    /// its name) that use `from` must use `to` from now on; then reload. `from` is
    /// drained afterwards.
    Swapped {
        from: UpstreamModel,
        to: UpstreamModel,
    },
}

/// Future of [`UpstreamListener::upstream_changed`]: resolves when the web server
/// runs the new configuration.
pub type UpstreamFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

/// Applies upstream changes to the web server (re-render + reload). Injected by the
/// daemon. Called from a task of the pool (never inside its actor), one change at a time
/// per pool, with a timeout; a failure is retried with backoff and the workers that left
/// keep serving until a change is applied.
pub trait UpstreamListener: Send + Sync {
    fn upstream_changed(&self, change: UpstreamChange) -> UpstreamFuture;
}

impl<F> UpstreamListener for F
where
    F: Fn(UpstreamChange) -> UpstreamFuture + Send + Sync,
{
    fn upstream_changed(&self, change: UpstreamChange) -> UpstreamFuture {
        self(change)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_series() {
        assert_eq!(upstream_name("8.3").unwrap(), "php_8_3");
        assert_eq!(upstream_name("8.3.36").unwrap(), "php_8_3_36");
        assert_eq!(upstream_name("8.5-rc").unwrap(), "php_8_5_rc");
        for bad in ["", "8 3", "8/3", "8;3", &"9".repeat(33)] {
            assert!(upstream_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn model_wire_format() {
        let model = UpstreamModel::new(
            "php_8_3",
            vec![
                "127.0.0.1:19083".parse().unwrap(),
                "127.0.0.1:19084".parse().unwrap(),
            ],
            LbPolicy::LeastConn,
        );
        assert_eq!(
            model.backend_strings(),
            vec!["127.0.0.1:19083", "127.0.0.1:19084"]
        );
        let json = serde_json::to_value(&model).unwrap();
        assert_eq!(json["policy"], "least_conn");
        assert_eq!(json["backends"][1], "127.0.0.1:19084");
        assert!(UpstreamModel::empty("php_8_4", LbPolicy::RoundRobin).is_empty());
        assert_eq!(ScaleReason::WorkerExit.as_str(), "worker_exit");
        assert_eq!(
            serde_json::to_value(ScaleReason::ScaleDown).unwrap(),
            "scale_down"
        );
    }
}
