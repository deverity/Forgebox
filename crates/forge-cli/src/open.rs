//! Opening files in the user's editor (`forge service config`, `forge php ini --open`):
//! the daemon only returns the paths (docs/09 mapping); the editor runs on the machine
//! of the person typing, so it works the same against a remote daemon.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{CliError, CliResult};

/// The editor command: `FORGE_EDITOR`, `VISUAL`, `EDITOR`, then Notepad on Windows.
pub fn editor_command() -> Option<Vec<String>> {
    for var in ["FORGE_EDITOR", "VISUAL", "EDITOR"] {
        if let Some(value) = std::env::var(var).ok().filter(|v| !v.trim().is_empty()) {
            return Some(split_command(&value));
        }
    }
    cfg!(windows).then(|| vec!["notepad.exe".to_owned()])
}

/// Splits `code --wait` into words, honouring double quotes (`"C:\Program Files\x.exe" -n`).
pub fn split_command(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
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
    if started {
        words.push(current);
    }
    words
}

/// Starts the editor on `files` without waiting for it.
pub fn open_in_editor(files: &[&Path]) -> CliResult<()> {
    let command = editor_command()
        .ok_or_else(|| CliError::io("defina EDITOR, VISUAL ou FORGE_EDITOR para abrir arquivos"))?;
    let (program, args) = command
        .split_first()
        .ok_or_else(|| CliError::io("o editor configurado está vazio"))?;
    Command::new(program)
        .args(args)
        .args(files)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| CliError::io(format!("não foi possível abrir o editor {program}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_split_like_a_shell_would() {
        assert_eq!(split_command("code --wait"), ["code", "--wait"]);
        assert_eq!(
            split_command(r#""C:\Program Files\Editor\ed.exe" -n"#),
            [r"C:\Program Files\Editor\ed.exe", "-n"]
        );
        assert_eq!(split_command("  a   b "), ["a", "b"]);
        assert!(split_command("   ").is_empty());
        assert_eq!(split_command(r#"x """#), ["x", ""]);
    }
}
