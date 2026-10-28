//! The local daemon of an installation root: connecting to it and starting it when
//! nobody answers (docs/09, ADR-0030). Shared by `forge` and `Forgebox.exe`, so both
//! find, start and wait for `forgeboxd` the same way. Errors are typed and carry no user
//! text: each front end localises them ([`LocalError`]).
//!
//! * [`try_connect`]: one attempt, no side effects; [`LocalError::Down`] when nobody
//!   answers on the root (no token file, no pipe, a stale token), so a daemon may be
//!   started; [`LocalError::Other`] when the daemon is there but refused or failed.
//! * [`find_daemon_exe`]: `FORGE_DAEMON_EXE`, else next to the running executable
//!   (`forge.exe`, `Forgebox.exe`), else `PATH`.
//! * [`spawn_daemon`]: `forgeboxd --root <root>` through
//!   [`spawn_detached_watched`](forge_core::platform::spawn_detached_watched): no
//!   console, own process group, nothing inherited but `NUL` (the inherited-pipe hang of
//!   ADR-0030 stays fixed), and the exit is watched.
//! * [`wait_ready`]: polls every [`POLL_INTERVAL`] until the API answers (the API is the
//!   last thing the daemon opens, so answering means `daemon.ready`). A started daemon
//!   that exits with [`EXIT_ALREADY_RUNNING`] lost the single-instance mutex to another
//!   one that is starting: that is success, and the wait goes on. Any other early exit
//!   fails at once instead of waiting for the timeout.
//! * [`connect_or_start`]: the above in order.
//!
//! The clients built here read `state/ipc.token` (in `spawn_blocking`) on every
//! connection attempt ([`CredentialsProvider::local`]), so after a daemon restart, which
//! writes a new token, reconnecting just works.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use forge_core::paths::ForgeRoot;
use forge_core::platform::ProcessExit;

use super::{Client, ClientError, ClientOptions, CredentialsProvider};
use crate::api::ApiError;

/// File name of the daemon executable.
pub const DAEMON_EXE: &str = if cfg!(windows) {
    "forgeboxd.exe"
} else {
    "forgeboxd"
};

/// Environment variable naming the daemon executable explicitly (tests, unusual
/// layouts). Used only if the file exists.
pub const DAEMON_EXE_ENV: &str = "FORGE_DAEMON_EXE";

/// How long a client waits for a daemon it just started.
pub const AUTOSTART_TIMEOUT: Duration = Duration::from_secs(10);

/// Poll interval while waiting.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Exit code of a `forgeboxd` that found another daemon serving the root (it lost the
/// single-instance mutex). For an auto-start this means "already starting".
pub const EXIT_ALREADY_RUNNING: u32 = 4;

/// Who connects, and how.
#[derive(Debug, Clone)]
pub struct LocalClientConfig {
    /// `client.name` of `system.hello` (`forge`, `forgebox-desktop`...).
    pub name: String,
    pub version: String,
    pub options: ClientOptions,
}

impl LocalClientConfig {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            options: ClientOptions::default(),
        }
    }

    pub fn with_options(mut self, options: ClientOptions) -> Self {
        self.options = options;
        self
    }

    /// A client of `root` that connects on first use (nothing is read or opened now).
    pub fn client(&self, root: &ForgeRoot) -> Client {
        Client::new(
            crate::transport::local_transport(root),
            CredentialsProvider::local(root, self.name.clone(), self.version.clone()),
            self.options.clone(),
        )
    }
}

/// Why the local daemon could not be reached or started. No user text: front ends
/// localise by variant and use [`LocalError::code`] for exit codes and `--json`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LocalError {
    /// Nobody answers on this root (no token file, no pipe, or a stale token): a daemon
    /// may be started.
    #[error("the daemon is not running: {0}")]
    Down(ClientError),
    /// The daemon is there but refused or failed.
    #[error(transparent)]
    Other(ClientError),
    /// No daemon executable ([`DAEMON_EXE_ENV`], next to the running executable, `PATH`).
    #[error(
        "{DAEMON_EXE} was not found ({DAEMON_EXE_ENV}, next to the running executable or on PATH)"
    )]
    DaemonNotFound,
    /// The daemon executable could not be started.
    #[error("could not start {}: {error}", exe.display())]
    SpawnFailed { exe: PathBuf, error: ApiError },
    /// The started daemon exited before its API answered (any code but
    /// [`EXIT_ALREADY_RUNNING`]); its log says why.
    #[error("the daemon exited with code {code} before answering")]
    Exited { code: u32 },
    /// The started daemon did not answer in time.
    #[error("the daemon did not answer within {} s ({last})", timeout.as_secs())]
    Timeout {
        timeout: Duration,
        last: ClientError,
    },
}

impl LocalError {
    /// Stable error code (docs/09 exit codes).
    pub fn code(&self) -> &str {
        match self {
            LocalError::Down(error) | LocalError::Other(error) => error.code(),
            LocalError::DaemonNotFound => "not_found",
            LocalError::SpawnFailed { .. } | LocalError::Exited { .. } => "io",
            LocalError::Timeout { .. } => "timeout",
        }
    }

    /// Nobody answers: starting a daemon may help.
    pub fn is_down(&self) -> bool {
        matches!(self, LocalError::Down(_))
    }

    /// Down or Other: the client error behind it.
    pub fn client_error(&self) -> Option<&ClientError> {
        match self {
            LocalError::Down(error) | LocalError::Other(error) => Some(error),
            _ => None,
        }
    }
}

/// Sorts a failed connection attempt into "nobody there" and "refused".
pub fn classify(error: ClientError) -> LocalError {
    let down = match &error {
        ClientError::Transport(_) | ClientError::Disconnected | ClientError::Timeout(_) => true,
        // A token left behind by a daemon that died: whoever answers now is not it.
        ClientError::Api(api) => api.code == "unauthorized",
        // The daemon never ran on this root.
        ClientError::Credentials(api) => api.code == "not_found",
        _ => false,
    };
    if down {
        LocalError::Down(error)
    } else {
        LocalError::Other(error)
    }
}

/// One connection attempt, no side effects.
pub async fn try_connect(
    root: &ForgeRoot,
    config: &LocalClientConfig,
) -> Result<Client, LocalError> {
    Client::connect(
        crate::transport::local_transport(root),
        CredentialsProvider::local(root, config.name.clone(), config.version.clone()),
        config.options.clone(),
    )
    .await
    .map_err(classify)
}

/// Whether the API of `root` answers right now.
pub async fn is_running(root: &ForgeRoot, config: &LocalClientConfig) -> bool {
    match try_connect(root, config).await {
        Ok(client) => {
            client.close();
            true
        }
        Err(_) => false,
    }
}

/// The daemon executable: [`DAEMON_EXE_ENV`] (only if it exists), else next to the
/// running executable, else on `PATH`. Blocking (file checks).
pub fn find_daemon_exe() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(DAEMON_EXE_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(path)).filter(|p| p.is_file());
    }
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(DAEMON_EXE)));
    if let Some(path) = sibling.filter(|p| p.is_file()) {
        return Some(path);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(DAEMON_EXE))
        .find(|p| p.is_file())
}

/// A daemon this process started; dropping it only stops watching its exit.
#[derive(Debug)]
pub struct SpawnedDaemon {
    pub exe: PathBuf,
    exit: ProcessExit,
}

impl SpawnedDaemon {
    pub fn pid(&self) -> u32 {
        self.exit.pid()
    }

    /// Exit code, once it has exited (non-blocking).
    pub fn exit_code(&self) -> Option<u32> {
        self.exit.try_exit_code().ok().flatten()
    }
}

/// Starts the daemon of `root` detached from this process (console, process group, job
/// and handles). The file checks and the process creation run in `spawn_blocking`.
pub async fn spawn_daemon(root: &ForgeRoot) -> Result<SpawnedDaemon, LocalError> {
    let root_path = root.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let exe = find_daemon_exe().ok_or(LocalError::DaemonNotFound)?;
        spawn_exe(exe, &root_path)
    })
    .await
    .map_err(|e| LocalError::Other(ClientError::Protocol(format!("daemon spawner: {e}"))))?
}

fn spawn_exe(exe: PathBuf, root: &Path) -> Result<SpawnedDaemon, LocalError> {
    let args: Vec<OsString> = vec!["--root".into(), root.as_os_str().into()];
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match forge_core::platform::spawn_detached_watched(&exe, &args, &cwd) {
        Ok(exit) => {
            tracing::info!(exe = %exe.display(), pid = exit.pid(), "daemon started");
            Ok(SpawnedDaemon { exe, exit })
        }
        Err(err) => Err(LocalError::SpawnFailed {
            error: ApiError::from(&err),
            exe,
        }),
    }
}

/// Polls until the API of `root` answers or `timeout` passes. With the daemon this
/// process `spawned`, an early exit fails at once, except [`EXIT_ALREADY_RUNNING`]
/// (another daemon is starting: keep waiting for it).
pub async fn wait_ready(
    root: &ForgeRoot,
    config: &LocalClientConfig,
    timeout: Duration,
    spawned: Option<SpawnedDaemon>,
) -> Result<Client, LocalError> {
    let deadline = Instant::now() + timeout;
    loop {
        match try_connect(root, config).await {
            Ok(client) => return Ok(client),
            Err(LocalError::Down(last)) => {
                if let Some(code) = spawned.as_ref().and_then(SpawnedDaemon::exit_code) {
                    if code != EXIT_ALREADY_RUNNING {
                        return Err(LocalError::Exited { code });
                    }
                }
                if Instant::now() >= deadline {
                    return Err(LocalError::Timeout { timeout, last });
                }
            }
            Err(other) => return Err(other),
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether [`connect_or_start`] may start the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoStart {
    /// Only connect (`forge --no-daemon`, a root that was never initialised).
    Never,
    /// Start it when nobody answers, and wait at most `timeout` for it.
    IfDown { timeout: Duration },
}

impl Default for AutoStart {
    fn default() -> Self {
        AutoStart::IfDown {
            timeout: AUTOSTART_TIMEOUT,
        }
    }
}

/// Connects to the daemon of `root`, starting it when nobody answers and `autostart`
/// allows it. `on_start` runs right before the daemon is started (the CLI prints its
/// notice there).
pub async fn connect_or_start(
    root: &ForgeRoot,
    config: &LocalClientConfig,
    autostart: AutoStart,
    on_start: impl FnOnce(),
) -> Result<Client, LocalError> {
    match (try_connect(root, config).await, autostart) {
        (Ok(client), _) => Ok(client),
        (Err(LocalError::Down(_)), AutoStart::IfDown { timeout }) => {
            on_start();
            let spawned = spawn_daemon(root).await?;
            wait_ready(root, config, timeout, Some(spawned)).await
        }
        (Err(err), _) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LocalClientConfig {
        LocalClientConfig::new("forge-test", "0.0.0").with_options(ClientOptions {
            connect_timeout: Duration::from_secs(2),
            ..ClientOptions::default()
        })
    }

    #[tokio::test]
    async fn a_root_without_daemon_is_down_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let err = try_connect(&root, &config()).await.unwrap_err();
        assert!(err.is_down(), "{err:?}");
        // No token file: the daemon never ran here.
        assert!(
            matches!(&err, LocalError::Down(ClientError::Credentials(api)) if api.code == "not_found"),
            "{err:?}"
        );
        assert_eq!(err.code(), "not_found");
        assert!(!is_running(&root, &config()).await);
    }

    #[tokio::test]
    async fn never_autostart_reports_down_without_starting_anything() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let mut started = false;
        let err = connect_or_start(&root, &config(), AutoStart::Never, || started = true)
            .await
            .unwrap_err();
        assert!(err.is_down(), "{err:?}");
        assert!(!started);
    }

    #[test]
    fn classification_follows_the_cli_contract() {
        use crate::transport::TransportError;
        let down = [
            ClientError::Disconnected,
            ClientError::Timeout(Duration::from_secs(1)),
            ClientError::Transport(TransportError::Closed),
            ClientError::Api(ApiError::new("unauthorized", "stale token")),
            ClientError::Credentials(ApiError::new("not_found", "no token")),
        ];
        for error in down {
            assert!(classify(error.clone()).is_down(), "{error:?}");
        }
        let other = [
            ClientError::Api(ApiError::new("forbidden", "x")),
            ClientError::Credentials(ApiError::new("io", "access denied")),
            ClientError::Protocol("x".into()),
            ClientError::Closed,
        ];
        for error in other {
            assert!(!classify(error.clone()).is_down(), "{error:?}");
        }
        assert_eq!(LocalError::DaemonNotFound.code(), "not_found");
        assert_eq!(LocalError::Exited { code: 1 }.code(), "io");
        assert!(LocalError::DaemonNotFound.to_string().contains("forgeboxd"));
    }

    /// `cmd /c exit <code>` standing in for a daemon that exits at once.
    #[cfg(windows)]
    fn exiting(code: u32, dir: &Path) -> SpawnedDaemon {
        let shell = PathBuf::from(std::env::var_os("ComSpec").unwrap());
        let args: Vec<OsString> = vec!["/c".into(), "exit".into(), code.to_string().into()];
        let exit = forge_core::platform::spawn_detached_watched(&shell, &args, dir).unwrap();
        SpawnedDaemon { exe: shell, exit }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_started_daemon_that_exits_fails_fast() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let started = Instant::now();
        let err = wait_ready(
            &root,
            &config(),
            Duration::from_secs(20),
            Some(exiting(1, dir.path())),
        )
        .await
        .unwrap_err();
        assert_eq!(err, LocalError::Exited { code: 1 });
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn exit_code_4_means_another_daemon_is_starting_so_the_wait_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let spawned = exiting(EXIT_ALREADY_RUNNING, dir.path());
        let timeout = Duration::from_millis(800);
        let started = Instant::now();
        let err = wait_ready(&root, &config(), timeout, Some(spawned))
            .await
            .unwrap_err();
        assert!(matches!(err, LocalError::Timeout { .. }), "{err:?}");
        assert!(started.elapsed() >= timeout);
        assert_eq!(err.code(), "timeout");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn spawning_a_missing_executable_is_spawn_failed_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = spawn_exe(dir.path().join("forgeboxd.exe"), dir.path()).unwrap_err();
        match err {
            LocalError::SpawnFailed { error, .. } => assert_eq!(error.code, "not_found"),
            other => panic!("{other:?}"),
        }
    }
}
