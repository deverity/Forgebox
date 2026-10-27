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
                    out.push(check(
                        &format!("dns.policy{}", state.tld),
                        if ok {
                            CheckStatus::Ok
                        } else {
                            CheckStatus::Warn
                        },
                        format!("{} → {:?}", state.tld, state.state),
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

async fn doctor(daemon: Daemon, params: DoctorParams, ctx: Ctx) -> Result<JobDto> {
    let d = daemon.clone();
    daemon.job(&ctx, "daemon.doctor", move |_job| async move {
        Ok(DoctorReportDto {
            checks: run_checks(&d, &params.checks).await,
        })
    })
}
