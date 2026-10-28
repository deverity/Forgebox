//! One `php-cgi.exe -b 127.0.0.1:<port>` of a pool (docs/03 §2.1): how it is spawned
//! through `forge-supervisor`, warmed up, observed and retired.
//!
//! * **Process.** Each worker is an [`InstanceHandle`] of the pool's scoped
//!   [`Supervisor`] (its own Job Object nested in the pool's job, events on the pool's
//!   private bus): `PATH` and environment are the supervisor's deterministic ones plus
//!   `PHPRC`, `PHP_INI_SCAN_DIR`, `PHP_FCGI_MAX_REQUESTS` and `FORGEBOX_SITE`
//!   ([`worker_env`]). Restart policy `never`: the pool decides what replaces a worker.
//!   Stop: `CTRL_BREAK` (php-cgi exits at once), then `TerminateJobObject`.
//! * **Launch** ([`launch`]): port from the injected [`PortReservations`] (range
//!   `php-pools`, owner `pool/<version id>#worker-<slot>`), baseline of the connections
//!   already on that port, spawn (pre-checks: VC++ runtime and the port), then warmup:
//!   `FCGI_GET_VALUES` + `GET /_forgebox/health.php` until it answers (the first attempts
//!   are refused while php-cgi binds), checking `PHP_VERSION` and that the answering pid
//!   is the worker's. Any failure stops the process and frees the port.
//! * **Requests served** are estimated from the TCP table ([`ConnTracker`]): php-cgi
//!   takes one connection per request (no keep-alive), and after it closes one of the two
//!   loopback ends stays in `TIME_WAIT` for at least 30 s, so every connection is seen by
//!   a sampler that runs more often than that. Connections are keyed by the client's
//!   endpoint, whichever end is listed.
//! * **Retire** ([`retire`]): stop, drop the actor, free the port.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::ids::InstanceId;
use forge_core::model::port::{OwnerKind, PortReservations, ReservationOwner};
use forge_core::paths::posix_string;
use forge_core::platform::{PortTable, TcpConnection, TcpState};
use forge_core::runtime::spawn_blocking_timeout;
use forge_core::{ForgeError, Result, Timestamp};
use forge_supervisor::{
    EnvMap, FailureReason, InstanceHandle, InstanceSpec, InstanceState, LogSubscription,
    MetricsOptions, PrecheckSpec, QUICK_ASK_TIMEOUT, RestartPolicy, SpawnSpec, StopPlan,
    Supervisor,
};
use serde::{Deserialize, Serialize};

use crate::pool::PoolSpec;
use crate::probe::{self, HealthReport, HealthScript};

/// Placeholder of the worker's port in [`PoolSpec::args`] (the manifest's
/// `{{ port }}`, left for the pool to fill in per worker).
pub const PORT_PLACEHOLDER: &str = "{{ port }}";

/// Accepted spellings of [`PORT_PLACEHOLDER`].
const PLACEHOLDERS: [&str; 2] = [PORT_PLACEHOLDER, "{{port}}"];

/// Requests a worker may serve before php-cgi exits by itself, for a recycle threshold
/// of `max_requests` (0 = no limit). Far above the threshold: the pool replaces the
/// worker after about `max_requests` (estimated), and php-cgi exiting on its own would
/// reset the connections still queued on it. The internal counter only bounds a worker
/// the pool failed to recycle.
pub fn hard_request_limit(max_requests: u64) -> u64 {
    if max_requests == 0 {
        return 0;
    }
    max_requests
        .saturating_mul(4)
        .max(max_requests.saturating_add(1000))
}

/// Lifecycle phase of a worker inside its pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerPhase {
    /// Port allocation, spawn and warmup in progress.
    Starting,
    /// Warm and in the upstream.
    Active,
    /// Left the upstream; waiting for the web server to apply that.
    Retiring,
    /// Out of the upstream; waiting for its connections to end.
    Draining,
    /// Being stopped.
    Stopping,
}

/// The arguments for `port` (every placeholder replaced).
pub fn worker_args(args: &[String], port: u16) -> Vec<String> {
    let port = port.to_string();
    args.iter()
        .map(|arg| {
            PLACEHOLDERS
                .iter()
                .fold(arg.clone(), |acc, p| acc.replace(p, &port))
        })
        .collect()
}

/// The address the workers bind: the IP of the argument `<ip>:{{ port }}`.
pub fn bind_address(args: &[String]) -> Result<IpAddr> {
    let with_port: Vec<&String> = args
        .iter()
        .filter(|a| PLACEHOLDERS.iter().any(|p| a.contains(p)))
        .collect();
    let [arg] = with_port.as_slice() else {
        return Err(ForgeError::InvalidInput(format!(
            "pool arguments need exactly one `<ip>:{PORT_PLACEHOLDER}` argument, got {args:?}"
        )));
    };
    let host = PLACEHOLDERS
        .iter()
        .find_map(|p| arg.strip_suffix(p))
        .and_then(|h| h.strip_suffix(':'))
        .ok_or_else(|| {
            ForgeError::InvalidInput(format!(
                "pool argument {arg:?} must be `<ip>:{PORT_PLACEHOLDER}`"
            ))
        })?;
    let ip: IpAddr = host.parse().map_err(|_| {
        ForgeError::InvalidInput(format!("pool argument {arg:?}: {host:?} is not an IP"))
    })?;
    if !ip.is_loopback() {
        return Err(ForgeError::InvalidInput(format!(
            "pool workers must bind a loopback address, not {ip}"
        )));
    }
    Ok(ip)
}

/// Environment of a worker: the spec's variables, `PHPRC` and `PHP_INI_SCAN_DIR` from
/// the ini dir unless set, `FORGEBOX_SITE` empty unless set (the web server sends the
/// real one per request, docs/06 §2.1) and `PHP_FCGI_MAX_REQUESTS` always set to
/// [`hard_request_limit`].
pub fn worker_env(spec: &PoolSpec) -> EnvMap {
    let mut env = spec.env.clone();
    if env.get("PHPRC").is_none() {
        env.set("PHPRC", posix_string(&spec.ini_dir));
    }
    if env.get("PHP_INI_SCAN_DIR").is_none() {
        env.set(
            "PHP_INI_SCAN_DIR",
            posix_string(&spec.ini_dir.join("conf.d")),
        );
    }
    if env.get("FORGEBOX_SITE").is_none() {
        env.set("FORGEBOX_SITE", "");
    }
    env.set(
        "PHP_FCGI_MAX_REQUESTS",
        hard_request_limit(spec.recycle.max_requests).to_string(),
    );
    env
}

/// Instance name of the worker on `port` (`8.3.36-19083`).
pub fn instance_name(spec: &PoolSpec, port: u16) -> String {
    let version: String = spec
        .version
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{version}-{port}")
}

/// The supervisor spec of the worker on `port`.
pub fn instance_spec(spec: &PoolSpec, port: u16) -> InstanceSpec {
    let mut spawn = SpawnSpec::new(&spec.exe, &spec.cwd).args(worker_args(&spec.args, port));
    spawn.env = worker_env(spec);
    spawn.path = spec.path.clone();
    let mut instance = InstanceSpec::new(
        InstanceId::new(),
        spec.tool.clone(),
        instance_name(spec, port),
        spawn,
    )
    .with_restart(RestartPolicy::Never)
    .with_precheck(PrecheckSpec {
        ports: vec![port],
        runtime_deps: spec.runtime_deps.clone(),
        ..PrecheckSpec::default()
    });
    instance.stop = StopPlan {
        timeout: spec.timing.stop_timeout,
        wm_close: false,
        ..StopPlan::default()
    };
    // The pool samples its workers itself (one table read for all of them).
    instance.metrics = MetricsOptions {
        interval: Duration::ZERO,
        publish_events: false,
    };
    if let Some(dir) = &spec.log_dir {
        instance.logs.file = Some(dir.join(format!("worker-{port}.out.log")));
    }
    instance
}

/// Port reservation owner of a worker slot.
pub fn reservation_owner(spec: &PoolSpec, slot: u64) -> ReservationOwner {
    ReservationOwner {
        kind: OwnerKind::Pool,
        id: spec.version_id.to_string(),
        port_name: format!("worker-{slot}"),
    }
}

/// Established connections accepted by the worker at `addr`, and the client endpoint
/// of every connection to it (server end listed with local `addr`, or client end listed
/// with remote `addr`; on loopback both appear and give the same key).
pub fn connection_keys(conns: &[TcpConnection], addr: SocketAddr) -> (u32, HashSet<SocketAddr>) {
    let mut established = 0_u32;
    let mut keys = HashSet::new();
    for conn in conns {
        if conn.local == addr {
            if conn.state == TcpState::Established {
                established += 1;
            }
            keys.insert(conn.remote);
        } else if conn.remote == addr && conn.local.ip().is_loopback() {
            keys.insert(conn.local);
        }
    }
    (established, keys)
}

/// Counts the connections (= requests) a worker took, from successive samples of the
/// TCP table. A client endpoint seen for the first time is a new connection; endpoints
/// that disappear are forgotten, so a reused client port counts again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnTracker {
    seen: HashSet<SocketAddr>,
    requests: u64,
}

impl ConnTracker {
    /// Tracker that ignores `baseline`, the connections already on the port before the
    /// worker existed (`TIME_WAIT` leftovers of a previous worker on the same port).
    pub fn with_baseline(baseline: HashSet<SocketAddr>) -> Self {
        Self {
            seen: baseline,
            requests: 0,
        }
    }

    /// Records a sample; returns how many connections are new since the previous one.
    pub fn observe(&mut self, keys: HashSet<SocketAddr>) -> u64 {
        let new = keys.difference(&self.seen).count() as u64;
        self.seen = keys;
        self.requests += new;
        new
    }

    /// Connections seen so far (an estimate of the requests served, probes included).
    pub fn requests(&self) -> u64 {
        self.requests
    }
}

/// A warm worker handed to the pool.
#[derive(Debug)]
pub struct Launched {
    pub port: u16,
    pub addr: SocketAddr,
    pub instance: InstanceHandle,
    pub pid: u32,
    /// Client endpoints already on the port before the spawn.
    pub baseline: HashSet<SocketAddr>,
    pub health: HealthReport,
}

/// What [`launch`] needs, shared by every launch of a pool.
pub struct LaunchContext {
    pub spec: Arc<PoolSpec>,
    /// The pool's scoped supervisor (nested jobs, private bus).
    pub supervisor: Supervisor,
    pub ports: Arc<dyn PortReservations>,
    pub table: Arc<dyn PortTable>,
    pub script: HealthScript,
    pub bind: IpAddr,
}

impl std::fmt::Debug for LaunchContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchContext")
            .field("version", &self.spec.version)
            .field("bind", &self.bind)
            .finish_non_exhaustive()
    }
}

/// Last lines of output of a failed worker (diagnosis), at most 10.
fn tail(logs: &mut Option<LogSubscription>) -> String {
    let Some(logs) = logs else {
        return String::new();
    };
    let mut lines = Vec::new();
    while let Some(line) = logs.try_recv() {
        lines.push(line.text);
    }
    let start = lines.len().saturating_sub(10);
    let text = lines[start..].join(" | ");
    if text.is_empty() {
        text
    } else {
        format!("; output: {text}")
    }
}

/// Allocates a port, spawns and warms up the worker of `slot`. On failure everything
/// done so far is undone (process stopped, port freed) before returning the error.
pub async fn launch(ctx: Arc<LaunchContext>, slot: u64) -> Result<Launched> {
    let spec = &ctx.spec;
    let owner = reservation_owner(spec, slot);
    let reservation = tokio::time::timeout(
        spec.timing.start_timeout,
        ctx.ports.allocate(&spec.port_range, owner.clone()),
    )
    .await
    .map_err(|_| ForgeError::Timeout {
        operation: format!("allocating a port in {}", spec.port_range),
        after: spec.timing.start_timeout,
    })??;
    let port = reservation.port;
    match spawn_and_warm(&ctx, port).await {
        Ok(launched) => Ok(launched),
        Err(err) => {
            release_port(&ctx.ports, &owner).await;
            Err(err)
        }
    }
}

async fn spawn_and_warm(ctx: &LaunchContext, port: u16) -> Result<Launched> {
    let spec = &ctx.spec;
    let addr = SocketAddr::new(ctx.bind, port);
    let table = Arc::clone(&ctx.table);
    let baseline = match spawn_blocking_timeout(
        "reading the TCP table",
        spec.timing.sample_timeout,
        move || table.connections(&[port]),
    )
    .await
    {
        Ok(conns) => connection_keys(&conns, addr).1,
        Err(err) => {
            tracing::debug!(%err, port, "no connection baseline for a new worker");
            HashSet::new()
        }
    };

    let instance = ctx.supervisor.spawn_instance(instance_spec(spec, port))?;
    let mut logs = instance.subscribe_logs(QUICK_ASK_TIMEOUT).await.ok();
    let warm = async {
        let pid = instance.start(spec.timing.start_timeout).await?;
        let health = warm_up(ctx, &instance, addr, pid).await?;
        Ok::<_, ForgeError>((pid, health))
    };
    match warm.await {
        Ok((pid, health)) => Ok(Launched {
            port,
            addr,
            instance,
            pid,
            baseline,
            health,
        }),
        Err(err) => {
            let output = tail(&mut logs);
            let _ = instance.stop(spec.timing.stop_wait()).await;
            instance.shutdown().await;
            Err(with_context(
                err,
                &format!("php-cgi worker on {addr} did not start{output}"),
            ))
        }
    }
}

/// Budget of one connection attempt while waiting for a new worker to listen (loopback
/// connects to a listening socket complete at once).
const LISTEN_ATTEMPT: Duration = Duration::from_millis(250);

/// Probes until the worker answers its health script (or the deadline / its exit).
async fn warm_up(
    ctx: &LaunchContext,
    instance: &InstanceHandle,
    addr: SocketAddr,
    pid: u32,
) -> Result<HealthReport> {
    let timing = &ctx.spec.timing;
    let deadline = Instant::now() + timing.start_timeout;
    let mut pause = Duration::from_millis(20);
    loop {
        // A refused connect takes about 2 s on Windows (SYN retries): wait for the
        // listener with short attempts before the real probe.
        let probed = if probe::accepts(addr, LISTEN_ATTEMPT).await {
            probe::health(addr, &ctx.script, timing.probe_timeout).await
        } else {
            Err(probe::ProbeError::Connect {
                addr,
                source: std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "not listening yet",
                ),
            })
        };
        match probed {
            Ok(report) => {
                if let Some(expected) = &ctx.spec.expect_php_version {
                    if &report.php_version != expected {
                        return Err(ForgeError::ConfigInvalid(format!(
                            "the worker answered PHP {}, expected {expected}",
                            report.php_version
                        )));
                    }
                }
                if report.pid.is_some_and(|answered| answered != pid) {
                    return Err(ForgeError::Conflict(format!(
                        "{addr} was answered by process {} instead of the worker {pid}",
                        report.pid.unwrap_or_default()
                    )));
                }
                return Ok(report);
            }
            Err(err) => {
                let status = instance.status(QUICK_ASK_TIMEOUT).await?;
                if !status.state.is_active() {
                    let cause = match &status.state {
                        InstanceState::Crashed { exit_code } => {
                            FailureReason::exited_during_start(*exit_code).to_error()
                        }
                        InstanceState::Failed { reason } => reason.to_error(),
                        other => ForgeError::Internal(format!("worker is {}", other.name())),
                    };
                    let probe = if err.is_connect() {
                        String::new()
                    } else {
                        format!(" (last probe: {err})")
                    };
                    return Err(with_context(
                        cause,
                        &format!("worker exited during warmup{probe}"),
                    ));
                }
                if Instant::now() >= deadline {
                    return Err(ForgeError::Timeout {
                        operation: format!("warming up the worker on {addr} (last probe: {err})"),
                        after: timing.start_timeout,
                    });
                }
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(Duration::from_millis(250));
            }
        }
    }
}

/// `err` with `context` in front of its message; the code (`port_in_use`,
/// `config_invalid`, `timeout`...) is kept.
pub fn with_context(err: ForgeError, context: &str) -> ForgeError {
    let wrap = |m: String| format!("{context}: {m}");
    match err {
        ForgeError::NotFound(m) => ForgeError::NotFound(wrap(m)),
        ForgeError::Conflict(m) => ForgeError::Conflict(wrap(m)),
        ForgeError::NeedsElevation(m) => ForgeError::NeedsElevation(wrap(m)),
        ForgeError::VerificationFailed(m) => ForgeError::VerificationFailed(wrap(m)),
        ForgeError::ConfigInvalid(m) => ForgeError::ConfigInvalid(wrap(m)),
        ForgeError::InvalidInput(m) => ForgeError::InvalidInput(wrap(m)),
        ForgeError::UnsupportedPlatform(m) => ForgeError::UnsupportedPlatform(wrap(m)),
        ForgeError::NotImplemented(m) => ForgeError::NotImplemented(wrap(m)),
        ForgeError::Cancelled(m) => ForgeError::Cancelled(wrap(m)),
        ForgeError::Internal(m) => ForgeError::Internal(wrap(m)),
        ForgeError::Timeout { operation, after } => ForgeError::Timeout {
            operation: wrap(operation),
            after,
        },
        ForgeError::Io {
            context: inner,
            source,
        } => ForgeError::Io {
            context: wrap(inner),
            source,
        },
        // The owner is the useful part; the context goes to the log.
        port @ ForgeError::PortInUse { .. } => {
            tracing::debug!(%context, "worker port taken");
            port
        }
        other => ForgeError::Internal(wrap(other.to_string())),
    }
}

async fn release_port(ports: &Arc<dyn PortReservations>, owner: &ReservationOwner) {
    let release = tokio::time::timeout(Duration::from_secs(10), ports.release(owner)).await;
    match release {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => tracing::warn!(%err, %owner, "could not release a worker port"),
        Err(_) => tracing::warn!(%owner, "releasing a worker port timed out"),
    }
}

/// Stops a worker (cascade), drops its actor and frees its port.
pub async fn retire(
    instance: Option<InstanceHandle>,
    ports: Arc<dyn PortReservations>,
    owner: ReservationOwner,
    stop_wait: Duration,
) {
    if let Some(instance) = instance {
        if let Err(err) = instance.stop(stop_wait).await {
            tracing::warn!(%err, worker = instance.name(), "stopping a worker failed");
        }
        instance.shutdown().await;
    }
    release_port(&ports, &owner).await;
}

/// Snapshot of a worker for [`crate::PoolStatus`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub slot: u64,
    pub phase: WorkerPhase,
    pub port: Option<u16>,
    pub pid: Option<u32>,
    pub started_at: Timestamp,
    /// Estimated requests served (connections seen, probes included).
    pub requests: u64,
    /// Established connections at the last sample (in service + queued).
    pub established: u32,
    pub rss_bytes: Option<u64>,
    /// `PHP_VERSION` reported by the warmup.
    pub php_version: Option<String>,
    /// Slot of the worker this one replaces / is replaced by.
    pub replaces: Option<u64>,
    pub replaced_by: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(local: &str, remote: &str, state: TcpState) -> TcpConnection {
        TcpConnection {
            local: local.parse().unwrap(),
            remote: remote.parse().unwrap(),
            state,
            pid: 0,
        }
    }

    #[test]
    fn args_and_bind_address() {
        let args: Vec<String> = [
            "-b",
            "127.0.0.1:{{ port }}",
            "-c",
            "C:/F/etc/php/8.3/php.ini",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            worker_args(&args, 19083),
            ["-b", "127.0.0.1:19083", "-c", "C:/F/etc/php/8.3/php.ini"]
        );
        assert_eq!(bind_address(&args).unwrap(), IpAddr::from([127, 0, 0, 1]));
        let compact = vec!["-b".to_owned(), "127.0.0.1:{{port}}".to_owned()];
        assert_eq!(worker_args(&compact, 1)[1], "127.0.0.1:1");
        assert!(bind_address(&compact).is_ok());
        for bad in [
            vec!["-b".to_owned(), "19000".to_owned()],
            vec!["{{ port }}".to_owned()],
            vec!["0.0.0.0:{{ port }}".to_owned()],
            vec!["a:{{ port }}".to_owned(), "b:{{ port }}".to_owned()],
            vec!["127.0.0.1:{{ port }}/x".to_owned()],
        ] {
            assert!(bind_address(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn context_keeps_the_code() {
        let err = with_context(ForgeError::ConfigInvalid("bad".into()), "worker on x");
        assert_eq!(err.code(), "config_invalid");
        assert_eq!(err.to_string(), "invalid configuration: worker on x: bad");
        let err = with_context(
            ForgeError::Timeout {
                operation: "warmup".into(),
                after: Duration::from_secs(1),
            },
            "w",
        );
        assert_eq!(err.code(), "timeout");
        assert!(err.to_string().contains("w: warmup"), "{err}");
        let err = with_context(
            ForgeError::PortInUse {
                port: 1,
                owner: None,
            },
            "w",
        );
        assert_eq!(err.code(), "port_in_use");
    }

    #[test]
    fn hard_limit_stays_far_above_the_threshold() {
        assert_eq!(hard_request_limit(0), 0, "no limit");
        assert_eq!(hard_request_limit(20), 1020);
        assert_eq!(hard_request_limit(500), 2000);
        assert_eq!(hard_request_limit(u64::MAX), u64::MAX);
    }

    #[test]
    fn connections_are_keyed_by_the_client_end() {
        let worker: SocketAddr = "127.0.0.1:19083".parse().unwrap();
        let conns = [
            // server end of an open connection and its client end
            conn("127.0.0.1:19083", "127.0.0.1:50001", TcpState::Established),
            conn("127.0.0.1:50001", "127.0.0.1:19083", TcpState::Established),
            // queued connection, server end only listed
            conn("127.0.0.1:19083", "127.0.0.1:50002", TcpState::Established),
            // closed by php-cgi: server end in TIME_WAIT
            conn("127.0.0.1:19083", "127.0.0.1:50003", TcpState::TimeWait),
            // closed by the client first: client end in TIME_WAIT
            conn("127.0.0.1:50004", "127.0.0.1:19083", TcpState::TimeWait),
            // another worker, and a remote host on the same port number
            conn("127.0.0.1:19084", "127.0.0.1:50005", TcpState::Established),
            conn(
                "192.168.0.10:50006",
                "10.0.0.1:19083",
                TcpState::Established,
            ),
        ];
        let (established, keys) = connection_keys(&conns, worker);
        assert_eq!(established, 2);
        let mut ports: Vec<u16> = keys.iter().map(SocketAddr::port).collect();
        ports.sort_unstable();
        assert_eq!(ports, vec![50001, 50002, 50003, 50004]);
    }

    #[test]
    fn tracker_counts_each_connection_once() {
        let key = |p: u16| SocketAddr::from(([127, 0, 0, 1], p));
        let set = |ports: &[u16]| ports.iter().map(|p| key(*p)).collect::<HashSet<_>>();
        let mut tracker = ConnTracker::with_baseline(set(&[40000]));
        assert_eq!(tracker.observe(set(&[40000, 1, 2])), 2, "baseline ignored");
        assert_eq!(tracker.observe(set(&[1, 2, 3])), 1);
        assert_eq!(tracker.observe(set(&[3])), 0);
        assert_eq!(
            tracker.observe(set(&[3, 1])),
            1,
            "port 1 reused after it vanished"
        );
        assert_eq!(tracker.requests(), 4);
    }
}
