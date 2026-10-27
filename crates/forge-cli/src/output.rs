//! Everything the CLI prints: human text (tables, colors, progress) or `--json`.
//!
//! * Colors go through `anstream`, so they disappear when stdout is not a terminal, with
//!   `NO_COLOR` and with `TERM=dumb`, and `CLICOLOR_FORCE` turns them on for pipes.
//! * `--json` prints one document per command (the DTO of the API, serde-exact) or JSON
//!   lines for streams; errors are `{"error": {code, message, details}}` on stdout, so
//!   stdout is always valid JSON in this mode.
//! * `--quiet` keeps only the result: no status lines, no progress.

use std::io::{IsTerminal, Write};

use anstyle::{AnsiColor, Style};
use serde::Serialize;
use tabled::builder::Builder;
use tabled::settings::{Padding, Style as TableStyle};

use crate::error::CliError;
use crate::messages::{Lang, Msg, format_message};

pub const BOLD: Style = Style::new().bold();
pub const DIM: Style = Style::new().dimmed();
pub const GREEN: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)));
pub const YELLOW: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Yellow)));
pub const RED: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Red)));
pub const CYAN: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Cyan)));

/// Text wrapped in `style` (the escape codes are stripped by `anstream` when colors are
/// off).
pub fn paint(style: Style, text: impl std::fmt::Display) -> String {
    format!("{style}{text}{style:#}")
}

/// Output settings of one run.
#[derive(Debug, Clone, Copy)]
pub struct Output {
    pub json: bool,
    pub quiet: bool,
    pub lang: Lang,
}

/// Writes to stdout ignoring a closed pipe (`forge ... | head`).
fn write_stdout(text: &str) {
    let mut out = anstream::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn write_stderr(text: &str) {
    let mut err = anstream::stderr().lock();
    let _ = err.write_all(text.as_bytes());
    let _ = err.flush();
}

impl Output {
    pub fn new(json: bool, quiet: bool, lang: Lang) -> Self {
        Self { json, quiet, lang }
    }

    /// Human output on: not `--json`.
    pub fn human(&self) -> bool {
        !self.json
    }

    /// Whether stderr is an interactive terminal (progress bars are drawn only there).
    pub fn interactive(&self) -> bool {
        !self.json && !self.quiet && std::io::stderr().is_terminal()
    }

    /// Localised fixed text.
    pub fn t(&self, msg: Msg) -> &'static str {
        msg.text(self.lang)
    }

    /// Localised text with `{0}`, `{1}`... replaced.
    pub fn tf(&self, msg: Msg, args: &[&dyn std::fmt::Display]) -> String {
        format_message(msg.text(self.lang), args)
    }

    /// One line of the result (always printed in human mode).
    pub fn line(&self, text: impl AsRef<str>) {
        if self.human() {
            write_stdout(&format!("{}\n", text.as_ref()));
        }
    }

    /// Raw text, exactly as given (file contents, scripts for `Invoke-Expression`),
    /// in every mode.
    pub fn raw(&self, text: &str) {
        write_stdout(text);
        if !text.ends_with('\n') {
            write_stdout("\n");
        }
    }

    /// A status line ("Site created"): human mode, not quiet.
    pub fn info(&self, text: impl AsRef<str>) {
        if self.human() && !self.quiet {
            write_stdout(&format!("{}\n", text.as_ref()));
        }
    }

    /// A success line.
    pub fn success(&self, text: impl AsRef<str>) {
        self.info(format!("{} {}", paint(GREEN, "ok"), text.as_ref()));
    }

    /// A warning on stderr (human mode, not quiet).
    pub fn warn(&self, text: impl AsRef<str>) {
        if self.human() && !self.quiet {
            write_stderr(&format!("{} {}\n", paint(YELLOW, "!"), text.as_ref()));
        }
    }

    /// A note on stderr in any mode but quiet (UAC notice, daemon autostart...).
    pub fn note(&self, text: impl AsRef<str>) {
        if !self.quiet {
            write_stderr(&format!("{}\n", text.as_ref()));
        }
    }

    /// Prints a JSON document (compact when piped, indented on a terminal).
    pub fn json_value<T: Serialize>(&self, value: &T) {
        let text = if std::io::stdout().is_terminal() {
            serde_json::to_string_pretty(value)
        } else {
            serde_json::to_string(value)
        };
        match text {
            Ok(text) => write_stdout(&format!("{text}\n")),
            Err(err) => write_stderr(&format!("JSON: {err}\n")),
        }
    }

    /// One JSON line (streams).
    pub fn json_line<T: Serialize>(&self, value: &T) {
        if let Ok(text) = serde_json::to_string(value) {
            write_stdout(&format!("{text}\n"));
        }
    }

    /// `--json`: the value; otherwise `human` prints it.
    pub fn emit<T: Serialize>(&self, value: &T, human: impl FnOnce(&Output)) {
        if self.json {
            self.json_value(value);
        } else {
            human(self);
        }
    }

    /// A table with bold headers; an empty one prints `empty` instead.
    pub fn table(&self, headers: &[&str], rows: Vec<Vec<String>>, empty: Msg) {
        if rows.is_empty() {
            self.line(self.t(empty));
            return;
        }
        self.line(render_table(headers, rows));
    }

    /// `key  value` pairs, aligned.
    pub fn pairs(&self, pairs: &[(&str, String)]) {
        let width = pairs
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(0);
        for (key, value) in pairs {
            let pad = " ".repeat(width - key.chars().count());
            self.line(format!("{}{pad}  {value}", paint(BOLD, key)));
        }
    }

    /// Prints an error: JSON on stdout in `--json` mode, otherwise on stderr.
    pub fn error(&self, error: &CliError) {
        if error.silent {
            return;
        }
        if self.json {
            self.json_value(&error.to_json());
        } else {
            let mut text = format!("{} {}", paint(RED, self.t(Msg::ErrorPrefix)), error.message);
            if let Some(hint) = hint_for(self, error) {
                text.push_str(&format!("\n  {}", paint(DIM, hint)));
            }
            write_stderr(&format!("{text}\n"));
        }
    }
}

/// A suggestion for the codes that have an obvious next step.
fn hint_for(out: &Output, error: &CliError) -> Option<String> {
    match error.code.as_str() {
        "needs_elevation" => Some(out.t(Msg::HintElevation).to_owned()),
        "unauthorized" => Some(out.t(Msg::HintUnauthorized).to_owned()),
        _ => None,
    }
}

/// Plain table (no borders, two spaces between columns, bold header).
pub fn render_table(headers: &[&str], rows: Vec<Vec<String>>) -> String {
    let mut builder = Builder::default();
    builder.push_record(headers.iter().map(|h| paint(BOLD, h)));
    for row in rows {
        builder.push_record(row);
    }
    let mut table = builder.build();
    table
        .with(TableStyle::empty())
        .with(Padding::new(0, 2, 0, 0));
    table
        .to_string()
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

/// `1.5 GiB`, `300 KiB`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `3d 4h`, `5m 12s`, `42s`.
pub fn format_duration(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// `-` for a missing value.
pub fn or_dash<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "-".into(), |v| v.to_string())
}

/// Removes the ANSI escape codes of `text` (tests and width computations).
pub fn strip_ansi(text: &str) -> String {
    String::from_utf8_lossy(&anstream::adapter::strip_bytes(text.as_bytes()).into_vec())
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_align_and_have_no_trailing_spaces() {
        let text = render_table(
            &["A", "BBB"],
            vec![
                vec!["x".into(), "1".into()],
                vec!["longer".into(), "2".into()],
            ],
        );
        let lines: Vec<String> = strip_ansi(&text).lines().map(str::to_owned).collect();
        assert_eq!(lines[0], "A       BBB");
        assert_eq!(lines[1], "x       1");
        assert_eq!(lines[2], "longer  2");
    }

    #[test]
    fn colored_cells_keep_the_alignment() {
        let text = render_table(
            &["A", "B"],
            vec![
                vec![paint(GREEN, "run"), "1".into()],
                vec!["stopped".into(), "2".into()],
            ],
        );
        let lines: Vec<String> = strip_ansi(&text).lines().map(str::to_owned).collect();
        assert_eq!(lines[1], "run      1");
        assert_eq!(lines[2], "stopped  2");
    }

    #[test]
    fn sizes_and_durations_are_readable() {
        assert_eq!(format_bytes(10), "10 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
        assert_eq!(format_duration(42), "42s");
        assert_eq!(format_duration(312), "5m 12s");
        assert_eq!(format_duration(3 * 3600 + 5 * 60), "3h 5m");
        assert_eq!(format_duration(2 * 86_400 + 3600), "2d 1h");
    }
}
