//! Logs of the daemon (docs/08 §2, docs/01 §2): `tracing` events as JSON lines in
//! `logs/forgeboxd.log.<YYYY-MM-DD>` (one file per day, the last [`KEEP_FILES`] kept,
//! written by a background thread so logging never blocks a worker), plus a human
//! format on stdout with `--foreground`. With the `console` feature the tokio-console
//! layer replaces both (development only, `--cfg tokio_unstable`).

use std::path::{Path, PathBuf};

use forge_core::error::{ForgeError, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Prefix of the daily files: `forgeboxd.log.2026-10-27`.
pub const LOG_PREFIX: &str = "forgeboxd.log";
/// Daily files kept.
pub const KEEP_FILES: usize = 14;

/// Keeps the file writer alive; dropping it flushes what is buffered.
pub struct LogGuard {
    _file: Option<WorkerGuard>,
}

impl std::fmt::Debug for LogGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogGuard").finish_non_exhaustive()
    }
}

fn filter(level: &str) -> Result<EnvFilter> {
    if let Ok(env) = EnvFilter::try_from_default_env() {
        return Ok(env);
    }
    EnvFilter::try_new(level)
        .map_err(|e| ForgeError::InvalidInput(format!("--log-level {level:?}: {e}")))
}

/// Installs the global subscriber. `logs` is `<root>/logs` (must exist).
#[cfg(not(feature = "console"))]
pub fn init(logs: &Path, level: &str, foreground: bool) -> Result<LogGuard> {
    let appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_PREFIX)
        .max_log_files(KEEP_FILES)
        .build(logs)
        .map_err(|e| ForgeError::Internal(format!("log file in {}: {e}", logs.display())))?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let file_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(false)
        .with_span_list(false)
        .with_ansi(false)
        .with_thread_names(true)
        .with_writer(writer);
    let stdout_layer = foreground.then(|| {
        tracing_subscriber::fmt::layer()
            .with_target(true)
            .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
            .with_writer(std::io::stdout)
    });
    tracing_subscriber::registry()
        .with(filter(level)?)
        .with(file_layer)
        .with(stdout_layer)
        .try_init()
        .map_err(|e| ForgeError::Internal(format!("installing the log subscriber: {e}")))?;
    Ok(LogGuard { _file: Some(guard) })
}

/// tokio-console build: the console layer (plus `RUST_LOG` on stdout) only.
#[cfg(feature = "console")]
pub fn init(_logs: &Path, _level: &str, _foreground: bool) -> Result<LogGuard> {
    forge_core::runtime::init_console();
    Ok(LogGuard { _file: None })
}

/// Daily log files of the daemon in `logs`, oldest first.
pub fn log_files(logs: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(logs)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with(LOG_PREFIX))
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_level_is_reported() {
        if std::env::var_os("RUST_LOG").is_none() {
            assert!(filter("info,forge_fcgi=debug").is_ok());
            assert_eq!(filter("[[").unwrap_err().code(), "invalid_input");
        }
    }

    #[test]
    fn log_files_are_sorted_and_filtered() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "forgeboxd.log.2026-10-27",
            "forgeboxd.log.2026-10-26",
            "helper.log",
        ] {
            std::fs::write(dir.path().join(name), "{}\n").unwrap();
        }
        let files = log_files(dir.path());
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["forgeboxd.log.2026-10-26", "forgeboxd.log.2026-10-27"]
        );
    }
}
