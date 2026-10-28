//! Command line of `forgebox-helper` (hand-written: the elevated binary keeps its
//! dependencies to the minimum).
//!
//! ```text
//! forgebox-helper --request <file> --response <file> [--dry-run]   # daemon (ADR-0022)
//! forgebox-helper --dry-run --request <file>                       # answer on stdout
//! forgebox-helper --dry-run --request -                            # request on stdin
//! forgebox-helper --version | --help
//! ```

use std::ffi::OsString;
use std::path::PathBuf;

/// Where the request comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// Standard input (`--request -`); only with `--dry-run`.
    Stdin,
    /// `<root>/state/locks/helper-<id>.request.json`.
    File(PathBuf),
}

/// A request to handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunArgs {
    pub request: Input,
    /// `<root>/state/locks/helper-<id>.response.json`; standard output when absent
    /// (dry runs only).
    pub response: Option<PathBuf>,
    /// Validate and plan, change nothing.
    pub dry_run: bool,
}

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run(RunArgs),
    Help,
    Version,
}

/// Parses the arguments (without the program name). The error is a message for stderr.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut request: Option<Input> = None;
    let mut response: Option<PathBuf> = None;
    let mut dry_run = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let Some(flag) = arg.to_str() else {
            return Err(format!("unexpected argument {arg:?}"));
        };
        match flag {
            "--help" | "-h" => return Ok(Command::Help),
            "--version" | "-V" => return Ok(Command::Version),
            "--dry-run" => {
                if dry_run {
                    return Err("--dry-run given twice".to_owned());
                }
                dry_run = true;
            }
            "--request" => {
                let value = args.next().ok_or("--request needs a value")?;
                if request.is_some() {
                    return Err("--request given twice".to_owned());
                }
                request = Some(if value == "-" {
                    Input::Stdin
                } else {
                    Input::File(PathBuf::from(value))
                });
            }
            "--response" => {
                let value = args.next().ok_or("--response needs a value")?;
                if response.is_some() {
                    return Err("--response given twice".to_owned());
                }
                response = Some(PathBuf::from(value));
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    let request = request.ok_or("--request is required")?;
    match (&request, &response, dry_run) {
        (Input::Stdin, _, false) => {
            Err("a request on stdin is only accepted with --dry-run".to_owned())
        }
        (Input::Stdin, Some(_), true) => {
            Err("with --request - the response goes to stdout; drop --response".to_owned())
        }
        (Input::File(_), None, false) => {
            Err("--response is required (unless --dry-run)".to_owned())
        }
        _ => Ok(Command::Run(RunArgs {
            request,
            response,
            dry_run,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn daemon_and_dry_run_forms() {
        assert_eq!(
            p(&["--request", "a.json", "--response", "b.json"]).unwrap(),
            Command::Run(RunArgs {
                request: Input::File("a.json".into()),
                response: Some("b.json".into()),
                dry_run: false,
            })
        );
        assert_eq!(
            p(&["--dry-run", "--request", "-"]).unwrap(),
            Command::Run(RunArgs {
                request: Input::Stdin,
                response: None,
                dry_run: true,
            })
        );
        assert_eq!(
            p(&["--request", "a.json", "--dry-run"]).unwrap(),
            Command::Run(RunArgs {
                request: Input::File("a.json".into()),
                response: None,
                dry_run: true,
            })
        );
        assert_eq!(p(&["--version"]).unwrap(), Command::Version);
        assert_eq!(p(&["--request", "x", "--help"]).unwrap(), Command::Help);
    }

    #[test]
    fn refusals() {
        for bad in [
            &[][..],
            &["--request"],
            &["--request", "-"],
            &["--request", "-", "--response", "r", "--dry-run"],
            &["--request", "a.json"],
            &["--request", "a", "--request", "b", "--dry-run"],
            &["--response", "r", "--response", "s", "--request", "a"],
            &["--dry-run", "--dry-run", "--request", "-"],
            &["--request", "a", "--response", "b", "--shell", "cmd"],
            &["/request", "a"],
        ] {
            assert!(p(bad).is_err(), "{bad:?} must be refused");
        }
    }
}
