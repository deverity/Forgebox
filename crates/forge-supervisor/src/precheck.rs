//! Pre-checks of a run (docs/03 §1.5): before an instance creates its process, prove that
//! starting it can work, and say precisely why not.
//!
//! In order, cheapest first (the first failure ends the checks and becomes the
//! instance's `Failed(reason)`):
//!
//! 1. **Runtime deps** (`spec.runtime_deps`): the VC++ Redistributable is new enough for
//!    each `vcredist-<flavor>` ([`forge_core::platform::RuntimeDeps`]) →
//!    [`FailureReason::MissingRuntime`] with the download link. Comes first because the
//!    configuration check below runs the tool's own executable, which would only die
//!    with `STATUS_DLL_NOT_FOUND` without it.
//! 2. **Dependencies** (`service.depends_on`): each one is `Running`, asked through the
//!    [`DependencyLookup`] the daemon injects → [`FailureReason::DependencyNotRunning`].
//! 3. **Ports**: no process outside the instance's own job listens on a declared port
//!    ([`forge_core::platform::PortTable`], IPv4 and IPv6, any address) →
//!    [`FailureReason::PortInUse`] with the holder's PID and executable.
//! 4. **Configuration** (`service.check`, `nginx -t`): the command runs in a Job Object of
//!    its own (child of the root job), output captured, bounded by
//!    [`ConfigCheck::timeout`]; exit ≠ 0 → [`FailureReason::ConfigInvalid`] with its
//!    output (stderr, then stdout; at most [`CHECK_OUTPUT_LIMIT`] bytes of each), a
//!    missing DLL → [`FailureReason::MissingDll`], no exit in time →
//!    [`FailureReason::Other`] with code `timeout`.
//!
//! Reading the TCP table or the registry can fail (access denied, a damaged IP Helper
//! service, a timeout...): those checks then **pass** with a warning in the log. The
//! pre-checks are a diagnosis aid, never a reason to refuse a start that may work; the
//! service's own bind error still surfaces as `exited_during_start` with its log.
//!
//! A restart of a running instance validates first (runtime deps, dependencies and
//! configuration; not the ports, which the instance itself holds) and keeps the instance
//! running when that fails: a broken configuration never takes down a working service.
//!
//! Everything blocking (TCP table, registry, `CreateProcess`, job queries) runs in
//! `spawn_blocking` with a timeout; dependencies are asked with a timeout too.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use forge_core::ForgeError;
use forge_core::ids::ToolId;
use forge_core::platform::{
    GroupOptions, PipeReader, PortTable, ProcessControl, ProcessGroup, RuntimeDep, RuntimeDeps,
};
use forge_core::runtime::spawn_blocking_timeout;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::events::wire_exit_code;
use crate::process::{SpawnSpec, Stdio};
use crate::service::{FailureReason, InstanceState};
use crate::stop::TERMINATED_EXIT_CODE;

/// Default budget of the configuration check (the manifest has no field for it).
pub const DEFAULT_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// Bytes kept from each output stream of the configuration check (the rest is read and
/// dropped, so the command never blocks on a full pipe).
pub const CHECK_OUTPUT_LIMIT: usize = 16 * 1024;

/// Budget of one read of the TCP table, of the runtime detection and of a dependency
/// lookup.
const QUICK_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the output of a finished check may still take to reach EOF.
const OUTPUT_GRACE: Duration = Duration::from_secs(1);

/// `service.check` of the manifest, rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigCheck {
    /// The command (`nginx.exe -t -c …`); its stdio setting is ignored (always captured).
    pub command: SpawnSpec,
    /// Budget of the whole check ([`DEFAULT_CHECK_TIMEOUT`]).
    pub timeout: Duration,
}

impl ConfigCheck {
    /// Check running `command` with the default timeout.
    pub fn new(command: SpawnSpec) -> Self {
        Self {
            command,
            timeout: DEFAULT_CHECK_TIMEOUT,
        }
    }

    /// Sets the budget.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// A service that must be `Running` first (`service.depends_on`): a tool, optionally a
/// named instance of it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DependencyRef {
    pub tool: ToolId,
    /// `None`: any (the default) instance of the tool, as the daemon resolves it.
    pub instance: Option<String>,
}

impl DependencyRef {
    /// The tool's service, whatever the instance.
    pub fn tool(tool: ToolId) -> Self {
        Self {
            tool,
            instance: None,
        }
    }

    /// A named instance.
    pub fn instance(tool: ToolId, instance: impl Into<String>) -> Self {
        Self {
            tool,
            instance: Some(instance.into()),
        }
    }
}

impl fmt::Display for DependencyRef {
    /// `mysql` or `mysql/default`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.instance {
            Some(instance) => write!(f, "{}/{instance}", self.tool),
            None => write!(f, "{}", self.tool),
        }
    }
}

/// What to check before a run of an instance (from its manifest and its reservations).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrecheckSpec {
    /// `service.check`.
    pub check: Option<ConfigCheck>,
    /// Ports the instance will listen on (its reservations, docs/03 §4).
    pub ports: Vec<u16>,
    /// `service.depends_on`.
    pub depends_on: Vec<DependencyRef>,
    /// `spec.runtime_deps` (parsed manifest ids, `vcredist-vs17`).
    pub runtime_deps: Vec<RuntimeDep>,
}

impl PrecheckSpec {
    /// Nothing to check: the run creates its process at once.
    pub fn is_empty(&self) -> bool {
        self.check.is_none()
            && self.ports.is_empty()
            && self.depends_on.is_empty()
            && self.runtime_deps.is_empty()
    }

    /// Checks the values (`InvalidInput`): no port 0, a non-zero check timeout.
    pub fn validate(&self) -> forge_core::Result<()> {
        if self.ports.contains(&0) {
            return Err(ForgeError::InvalidInput(
                "pre-checks: port 0 cannot be declared".to_owned(),
            ));
        }
        if self.check.as_ref().is_some_and(|c| c.timeout.is_zero()) {
            return Err(ForgeError::InvalidInput(
                "pre-checks: the configuration check timeout must be greater than zero".to_owned(),
            ));
        }
        Ok(())
    }

    /// Sets the configuration check.
    pub fn with_check(mut self, check: ConfigCheck) -> Self {
        self.check = Some(check);
        self
    }

    /// Adds declared ports.
    pub fn with_ports(mut self, ports: impl IntoIterator<Item = u16>) -> Self {
        self.ports.extend(ports);
        self
    }

    /// Adds a dependency.
    pub fn with_dependency(mut self, dependency: DependencyRef) -> Self {
        self.depends_on.push(dependency);
        self
    }

    /// Adds a runtime dependency.
    pub fn with_runtime(mut self, dep: RuntimeDep) -> Self {
        self.runtime_deps.push(dep);
        self
    }
}

/// Answer of a [`DependencyLookup`].
pub type DependencyFuture = Pin<Box<dyn Future<Output = Option<InstanceState>> + Send>>;

/// How the pre-checks learn the state of a `depends_on` service. Injected by the daemon,
/// which knows every instance (typically an `ask(Status)` to the dependency's actor with
/// a short timeout). The lookup runs in the pre-check task, never in an instance actor,
/// so instances that depend on each other cannot deadlock.
pub trait DependencyLookup: Send + Sync {
    /// Current state of `dependency`; `None` when no such service exists.
    fn state(&self, dependency: &DependencyRef) -> DependencyFuture;
}

impl<F> DependencyLookup for F
where
    F: Fn(&DependencyRef) -> DependencyFuture + Send + Sync,
{
    fn state(&self, dependency: &DependencyRef) -> DependencyFuture {
        self(dependency)
    }
}

/// Default lookup: knows no service, so every `depends_on` fails with state `unknown`
/// (the daemon always installs its own).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoDependencies;

impl DependencyLookup for NoDependencies {
    fn state(&self, _: &DependencyRef) -> DependencyFuture {
        Box::pin(async { None })
    }
}

/// What the pre-checks use from the platform and the daemon.
#[derive(Clone)]
pub struct PrecheckEnv {
    pub ports: Arc<dyn PortTable>,
    pub runtimes: Arc<dyn RuntimeDeps>,
    pub dependencies: Arc<dyn DependencyLookup>,
    pub processes: Arc<dyn ProcessControl>,
    /// Parent of the configuration check's job (the daemon's root job).
    pub root: Arc<dyn ProcessGroup>,
}

impl fmt::Debug for PrecheckEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrecheckEnv")
            .field("ports", &self.ports)
            .field("runtimes", &self.runtimes)
            .finish_non_exhaustive()
    }
}

/// Which checks run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrecheckScope {
    /// Before creating the process: everything.
    Start,
    /// Before stopping a running instance for a restart: runtime deps, dependencies and
    /// configuration (its ports are held by the instance itself).
    Restart,
}

/// Runs the checks of `scope` in order; the first failure is the answer. `own_pids`: the
/// processes of the instance's own job (never a port conflict).
pub async fn run(
    spec: &PrecheckSpec,
    env: &PrecheckEnv,
    scope: PrecheckScope,
    own_pids: &BTreeSet<u32>,
) -> Result<(), FailureReason> {
    check_runtime_deps(&spec.runtime_deps, &env.runtimes).await?;
    check_dependencies(&spec.depends_on, env.dependencies.as_ref()).await?;
    if scope == PrecheckScope::Start {
        check_ports(&spec.ports, &env.ports, own_pids).await?;
    }
    if let Some(check) = &spec.check {
        check_config(check, &env.processes, &env.root).await?;
    }
    Ok(())
}

/// Step 1: every runtime dependency is satisfied. A detection error passes (logged).
pub async fn check_runtime_deps(
    deps: &[RuntimeDep],
    runtimes: &Arc<dyn RuntimeDeps>,
) -> Result<(), FailureReason> {
    if deps.is_empty() {
        return Ok(());
    }
    let detector = Arc::clone(runtimes);
    let installed =
        match spawn_blocking_timeout("detecting the VC++ runtime", QUICK_TIMEOUT, move || {
            detector.vcredist_version()
        })
        .await
        {
            Ok(installed) => installed,
            Err(err) => {
                tracing::warn!(%err, "runtime pre-check skipped: detection failed");
                return Ok(());
            }
        };
    for dep in deps {
        match dep {
            RuntimeDep::VcRedist(flavor) => {
                if !installed.is_some_and(|v| v.satisfies(*flavor)) {
                    return Err(FailureReason::MissingRuntime {
                        flavor: flavor.as_str().to_owned(),
                        download_url: flavor.download_url().to_owned(),
                        installed: installed.map(|v| v.to_string()),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Step 2: every dependency is `Running`. A lookup that does not answer within its
/// budget counts as state `unknown`.
pub async fn check_dependencies(
    deps: &[DependencyRef],
    lookup: &dyn DependencyLookup,
) -> Result<(), FailureReason> {
    for dep in deps {
        let state = tokio::time::timeout(QUICK_TIMEOUT, lookup.state(dep))
            .await
            .ok()
            .flatten();
        match state {
            Some(InstanceState::Running) => {}
            other => {
                return Err(FailureReason::DependencyNotRunning {
                    dependency: dep.to_string(),
                    state: other.map_or("unknown", |s| s.name()).to_owned(),
                });
            }
        }
    }
    Ok(())
}

/// Step 3: no process outside `own_pids` listens on a declared port (the first such
/// port, in declaration order, is reported with its holder). A TCP table read error
/// passes (logged).
pub async fn check_ports(
    ports: &[u16],
    table: &Arc<dyn PortTable>,
    own_pids: &BTreeSet<u32>,
) -> Result<(), FailureReason> {
    if ports.is_empty() {
        return Ok(());
    }
    let reader = Arc::clone(table);
    let listeners =
        match spawn_blocking_timeout("reading the TCP table", QUICK_TIMEOUT, move || {
            reader.listeners()
        })
        .await
        {
            Ok(listeners) => listeners,
            Err(err) => {
                tracing::warn!(%err, "port pre-check skipped: the TCP table is unreadable");
                return Ok(());
            }
        };
    for port in ports {
        if let Some(holder) = listeners
            .iter()
            .find(|l| l.port == *port && !own_pids.contains(&l.pid))
        {
            return Err(FailureReason::PortInUse {
                port: *port,
                pid: Some(holder.pid),
                exe: holder.exe_display(),
            });
        }
    }
    Ok(())
}

/// Step 4: the configuration check exits with 0 within its budget.
pub async fn check_config(
    check: &ConfigCheck,
    processes: &Arc<dyn ProcessControl>,
    root: &Arc<dyn ProcessGroup>,
) -> Result<(), FailureReason> {
    let command = check.command.clone().stdio(Stdio::Capture);
    let processes = Arc::clone(processes);
    let root = Arc::clone(root);
    let spawned = spawn_blocking_timeout(
        "starting the configuration check",
        QUICK_TIMEOUT.max(check.timeout),
        move || {
            let cmd = command.to_command(processes.as_ref())?;
            let job = root.create_child(GroupOptions::INSTANCE)?;
            let process = job.spawn(&cmd)?;
            Ok((job, process))
        },
    )
    .await;
    let (job, mut process) = spawned.map_err(|err| FailureReason::from_error(&err))?;
    let stdout = tokio::spawn(read_capped(process.stdout.take()));
    let stderr = tokio::spawn(read_capped(process.stderr.take()));
    let waited = process.exit.wait_timeout(check.timeout).await;
    // Nothing of the check survives it (returns at once when it already exited); this
    // also closes the pipes its descendants may still hold.
    let killer = Arc::clone(&job);
    if let Err(err) = spawn_blocking_timeout(
        "ending the configuration check",
        QUICK_TIMEOUT * 2,
        move || killer.terminate_all(TERMINATED_EXIT_CODE),
    )
    .await
    {
        tracing::warn!(%err, "the configuration check job did not empty");
    }
    drop(job);
    let (out, err) = (collect(stdout).await, collect(stderr).await);
    let program = check.command.exe.file_name().map_or_else(
        || check.command.exe.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    match waited {
        Ok(Some(0)) => Ok(()),
        Ok(Some(code)) => match FailureReason::for_exit_code(code) {
            Some(reason) => Err(reason),
            None => Err(FailureReason::ConfigInvalid {
                stderr: check_output(&err, &out, &program, code),
            }),
        },
        Ok(None) => Err(FailureReason::Other {
            code: "timeout".to_owned(),
            message: format!(
                "the configuration check ({program}) did not finish within {:?}",
                check.timeout
            ),
        }),
        Err(error) => Err(FailureReason::from_error(&error)),
    }
}

/// Output of a pipe reader task, waiting at most [`OUTPUT_GRACE`] for its EOF.
async fn collect(task: tokio::task::JoinHandle<String>) -> String {
    match tokio::time::timeout(OUTPUT_GRACE, task).await {
        Ok(Ok(text)) => text,
        _ => String::new(),
    }
}

/// Reads a captured pipe to EOF, keeping the first [`CHECK_OUTPUT_LIMIT`] bytes.
async fn read_capped(reader: Option<PipeReader>) -> String {
    let Some(mut reader) = reader else {
        return String::new();
    };
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut buf = vec![0_u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = CHECK_OUTPUT_LIMIT.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
                truncated |= n > room;
            }
        }
    }
    let mut text = String::from_utf8_lossy(&kept).replace("\r\n", "\n");
    if truncated {
        text.push_str("\n[output truncated]");
    }
    text
}

/// What `ConfigInvalid` carries: stderr, then stdout (trimmed, LF), or a fallback line.
fn check_output(stderr: &str, stdout: &str, program: &str, code: u32) -> String {
    let parts: Vec<&str> = [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        format!(
            "{program} exited with code {} and printed nothing",
            wire_exit_code(code)
        )
    } else {
        parts.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use forge_core::platform::{PortOwner, PortProto, VcRedistFlavor, VcRuntimeVersion};

    use super::*;

    #[derive(Debug)]
    struct Runtime(Option<Option<VcRuntimeVersion>>);

    impl RuntimeDeps for Runtime {
        fn vcredist_version(&self) -> forge_core::Result<Option<VcRuntimeVersion>> {
            self.0
                .ok_or_else(|| forge_core::ForgeError::Internal("registry broken".into()))
        }
    }

    fn runtime(v: Option<&str>) -> Arc<dyn RuntimeDeps> {
        Arc::new(Runtime(Some(v.map(|v| v.parse().unwrap()))))
    }

    #[tokio::test]
    async fn runtime_deps_need_a_new_enough_runtime() {
        let vs17 = [RuntimeDep::VcRedist(VcRedistFlavor::Vs17)];
        check_runtime_deps(&vs17, &runtime(Some("14.44.35211.0")))
            .await
            .unwrap();
        check_runtime_deps(&[], &runtime(None)).await.unwrap();
        let err = check_runtime_deps(&vs17, &runtime(Some("14.29.30133.0")))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            FailureReason::MissingRuntime {
                flavor: "vs17".into(),
                download_url: forge_core::platform::VCREDIST_X64_URL.into(),
                installed: Some("14.29.30133.0".into()),
            }
        );
        let err = check_runtime_deps(&vs17, &runtime(None)).await.unwrap_err();
        assert_eq!(err.kind(), "missing_runtime");
        let broken: Arc<dyn RuntimeDeps> = Arc::new(Runtime(None));
        check_runtime_deps(&vs17, &broken).await.unwrap();
    }

    #[tokio::test]
    async fn dependencies_must_be_running() {
        let mysql = DependencyRef::instance(ToolId::new("mysql").unwrap(), "default");
        let redis = DependencyRef::tool(ToolId::new("redis").unwrap());
        assert_eq!(mysql.to_string(), "mysql/default");
        assert_eq!(redis.to_string(), "redis");
        let lookup = |dep: &DependencyRef| -> DependencyFuture {
            let state = match dep.tool.as_str() {
                "mysql" => Some(InstanceState::Running),
                "redis" => Some(InstanceState::Stopped),
                _ => None,
            };
            Box::pin(async move { state })
        };
        check_dependencies(std::slice::from_ref(&mysql), &lookup)
            .await
            .unwrap();
        let err = check_dependencies(&[mysql.clone(), redis], &lookup)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            FailureReason::DependencyNotRunning {
                dependency: "redis".into(),
                state: "stopped".into()
            }
        );
        let err = check_dependencies(&[mysql], &NoDependencies)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            FailureReason::DependencyNotRunning {
                dependency: "mysql/default".into(),
                state: "unknown".into()
            }
        );
    }

    #[derive(Debug, Default)]
    struct Table(Mutex<Vec<PortOwner>>);

    impl PortTable for Table {
        fn listeners(&self) -> forge_core::Result<Vec<PortOwner>> {
            Ok(self.0.lock().unwrap().clone())
        }

        fn established_count(&self, _: u16) -> forge_core::Result<u32> {
            Ok(0)
        }
    }

    fn listener(port: u16, pid: u32, exe: Option<&str>) -> PortOwner {
        PortOwner {
            port,
            addr: IpAddr::from([0, 0, 0, 0]),
            pid,
            exe_path: exe.map(PathBuf::from),
            proto: PortProto::Tcp,
        }
    }

    #[tokio::test]
    async fn ports_held_by_others_fail_in_declaration_order() {
        let table = Table::default();
        table.0.lock().unwrap().extend([
            listener(80, 4, None),
            listener(3306, 4412, Some(r"C:\xampp\mysql\bin\mysqld.exe")),
            listener(8080, 77, Some(r"C:\Forgebox\bin\nginx\nginx.exe")),
        ]);
        let table: Arc<dyn PortTable> = Arc::new(table);
        let own = BTreeSet::from([77]);
        check_ports(&[8080, 9000], &table, &own).await.unwrap();
        let err = check_ports(&[8080, 3306, 80], &table, &own)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            FailureReason::PortInUse {
                port: 3306,
                pid: Some(4412),
                exe: Some(r"C:\xampp\mysql\bin\mysqld.exe".into()),
            }
        );
        let err = check_ports(&[80], &table, &own).await.unwrap_err();
        assert_eq!(
            err,
            FailureReason::PortInUse {
                port: 80,
                pid: Some(4),
                exe: None
            }
        );
    }

    #[test]
    fn check_output_prefers_stderr_and_never_is_empty() {
        assert_eq!(
            check_output(
                "nginx: [emerg] unknown directive \"lsten\"\n",
                "",
                "nginx.exe",
                1
            ),
            "nginx: [emerg] unknown directive \"lsten\""
        );
        assert_eq!(check_output("  e  ", " o ", "x.exe", 1), "e\no");
        assert_eq!(
            check_output("", "", "x.exe", 0xFFFF_FFFF),
            "x.exe exited with code -1 and printed nothing"
        );
    }

    #[test]
    fn specs_know_when_they_are_empty() {
        assert!(PrecheckSpec::default().is_empty());
        assert!(!PrecheckSpec::default().with_ports([80]).is_empty());
        let spec = PrecheckSpec::default().with_runtime(RuntimeDep::VcRedist(VcRedistFlavor::Vs17));
        assert!(!spec.is_empty());
        let check = ConfigCheck::new(SpawnSpec::new("C:/x.exe", "C:/"));
        assert_eq!(check.timeout, DEFAULT_CHECK_TIMEOUT);
        assert!(!PrecheckSpec::default().with_check(check).is_empty());
        let dep = DependencyRef::tool(ToolId::new("mysql").unwrap());
        assert!(!PrecheckSpec::default().with_dependency(dep).is_empty());
    }
}
