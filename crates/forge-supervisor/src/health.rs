//! Health probes (docs/03 §1.4, docs/01 §6, manifest `service.daemon.health`).
//!
//! One probe per instance: [`Probe::Tcp`] (connect), [`Probe::Http`] (`GET`, expected
//! status), [`Probe::Command`] (a command of the tool, expected exit code) or
//! [`Probe::Pid`] (the main process is still alive). Without a [`HealthSpec`] an instance
//! is `Running` as soon as its process was created (docs/04: "the process being alive is
//! enough").
//!
//! With a spec, the instance actor runs one probe loop per run: the first probe right
//! after the spawn, then every [`HealthSpec::start_interval`] while `Starting` and every
//! [`HealthSpec::interval`] once healthy, each wait with ±10 % jitter (instances
//! started together do not probe in lockstep). Each probe is bounded by
//! [`HealthSpec::timeout`]. `success_threshold` consecutive successes turn `Starting`
//! into `Running`; `start_timeout` without that ends in `Failed(HealthTimeout)`;
//! `failure_threshold` consecutive failures while `Running` stop the instance for the
//! restart policy (unless the policy is `never`: then it stays `Running`, reported as
//! failing in [`HealthStatus`]).
//!
//! Probes never block the actor: connects and HTTP are async, the pid check and the
//! command spawn go through `spawn_blocking`, and the command runs in a Job Object of
//! its own (killed when the probe ends or times out).

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::time::Duration;

use forge_core::platform::{GroupOptions, ProcessControl, ProcessGroup};
use forge_core::{ForgeError, Result, Timestamp};
use serde::{Deserialize, Serialize};

use crate::events::wire_exit_code;
use crate::process::{SpawnSpec, Stdio};
use crate::stop::TERMINATED_EXIT_CODE;

/// What to check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Probe {
    /// TCP connect to `host:port` (`127.0.0.1:3306`).
    Tcp { addr: String },
    /// `GET url`; healthy when the status is `expect_status`. Redirects are not followed,
    /// no proxy is used and certificates are not verified (local services often use the
    /// Forgebox CA or self-signed certificates; the probe only reads the status).
    Http { url: String, expect_status: u16 },
    /// Runs `command` (already rendered, stdio discarded, own Job Object); healthy when
    /// it exits with `exit_code` (`DWORD` bits as `i32`).
    Command { command: SpawnSpec, exit_code: i32 },
    /// The main process is still alive.
    Pid,
}

impl Probe {
    /// Wire name (`tcp`, `http`, `command`, `pid`).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Tcp { .. } => "tcp",
            Self::Http { .. } => "http",
            Self::Command { .. } => "command",
            Self::Pid => "pid",
        }
    }
}

/// Health check of an instance (manifest `service.daemon.health` + `start_timeout`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthSpec {
    pub probe: Probe,
    /// Between probes once healthy (manifest `interval`, default 5 s).
    pub interval: Duration,
    /// Between probes while `Starting` (default 500 ms, never above `interval`): a
    /// service is reported `Running` soon after it is ready.
    pub start_interval: Duration,
    /// Budget of one probe (default 5 s).
    pub timeout: Duration,
    /// Consecutive successes that make a starting instance `Running` (default 1).
    pub success_threshold: u32,
    /// Consecutive failures that make a running instance unhealthy (default 3).
    pub failure_threshold: u32,
    /// Deadline of `Starting`, counted from the start request (manifest
    /// `start_timeout`, default 30 s).
    pub start_timeout: Duration,
}

impl HealthSpec {
    /// Spec for `probe` with the defaults of the manifest.
    pub fn new(probe: Probe) -> Self {
        Self {
            probe,
            interval: Duration::from_secs(5),
            start_interval: Duration::from_millis(500),
            timeout: Duration::from_secs(5),
            success_threshold: 1,
            failure_threshold: 3,
            start_timeout: Duration::from_secs(30),
        }
    }

    /// Checks the values (`InvalidInput`): non-zero durations and thresholds, a parsable
    /// URL with `http`/`https`, a non-empty TCP address.
    pub fn validate(&self) -> Result<()> {
        let bad = |what: &str| Err(ForgeError::InvalidInput(format!("health check: {what}")));
        if self.interval.is_zero() || self.start_interval.is_zero() || self.timeout.is_zero() {
            return bad("intervals and timeout must be greater than zero");
        }
        if self.start_timeout.is_zero() {
            return bad("start_timeout must be greater than zero");
        }
        if self.success_threshold == 0 || self.failure_threshold == 0 {
            return bad("thresholds must be at least 1");
        }
        match &self.probe {
            Probe::Tcp { addr } if addr.trim().is_empty() => bad("empty tcp address"),
            Probe::Http { url, .. } => match reqwest::Url::parse(url) {
                Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => Ok(()),
                Ok(parsed) => bad(&format!("unsupported URL scheme {:?}", parsed.scheme())),
                Err(err) => bad(&format!("invalid URL {url:?}: {err}")),
            },
            _ => Ok(()),
        }
    }

    /// Pause before the next probe: `start_interval` (capped by `interval`) until the
    /// instance was healthy once, then `interval`; ±10 % jitter.
    pub(crate) fn next_wait(&self, healthy_once: bool) -> Duration {
        let base = if healthy_once {
            self.interval
        } else {
            self.start_interval.min(self.interval)
        };
        jittered(base, 0.1)
    }
}

/// Health of the current run, as reported by `Status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthStatus {
    /// `tcp`, `http`, `command` or `pid`.
    pub probe: String,
    /// Result of the last probe; `None` before the first one of the run.
    pub passing: Option<bool>,
    pub consecutive_successes: u32,
    pub consecutive_failures: u32,
    /// Error of the last failed probe of the run.
    pub last_error: Option<String>,
    pub last_check: Option<Timestamp>,
    /// Probes completed in this run.
    pub checks: u64,
}

impl HealthStatus {
    pub(crate) fn new(probe: &Probe) -> Self {
        Self {
            probe: probe.kind().to_owned(),
            passing: None,
            consecutive_successes: 0,
            consecutive_failures: 0,
            last_error: None,
            last_check: None,
            checks: 0,
        }
    }

    /// Counts one probe result.
    pub(crate) fn record(&mut self, outcome: &std::result::Result<(), String>, at: Timestamp) {
        self.checks += 1;
        self.last_check = Some(at);
        match outcome {
            Ok(()) => {
                self.passing = Some(true);
                self.consecutive_successes = self.consecutive_successes.saturating_add(1);
                self.consecutive_failures = 0;
            }
            Err(err) => {
                self.passing = Some(false);
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                self.consecutive_successes = 0;
                self.last_error = Some(err.clone());
            }
        }
    }

    /// `success_threshold` reached.
    pub(crate) fn is_healthy(&self, spec: &HealthSpec) -> bool {
        self.consecutive_successes >= spec.success_threshold
    }

    /// The last probe made the failures reach `failure_threshold` (true once per streak
    /// of failures, so the instance is not stopped again for each further failure).
    pub(crate) fn became_unhealthy(&self, spec: &HealthSpec) -> bool {
        self.consecutive_failures == spec.failure_threshold
    }
}

/// `base` ± `fraction` (uniform), from the per-process random hasher keys mixed with a
/// counter: no RNG dependency, different for every call.
pub(crate) fn jittered(base: Duration, fraction: f64) -> Duration {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(CALLS.fetch_add(1, Ordering::Relaxed));
    // 53 random bits → [0, 1).
    let unit = (hasher.finish() >> 11) as f64 / (1_u64 << 53) as f64;
    let factor = 1.0 + fraction * (2.0 * unit - 1.0);
    base.mul_f64(factor.max(0.0))
}

/// Runs the probes of one run (owned by its probe task).
pub(crate) struct Prober {
    probe: Probe,
    timeout: Duration,
    /// Main process of the run.
    pid: u32,
    processes: Arc<dyn ProcessControl>,
    /// Parent of the command probe's job.
    root: Arc<dyn ProcessGroup>,
    http: Option<reqwest::Client>,
}

impl Prober {
    pub(crate) fn new(
        spec: &HealthSpec,
        pid: u32,
        processes: Arc<dyn ProcessControl>,
        root: Arc<dyn ProcessGroup>,
    ) -> Result<Self> {
        let http = match &spec.probe {
            Probe::Http { .. } => Some(
                reqwest::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(spec.timeout)
                    .connect_timeout(spec.timeout)
                    .pool_max_idle_per_host(0)
                    .tls_danger_accept_invalid_certs(true)
                    .build()
                    .map_err(|e| ForgeError::Internal(format!("health HTTP client: {e}")))?,
            ),
            _ => None,
        };
        Ok(Self {
            probe: spec.probe.clone(),
            timeout: spec.timeout,
            pid,
            processes,
            root,
            http,
        })
    }

    /// One probe; `Err` carries a one-line reason. Never takes (much) longer than the
    /// probe timeout.
    pub(crate) async fn check(&self) -> std::result::Result<(), String> {
        let slack = Duration::from_secs(2);
        match tokio::time::timeout(self.timeout + slack, self.check_inner()).await {
            Ok(result) => result,
            Err(_) => Err(format!("timed out after {:?}", self.timeout)),
        }
    }

    async fn check_inner(&self) -> std::result::Result<(), String> {
        match &self.probe {
            Probe::Tcp { addr } => {
                match tokio::time::timeout(self.timeout, tokio::net::TcpStream::connect(addr)).await
                {
                    Ok(Ok(_stream)) => Ok(()),
                    Ok(Err(err)) => Err(format!("connect {addr}: {err}")),
                    Err(_) => Err(format!(
                        "connect {addr}: timed out after {:?}",
                        self.timeout
                    )),
                }
            }
            Probe::Http { url, expect_status } => {
                let Some(client) = &self.http else {
                    return Err("no HTTP client".to_owned());
                };
                match client.get(url).send().await {
                    Ok(response) if response.status().as_u16() == *expect_status => Ok(()),
                    Ok(response) => Err(format!(
                        "GET {url}: status {}, expected {expect_status}",
                        response.status().as_u16()
                    )),
                    Err(err) => Err(format!("GET {url}: {}", error_chain(&err))),
                }
            }
            Probe::Command { command, exit_code } => self.check_command(command, *exit_code).await,
            Probe::Pid => {
                let processes = Arc::clone(&self.processes);
                let pid = self.pid;
                match tokio::task::spawn_blocking(move || processes.is_alive(pid)).await {
                    Ok(Ok(true)) => Ok(()),
                    Ok(Ok(false)) => Err(format!("process {pid} is not running")),
                    Ok(Err(err)) => Err(err.to_string()),
                    Err(err) => Err(format!("pid probe task failed: {err}")),
                }
            }
        }
    }

    async fn check_command(
        &self,
        command: &SpawnSpec,
        expected: i32,
    ) -> std::result::Result<(), String> {
        let command = command.clone().stdio(Stdio::Discard);
        let processes = Arc::clone(&self.processes);
        let root = Arc::clone(&self.root);
        let spawned = tokio::task::spawn_blocking(move || -> Result<_> {
            let cmd = command.to_command(processes.as_ref())?;
            let job = root.create_child(GroupOptions::INSTANCE)?;
            let process = job.spawn(&cmd)?;
            Ok((job, process))
        })
        .await;
        let (job, mut process) = match spawned {
            Ok(Ok(spawned)) => spawned,
            Ok(Err(err)) => return Err(format!("health command could not start: {err}")),
            Err(err) => return Err(format!("health command task failed: {err}")),
        };
        let waited = process.exit.wait_timeout(self.timeout).await;
        // Whatever happened, nothing of the probe survives it (waits until the job is
        // empty; returns at once when the command already exited).
        let _ = tokio::task::spawn_blocking(move || job.terminate_all(TERMINATED_EXIT_CODE)).await;
        match waited {
            Ok(Some(code)) if wire_exit_code(code) == expected => Ok(()),
            Ok(Some(code)) => Err(format!(
                "health command exited with {}, expected {expected}",
                wire_exit_code(code)
            )),
            Ok(None) => Err(format!("health command timed out after {:?}", self.timeout)),
            Err(err) => Err(format!("health command: {err}")),
        }
    }
}

/// `err: cause: cause…` on one line.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_manifest() {
        let spec = HealthSpec::new(Probe::Tcp {
            addr: "127.0.0.1:3306".into(),
        });
        assert_eq!(spec.interval, Duration::from_secs(5));
        assert_eq!(spec.success_threshold, 1);
        assert_eq!(spec.failure_threshold, 3);
        assert_eq!(spec.start_timeout, Duration::from_secs(30));
        spec.validate().unwrap();
    }

    #[test]
    fn invalid_specs_are_rejected() {
        let mut spec = HealthSpec::new(Probe::Pid);
        spec.success_threshold = 0;
        assert_eq!(spec.validate().unwrap_err().code(), "invalid_input");
        let mut spec = HealthSpec::new(Probe::Pid);
        spec.interval = Duration::ZERO;
        assert!(spec.validate().is_err());
        let mut spec = HealthSpec::new(Probe::Pid);
        spec.start_timeout = Duration::ZERO;
        assert!(spec.validate().is_err());
        for url in ["not a url", "ftp://127.0.0.1/"] {
            let spec = HealthSpec::new(Probe::Http {
                url: url.into(),
                expect_status: 200,
            });
            assert!(spec.validate().is_err(), "{url}");
        }
        let spec = HealthSpec::new(Probe::Tcp { addr: " ".into() });
        assert!(spec.validate().is_err());
        HealthSpec::new(Probe::Http {
            url: "https://localhost:8443/health".into(),
            expect_status: 204,
        })
        .validate()
        .unwrap();
    }

    #[test]
    fn jitter_stays_within_ten_percent_and_varies() {
        let base = Duration::from_secs(2);
        let samples: Vec<Duration> = (0..200).map(|_| jittered(base, 0.1)).collect();
        assert!(
            samples
                .iter()
                .all(|d| *d >= Duration::from_millis(1800) && *d <= Duration::from_millis(2200)),
            "{samples:?}"
        );
        let distinct: std::collections::BTreeSet<_> = samples.iter().collect();
        assert!(
            distinct.len() > 150,
            "jitter barely varies: {}",
            distinct.len()
        );
        let low = samples.iter().filter(|d| **d < base).count();
        assert!(
            (40..160).contains(&low),
            "skewed jitter: {low}/200 below base"
        );
    }

    #[test]
    fn start_interval_until_healthy_once() {
        let mut spec = HealthSpec::new(Probe::Pid);
        spec.interval = Duration::from_secs(10);
        spec.start_interval = Duration::from_millis(500);
        assert!(spec.next_wait(false) <= Duration::from_millis(550));
        assert!(spec.next_wait(true) >= Duration::from_secs(9));
        spec.interval = Duration::from_millis(100);
        assert!(
            spec.next_wait(false) <= Duration::from_millis(110),
            "capped by interval"
        );
    }

    #[test]
    fn status_counts_consecutive_results() {
        let spec = HealthSpec {
            success_threshold: 2,
            failure_threshold: 2,
            ..HealthSpec::new(Probe::Pid)
        };
        let mut status = HealthStatus::new(&spec.probe);
        assert_eq!(status.passing, None);
        let now = Timestamp::now();
        status.record(&Ok(()), now);
        assert!(!status.is_healthy(&spec));
        status.record(&Ok(()), now);
        assert!(status.is_healthy(&spec));
        status.record(&Err("refused".into()), now);
        assert!(!status.is_healthy(&spec) && !status.became_unhealthy(&spec));
        status.record(&Err("refused".into()), now);
        assert!(status.became_unhealthy(&spec));
        assert_eq!(status.passing, Some(false));
        assert_eq!(status.last_error.as_deref(), Some("refused"));
        status.record(&Err("refused".into()), now);
        assert!(!status.became_unhealthy(&spec), "only once per streak");
        assert_eq!(status.checks, 5);
        status.record(&Ok(()), now);
        assert_eq!(status.consecutive_failures, 0);
        assert_eq!(status.probe, "pid");
    }

    #[tokio::test]
    async fn tcp_probe_follows_the_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let spec = HealthSpec {
            timeout: Duration::from_secs(3),
            ..HealthSpec::new(Probe::Tcp { addr: addr.clone() })
        };
        let processes = forge_core::platform::process_control(Default::default());
        let Ok(root) = processes.create_group(GroupOptions::ROOT) else {
            return; // unsupported platform
        };
        let prober = Prober::new(&spec, std::process::id(), Arc::clone(&processes), root).unwrap();
        prober.check().await.unwrap();
        drop(listener);
        let err = prober.check().await.unwrap_err();
        assert!(err.contains(&addr), "{err}");
    }

    #[tokio::test]
    async fn http_probe_checks_the_status() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for status in ["204 No Content", "503 Service Unavailable"] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = [0_u8; 1024];
                let _ = socket.read(&mut buf).await.unwrap();
                let response =
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let spec = HealthSpec::new(Probe::Http {
            url: format!("http://{addr}/health"),
            expect_status: 204,
        });
        let processes = forge_core::platform::process_control(Default::default());
        let Ok(root) = processes.create_group(GroupOptions::ROOT) else {
            return;
        };
        let prober = Prober::new(&spec, std::process::id(), processes, root).unwrap();
        prober.check().await.unwrap();
        let err = prober.check().await.unwrap_err();
        assert!(err.contains("status 503, expected 204"), "{err}");
        server.await.unwrap();
        let err = prober.check().await.unwrap_err();
        assert!(err.starts_with("GET http://"), "{err}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn pid_and_command_probes() {
        let processes = forge_core::platform::process_control(Default::default());
        let root = processes.create_group(GroupOptions::ROOT).unwrap();
        let spec = HealthSpec::new(Probe::Pid);
        let prober = Prober::new(
            &spec,
            std::process::id(),
            Arc::clone(&processes),
            Arc::clone(&root),
        )
        .unwrap();
        prober.check().await.unwrap();
        let gone = Prober::new(
            &spec,
            u32::MAX - 3,
            Arc::clone(&processes),
            Arc::clone(&root),
        )
        .unwrap();
        assert!(gone.check().await.is_err());

        let system = processes.system_path().remove(0);
        let cmd = |code: &str| {
            SpawnSpec::new(system.join("cmd.exe"), &system).args([
                "/d",
                "/c",
                &format!("exit {code}"),
            ])
        };
        let ok = HealthSpec::new(Probe::Command {
            command: cmd("0"),
            exit_code: 0,
        });
        Prober::new(&ok, 0, Arc::clone(&processes), Arc::clone(&root))
            .unwrap()
            .check()
            .await
            .unwrap();
        let wrong = HealthSpec::new(Probe::Command {
            command: cmd("2"),
            exit_code: 0,
        });
        let err = Prober::new(&wrong, 0, Arc::clone(&processes), Arc::clone(&root))
            .unwrap()
            .check()
            .await
            .unwrap_err();
        assert!(err.contains("exited with 2, expected 0"), "{err}");
        let hung = HealthSpec {
            timeout: Duration::from_millis(300),
            ..HealthSpec::new(Probe::Command {
                command: SpawnSpec::new(system.join("PING.EXE"), &system).args([
                    "-n",
                    "30",
                    "127.0.0.1",
                ]),
                exit_code: 0,
            })
        };
        let started = std::time::Instant::now();
        let err = Prober::new(&hung, 0, Arc::clone(&processes), Arc::clone(&root))
            .unwrap()
            .check()
            .await
            .unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(root.pids().unwrap().is_empty(), "the hung probe was killed");
    }
}
