//! Opening things with the program the user's shell associates with them: a URL in the
//! default browser, a folder in the file manager (docs/07 §4, ADR-0039). The daemon uses
//! it for `sites.open`; later tasks reuse it (database GUI, `php.ini`, mail UI).
//!
//! On Windows this is `ShellExecuteExW` with the default verb on a COM-initialized
//! thread ([`super::windows::WindowsShellOpener`]). The calls are **blocking** (the shell
//! may load a handler first): async code calls them from `spawn_blocking`
//! ([`crate::runtime::spawn_blocking_timeout`], ADR-0011).
//!
//! The inputs are checked here, before any OS call, because the shell would happily run
//! whatever it is handed: only `http`/`https` URLs, and paths that exist, are absolute and
//! are folders or documents (never something the shell would execute, see
//! [`is_executable_extension`]).

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::{ForgeError, Result};

/// Opens URLs and paths with the user's default applications.
pub trait ShellOpener: Send + Sync + fmt::Debug {
    /// Opens an `http`/`https` URL in the default browser. `InvalidInput` for anything
    /// else. Blocking.
    fn open_url(&self, url: &str) -> Result<()>;

    /// Opens an existing absolute folder in the file manager, or a document with its
    /// associated program. `InvalidInput` for relative paths and for files the shell would
    /// execute; `NotFound` when the path does not exist. Blocking.
    fn open_path(&self, path: &Path) -> Result<()>;

    /// Starts a program for the user to see (an external terminal or editor, M2-E3-T9),
    /// detached from the caller: it outlives the daemon. Returns the pid. `NotFound` when
    /// the executable is missing. Blocking.
    fn launch(&self, spec: &LaunchSpec) -> Result<u32>;
}

/// A program to start with [`ShellOpener::launch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    /// Working folder (an existing absolute folder).
    pub cwd: PathBuf,
    /// The complete environment; the caller's when `None`.
    pub env: Option<Vec<(OsString, OsString)>>,
    /// Give a console program (`pwsh`, `cmd`) a console window of its own. A GUI program
    /// (`wt`, an editor) needs none.
    pub new_console: bool,
}

/// File extensions the shell runs instead of showing (`open_path` refuses them).
const EXECUTABLE_EXTENSIONS: &[&str] = &[
    "exe", "com", "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "msi",
    "msp", "scr", "pif", "lnk", "url", "cpl", "hta", "jar", "reg", "msc", "appx", "msix", "dll",
];

/// Whether the shell would run a file with this extension (case-insensitive).
pub fn is_executable_extension(ext: &str) -> bool {
    EXECUTABLE_EXTENSIONS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(ext))
}

/// Checks that `url` is an `http`/`https` URL without spaces or control characters;
/// returns it trimmed.
pub fn validate_url(url: &str) -> Result<&str> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    let has_scheme = lower.starts_with("http://") || lower.starts_with("https://");
    let rest_len = url.split_once("://").map_or(0, |(_, rest)| rest.len());
    if !has_scheme || rest_len == 0 || url.chars().any(|c| c.is_control() || c == ' ') {
        return Err(ForgeError::InvalidInput(format!(
            "only http:// and https:// URLs can be opened, got {url:?}"
        )));
    }
    Ok(url)
}

/// Checks that `path` is absolute, exists and is not something the shell would execute.
pub fn validate_path(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(ForgeError::InvalidInput(format!(
            "{} must be an absolute path",
            path.display()
        )));
    }
    let meta = std::fs::metadata(path).map_err(|e| ForgeError::io_at("opening", path, e))?;
    if meta.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(is_executable_extension)
    {
        return Err(ForgeError::InvalidInput(format!(
            "{} is a program or shortcut; the shell opener only opens folders and documents",
            path.display()
        )));
    }
    Ok(())
}

/// The opener of the running OS.
#[cfg(windows)]
pub fn shell_opener() -> Arc<dyn ShellOpener> {
    Arc::new(super::windows::WindowsShellOpener)
}

/// The opener of the running OS (not implemented outside Windows yet: `xdg-open`/`open`
/// arrive with the Linux/macOS ports, docs/13 §4).
#[cfg(not(windows))]
pub fn shell_opener() -> Arc<dyn ShellOpener> {
    Arc::new(Unsupported)
}

#[cfg(not(windows))]
#[derive(Debug)]
struct Unsupported;

#[cfg(not(windows))]
impl ShellOpener for Unsupported {
    fn open_url(&self, _: &str) -> Result<()> {
        Err(unsupported())
    }
    fn open_path(&self, _: &Path) -> Result<()> {
        Err(unsupported())
    }
    fn launch(&self, _: &LaunchSpec) -> Result<u32> {
        Err(unsupported())
    }
}

#[cfg(not(windows))]
fn unsupported() -> ForgeError {
    ForgeError::UnsupportedPlatform(format!(
        "opening files and URLs is not implemented on {}",
        std::env::consts::OS
    ))
}

/// What a [`RecordingShellOpener`] was asked to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellOpen {
    Url(String),
    Path(std::path::PathBuf),
    Launch(LaunchSpec),
}

/// A [`ShellOpener`] that opens nothing and remembers the requests: tests, and callers
/// that must not touch the desktop. Applies the same input checks as the real one unless
/// built with [`RecordingShellOpener::lenient`].
#[derive(Debug, Default, Clone)]
pub struct RecordingShellOpener {
    calls: Arc<Mutex<Vec<ShellOpen>>>,
    skip_validation: bool,
}

impl RecordingShellOpener {
    /// Records after validating the input like the real opener.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records without checking the input (paths need not exist).
    pub fn lenient() -> Self {
        Self {
            skip_validation: true,
            ..Self::default()
        }
    }

    /// The requests so far, oldest first.
    pub fn calls(&self) -> Vec<ShellOpen> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }

    fn record(&self, call: ShellOpen) {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(call);
        }
    }
}

impl ShellOpener for RecordingShellOpener {
    fn open_url(&self, url: &str) -> Result<()> {
        let url = if self.skip_validation {
            url
        } else {
            validate_url(url)?
        };
        self.record(ShellOpen::Url(url.to_owned()));
        Ok(())
    }

    fn open_path(&self, path: &Path) -> Result<()> {
        if !self.skip_validation {
            validate_path(path)?;
        }
        self.record(ShellOpen::Path(path.to_path_buf()));
        Ok(())
    }

    fn launch(&self, spec: &LaunchSpec) -> Result<u32> {
        self.record(ShellOpen::Launch(spec.clone()));
        Ok(0)
    }
}

// --------------------------------------------------------------------------- commands

/// Splits a configured command (`[editor] command`, `[terminal] command`) into words:
/// whitespace separates, double quotes group (and may sit inside a word); there are no
/// escapes, so Windows paths keep their backslashes. `InvalidInput` for an unbalanced quote
/// or no word at all.
pub fn split_command(text: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if quoted {
        return Err(ForgeError::InvalidInput(format!(
            "unbalanced quote in command {text:?}"
        )));
    }
    if started {
        words.push(current);
    }
    if words.first().is_none_or(String::is_empty) {
        return Err(ForgeError::InvalidInput(
            "the command needs a program".into(),
        ));
    }
    Ok(words)
}

/// Where programs are looked up (pure inputs, so the rules are testable with a fake `PATH`).
pub struct ProgramLookup<'a> {
    /// `PATH` of the daemon.
    pub path: &'a OsStr,
    /// `%SystemRoot%\System32`.
    pub system32: &'a Path,
    /// `%LOCALAPPDATA%`.
    pub local_app_data: Option<&'a Path>,
    /// `%ProgramFiles%`.
    pub program_files: Option<&'a Path>,
    /// Whether a file exists (app execution aliases such as `wt.exe` included).
    pub is_file: &'a dyn Fn(&Path) -> bool,
}

impl ProgramLookup<'_> {
    /// The first `dir\file` of `PATH` that exists.
    pub fn in_path(&self, file: &str) -> Option<PathBuf> {
        std::env::split_paths(self.path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(file))
            .find(|p| (self.is_file)(p))
    }

    fn existing(&self, path: PathBuf) -> Option<PathBuf> {
        (self.is_file)(&path).then_some(path)
    }
}

/// What to run, how: the output of the resolvers below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    /// Needs a console window of its own ([`LaunchSpec::new_console`]).
    pub new_console: bool,
}

/// Replaces `{name}` placeholders of one word, in one pass (a value that contains a
/// placeholder is not expanded again).
fn fill(word: &str, values: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(word.len());
    let mut rest = word;
    'scan: while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        for (name, value) in values {
            let token = format!("{{{name}}}");
            if let Some(after) = rest.strip_prefix(&token) {
                out.push_str(value);
                rest = after;
                continue 'scan;
            }
        }
        out.push('{');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

/// The program of a configured command: absolute as written, or looked up in `PATH`
/// (`.exe`, `.cmd`, `.bat` when it has no extension). Batch files run through
/// `cmd.exe /d /v:off /s /c` with every word quoted.
fn resolve_program(
    program: &str,
    rest: Vec<OsString>,
    lookup: &ProgramLookup<'_>,
) -> Result<(PathBuf, Vec<OsString>)> {
    let as_path = PathBuf::from(program);
    let found = if as_path.is_absolute() {
        lookup.existing(as_path)
    } else if program.contains(['\\', '/', ':']) {
        None
    } else if as_path.extension().is_some() {
        lookup.in_path(program)
    } else {
        ["exe", "cmd", "bat"]
            .iter()
            .find_map(|ext| lookup.in_path(&format!("{program}.{ext}")))
    };
    let exe = found
        .ok_or_else(|| ForgeError::NotFound(format!("program {program:?} (configured command)")))?;
    let batch = exe
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|e| e.eq_ignore_ascii_case("bat") || e.eq_ignore_ascii_case("cmd"));
    if batch {
        // cmd.exe re-parses the whole line: `&`, `|`, `<`, `>` and `^` split or redirect
        // unless quoted, and `%VAR%` expands even inside quotes. So every word is quoted
        // and `"`, `%`, CR and LF (which cannot be quoted safely) are refused. The `/s`
        // form strips exactly the outer pair of quotes, which keeps a batch path with
        // spaces intact; `/v:off` disables `!var!` expansion. The Windows launcher passes
        // the last argument to CreateProcess verbatim (`command_line`).
        check_batch_arg(exe.as_os_str())?;
        let mut tail = OsString::from("\"\"");
        tail.push(exe.as_os_str());
        tail.push("\"");
        for word in &rest {
            check_batch_arg(word)?;
            tail.push(" \"");
            tail.push(word);
            tail.push("\"");
        }
        tail.push("\"");
        let args = vec![
            OsString::from("/d"),
            OsString::from("/v:off"),
            OsString::from("/s"),
            OsString::from("/c"),
            tail,
        ];
        Ok((lookup.system32.join("cmd.exe"), args))
    } else {
        Ok((exe, rest))
    }
}

/// Refuses what cannot be passed to a batch file through `cmd.exe` safely.
fn check_batch_arg(arg: &OsStr) -> Result<()> {
    let text = arg.to_string_lossy();
    if text.contains(['"', '%', '\r', '\n', '\0']) {
        return Err(ForgeError::InvalidInput(format!(
            "{text:?} cannot be passed to a batch file (quote, percent sign or line break)"
        )));
    }
    Ok(())
}

/// A file or folder to open in an editor at an optional position (1-based).
#[derive(Debug, Clone, Copy)]
pub struct EditorTarget<'a> {
    pub path: &'a Path,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// The editor command: the configured template (`{path}`, `{line}`, `{column}`; a template
/// without `{path}` gets the path appended as the last argument; line and column default to
/// 1), else the first editor found: VS Code, PhpStorm, Notepad. Each word is one argument,
/// so paths with spaces need no quoting.
pub fn resolve_editor(
    configured: Option<&str>,
    target: EditorTarget<'_>,
    lookup: &ProgramLookup<'_>,
) -> Result<Resolved> {
    let path_text = target.path.display().to_string();
    let plain = |exe: PathBuf, args: Vec<OsString>| Resolved {
        exe,
        args,
        new_console: false,
    };
    if let Some(template) = configured.map(str::trim).filter(|t| !t.is_empty()) {
        let words = split_command(template)?;
        let values = [
            ("path", path_text.clone()),
            ("line", target.line.unwrap_or(1).to_string()),
            ("column", target.column.unwrap_or(1).to_string()),
        ];
        let has_path = words[1..].iter().any(|w| w.contains("{path}"));
        let mut rest: Vec<OsString> = words[1..]
            .iter()
            .map(|w| OsString::from(fill(w, &values)))
            .collect();
        if !has_path {
            rest.push(target.path.as_os_str().to_owned());
        }
        let (exe, args) = resolve_program(&words[0], rest, lookup)?;
        return Ok(plain(exe, args));
    }
    // VS Code: `code.exe`, the `Code.exe` next to `bin\code.cmd`, then the usual installs.
    let vscode = lookup
        .in_path("code.exe")
        .or_else(|| {
            lookup
                .in_path("code.cmd")
                .and_then(|cmd| cmd.parent()?.parent().map(|d| d.join("Code.exe")))
                .and_then(|p| lookup.existing(p))
        })
        .or_else(|| {
            lookup.local_app_data.and_then(|d| {
                lookup.existing(
                    d.join("Programs")
                        .join("Microsoft VS Code")
                        .join("Code.exe"),
                )
            })
        })
        .or_else(|| {
            lookup
                .program_files
                .and_then(|d| lookup.existing(d.join("Microsoft VS Code").join("Code.exe")))
        });
    if let Some(exe) = vscode {
        let args = match target.line {
            Some(line) => vec![
                OsString::from("--goto"),
                OsString::from(format!("{path_text}:{line}:{}", target.column.unwrap_or(1))),
            ],
            None => vec![target.path.as_os_str().to_owned()],
        };
        return Ok(plain(exe, args));
    }
    let phpstorm = [
        "phpstorm64.exe",
        "phpstorm.exe",
        "phpstorm.cmd",
        "phpstorm.bat",
    ]
    .iter()
    .find_map(|name| lookup.in_path(name));
    if let Some(found) = phpstorm {
        let mut rest = Vec::new();
        if let Some(line) = target.line {
            rest.extend([
                OsString::from("--line"),
                OsString::from(line.to_string()),
                OsString::from("--column"),
                OsString::from(target.column.unwrap_or(1).to_string()),
            ]);
        }
        rest.push(target.path.as_os_str().to_owned());
        let (exe, args) = resolve_program(&found.display().to_string(), rest, lookup)?;
        return Ok(plain(exe, args));
    }
    let notepad = lookup
        .existing(lookup.system32.join("notepad.exe"))
        .or_else(|| lookup.in_path("notepad.exe"))
        .ok_or_else(|| {
            ForgeError::NotFound("no editor found (VS Code, PhpStorm, Notepad)".into())
        })?;
    Ok(plain(notepad, vec![target.path.as_os_str().to_owned()]))
}

/// Whether `exe` is a console program (it needs a window of its own).
fn is_console_shell(exe: &Path) -> bool {
    exe.file_stem().and_then(OsStr::to_str).is_some_and(|stem| {
        ["cmd", "pwsh", "powershell"]
            .iter()
            .any(|known| known.eq_ignore_ascii_case(stem))
    })
}

/// The external terminal command for `dir`: the configured template (`{dir}`; the program
/// starts in `dir` either way), else Windows Terminal (`wt -d dir`), `pwsh`, `powershell`.
pub fn resolve_terminal(
    configured: Option<&str>,
    dir: &Path,
    lookup: &ProgramLookup<'_>,
) -> Result<Resolved> {
    if let Some(template) = configured.map(str::trim).filter(|t| !t.is_empty()) {
        let words = split_command(template)?;
        let values = [("dir", dir.display().to_string())];
        let rest = words[1..]
            .iter()
            .map(|w| OsString::from(fill(w, &values)))
            .collect();
        let (exe, args) = resolve_program(&words[0], rest, lookup)?;
        let new_console = is_console_shell(&exe);
        return Ok(Resolved {
            exe,
            args,
            new_console,
        });
    }
    if let Some(wt) = lookup.in_path("wt.exe") {
        return Ok(Resolved {
            exe: wt,
            args: vec![OsString::from("-d"), dir.as_os_str().to_owned()],
            new_console: false,
        });
    }
    let pwsh = lookup.in_path("pwsh.exe").or_else(|| {
        lookup
            .program_files
            .and_then(|d| lookup.existing(d.join("PowerShell").join("7").join("pwsh.exe")))
    });
    let powershell = || {
        lookup
            .existing(
                lookup
                    .system32
                    .join("WindowsPowerShell")
                    .join("v1.0")
                    .join("powershell.exe"),
            )
            .or_else(|| lookup.in_path("powershell.exe"))
    };
    let exe = pwsh.or_else(powershell).ok_or_else(|| {
        ForgeError::NotFound("no terminal found (Windows Terminal, pwsh, powershell)".into())
    })?;
    Ok(Resolved {
        exe,
        args: vec![OsString::from("-NoLogo"), OsString::from("-NoExit")],
        new_console: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_urls_pass() {
        assert_eq!(
            validate_url(" https://loja.test/ ").unwrap(),
            "https://loja.test/"
        );
        assert!(validate_url("HTTP://a.test").is_ok());
        for bad in [
            "file:///C:/Windows/notepad.exe",
            "calc.exe",
            "javascript:alert(1)",
            "ms-msdt:/id x",
            "https://",
            "https://a.test/ b",
            "https://a.test/\u{0}",
            "",
        ] {
            let err = validate_url(bad).unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{bad:?}");
        }
    }

    #[test]
    fn paths_must_exist_and_not_be_programs() {
        let dir = tempfile::tempdir().unwrap();
        validate_path(dir.path()).unwrap();
        let doc = dir.path().join("readme.txt");
        std::fs::write(&doc, b"x").unwrap();
        validate_path(&doc).unwrap();
        for name in ["setup.EXE", "run.bat", "x.ps1", "link.lnk", "web.url"] {
            let file = dir.path().join(name);
            std::fs::write(&file, b"x").unwrap();
            assert_eq!(
                validate_path(&file).unwrap_err().code(),
                "invalid_input",
                "{name}"
            );
        }
        assert_eq!(
            validate_path(&dir.path().join("missing"))
                .unwrap_err()
                .code(),
            "not_found"
        );
        assert_eq!(
            validate_path(Path::new("relative/dir")).unwrap_err().code(),
            "invalid_input"
        );
        assert!(is_executable_extension("Cmd") && !is_executable_extension("php"));
    }

    #[test]
    fn the_recording_opener_remembers_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        let opener = RecordingShellOpener::new();
        opener.open_url("https://a.test").unwrap();
        opener.open_path(dir.path()).unwrap();
        assert!(opener.open_url("ftp://a.test").is_err());
        assert!(opener.open_path(&dir.path().join("nope")).is_err());
        assert_eq!(
            opener.calls(),
            vec![
                ShellOpen::Url("https://a.test".into()),
                ShellOpen::Path(dir.path().to_path_buf())
            ]
        );
        let lenient = RecordingShellOpener::lenient();
        lenient.open_path(Path::new("C:/missing")).unwrap();
        assert_eq!(lenient.calls().len(), 1);
    }

    #[test]
    fn commands_split_into_words_with_quotes() {
        assert_eq!(
            split_command(r#""C:\Program Files\Code\Code.exe" --goto {path}:{line}"#).unwrap(),
            vec![r"C:\Program Files\Code\Code.exe", "--goto", "{path}:{line}"]
        );
        assert_eq!(split_command(r#"a "" b"#).unwrap(), vec!["a", "", "b"]);
        assert_eq!(split_command(r#"x"y z"w"#).unwrap(), vec!["xy zw"]);
        for bad in ["", "   ", r#""open"#, r#""" a"#] {
            assert_eq!(
                split_command(bad).unwrap_err().code(),
                "invalid_input",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn placeholders_are_filled_once() {
        let values = [("path", "{line}".to_owned()), ("line", "7".to_owned())];
        assert_eq!(fill("{path}:{line}:{x}", &values), "{line}:7:{x}");
        assert_eq!(fill("no {braces", &values), "no {braces");
    }

    /// A fake machine: `files` exist, `PATH` is the given folders.
    #[cfg(windows)]
    fn machine(files: &[&str], path: &str) -> (Vec<PathBuf>, OsString) {
        (
            files.iter().map(PathBuf::from).collect(),
            OsString::from(path),
        )
    }

    #[cfg(windows)]
    fn lookup_of<'a>(path: &'a OsString, is_file: &'a dyn Fn(&Path) -> bool) -> ProgramLookup<'a> {
        ProgramLookup {
            path,
            system32: Path::new(r"C:\Windows\System32"),
            local_app_data: Some(Path::new(r"C:\Users\dev\AppData\Local")),
            program_files: Some(Path::new(r"C:\Program Files")),
            is_file,
        }
    }

    #[cfg(windows)]
    fn editor(
        configured: Option<&str>,
        files: &[&str],
        path: &str,
        target: &str,
        line: Option<u32>,
        column: Option<u32>,
    ) -> Result<Resolved> {
        let (files, path) = machine(files, path);
        let exists = |p: &Path| files.iter().any(|f| f.as_path() == p);
        let lookup = lookup_of(&path, &exists);
        resolve_editor(
            configured,
            EditorTarget {
                path: Path::new(target),
                line,
                column,
            },
            &lookup,
        )
    }

    fn args(resolved: &Resolved) -> Vec<String> {
        resolved
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[cfg(windows)]
    #[test]
    fn vscode_command_line_with_and_without_a_position() {
        let files = [r"C:\Apps\VSCode\Code.exe", r"C:\Apps\VSCode\bin\code.cmd"];
        let target = r"C:\www\my site\app.php";
        // `code.cmd` in PATH: the Code.exe above `bin` runs, so no cmd.exe round trip.
        let r = editor(
            None,
            &files,
            r"C:\Apps\VSCode\bin",
            target,
            Some(42),
            Some(3),
        )
        .unwrap();
        assert_eq!(r.exe, PathBuf::from(r"C:\Apps\VSCode\Code.exe"));
        assert_eq!(args(&r), ["--goto", r"C:\www\my site\app.php:42:3"]);
        let r = editor(None, &files, r"C:\Apps\VSCode\bin", target, Some(9), None).unwrap();
        assert_eq!(args(&r), ["--goto", r"C:\www\my site\app.php:9:1"]);
        let r = editor(None, &files, r"C:\Apps\VSCode\bin", target, None, Some(5)).unwrap();
        assert_eq!(args(&r), [target]);
        // Not in PATH: the per-user install, then Program Files.
        let user = r"C:\Users\dev\AppData\Local\Programs\Microsoft VS Code\Code.exe";
        let r = editor(None, &[user], "", target, None, None).unwrap();
        assert_eq!(r.exe, PathBuf::from(user));
        let system = r"C:\Program Files\Microsoft VS Code\Code.exe";
        let r = editor(None, &[system, user], "", target, None, None).unwrap();
        assert_eq!(r.exe, PathBuf::from(user), "the per-user install wins");
        let r = editor(None, &[system], "", target, None, None).unwrap();
        assert_eq!(r.exe, PathBuf::from(system));
    }

    #[cfg(windows)]
    #[test]
    fn autodetect_order_is_vscode_phpstorm_notepad() {
        let all = [
            r"C:\a\code.exe",
            r"C:\b\phpstorm64.exe",
            r"C:\Windows\System32\notepad.exe",
        ];
        let path = r"C:\b;C:\a";
        let r = editor(None, &all, path, r"C:\x\f.php", Some(1), None).unwrap();
        assert_eq!(r.exe, PathBuf::from(r"C:\a\code.exe"));
        // PhpStorm: --line/--column before the path.
        let r = editor(None, &all[1..], path, r"C:\x y\f.php", Some(42), Some(3)).unwrap();
        assert_eq!(r.exe, PathBuf::from(r"C:\b\phpstorm64.exe"));
        assert_eq!(args(&r), ["--line", "42", "--column", "3", r"C:\x y\f.php"]);
        let r = editor(None, &all[1..], path, r"C:\x\f.php", None, None).unwrap();
        assert_eq!(args(&r), [r"C:\x\f.php"]);
        // Notepad takes only the path.
        let r = editor(None, &all[2..], path, r"C:\x\f.php", Some(42), Some(3)).unwrap();
        assert_eq!(r.exe, PathBuf::from(r"C:\Windows\System32\notepad.exe"));
        assert_eq!(args(&r), [r"C:\x\f.php"]);
        // A PATH folder that does not exist or is relative finds nothing.
        let err = editor(None, &[], r"relative;C:\nowhere", r"C:\x", None, None).unwrap_err();
        assert_eq!(err.code(), "not_found");
        // A batch PhpStorm runs through cmd.exe.
        let r = editor(
            None,
            &[r"C:\b\phpstorm.cmd"],
            path,
            r"C:\x\f.php",
            None,
            None,
        )
        .unwrap();
        assert_eq!(r.exe, PathBuf::from(r"C:\Windows\System32\cmd.exe"));
        assert_eq!(args(&r)[..4], ["/d", "/v:off", "/s", "/c"]);
    }

    #[cfg(windows)]
    #[test]
    fn a_custom_editor_template_fills_placeholders_and_keeps_spaces() {
        let files = [r"C:\Tools\My Editor\ed.exe", r"C:\Tools\sub.cmd"];
        let template = r#""C:\Tools\My Editor\ed.exe" -g {path}:{line}:{column} --reuse"#;
        let r = editor(
            Some(template),
            &files,
            "",
            r"C:\www\a b\c.php",
            Some(42),
            Some(3),
        )
        .unwrap();
        assert_eq!(r.exe, PathBuf::from(r"C:\Tools\My Editor\ed.exe"));
        assert_eq!(args(&r), ["-g", r"C:\www\a b\c.php:42:3", "--reuse"]);
        // Defaults 1:1; no {path} in the template: the path is the last argument.
        let r = editor(
            Some(r#""C:\Tools\My Editor\ed.exe" --line {line} --col {column}"#),
            &files,
            "",
            r"C:\x\f.php",
            None,
            None,
        )
        .unwrap();
        assert_eq!(args(&r), ["--line", "1", "--col", "1", r"C:\x\f.php"]);
        // Bare names are found in PATH (.exe, .cmd, .bat); a batch runs through cmd.exe.
        let r = editor(Some("sub {path}"), &files, r"C:\Tools", r"C:\x", None, None).unwrap();
        assert_eq!(r.exe, PathBuf::from(r"C:\Windows\System32\cmd.exe"));
        assert_eq!(
            args(&r),
            ["/d", "/v:off", "/s", "/c", r#"""C:\Tools\sub.cmd" "C:\x"""#]
        );
        // A configured program that is missing is not_found, never a silent fallback.
        let err = editor(
            Some("nope {path}"),
            &files,
            r"C:\Tools",
            r"C:\x",
            None,
            None,
        );
        assert_eq!(err.unwrap_err().code(), "not_found");
        let err = editor(Some(r#""unclosed"#), &files, "", r"C:\x", None, None);
        assert_eq!(err.unwrap_err().code(), "invalid_input");
    }

    #[cfg(windows)]
    #[test]
    fn terminal_autodetect_is_wt_pwsh_powershell() {
        let dir = Path::new(r"C:\www\my site");
        let term = |configured: Option<&str>, files: &[&str], path: &str| {
            let (files, path) = machine(files, path);
            let exists = |p: &Path| files.iter().any(|f| f.as_path() == p);
            resolve_terminal(configured, dir, &lookup_of(&path, &exists))
        };
        let all = [
            r"C:\w\wt.exe",
            r"C:\p\pwsh.exe",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        ];
        let r = term(None, &all, r"C:\w;C:\p").unwrap();
        assert_eq!(r.exe, PathBuf::from(r"C:\w\wt.exe"));
        assert_eq!(args(&r), ["-d", r"C:\www\my site"]);
        assert!(!r.new_console);
        let r = term(None, &all[1..], r"C:\w;C:\p").unwrap();
        assert_eq!(r.exe, PathBuf::from(r"C:\p\pwsh.exe"));
        assert!(r.new_console);
        let r = term(None, &all[2..], r"C:\w;C:\p").unwrap();
        assert!(r.exe.ends_with("powershell.exe") && r.new_console);
        let pf = r"C:\Program Files\PowerShell\7\pwsh.exe";
        assert_eq!(term(None, &[pf], "").unwrap().exe, PathBuf::from(pf));
        assert_eq!(term(None, &[], "").unwrap_err().code(), "not_found");
        // A configured terminal wins and gets {dir}.
        let r = term(
            Some(r#"alacritty --working-directory {dir}"#),
            &[r"C:\t\alacritty.exe"],
            r"C:\t",
        )
        .unwrap();
        assert_eq!(args(&r), ["--working-directory", r"C:\www\my site"]);
        assert!(!r.new_console);
        let r = term(Some("pwsh -NoExit"), &all, r"C:\p").unwrap();
        assert!(r.new_console, "a configured console shell gets its window");
    }

    #[cfg(windows)]
    #[test]
    fn batch_editors_get_every_argument_quoted() {
        let files = [r"C:\Tools\sub.cmd", r"C:\Program Files\x\ed.cmd"];
        // `&` stays inside quotes: never split.
        let r = editor(
            Some("sub {path}"),
            &files,
            r"C:\Tools",
            r"C:\www\a&b.php",
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            args(&r),
            [
                "/d",
                "/v:off",
                "/s",
                "/c",
                r#"""C:\Tools\sub.cmd" "C:\www\a&b.php"""#
            ]
        );
        // Spaces in both the batch and the path: one outer pair for /s to strip.
        let template = r#""C:\Program Files\x\ed.cmd" -g {path}:{line}"#;
        let r = editor(
            Some(template),
            &files,
            r"C:\Tools",
            r"C:\my dir\f.php",
            Some(4),
            None,
        )
        .unwrap();
        assert_eq!(
            args(&r)[4],
            r#"""C:\Program Files\x\ed.cmd" "-g" "C:\my dir\f.php:4"""#
        );
        // `%` expands inside quotes and `"` cannot be escaped: both refused.
        for bad in [r"C:\x\%PATH%.php", "C:\\x\\a\"b.php", "C:\\x\\a\nb.php"] {
            let err = editor(Some("sub {path}"), &files, r"C:\Tools", bad, None, None).unwrap_err();
            assert_eq!(err.code(), "invalid_input", "{bad}");
        }
    }

    #[test]
    fn the_recording_opener_records_launches() {
        let opener = RecordingShellOpener::new();
        let spec = LaunchSpec {
            exe: PathBuf::from("ed.exe"),
            args: vec!["a b".into()],
            cwd: PathBuf::from("."),
            env: None,
            new_console: false,
        };
        assert_eq!(opener.launch(&spec).unwrap(), 0);
        assert_eq!(opener.calls(), vec![ShellOpen::Launch(spec)]);
    }
}
