//! `daemon.*` (`daemon.status` is a server built-in): stop, restart, doctor.

use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::platform::VcRedistFlavor;
use forge_ipc::api::methods::{daemon_doctor, daemon_restart, daemon_stop};
use forge_ipc::api::{
    Ack, CheckDto, CheckStatus, DaemonStopParams, DoctorParams, DoctorReportDto, JobDto, NoParams,
};
use forge_ipc::server::{Ctx, Router};

use super::route;
use crate::daemon::{Daemon, ShutdownRequest};

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, daemon_stop, stop);
    route!(router, daemon, daemon_restart, restart);
    route!(router, daemon, daemon_doctor, doctor);
}

async fn stop(daemon: Daemon, params: DaemonStopParams, _ctx: Ctx) -> Result<Ack> {
    if params.keep_services {
        return Err(ForgeError::NotImplemented(
            "keep_services: services live in the daemon's Job Object and die with it; \
             handing them over to a new daemon is planned with the self-update handoff \
             (docs/05 §5, M4-E1)"
                .into(),
        ));
    }
    daemon.shutdown.request(ShutdownRequest {
        reason: "user".into(),
        restart: false,
        keep_services: false,
    });
    Ok(Ack::with_message("daemon stopping"))
}

async fn restart(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<Ack> {
    daemon.shutdown.request(ShutdownRequest {
        reason: "restart".into(),
        restart: true,
        keep_services: false,
    });
    Ok(Ack::with_message("daemon restarting"))
}

fn wanted(filters: &[String], id: &str) -> bool {
    filters.is_empty() || filters.iter().any(|f| id.starts_with(f.as_str()))
}

fn check(id: &str, status: CheckStatus, message: impl Into<String>, fix: Option<&str>) -> CheckDto {
    CheckDto {
        id: id.to_owned(),
        status,
        message: message.into(),
        fix: fix.map(str::to_owned),
        fix_method: None,
        fix_params: None,
    }
}

/// Runs the diagnostics (each bounded) and returns them; a check that cannot run is
/// `skipped`, never an error of the job.
pub async fn run_checks(daemon: &Daemon, filters: &[String]) -> Vec<CheckDto> {
    let mut out = Vec::new();
    if wanted(filters, "root.writable") {
        let tmp = daemon.root.tmp();
        let result = forge_core::runtime::spawn_blocking_timeout(
            "root check",
            Duration::from_secs(5),
            move || {
                let probe = tmp.join(format!(".doctor-{}", std::process::id()));
                std::fs::write(&probe, b"ok")
                    .map_err(|e| ForgeError::io_at("writing", &probe, e))?;
                let _ = std::fs::remove_file(&probe);
                Ok(())
            },
        )
        .await;
        out.push(match result {
            Ok(()) => check(
                "root.writable",
                CheckStatus::Ok,
                format!("{} is writable", daemon.root),
                None,
            ),
            Err(err) => check("root.writable", CheckStatus::Fail, err.to_string(), None),
        });
    }
    if wanted(filters, "store.schema") {
        let latest = forge_core::store::migrations::latest_version();
        let current = daemon
            .reader
            .query(forge_core::store::migrations::current_version)
            .await;
        out.push(match current {
            Ok(v) if v == latest => check(
                "store.schema",
                CheckStatus::Ok,
                format!("schema v{v}"),
                None,
            ),
            Ok(v) => check(
                "store.schema",
                CheckStatus::Warn,
                format!("schema v{v}, build knows v{latest}"),
                None,
            ),
            Err(err) => check("store.schema", CheckStatus::Fail, err.to_string(), None),
        });
    }
    if wanted(filters, "dns") {
        let config = daemon.config();
        match daemon.dns.stats().await {
            Ok(stats) if stats.running => out.push(check(
                "dns.server",
                CheckStatus::Ok,
                format!(
                    "resolver on {}",
                    stats.listen.map(|a| a.to_string()).unwrap_or_default()
                ),
                None,
            )),
            Ok(stats) => out.push(check(
                "dns.server",
                if config.dns.enabled {
                    CheckStatus::Fail
                } else {
                    CheckStatus::Skipped
                },
                stats
                    .last_error
                    .unwrap_or_else(|| "resolver not running".into()),
                Some("forge dns enable"),
            )),
            Err(err) => out.push(check(
                "dns.server",
                CheckStatus::Fail,
                err.to_string(),
                None,
            )),
        }
        let nameserver = daemon
            .dns
            .stats()
            .await
            .ok()
            .and_then(|s| s.listen)
            .filter(|a| a.port() == 53)
            .map(|a| a.ip());
        match tokio::time::timeout(
            Duration::from_secs(5),
            daemon.dns_policy.status(&config.tlds(), nameserver),
        )
        .await
        {
            Ok(Ok(states)) => {
                for state in states {
                    let ok = !matches!(state.state, forge_net::PolicyState::NotApplied);
                    let mut detail = format!(
                        "{} → {}",
                        state.tld,
                        match state.state.mode() {
                            forge_net::PolicyMode::Nrpt => "nrpt",
                            forge_net::PolicyMode::Hosts => "hosts",
                            forge_net::PolicyMode::None => "none",
                        }
                    );
                    if let Some(reason) = state.reason {
                        detail.push_str(&format!(" ({})", reason.as_str()));
                    }
                    out.push(check(
                        &format!("dns.policy{}", state.tld),
                        if ok {
                            CheckStatus::Ok
                        } else {
                            CheckStatus::Warn
                        },
                        detail,
                        (!ok).then_some("forge dns enable"),
                    ));
                }
            }
            Ok(Err(err)) => out.push(check(
                "dns.policy",
                CheckStatus::Skipped,
                err.to_string(),
                None,
            )),
            Err(_) => out.push(check("dns.policy", CheckStatus::Skipped, "timed out", None)),
        }
    }
    if wanted(filters, "tls") {
        match daemon.tls.status().await {
            Ok(status) => {
                out.push(match &status.ca {
                    Some(ca) => check("tls.ca", CheckStatus::Ok, ca.subject.clone(), None),
                    None => check(
                        "tls.ca",
                        CheckStatus::Warn,
                        "no local CA yet",
                        Some("forge tls ca install"),
                    ),
                });
                if status.ca.is_some() {
                    let trusted = status.trusted_current_user || status.trusted_local_machine;
                    out.push(check(
                        "tls.ca_trusted",
                        if trusted {
                            CheckStatus::Ok
                        } else {
                            CheckStatus::Warn
                        },
                        if trusted {
                            "trusted by Windows"
                        } else {
                            "not trusted by Windows"
                        },
                        (!trusted).then_some("forge tls ca install"),
                    ));
                }
            }
            Err(err) => out.push(check("tls.ca", CheckStatus::Fail, err.to_string(), None)),
        }
    }
    if wanted(filters, "port") {
        let table = std::sync::Arc::clone(daemon.platform.port_table());
        let listeners = forge_core::runtime::spawn_blocking_timeout(
            "listeners",
            Duration::from_secs(5),
            move || table.listeners(),
        )
        .await
        .unwrap_or_default();
        let config = daemon.config();
        let web: Vec<u16> = config
            .ports
            .range("web")
            .map(|r| r.ports().collect())
            .unwrap_or_default();
        let tree = daemon.job_tree.clone();
        let ours = forge_core::runtime::spawn_blocking_timeout(
            "pids",
            Duration::from_secs(5),
            move || tree.pids(),
        )
        .await
        .unwrap_or_default();
        for port in web {
            let holder = listeners
                .iter()
                .find(|l| l.port == port && !ours.contains(&l.pid));
            out.push(match holder {
                None => check(
                    &format!("port.{port}"),
                    CheckStatus::Ok,
                    format!("{port} free or ours"),
                    None,
                ),
                Some(h) => check(
                    &format!("port.{port}"),
                    CheckStatus::Warn,
                    format!(
                        "{port} in use by PID {} ({})",
                        h.pid,
                        h.exe_path
                            .as_deref()
                            .map(forge_core::paths::posix_string)
                            .unwrap_or_else(|| "?".into())
                    ),
                    Some("forge service ports"),
                ),
            });
        }
    }
    if wanted(filters, "runtime.vcredist") {
        let deps = std::sync::Arc::clone(daemon.platform.runtime_deps());
        let version = forge_core::runtime::spawn_blocking_timeout(
            "vcredist",
            Duration::from_secs(5),
            move || deps.vcredist_version(),
        )
        .await;
        out.push(match version {
            Ok(Some(v)) if v.satisfies(VcRedistFlavor::Vs17) => check(
                "runtime.vcredist",
                CheckStatus::Ok,
                format!("VC++ {v}"),
                None,
            ),
            Ok(Some(v)) => check(
                "runtime.vcredist",
                CheckStatus::Warn,
                format!("VC++ {v} is older than VS 2022"),
                Some(forge_core::platform::VCREDIST_X64_URL),
            ),
            Ok(None) => check(
                "runtime.vcredist",
                CheckStatus::Warn,
                "VC++ Redistributable not installed",
                Some(forge_core::platform::VCREDIST_X64_URL),
            ),
            Err(err) => check(
                "runtime.vcredist",
                CheckStatus::Skipped,
                err.to_string(),
                None,
            ),
        });
    }
    if wanted(filters, "path") {
        out.push(path_check(daemon).await);
    }
    if wanted(filters, "defender") {
        out.push(defender_check(daemon).await);
    }
    if wanted(filters, "versions") {
        out.push(versions_check(daemon).await);
    }
    if wanted(filters, "registry") {
        for info in daemon.registry.list() {
            out.push(match &info.last_error {
                None => check(
                    &format!("registry.{}", info.name),
                    CheckStatus::Ok,
                    format!("{} tools", info.tools),
                    None,
                ),
                Some(err) => check(
                    &format!("registry.{}", info.name),
                    CheckStatus::Warn,
                    err.clone(),
                    Some("forge registry update"),
                ),
            });
        }
    }
    if wanted(filters, "service") {
        for (entry, status) in daemon.services.statuses().await {
            let id = format!("service.{}", entry.label());
            out.push(match (&entry.error, status) {
                (Some(err), _) => check(&id, CheckStatus::Fail, err.clone(), None),
                (None, Some(s))
                    if matches!(s.state, forge_supervisor::InstanceState::Failed { .. }) =>
                {
                    check(
                        &id,
                        CheckStatus::Fail,
                        format!("{:?}", s.state),
                        Some("forge logs"),
                    )
                }
                (None, Some(s)) => check(&id, CheckStatus::Ok, s.state.name(), None),
                (None, None) => check(&id, CheckStatus::Skipped, "no answer", None),
            });
        }
    }
    out
}

/// The user `PATH` has the Forgebox entries (portable mode never touches it).
async fn path_check(daemon: &Daemon) -> CheckDto {
    if daemon.root.is_portable() {
        return check(
            "path.user",
            CheckStatus::Skipped,
            "portable mode never changes the user PATH",
            None,
        );
    }
    match super::shell::snapshot(daemon).await {
        Ok(path) if path.missing.is_empty() => check(
            "path.user",
            CheckStatus::Ok,
            format!(
                "{} Forgebox entries in the user PATH",
                path.forgebox_entries.len()
            ),
            None,
        ),
        Ok(path) => check(
            "path.user",
            CheckStatus::Warn,
            format!(
                "{} Forgebox entries missing from the user PATH (new terminals will not find the tools)",
                path.missing.len()
            ),
            Some("forge path add"),
        ),
        Err(err) => check("path.user", CheckStatus::Skipped, err.to_string(), None),
    }
}

/// What `Get-MpPreference` says about the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefenderState {
    /// The root, or a folder above it, is excluded.
    Excluded,
    NotExcluded,
    /// Not an administrator (the list is hidden) or no Defender: cannot tell.
    Unreadable,
}

/// Reads the exclusion list printed one path per line.
pub fn defender_state(output: &str, root: &str) -> DefenderState {
    fn key(path: &str) -> String {
        path.trim()
            .trim_matches('"')
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    }
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.iter().any(|l| l.starts_with("N/A")) {
        return DefenderState::Unreadable;
    }
    let root = key(root);
    let excluded = lines
        .iter()
        .map(|l| key(l))
        .any(|e| !e.is_empty() && (root == e || root.starts_with(&format!("{e}/"))));
    if excluded {
        DefenderState::Excluded
    } else {
        DefenderState::NotExcluded
    }
}

/// Microsoft Defender exclusion of the root (`Get-MpPreference`), a big win for build and
/// extraction speed (docs/11).
async fn defender_check(daemon: &Daemon) -> CheckDto {
    let root = forge_core::paths::plain_path(daemon.root.path())
        .to_string_lossy()
        .into_owned();
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let exe = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let probe = forge_core::platform::ProbeCommand {
        exe,
        args: [
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-MpPreference).ExclusionPath",
        ]
        .map(Into::into)
        .to_vec(),
        cwd: daemon.root.tmp(),
    };
    let result = forge_core::runtime::spawn_blocking_timeout(
        "defender probe",
        Duration::from_secs(20),
        move || forge_core::platform::run_probe(&probe, Duration::from_secs(15)),
    )
    .await;
    match result {
        Ok(output) if output.exit_code == Some(0) => match defender_state(&output.stdout, &root) {
            DefenderState::Excluded => check(
                "defender.exclusion",
                CheckStatus::Ok,
                format!("{root} is excluded from Defender scanning"),
                None,
            ),
            DefenderState::NotExcluded => check(
                "defender.exclusion",
                CheckStatus::Warn,
                format!(
                    "{root} is not excluded from Defender real-time scanning (installs and builds are slower)"
                ),
                Some("Add-MpPreference -ExclusionPath <root> (administrator PowerShell)"),
            ),
            DefenderState::Unreadable => check(
                "defender.exclusion",
                CheckStatus::Skipped,
                "the exclusion list is only visible to administrators",
                None,
            ),
        },
        Ok(output) => check(
            "defender.exclusion",
            CheckStatus::Skipped,
            format!(
                "Get-MpPreference failed (exit {:?}): Defender is probably not the active antivirus",
                output.exit_code
            ),
            None,
        ),
        Err(err) => check(
            "defender.exclusion",
            CheckStatus::Skipped,
            err.to_string(),
            None,
        ),
    }
}

/// Versions of the daemon, the API and the installed tools.
async fn versions_check(daemon: &Daemon) -> CheckDto {
    let installed = crate::catalog::all_installed(&daemon.reader)
        .await
        .map(|rows| rows.len());
    let api = forge_ipc::api::API_VERSION;
    match installed {
        Ok(count) => check(
            "versions.daemon",
            CheckStatus::Ok,
            format!(
                "forgeboxd {}, API {api}, {count} tool versions installed",
                env!("CARGO_PKG_VERSION")
            ),
            None,
        ),
        Err(err) => check(
            "versions.daemon",
            CheckStatus::Skipped,
            err.to_string(),
            None,
        ),
    }
}

async fn doctor(daemon: Daemon, params: DoctorParams, ctx: Ctx) -> Result<JobDto> {
    let d = daemon.clone();
    daemon.job(&ctx, "daemon.doctor", move |_job| async move {
        Ok(DoctorReportDto {
            checks: run_checks(&d, &params.checks).await,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defender_exclusions_match_the_root_or_a_parent() {
        let root = r"C:\Forgebox";
        assert_eq!(
            defender_state("C:\\Forgebox\nD:\\other\n", root),
            DefenderState::Excluded
        );
        assert_eq!(
            defender_state("c:\\FORGEBOX\\\n", root),
            DefenderState::Excluded
        );
        assert_eq!(defender_state("C:\\\n", root), DefenderState::Excluded);
        assert_eq!(
            defender_state("C:\\Forge\nC:\\Forgebox2\n", root),
            DefenderState::NotExcluded
        );
        assert_eq!(defender_state("", root), DefenderState::NotExcluded);
        assert_eq!(
            defender_state("N/A: Must be an administrator to view exclusions\n", root),
            DefenderState::Unreadable
        );
    }

    #[test]
    fn defender_nested_root_is_covered_by_a_parent_exclusion() {
        assert_eq!(
            defender_state("D:\\dev\n", r"D:\dev\forgebox"),
            DefenderState::Excluded
        );
        assert_eq!(
            defender_state("D:\\de\n", r"D:\dev\forgebox"),
            DefenderState::NotExcluded
        );
    }
}
