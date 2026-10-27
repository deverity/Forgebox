//! Command lines of Procfile processes: `$PORT` substitution, Windows argument splitting
//! and how the first word becomes an executable.
//!
//! A process `cmd` is a command line as in a Procfile (`npm run dev -- --port $PORT`).
//! The supervisor only starts real executables (`.cmd`/`.bat` are refused by the platform
//! layer because `cmd.exe` quoting is not safe to build from arguments), so [`plan_launch`]
//! decides between two ways, always against the **deterministic** search path of the
//! process (runtime folders, then the system folder; never the user's `PATH`):
//!
//! * **direct**: the first word is an `.exe`/`.com` found in that path (`node server.js`,
//!   `php artisan queue:work`); the rest is split with the MSVCRT rules and passed as
//!   arguments, which the platform quotes back identically;
//! * **shell**: the first word is a `.cmd`/`.bat` (`npm`, `npx`, `pnpm`), a `cmd.exe`
//!   internal (`echo`) or the line uses shell syntax (`&&`, `|`, `>`, `%VAR%`). The process
//!   is `cmd.exe /d /c %FORGEBOX_COMMAND%` and the line travels in that environment
//!   variable: `cmd.exe` expands it itself, so quotes in the line reach it untouched (no
//!   argument quoting is involved).

use std::path::{Path, PathBuf};

use forge_core::error::{ForgeError, Result};

/// Environment variable that carries the command line of a shell-launched process.
pub const SHELL_COMMAND_VAR: &str = "FORGEBOX_COMMAND";

/// Extensions tried, in this order, for a word without one (Windows' `PATHEXT` subset the
/// supervisor understands).
const EXTENSIONS: [&str; 4] = ["exe", "com", "cmd", "bat"];

/// Commands that only exist inside `cmd.exe`.
const CMD_INTERNALS: [&str; 16] = [
    "assoc", "call", "cd", "chdir", "copy", "del", "dir", "echo", "md", "mkdir", "move", "ren",
    "rd", "rmdir", "set", "type",
];

/// How a process command line is started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchKind {
    /// `exe` with the split arguments.
    Direct { exe: PathBuf },
    /// `cmd.exe /d /c %FORGEBOX_COMMAND%`.
    Shell { cmd_exe: PathBuf },
}

/// Whether `text` mentions `$PORT` or `${PORT}`.
pub fn references_port(text: &str) -> bool {
    port_positions(text).next().is_some()
}

/// Byte ranges of `$PORT` / `${PORT}` references (`$PORTAL` is not one).
fn port_positions(text: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let bytes = text.as_bytes();
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < bytes.len() {
            if bytes[i] == b'$' {
                if bytes[i + 1..].starts_with(b"{PORT}") {
                    let range = (i, i + 7);
                    i += 7;
                    return Some(range);
                }
                if bytes[i + 1..].starts_with(b"PORT") {
                    let after = bytes.get(i + 5).copied();
                    let word = after.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_');
                    if !word {
                        let range = (i, i + 5);
                        i += 5;
                        return Some(range);
                    }
                }
            }
            i += 1;
        }
        None
    })
}

/// Replaces `$PORT` and `${PORT}` by `port`. A reference without a port is an error: the
/// process would silently receive the literal text.
pub fn substitute_port(text: &str, port: Option<u16>) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, end) in port_positions(text) {
        let port = port.ok_or_else(|| {
            ForgeError::InvalidInput(
                "o comando usa $PORT, mas o processo não tem porta (defina port: auto)".to_owned(),
            )
        })?;
        out.push_str(&text[last..start]);
        out.push_str(&port.to_string());
        last = end;
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// Splits a command line like `CommandLineToArgvW`/the MSVCRT: whitespace separates,
/// double quotes group, `2n` backslashes before a quote are `n`, `2n+1` are `n` and a
/// literal quote.
pub fn split_args(line: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let mut backslashes = 1;
                while chars.peek() == Some(&'\\') {
                    chars.next();
                    backslashes += 1;
                }
                started = true;
                if chars.peek() == Some(&'"') {
                    current.extend(std::iter::repeat_n('\\', backslashes / 2));
                    if backslashes % 2 == 1 {
                        chars.next();
                        current.push('"');
                    }
                } else {
                    current.extend(std::iter::repeat_n('\\', backslashes));
                }
            }
            '"' => {
                started = true;
                in_quotes = !in_quotes;
            }
            c if c.is_whitespace() && !in_quotes => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                started = true;
                current.push(c);
            }
        }
    }
    if in_quotes {
        return Err(ForgeError::InvalidInput(format!(
            "aspas sem fechar no comando: {line}"
        )));
    }
    if started {
        args.push(current);
    }
    Ok(args)
}

/// Whether `line` needs `cmd.exe`: shell operators outside double quotes, or any `%`
/// (`%PORT%`, `%CD%`).
pub fn needs_shell(line: &str) -> bool {
    let mut in_quotes = false;
    for c in line.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            '%' => return true,
            '&' | '|' | '<' | '>' | '^' if !in_quotes => return true,
            _ => {}
        }
    }
    false
}

/// File of `word` in `dir` (trying the known extensions when it has none).
fn find_in(dir: &Path, word: &str) -> Option<PathBuf> {
    let has_known_extension = Path::new(word)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.iter().any(|k| k.eq_ignore_ascii_case(e)));
    if has_known_extension {
        let candidate = dir.join(word);
        return candidate.is_file().then_some(candidate);
    }
    EXTENSIONS
        .iter()
        .map(|ext| dir.join(format!("{word}.{ext}")))
        .find(|candidate| candidate.is_file())
}

/// How the line starts, searching `search_path` (in order) for its first word. Blocking
/// (stats files). `cwd` resolves words with a directory part (`./bin/run`).
pub fn plan_launch(
    line: &str,
    cwd: &Path,
    search_path: &[PathBuf],
    system_dirs: &[PathBuf],
) -> Result<LaunchKind> {
    let cmd_exe = system_dirs
        .iter()
        .map(|dir| dir.join("cmd.exe"))
        .find(|p| p.is_file())
        .ok_or_else(|| ForgeError::NotFound("cmd.exe in the system folder".to_owned()))?;
    let shell = LaunchKind::Shell {
        cmd_exe: cmd_exe.clone(),
    };
    if needs_shell(line) {
        return Ok(shell);
    }
    let words = split_args(line)?;
    let Some(first) = words.first() else {
        return Err(ForgeError::InvalidInput("comando vazio".to_owned()));
    };
    let with_directory = first.contains(['/', '\\', ':']);
    let found = if with_directory {
        let path = Path::new(first);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        let parent = path.parent().unwrap_or(cwd);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        find_in(parent, &name)
    } else if CMD_INTERNALS.contains(&first.to_ascii_lowercase().as_str()) {
        return Ok(shell);
    } else {
        search_path
            .iter()
            .chain(system_dirs)
            .find_map(|dir| find_in(dir, first))
    };
    let Some(found) = found else {
        let searched: Vec<String> = search_path
            .iter()
            .chain(system_dirs)
            .map(|d| d.display().to_string())
            .collect();
        return Err(ForgeError::NotFound(format!(
            "executável '{first}' não encontrado (procurado em: {})",
            searched.join("; ")
        )));
    };
    let extension = found
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "cmd" | "bat" => Ok(shell),
        _ => Ok(LaunchKind::Direct { exe: found }),
    }
}

/// Arguments for `kind`: the split line (direct) or `/d /c %FORGEBOX_COMMAND%` (shell).
pub fn launch_args(kind: &LaunchKind, line: &str) -> Result<Vec<String>> {
    match kind {
        LaunchKind::Direct { .. } => Ok(split_args(line)?.split_off(1)),
        LaunchKind::Shell { .. } => Ok(vec![
            "/d".to_owned(),
            "/c".to_owned(),
            format!("%{SHELL_COMMAND_VAR}%"),
        ]),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn port_references_are_replaced() {
        assert_eq!(
            substitute_port("npm run dev -- --port $PORT", Some(3001)).unwrap(),
            "npm run dev -- --port 3001"
        );
        assert_eq!(
            substitute_port("a ${PORT} b $PORT", Some(80)).unwrap(),
            "a 80 b 80"
        );
        assert_eq!(
            substitute_port("$PORTAL $PORT_X $PORTS x$PORT", Some(1)).unwrap(),
            "$PORTAL $PORT_X $PORTS x1"
        );
        assert_eq!(substitute_port("no refs", None).unwrap(), "no refs");
        assert!(references_port("x --port $PORT"));
        assert!(!references_port("x --port $PORTAL"));
        assert_eq!(
            substitute_port("x $PORT", None).unwrap_err().code(),
            "invalid_input"
        );
    }

    #[test]
    fn arguments_follow_the_msvcrt_rules() {
        assert_eq!(
            split_args(r#"node "my app/server.js" --name "a b" c\d "e\"f" "g\\" ''"#).unwrap(),
            [
                "node",
                "my app/server.js",
                "--name",
                "a b",
                r"c\d",
                "e\"f",
                r"g\",
                "''"
            ]
        );
        assert_eq!(split_args("  a   b ").unwrap(), ["a", "b"]);
        assert_eq!(split_args(r#"a "" b"#).unwrap(), ["a", "", "b"]);
        assert!(split_args("").unwrap().is_empty());
        assert_eq!(split_args(r#"a "b"#).unwrap_err().code(), "invalid_input");
    }

    #[test]
    fn shell_syntax_is_detected_outside_quotes() {
        assert!(needs_shell("npm run a && npm run b"));
        assert!(needs_shell("node a.js > out.log"));
        assert!(needs_shell("echo %PORT%"));
        assert!(!needs_shell(r#"node "a&b.js" --x "c|d""#));
        assert!(!needs_shell("node server.js --port 3000"));
    }

    fn touch(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), b"").unwrap();
    }

    #[test]
    fn first_word_is_searched_only_in_the_given_path() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("bin").join("node").join("20.1.0");
        let system = tmp.path().join("System32");
        let user = tmp.path().join("user-path");
        touch(&runtime, "node.exe");
        touch(&runtime, "npm.cmd");
        touch(&system, "cmd.exe");
        touch(&system, "ping.exe");
        touch(&user, "php.exe");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let search = [runtime.clone()];
        let sys = [system.clone()];

        assert_eq!(
            plan_launch("node server.js", &cwd, &search, &sys).unwrap(),
            LaunchKind::Direct {
                exe: runtime.join("node.exe")
            }
        );
        assert_eq!(
            plan_launch("ping -n 1 x", &cwd, &search, &sys).unwrap(),
            LaunchKind::Direct {
                exe: system.join("ping.exe")
            }
        );
        let shell = LaunchKind::Shell {
            cmd_exe: system.join("cmd.exe"),
        };
        assert_eq!(
            plan_launch("npm run dev", &cwd, &search, &sys).unwrap(),
            shell
        );
        assert_eq!(plan_launch("echo hi", &cwd, &search, &sys).unwrap(), shell);
        assert_eq!(
            plan_launch("node a.js && node b.js", &cwd, &search, &sys).unwrap(),
            shell
        );
        // A php.exe in the user's PATH is invisible.
        let err = plan_launch("php artisan queue:work", &cwd, &search, &sys).unwrap_err();
        assert_eq!(err.code(), "not_found");
        // Words with a directory part resolve against the project folder.
        touch(&cwd.join("bin"), "serve.exe");
        assert_eq!(
            plan_launch(r".\bin\serve --x", &cwd, &search, &sys).unwrap(),
            LaunchKind::Direct {
                exe: cwd.join("bin").join("serve.exe")
            }
        );
        assert_eq!(
            plan_launch("", &cwd, &search, &sys).unwrap_err().code(),
            "invalid_input"
        );
    }

    #[test]
    fn launch_arguments() {
        let direct = LaunchKind::Direct {
            exe: PathBuf::from("C:/n/node.exe"),
        };
        assert_eq!(
            launch_args(&direct, "node a.js --p 1").unwrap(),
            ["a.js", "--p", "1"]
        );
        let shell = LaunchKind::Shell {
            cmd_exe: PathBuf::from("C:/Windows/System32/cmd.exe"),
        };
        assert_eq!(
            launch_args(&shell, "npm run dev").unwrap(),
            ["/d", "/c", "%FORGEBOX_COMMAND%"]
        );
    }
}
