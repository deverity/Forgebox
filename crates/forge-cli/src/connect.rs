//! Connection to the local daemon, with autostart (docs/09): when the pipe does not answer
//! and `--no-daemon` was not given, `forgeboxd.exe` (next to `forge.exe`, or on `PATH`) is
//! started detached and the CLI waits until the API answers, at most [`AUTOSTART_TIMEOUT`].
//! The API is the last thing the daemon opens (`daemon.ready`, docs/01 §5.1), so a
//! successful connection means it is ready.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use forge_ipc::base::paths::ForgeRoot;
use forge_ipc::{Client, ClientError, ClientOptions, Credentials};

use crate::error::{CliError, CliResult};
use crate::messages::Msg;
use crate::output::Output;

/// How long the CLI waits for a daemon it just started.
pub const AUTOSTART_TIMEOUT: Duration = Duration::from_secs(10);
/// Poll interval while waiting.
const POLL: Duration = Duration::from_millis(100);
/// Name the CLI gives itself in `system.hello`.
pub const CLIENT_NAME: &str = "forge";

/// File name of the daemon executable.
pub const DAEMON_EXE: &str = if cfg!(windows) {
    "forgeboxd.exe"
} else {
    "forgeboxd"
};

/// Why a connection attempt failed.
#[derive(Debug)]
pub enum Failure {
    /// Nobody answers on this root (no token file, no pipe, or a stale token): a daemon
    /// may be started.
    Down(CliError),
    /// The daemon is there but refused or failed.
    Other(CliError),
}

impl Failure {
    pub fn into_error(self) -> CliError {
        match self {
            Failure::Down(e) | Failure::Other(e) => e,
        }
    }
}

fn options() -> ClientOptions {
    ClientOptions {
        call_timeout: Duration::from_secs(20),
        connect_timeout: Duration::from_secs(3),
        ..ClientOptions::default()
    }
}

/// One connection attempt, no side effects.
pub async fn try_connect(root: &ForgeRoot) -> Result<Client, Failure> {
    let owned = root.clone();
    let credentials = tokio::task::spawn_blocking(move || {
        Credentials::local(&owned, CLIENT_NAME, env!("CARGO_PKG_VERSION"))
    })
    .await
    .map_err(|e| Failure::Other(CliError::new("internal", e.to_string())))?;
    let credentials = match credentials {
        Ok(credentials) => credentials,
        Err(e) if e.code() == "not_found" => {
            return Err(Failure::Down(CliError::new("not_found", e.to_string())));
        }
        Err(e) => return Err(Failure::Other(CliError::new(e.code(), e.to_string()))),
    };
    let transport = forge_ipc::transport::local_transport(root);
    match Client::connect(transport, credentials, options()).await {
        Ok(client) => Ok(client),
        Err(
            err @ (ClientError::Transport(_) | ClientError::Disconnected | ClientError::Timeout(_)),
        ) => Err(Failure::Down(err.into())),
        Err(ClientError::Api(api)) if api.code == "unauthorized" => {
            Err(Failure::Down(CliError::from(api)))
        }
        Err(other) => Err(Failure::Other(other.into())),
    }
}

/// The daemon executable: next to this executable, else on `PATH`.
pub fn find_daemon_exe() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("FORGE_DAEMON_EXE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(path));
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

/// Starts the daemon of `root` detached from this console.
pub fn spawn_daemon(root: &ForgeRoot) -> CliResult<u32> {
    let exe = find_daemon_exe().ok_or_else(|| {
        CliError::new(
            "not_found",
            format!("{DAEMON_EXE} não encontrado (esperado ao lado de forge.exe ou no PATH)"),
        )
    })?;
    let args: Vec<OsString> = vec!["--root".into(), root.path().as_os_str().into()];
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    forge_ipc::base::platform::spawn_detached(&exe, &args, &cwd)
        .map_err(|e| CliError::io(format!("não foi possível iniciar {}: {e}", exe.display())))
}

/// Polls until the API answers or `timeout` passes.
pub async fn wait_ready(root: &ForgeRoot, timeout: Duration) -> CliResult<Client> {
    let deadline = Instant::now() + timeout;
    loop {
        match try_connect(root).await {
            Ok(client) => return Ok(client),
            Err(Failure::Other(err)) => return Err(err),
            Err(Failure::Down(err)) => {
                if Instant::now() >= deadline {
                    return Err(CliError::new(
                        "timeout",
                        format!(
                            "o daemon não respondeu em {} s ({})",
                            timeout.as_secs(),
                            err.message
                        ),
                    ));
                }
            }
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Connects, starting the daemon when allowed.
pub async fn connect_or_start(
    root: &ForgeRoot,
    no_daemon: bool,
    out: &Output,
) -> CliResult<Client> {
    match try_connect(root).await {
        Ok(client) => Ok(client),
        Err(Failure::Other(err)) => Err(err),
        Err(Failure::Down(_)) => {
            if no_daemon {
                return Err(CliError::io(
                    out.tf(Msg::DaemonNotRunning, &[&root.to_posix_string()]),
                ));
            }
            out.note(out.tf(Msg::StartingDaemon, &[&root.to_posix_string()]));
            spawn_daemon(root)?;
            wait_ready(root, AUTOSTART_TIMEOUT).await
        }
    }
}

/// Whether the API of `root` answers right now.
pub async fn is_running(root: &ForgeRoot) -> bool {
    try_connect(root).await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_root_without_daemon_is_down_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        assert!(matches!(try_connect(&root).await, Err(Failure::Down(_))));
        assert!(!is_running(&root).await);
    }

    #[tokio::test]
    async fn no_daemon_flag_never_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let out = Output::new(false, true, crate::messages::Lang::PtBr);
        let err = connect_or_start(&root, true, &out).await.unwrap_err();
        assert_eq!(err.exit, 1);
        assert!(err.message.contains("daemon"), "{}", err.message);
    }
}
