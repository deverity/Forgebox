//! Blue/green between two pools (docs/03 §2.2, docs/06 §2.2, docs/01 §5.3): a site moving
//! from PHP 8.3 to 8.4, or a patch update 8.3.36 → 8.3.37 (and its rollback).
//!
//! [`swap`]:
//! 1. the target pool is made ready: started on **standby** when it is not running (warm
//!    workers, but its own upstream changes do not reach the web server yet — both
//!    versions of a series share the upstream name `php_8_3`);
//! 2. the source goes on standby: from here on its own scaling cannot overwrite the
//!    swap in the web server (the call returns once no change of it is in flight);
//! 3. the [`UpstreamListener`] gets [`UpstreamChange::Swapped`] and applies it
//!    (re-render of the vhosts or of the shared upstream, then reload; done by whoever
//!    injected the listener, the daemon);
//! 4. the target is promoted (its changes go to the web server from now on);
//! 5. unless told otherwise (other sites still use it), the source pool is drained:
//!    out of the web server, each worker stopped once its connections ended (or after
//!    `drain_timeout`, 30 s), then `Stopped`.
//!
//! A failure before step 3 completes puts the source back in service as before; a
//! target this swap started is stopped again. No request is lost and the web server is
//! never restarted, only reloaded.
//!
//! The update engine (M1-E2-T5) switches pools by version id: [`Switcher`] resolves both
//! ids through a [`PoolDirectory`] (the daemon's pool table) and runs [`swap`]. The daemon
//! implements the update engine's `PoolSwitcher` by delegating to
//! [`Switcher::switch`]; `forge-fcgi` does not depend on `forge-provider`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::ids::VersionId;
use forge_core::{ForgeError, Result};
use serde::{Deserialize, Serialize};

use crate::pool::{DrainReport, POOL_QUICK_TIMEOUT, PoolHandle, PoolState};
use crate::upstream::{UpstreamChange, UpstreamListener, UpstreamModel};

/// Budgets of a swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwapOptions {
    /// Starting the target pool (its `min` workers warm).
    pub start_timeout: Duration,
    /// Applying the change in the web server (re-render + reload).
    pub listener_timeout: Duration,
    /// Waiting for the source pool to drain and stop (its own `drain_timeout`, 30 s by
    /// default, plus the workers' stop).
    pub drain_wait: Duration,
    /// Drain the source afterwards. `false` when other sites still use it (docs/06 §2.2:
    /// a pool is drained only once no site references it).
    pub drain_source: bool,
}

impl Default for SwapOptions {
    fn default() -> Self {
        Self {
            start_timeout: Duration::from_secs(60),
            listener_timeout: Duration::from_secs(30),
            drain_wait: Duration::from_secs(90),
            drain_source: true,
        }
    }
}

/// What a swap did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapReport {
    pub from: VersionId,
    pub to: VersionId,
    /// Upstream of the source when the traffic moved.
    pub upstream_from: UpstreamModel,
    /// Upstream of the target when the traffic moved.
    pub upstream_to: UpstreamModel,
    /// The target was not running and was started by the swap.
    pub started_target: bool,
    /// The source's drain (`None` when not drained).
    pub drain: Option<DrainReport>,
    pub elapsed_ms: u64,
}

/// Moves the traffic of `from` to `to` (see the module docs).
pub async fn swap(
    from: &PoolHandle,
    to: &PoolHandle,
    listener: &dyn UpstreamListener,
    options: &SwapOptions,
) -> Result<SwapReport> {
    if from.version_id() == to.version_id() {
        return Err(ForgeError::InvalidInput(
            "a pool cannot be swapped with itself".to_owned(),
        ));
    }
    let started = Instant::now();
    let target_state = to.status(POOL_QUICK_TIMEOUT).await?.state;
    let started_target = !matches!(target_state, PoolState::Running | PoolState::Starting);
    // 1. Target ready (on standby if this swap starts it).
    let upstream_to = to.start_standby(options.start_timeout).await?;
    // 2. Source on standby: its changes no longer reach the web server. Needed when it
    // leaves the web server, or when both pools share the upstream name (otherwise the
    // source keeps serving its other sites).
    let demote = options.drain_source || from.upstream_name() == to.upstream_name();
    let source_was_serving = from.status(POOL_QUICK_TIMEOUT).await?.serving;
    if demote {
        from.standby(options.listener_timeout + POOL_QUICK_TIMEOUT)
            .await?;
    }
    let upstream_from = from.upstream(POOL_QUICK_TIMEOUT).await?;
    // 3. Web server switched.
    let change = UpstreamChange::Swapped {
        from: upstream_from.clone(),
        to: upstream_to.clone(),
    };
    let applied =
        match tokio::time::timeout(options.listener_timeout, listener.upstream_changed(change))
            .await
        {
            Ok(result) => result,
            Err(_) => Err(ForgeError::Timeout {
                operation: format!(
                    "switching the web server from {} to {}",
                    upstream_from.name, upstream_to.name
                ),
                after: options.listener_timeout,
            }),
        };
    if let Err(err) = applied {
        if demote && source_was_serving {
            // Back in service; publishes its current model if it changed meanwhile.
            if let Err(back) = from
                .promote(upstream_from.clone(), POOL_QUICK_TIMEOUT)
                .await
            {
                tracing::warn!(%back, "putting the source of a failed swap back in service");
            }
        }
        if started_target {
            if let Err(stop) = to.stop(options.drain_wait).await {
                tracing::warn!(%stop, "stopping the target of a failed swap");
            }
        }
        return Err(err);
    }
    // 4. Target serving.
    to.promote(upstream_to.clone(), POOL_QUICK_TIMEOUT).await?;
    // 5. Source drained.
    let drain = if options.drain_source {
        Some(from.drain(options.drain_wait).await?)
    } else {
        None
    };
    Ok(SwapReport {
        from: from.version_id(),
        to: to.version_id(),
        upstream_from,
        upstream_to,
        started_target,
        drain,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

/// Future of [`PoolDirectory::pool`].
pub type PoolLookup<'a> = Pin<Box<dyn Future<Output = Result<PoolHandle>> + Send + 'a>>;

/// The daemon's pools by version: the existing pool of `version`, or a new (stopped) one
/// built from its installed `ToolVersion`. `NotFound` when the version is not installed.
pub trait PoolDirectory: Send + Sync {
    fn pool(&self, version: VersionId) -> PoolLookup<'_>;
}

impl<F> PoolDirectory for F
where
    F: Fn(VersionId) -> PoolLookup<'static> + Send + Sync,
{
    fn pool(&self, version: VersionId) -> PoolLookup<'_> {
        self(version)
    }
}

/// Swaps pools by version id (for the update engine's `PoolSwitcher`, docs/05 §4).
#[derive(Clone)]
pub struct Switcher {
    directory: Arc<dyn PoolDirectory>,
    listener: Arc<dyn UpstreamListener>,
    options: SwapOptions,
}

impl std::fmt::Debug for Switcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Switcher")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl Switcher {
    pub fn new(
        directory: Arc<dyn PoolDirectory>,
        listener: Arc<dyn UpstreamListener>,
        options: SwapOptions,
    ) -> Self {
        Self {
            directory,
            listener,
            options,
        }
    }

    /// Moves the traffic of the pool of `from` to the pool of `to` and drains `from`
    /// (an update 8.3.36 → 8.3.37, or its rollback).
    pub async fn switch(&self, from: VersionId, to: VersionId) -> Result<SwapReport> {
        let source = self.directory.pool(from).await?;
        let target = self.directory.pool(to).await?;
        swap(&source, &target, self.listener.as_ref(), &self.options).await
    }
}
