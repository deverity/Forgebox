//! Connection to the local daemon, with autostart (docs/09): when the pipe does not answer
//! and `--no-daemon` was not given, `forgeboxd.exe` is started detached and the CLI waits
//! until the API answers, at most [`AUTOSTART_TIMEOUT`]. The mechanics (finding the
//! executable, the detached spawn, the wait, "exit 4 = another daemon is starting") live
//! in [`forge_ipc::client::local`], shared with the desktop app; this module only names
//! the client and turns [`LocalError`] into localised [`CliError`]s.

use std::time::Duration;

use forge_ipc::Client;
use forge_ipc::base::paths::ForgeRoot;
use forge_ipc::client::ClientOptions;
use forge_ipc::client::local::{self, AutoStart, LocalClientConfig, SpawnedDaemon};
pub use forge_ipc::client::local::{AUTOSTART_TIMEOUT, DAEMON_EXE, LocalError};

use crate::error::{CliError, CliResult};
use crate::messages::Msg;
use crate::output::Output;

/// Name the CLI gives itself in `system.hello`.
pub const CLIENT_NAME: &str = "forge";

fn config() -> LocalClientConfig {
    LocalClientConfig::new(CLIENT_NAME, env!("CARGO_PKG_VERSION")).with_options(ClientOptions {
        call_timeout: Duration::from_secs(20),
        connect_timeout: Duration::from_secs(3),
        ..ClientOptions::default()
    })
}

/// The localised error for a failure to reach or start the daemon of `root`.
pub fn localize(error: LocalError, root: &ForgeRoot, out: &Output) -> CliError {
    match error {
        LocalError::Down(error) | LocalError::Other(error) => error.into(),
        LocalError::DaemonNotFound => {
            CliError::new("not_found", out.tf(Msg::DaemonExeNotFound, &[&DAEMON_EXE]))
        }
        LocalError::SpawnFailed { exe, error } => {
            CliError::io(out.tf(Msg::DaemonSpawnFailed, &[&exe.display(), &error.message]))
        }
        LocalError::Exited { code } => {
            CliError::io(out.tf(Msg::DaemonExitedEarly, &[&code, &root.logs().display()]))
        }
        LocalError::Timeout { timeout, last } => CliError::new(
            "timeout",
            out.tf(Msg::DaemonStartTimeout, &[&timeout.as_secs(), &last]),
        ),
    }
}

/// One connection attempt, no side effects.
pub async fn try_connect(root: &ForgeRoot) -> Result<Client, LocalError> {
    local::try_connect(root, &config()).await
}

/// Starts the daemon of `root` detached from this console.
pub async fn spawn_daemon(root: &ForgeRoot, out: &Output) -> CliResult<SpawnedDaemon> {
    local::spawn_daemon(root)
        .await
        .map_err(|e| localize(e, root, out))
}

/// Polls until the API answers or `timeout` passes (fails at once if the daemon this
/// command started exits, unless another daemon won the race).
pub async fn wait_ready(
    root: &ForgeRoot,
    timeout: Duration,
    spawned: Option<SpawnedDaemon>,
    out: &Output,
) -> CliResult<Client> {
    local::wait_ready(root, &config(), timeout, spawned)
        .await
        .map_err(|e| localize(e, root, out))
}

/// Connects, starting the daemon when allowed.
pub async fn connect_or_start(
    root: &ForgeRoot,
    no_daemon: bool,
    out: &Output,
) -> CliResult<Client> {
    let autostart = if no_daemon {
        AutoStart::Never
    } else {
        AutoStart::IfDown {
            timeout: AUTOSTART_TIMEOUT,
        }
    };
    let starting = || out.note(out.tf(Msg::StartingDaemon, &[&root.to_posix_string()]));
    match local::connect_or_start(root, &config(), autostart, starting).await {
        Ok(client) => Ok(client),
        Err(LocalError::Down(_)) if no_daemon => Err(CliError::io(
            out.tf(Msg::DaemonNotRunning, &[&root.to_posix_string()]),
        )),
        Err(err) => Err(localize(err, root, out)),
    }
}

/// Whether the API of `root` answers right now.
pub async fn is_running(root: &ForgeRoot) -> bool {
    local::is_running(root, &config()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::Lang;

    #[tokio::test]
    async fn a_root_without_daemon_is_down_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        assert!(try_connect(&root).await.unwrap_err().is_down());
        assert!(!is_running(&root).await);
    }

    #[tokio::test]
    async fn no_daemon_flag_never_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let out = Output::new(false, true, Lang::PtBr);
        let err = connect_or_start(&root, true, &out).await.unwrap_err();
        assert_eq!(err.exit, 1);
        assert!(err.message.contains("daemon"), "{}", err.message);
    }

    #[test]
    fn start_failures_are_localised_with_stable_codes() {
        let root = ForgeRoot::new(r"C:\Forgebox").unwrap();
        let pt = Output::new(false, true, Lang::PtBr);
        let en = Output::new(false, true, Lang::EnUs);

        let missing = localize(LocalError::DaemonNotFound, &root, &pt);
        assert_eq!((missing.code.as_str(), missing.exit), ("not_found", 3));
        assert!(missing.message.contains("forgeboxd"), "{}", missing.message);
        assert!(
            missing.message.contains("não encontrado"),
            "{}",
            missing.message
        );
        let missing = localize(LocalError::DaemonNotFound, &root, &en);
        assert!(missing.message.contains("not found"), "{}", missing.message);

        let exited = localize(LocalError::Exited { code: 1 }, &root, &en);
        assert_eq!(exited.code, "io");
        assert!(exited.message.contains("code 1"), "{}", exited.message);

        let timeout = localize(
            LocalError::Timeout {
                timeout: AUTOSTART_TIMEOUT,
                last: forge_ipc::ClientError::Disconnected,
            },
            &root,
            &pt,
        );
        assert_eq!(timeout.code, "timeout");
        assert!(timeout.message.contains("10 s"), "{}", timeout.message);
    }
}
