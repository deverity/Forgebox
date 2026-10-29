//! Human renderers of the DTOs shared by several commands (`--json` prints the DTO as it
//! is; these functions are only for people).

use forge_ipc::api::{
    CaseSensitivityDto, CheckDto, CheckStatus, FileRefDto, InstanceDto, JobDto, JobState, PoolDto,
    PortDto, ServiceState, ServiceStatusDto, SiteDto, SiteKind, ToolVersionDto,
};
use forge_ipc::base::Timestamp;

use crate::messages::Msg;
use crate::output::{
    CYAN, DIM, GREEN, Output, RED, YELLOW, format_bytes, format_duration, or_dash, paint,
};

/// A service state with its color.
pub fn state_cell(out: &Output, state: ServiceState) -> String {
    let (word, style) = match state {
        ServiceState::Running => (Msg::StateRunning, GREEN),
        ServiceState::Starting => (Msg::StateStarting, YELLOW),
        ServiceState::Stopping => (Msg::StateStopping, YELLOW),
        ServiceState::Stopped => (Msg::StateStopped, DIM),
        ServiceState::Crashed => (Msg::StateCrashed, RED),
        ServiceState::Failed => (Msg::StateFailed, RED),
        ServiceState::Maintenance => (Msg::StateMaintenance, CYAN),
    };
    paint(style, out.t(word))
}

/// Seconds since `ts`, never negative.
pub fn age_secs(ts: Timestamp) -> u64 {
    u64::try_from(Timestamp::now().as_second() - ts.as_second()).unwrap_or(0)
}

/// `2026-10-27 12:00` (UTC).
pub fn time_cell(ts: Timestamp) -> String {
    ts.strftime("%Y-%m-%d %H:%M").to_string()
}

fn ports_cell(ports: &[PortDto]) -> String {
    if ports.is_empty() {
        return "-".into();
    }
    ports
        .iter()
        .map(|p| p.port.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn instance_row(out: &Output, i: &InstanceDto) -> Vec<String> {
    vec![
        format!("{}/{}", i.tool, i.name),
        i.version.clone(),
        state_cell(out, i.state),
        or_dash(i.pid),
        ports_cell(&i.ports),
        i.cpu_pct.map_or_else(|| "-".into(), |c| format!("{c:.1}%")),
        or_dash(i.rss_bytes.map(format_bytes)),
        i.started_at
            .filter(|_| i.state == ServiceState::Running)
            .map_or_else(|| "-".into(), |t| format_duration(age_secs(t))),
    ]
}

fn pool_row(out: &Output, p: &PoolDto) -> Vec<String> {
    vec![
        format!("php@{}", p.series),
        p.version.clone(),
        state_cell(out, p.state),
        "-".into(),
        if p.ports.is_empty() {
            "-".into()
        } else {
            p.ports
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(",")
        },
        "-".into(),
        or_dash(p.rss_bytes.map(format_bytes)),
        out.tf(Msg::PoolWorkers, &[&p.busy_workers, &p.workers]),
    ]
}

/// The table of `forge status`, `forge start|stop|restart`.
pub fn service_table(out: &Output, status: &ServiceStatusDto) {
    let mut rows: Vec<Vec<String>> = status
        .instances
        .iter()
        .map(|i| instance_row(out, i))
        .collect();
    rows.extend(status.pools.iter().map(|p| pool_row(out, p)));
    out.table(
        &[
            out.t(Msg::HName),
            out.t(Msg::HVersion),
            out.t(Msg::HState),
            "PID",
            out.t(Msg::HPorts),
            "CPU",
            "RAM",
            out.t(Msg::HUptime),
        ],
        rows,
        Msg::NoServices,
    );
}

/// The table of `forge site list`.
pub fn site_table(out: &Output, sites: &[SiteDto]) {
    let rows = sites
        .iter()
        .map(|s| {
            vec![
                s.host.clone(),
                site_kind(out, s.kind).to_owned(),
                site_php(s),
                s.server
                    .as_ref()
                    .map_or_else(|| "-".into(), |t| t.to_string()),
                if s.secure {
                    paint(GREEN, "https")
                } else {
                    "http".into()
                },
                if s.case_sensitive {
                    out.t(Msg::Yes).to_owned()
                } else {
                    out.t(Msg::No).to_owned()
                },
                s.root
                    .clone()
                    .or_else(|| s.upstream.clone())
                    .unwrap_or_else(|| "-".into()),
            ]
        })
        .collect();
    out.table(
        &[
            "HOST",
            out.t(Msg::HType),
            "PHP",
            out.t(Msg::HServer),
            "URL",
            "CASE",
            out.t(Msg::HTarget),
        ],
        rows,
        Msg::NoSites,
    );
}

pub fn site_kind(out: &Output, kind: SiteKind) -> &'static str {
    match kind {
        SiteKind::Php => "php",
        SiteKind::Static => out.t(Msg::KindStatic),
        SiteKind::Proxy => "proxy",
        SiteKind::App => "app",
    }
}

/// `8.3` (resolved `8.3.36`), `-` for non-PHP sites.
fn site_php(site: &SiteDto) -> String {
    match &site.php {
        Some(php) => php
            .resolved
            .clone()
            .or_else(|| php.exact.clone())
            .or_else(|| php.series.clone())
            .unwrap_or_else(|| "-".into()),
        None => "-".into(),
    }
}

/// One site, as `key  value` lines.
pub fn site_detail(out: &Output, site: &SiteDto) {
    let mut pairs = vec![
        ("Host", site.host.clone()),
        ("URL", site.url.clone()),
        (out.t(Msg::HType), site_kind(out, site.kind).to_owned()),
        (
            out.t(Msg::HTarget),
            site.root
                .clone()
                .or_else(|| site.upstream.clone())
                .unwrap_or_else(|| "-".into()),
        ),
        ("PHP", site_php(site)),
        ("Driver", or_dash(site.driver.as_deref())),
        (out.t(Msg::HServer), or_dash(site.server.as_ref())),
        ("HTTPS", yes_no(out, site.secure)),
        ("Case-sensitive", yes_no(out, site.case_sensitive)),
    ];
    if !site.aliases.is_empty() {
        pairs.push(("Aliases", site.aliases.join(", ")));
    }
    out.pairs(&pairs);
}

pub fn yes_no(out: &Output, value: bool) -> String {
    out.t(if value { Msg::Yes } else { Msg::No }).to_owned()
}

/// One diagnostic check per line, with the suggested fix underneath.
pub fn checks(out: &Output, checks: &[CheckDto]) {
    for check in checks {
        let (tag, style) = match check.status {
            CheckStatus::Ok => ("ok", GREEN),
            CheckStatus::Warn => ("!!", YELLOW),
            CheckStatus::Fail => ("xx", RED),
            CheckStatus::Skipped => ("--", DIM),
        };
        out.line(format!(
            "[{}] {}  {}",
            paint(style, tag),
            paint(crate::output::BOLD, &check.id),
            check.message
        ));
        if let Some(fix) = &check.fix {
            out.line(format!("     {}", paint(DIM, format!("-> {fix}"))));
        }
    }
}

/// Counts of a check list: (ok, warnings, failures).
pub fn check_totals(checks: &[CheckDto]) -> (usize, usize, usize) {
    let count = |s: CheckStatus| checks.iter().filter(|c| c.status == s).count();
    (
        count(CheckStatus::Ok),
        count(CheckStatus::Warn),
        count(CheckStatus::Fail),
    )
}

/// Files of a config listing.
pub fn files(out: &Output, files: &[FileRefDto]) {
    let rows = files
        .iter()
        .map(|f| {
            vec![
                f.role.clone(),
                f.path.clone(),
                if f.managed {
                    out.t(Msg::Managed).to_owned()
                } else {
                    "-".into()
                },
            ]
        })
        .collect();
    out.table(
        &[out.t(Msg::HRole), out.t(Msg::HPath), out.t(Msg::HOrigin)],
        rows,
        Msg::NoFiles,
    );
}

/// An installed version, one line.
pub fn version_line(v: &ToolVersionDto) -> String {
    let current = if v.is_current { " (current)" } else { "" };
    format!("{} {}{current}", v.tool, v.canonical_name)
}

/// The table of `forge jobs`.
pub fn job_table(out: &Output, jobs: &[JobDto]) {
    let rows = jobs
        .iter()
        .map(|j| {
            let state = match j.state {
                JobState::Running => paint(YELLOW, out.t(Msg::JobRunning)),
                JobState::Done => paint(GREEN, out.t(Msg::JobDone)),
                JobState::Failed => paint(RED, out.t(Msg::JobFailed)),
                JobState::Cancelled => paint(DIM, out.t(Msg::JobCancelled)),
            };
            vec![
                j.id.to_string(),
                j.name.clone(),
                state,
                j.progress
                    .pct
                    .map_or_else(|| "-".into(), |p| format!("{p:.0}%")),
                time_cell(j.started_at),
            ]
        })
        .collect();
    out.table(
        &[
            "ID",
            out.t(Msg::HName),
            out.t(Msg::HState),
            "%",
            out.t(Msg::HStarted),
        ],
        rows,
        Msg::NoJobs,
    );
}

/// Case-sensitivity of a directory, one line.
pub fn case_status(out: &Output, status: &CaseSensitivityDto) {
    let state = match (status.supported, status.enabled) {
        (false, _) => paint(DIM, out.t(Msg::CaseUnsupported)),
        (true, Some(true)) => paint(GREEN, out.t(Msg::CaseOn)),
        (true, Some(false)) => paint(YELLOW, out.t(Msg::CaseOff)),
        (true, None) => out.t(Msg::CaseUnknown).to_owned(),
    };
    out.line(format!("{}  {}", status.dir, state));
    if let Some(reason) = &status.reason {
        out.line(format!("  {}", paint(DIM, reason)));
    }
    if let Some(fs) = &status.fs_type {
        out.line(format!("  {}", paint(DIM, format!("fs: {fs}"))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::Lang;
    use crate::output::strip_ansi;

    #[test]
    fn totals_count_each_status() {
        let c = |status| CheckDto {
            id: "x".into(),
            status,
            message: String::new(),
            fix: None,
            fix_method: None,
            fix_params: None,
        };
        let list = [
            c(CheckStatus::Ok),
            c(CheckStatus::Ok),
            c(CheckStatus::Warn),
            c(CheckStatus::Fail),
            c(CheckStatus::Skipped),
        ];
        assert_eq!(check_totals(&list), (2, 1, 1));
    }

    #[test]
    fn states_have_words_in_both_languages() {
        for lang in [Lang::PtBr, Lang::EnUs] {
            let out = Output::new(false, false, lang);
            for state in [
                ServiceState::Running,
                ServiceState::Starting,
                ServiceState::Stopping,
                ServiceState::Stopped,
                ServiceState::Crashed,
                ServiceState::Failed,
                ServiceState::Maintenance,
            ] {
                assert!(!strip_ansi(&state_cell(&out, state)).is_empty());
            }
        }
    }
}
