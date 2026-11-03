//! Ending the process that holds a port: the "Encerrar" of a port conflict
//! (`services.kill_port_holder`, docs/07 §1.2, docs/11 §4, ADR-0045).
//!
//! Terminating an arbitrary pid is the most dangerous thing the daemon does without the
//! helper, so the decision is split in two:
//!
//! * [`KillPolicy::check`] is pure: from the [`ProcessFacts`] of a process it refuses the
//!   daemon itself, every Forgebox process (the daemon's Job tree, executables under the
//!   root, `forgeboxd`/`Forgebox`/`forge`/helper names), critical processes, services,
//!   session 0, system accounts, other users' (or unreadable) processes and executables
//!   under the Windows folder. Unit-tested with crafted facts, since a test cannot (and
//!   must not) try to end a real service or `csrss.exe`.
//! * [`ProcessTerminator::terminate_checked`] opens the pid **once**, reads the facts from
//!   that handle, runs the caller's decision (the policy plus "does it still hold the
//!   port?") and terminates **through the same handle**. An open handle keeps the process
//!   object alive, so the pid cannot be recycled between the check and the kill.
//!
//! Every method is blocking (process and token queries, the service manager, waiting for
//! the exit): async code calls it from `spawn_blocking` with a timeout (ADR-0011).

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{ForgeError, Result};

/// Executables of Forgebox itself (lowercase file names), refused whatever their folder.
pub const FORGEBOX_EXE_NAMES: [&str; 5] = [
    "forgeboxd.exe",
    "forgebox.exe",
    "forge.exe",
    "forgebox-helper.exe",
    "forgebox-ctrl.exe",
];

/// Exit code given to a process ended by `services.kill_port_holder` (what Task Manager
/// uses for "End task" is 1 as well).
pub const KILL_EXIT_CODE: u32 = 1;

/// How long [`ProcessTerminator::terminate_checked`] waits for the process to be gone
/// (its sockets closed) after `TerminateProcess`.
pub const KILL_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The account a process runs as, seen from the daemon's user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessAccount {
    /// The same user as the daemon.
    CurrentUser,
    /// `LocalSystem`, `LocalService` or `NetworkService`.
    System,
    /// Somebody else.
    OtherUser,
    /// The token could not be read (protected process, access denied).
    Unknown,
}

/// What the OS says about a process, read through one handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessFacts {
    pub pid: u32,
    /// Full path of the executable, when the OS tells it.
    pub exe_path: Option<PathBuf>,
    pub account: ProcessAccount,
    /// Terminal Services session (0 = services and system processes).
    pub session_id: Option<u32>,
    /// Marked critical (`IsProcessCritical`): ending it bug-checks the machine.
    pub critical: bool,
    /// Names of the running services hosted by the process.
    pub services: Vec<String>,
}

impl ProcessFacts {
    /// A plain process of the current user (tests start from it).
    pub fn user_process(pid: u32, exe: impl Into<PathBuf>) -> Self {
        Self {
            pid,
            exe_path: Some(exe.into()),
            account: ProcessAccount::CurrentUser,
            session_id: Some(1),
            critical: false,
            services: Vec::new(),
        }
    }

    /// The executable for messages.
    pub fn exe_display(&self) -> String {
        self.exe_path
            .as_deref()
            .map(crate::paths::posix_string)
            .unwrap_or_else(|| format!("PID {}", self.pid))
    }
}

/// Which processes `services.kill_port_holder` refuses to end (ADR-0045).
#[derive(Debug, Clone, Default)]
pub struct KillPolicy {
    /// The daemon's own pid.
    pub daemon_pid: u32,
    /// The Forgebox root: executables below it are Forgebox's (services, pools, tools).
    pub forgebox_root: Option<PathBuf>,
    /// Processes of the daemon's Job tree (services, pools, terminals, jobs).
    pub forgebox_pids: BTreeSet<u32>,
    /// The Windows folder (`%SystemRoot%`): executables below it are system ones.
    pub system_root: Option<PathBuf>,
}

impl KillPolicy {
    /// The policy of the daemon running now: its pid, `root`, `forgebox_pids` and
    /// `%SystemRoot%`.
    pub fn for_daemon(root: &Path, forgebox_pids: impl IntoIterator<Item = u32>) -> Self {
        Self {
            daemon_pid: std::process::id(),
            forgebox_root: Some(root.to_path_buf()),
            forgebox_pids: forgebox_pids.into_iter().collect(),
            system_root: std::env::var_os("SystemRoot").map(PathBuf::from),
        }
    }

    /// The refusals known from the pid alone (the kernel, the daemon, the Job tree),
    /// before any process is opened.
    pub fn check_pid(&self, pid: u32) -> Result<()> {
        let refuse = |why: &str| Err(ForgeError::Forbidden(format!("PID {pid} {why}")));
        if pid == 0 || pid == 4 {
            return refuse("is the Windows kernel");
        }
        if pid == self.daemon_pid {
            return refuse("is the Forgebox daemon itself");
        }
        if self.forgebox_pids.contains(&pid) {
            return refuse("is managed by Forgebox: stop it with `forge stop` instead");
        }
        Ok(())
    }

    /// `Ok` when the process may be ended; `forbidden` with the reason otherwise.
    pub fn check(&self, facts: &ProcessFacts) -> Result<()> {
        self.check_pid(facts.pid)?;
        let who = facts.exe_display();
        let refuse = |why: &str| {
            Err(ForgeError::Forbidden(format!(
                "{who} (PID {}) {why}",
                facts.pid
            )))
        };
        let exe_name = facts
            .exe_path
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().to_ascii_lowercase());
        if exe_name
            .as_deref()
            .is_some_and(|n| FORGEBOX_EXE_NAMES.contains(&n))
        {
            return refuse("is a Forgebox program");
        }
        if let (Some(exe), Some(root)) = (&facts.exe_path, &self.forgebox_root) {
            if is_within(exe, root) {
                return refuse(
                    "runs from the Forgebox folder: stop it with `forge stop` or restart the daemon",
                );
            }
        }
        if facts.critical {
            return refuse("is a critical system process");
        }
        if !facts.services.is_empty() {
            return refuse(&format!(
                "hosts the Windows service {}: stop it in services.msc",
                facts.services.join(", ")
            ));
        }
        if facts.session_id == Some(0) {
            return refuse("runs in session 0 (a service or system process)");
        }
        match facts.account {
            ProcessAccount::CurrentUser => {}
            ProcessAccount::System => return refuse("runs as a system account"),
            ProcessAccount::OtherUser => return refuse("belongs to another user"),
            ProcessAccount::Unknown => {
                return refuse("cannot be inspected (protected or another user's process)");
            }
        }
        if let (Some(exe), Some(windows)) = (&facts.exe_path, &self.system_root) {
            if is_within(exe, windows) {
                return refuse("is a Windows program");
            }
        }
        if facts.exe_path.is_none() {
            return refuse("has no readable executable path");
        }
        Ok(())
    }
}

/// Lowercase normal components of `path` (`\\?\` and the drive prefix kept as text).
fn key(path: &Path) -> Vec<String> {
    crate::paths::plain_path(path)
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

/// `path` is `base` or below it, ignoring case (Windows paths).
fn is_within(path: &Path, base: &Path) -> bool {
    let (path, base) = (key(path), key(base));
    !base.is_empty() && path.len() >= base.len() && path[..base.len()] == base[..]
}

/// Ends processes after a last check made on the same process object.
pub trait ProcessTerminator: Send + Sync + fmt::Debug {
    /// Opens `pid`, reads its [`ProcessFacts`], calls `decide` with them and, when it
    /// answers `Ok`, terminates that same process with `exit_code` and waits up to
    /// [`KILL_WAIT`] for it to be gone. Returns the facts.
    ///
    /// Errors: `not_found` when no such process exists, `forbidden` when it cannot be
    /// opened for termination, whatever `decide` returns, `io` when the termination fails.
    fn terminate_checked(
        &self,
        pid: u32,
        exit_code: u32,
        decide: &mut dyn FnMut(&ProcessFacts) -> Result<()>,
    ) -> Result<ProcessFacts>;
}

/// The terminator of the running OS.
#[cfg(windows)]
pub fn process_terminator() -> Arc<dyn ProcessTerminator> {
    Arc::new(super::windows::WindowsProcessTerminator)
}

/// The terminator of the running OS (stub until the Linux/macOS ports).
#[cfg(not(windows))]
pub fn process_terminator() -> Arc<dyn ProcessTerminator> {
    Arc::new(Unsupported)
}

#[cfg(not(windows))]
#[derive(Debug)]
struct Unsupported;

#[cfg(not(windows))]
impl ProcessTerminator for Unsupported {
    fn terminate_checked(
        &self,
        _: u32,
        _: u32,
        _: &mut dyn FnMut(&ProcessFacts) -> Result<()>,
    ) -> Result<ProcessFacts> {
        Err(ForgeError::UnsupportedPlatform(format!(
            "ending processes is not implemented on {}",
            std::env::consts::OS
        )))
    }
}

// Windows paths in the cases (drive letters, `\`, case-insensitive).
#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn policy() -> KillPolicy {
        KillPolicy {
            daemon_pid: 100,
            forgebox_root: Some(PathBuf::from(r"C:\Forgebox")),
            forgebox_pids: [200].into(),
            system_root: Some(PathBuf::from(r"C:\WINDOWS")),
        }
    }

    #[test]
    fn a_plain_user_process_may_be_ended() {
        let facts = ProcessFacts::user_process(4412, r"C:\xampp\mysql\bin\mysqld.exe");
        policy().check(&facts).unwrap();
    }

    #[test]
    fn forgebox_system_and_foreign_processes_are_refused() {
        type Change = fn(&mut ProcessFacts);
        let cases: [(&str, Change, &str); 14] = [
            ("kernel", |f| f.pid = 4, "kernel"),
            ("idle", |f| f.pid = 0, "kernel"),
            ("daemon", |f| f.pid = 100, "daemon itself"),
            ("job tree", |f| f.pid = 200, "managed by Forgebox"),
            (
                "gui",
                |f| f.exe_path = Some(r"D:\Apps\Forgebox\Forgebox.exe".into()),
                "Forgebox program",
            ),
            (
                "cli",
                |f| f.exe_path = Some(r"D:\tools\FORGE.EXE".into()),
                "Forgebox program",
            ),
            (
                "under the root",
                |f| f.exe_path = Some(r"c:\forgebox\bin\nginx\1.27.4\nginx.exe".into()),
                "Forgebox folder",
            ),
            ("critical", |f| f.critical = true, "critical"),
            (
                "service",
                |f| f.services = vec!["MySQL80".into()],
                "service MySQL80",
            ),
            ("session 0", |f| f.session_id = Some(0), "session 0"),
            ("system", |f| f.account = ProcessAccount::System, "system"),
            (
                "other user",
                |f| f.account = ProcessAccount::OtherUser,
                "another user",
            ),
            (
                "unknown",
                |f| f.account = ProcessAccount::Unknown,
                "cannot be inspected",
            ),
            (
                "windows folder",
                |f| f.exe_path = Some(r"C:\Windows\System32\svchost.exe".into()),
                "Windows program",
            ),
        ];
        for (what, change, reason) in cases {
            let mut facts = ProcessFacts::user_process(4412, r"C:\xampp\mysql\bin\mysqld.exe");
            change(&mut facts);
            let err = policy().check(&facts).unwrap_err();
            assert_eq!(err.code(), "forbidden", "{what}");
            assert!(err.to_string().contains(reason), "{what}: {err}");
        }
        let mut unnamed = ProcessFacts::user_process(4412, "x");
        unnamed.exe_path = None;
        assert_eq!(policy().check(&unnamed).unwrap_err().code(), "forbidden");
    }

    #[test]
    fn a_sibling_folder_of_the_root_is_not_the_root() {
        let facts = ProcessFacts::user_process(4412, r"C:\Forgebox-old\bin\nginx.exe");
        policy().check(&facts).unwrap();
        assert!(is_within(
            Path::new(r"\\?\C:\Forgebox\bin\x.exe"),
            Path::new(r"C:\forgebox")
        ));
        assert!(!is_within(Path::new(r"C:\x.exe"), Path::new("")));
    }

    #[test]
    fn the_daemon_policy_knows_this_process() {
        let policy = KillPolicy::for_daemon(Path::new(r"C:\Forgebox"), [7]);
        assert_eq!(policy.daemon_pid, std::process::id());
        assert!(policy.forgebox_pids.contains(&7));
    }
}
