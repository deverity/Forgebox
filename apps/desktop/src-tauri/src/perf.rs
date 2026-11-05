//! Startup measurement (docs/07 §10: first frame < 800 ms; cold-start definition of
//! ADR-0036): from the OS creation time of this process to the `ready` command, which the
//! webview sends after a double `requestAnimationFrame` following the first shell commit
//! (the window is shown at that moment).
//!
//! * `perf_startup` (command) returns the marks for the QA harness;
//! * `Forgebox.exe --perf-startup` prints one JSON line (`{"first_frame_ms": …}` plus
//!   context) on stdout when `ready` arrives and exits 0, or prints `first_frame_ms: null`
//!   with an `error` and exits 1 when `ready` never comes. Stdout must be redirected
//!   (pipe/file): a GUI-subsystem process has no console of its own. With another
//!   Forgebox GUI already running the launch only focuses it (single instance) and prints
//!   nothing.
//!
//! The creation time comes from `forge_ipc::base::platform::process_creation_time`
//! (Win32 stays in forge-core, ADR-0035).

use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// How long `--perf-startup` waits for `ready` before giving up (exit 1).
pub const PERF_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Process creation time and the `ready` mark.
#[derive(Debug)]
pub struct StartupClock {
    created: Option<SystemTime>,
    /// Fallback origin when the creation time is unknown: the first line of `main`.
    started: SystemTime,
    ready: OnceLock<SystemTime>,
}

impl StartupClock {
    /// Reads the creation time of the current process. Blocking but fast (one
    /// `OpenProcess` + `GetProcessTimes`).
    pub fn for_current_process() -> Self {
        let started = SystemTime::now();
        let created = forge_ipc::base::platform::process_creation_time(std::process::id()).ok();
        Self::new(created, started)
    }

    pub fn new(created: Option<SystemTime>, started: SystemTime) -> Self {
        Self {
            created,
            started,
            ready: OnceLock::new(),
        }
    }

    /// Records `ready` at `now`; only the first call counts. Returns whether it was the
    /// first.
    pub fn mark_ready(&self, now: SystemTime) -> bool {
        self.ready.set(now).is_ok()
    }

    pub fn is_ready(&self) -> bool {
        self.ready.get().is_some()
    }

    /// The marks so far.
    pub fn report(&self) -> PerfReport {
        let origin = self.created.unwrap_or(self.started);
        let ready = self.ready.get().copied();
        PerfReport {
            first_frame_ms: ready.map(|r| elapsed_ms(origin, r)),
            origin: if self.created.is_some() {
                "process_created"
            } else {
                "main"
            },
            process_created_unix_ms: self.created.map(unix_ms),
            ready_unix_ms: ready.map(unix_ms),
        }
    }
}

/// What `perf_startup` answers and `--perf-startup` prints (plus [`PerfLine`] context).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PerfReport {
    /// Milliseconds from the origin to `ready`; `None` before `ready`.
    pub first_frame_ms: Option<u64>,
    /// `process_created` (OS creation time) or `main` (creation time unavailable).
    pub origin: &'static str,
    pub process_created_unix_ms: Option<u64>,
    pub ready_unix_ms: Option<u64>,
}

/// The JSON line of `--perf-startup`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PerfLine {
    #[serde(flatten)]
    pub report: PerfReport,
    /// `rdp` or `console` (the quiet-machine rule asks for it).
    pub session: &'static str,
    pub os_build: u32,
    /// Window material in effect (`mica`, `acrylic`, `none`).
    pub effect: &'static str,
    pub version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'static str>,
}

impl PerfLine {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"first_frame_ms\":null}".to_owned())
    }
}

/// Writes `line` and a newline to stdout and flushes; errors are ignored (no console).
pub fn print_line(line: &PerfLine) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", line.to_json());
    let _ = out.flush();
}

fn elapsed_ms(from: SystemTime, to: SystemTime) -> u64 {
    to.duration_since(from)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn unix_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// The startup marks so far (QA harness, diagnostics).
#[tauri::command]
pub async fn perf_startup(
    shell: tauri::State<'_, std::sync::Arc<crate::window::WindowShell>>,
) -> Result<PerfReport, ()> {
    Ok(shell.clock().report())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(ms)
    }

    #[test]
    fn first_frame_is_creation_to_the_first_ready() {
        let clock = StartupClock::new(Some(at(1_000)), at(1_050));
        assert_eq!(clock.report().first_frame_ms, None);
        assert!(!clock.is_ready());
        assert!(clock.mark_ready(at(1_640)));
        assert!(
            !clock.mark_ready(at(9_000)),
            "a reload does not move the mark"
        );
        let report = clock.report();
        assert_eq!(report.first_frame_ms, Some(640));
        assert_eq!(report.origin, "process_created");
        assert_eq!(report.process_created_unix_ms, Some(1_000));
        assert_eq!(report.ready_unix_ms, Some(1_640));
    }

    #[test]
    fn without_a_creation_time_main_is_the_origin() {
        let clock = StartupClock::new(None, at(2_000));
        clock.mark_ready(at(2_300));
        let report = clock.report();
        assert_eq!(report.first_frame_ms, Some(300));
        assert_eq!(report.origin, "main");
        assert_eq!(report.process_created_unix_ms, None);
    }

    #[test]
    fn a_clock_going_backwards_reads_zero() {
        let clock = StartupClock::new(Some(at(5_000)), at(5_000));
        clock.mark_ready(at(4_000));
        assert_eq!(clock.report().first_frame_ms, Some(0));
    }

    #[test]
    fn the_perf_line_is_one_json_object() {
        let clock = StartupClock::new(Some(at(1_000)), at(1_000));
        clock.mark_ready(at(1_512));
        let line = PerfLine {
            report: clock.report(),
            session: "rdp",
            os_build: 19045,
            effect: "none",
            version: "0.1.0",
            error: None,
        };
        let json: serde_json::Value = serde_json::from_str(&line.to_json()).unwrap();
        assert_eq!(json["first_frame_ms"], 512);
        assert_eq!(json["session"], "rdp");
        assert_eq!(json["effect"], "none");
        assert!(json.get("error").is_none());
        assert!(!line.to_json().contains('\n'));

        let timeout = PerfLine {
            report: StartupClock::new(None, at(0)).report(),
            error: Some("ready_timeout"),
            ..line
        };
        let json: serde_json::Value = serde_json::from_str(&timeout.to_json()).unwrap();
        assert!(json["first_frame_ms"].is_null());
        assert_eq!(json["error"], "ready_timeout");
    }

    #[test]
    fn the_current_process_has_a_creation_time() {
        let clock = StartupClock::for_current_process();
        if cfg!(windows) {
            assert!(clock.created.is_some());
            assert!(clock.created.unwrap() <= clock.started);
        }
    }
}
