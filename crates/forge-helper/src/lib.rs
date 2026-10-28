//! `forgebox-helper`: the only elevated binary of Forgebox (docs/11 §1, ADR-0022,
//! ADR-0024). It runs **one** allowlisted operation per process, started by the daemon
//! through UAC, and exits.
//!
//! A run, in order (any failure ends it with a refusal, nothing changed):
//! 1. open `<root>/logs/helper.log` (no log, no action), where `<root>` is the folder of
//!    the executable;
//! 2. check the exchange files' location (`state/locks/helper-<id>.request.json` and its
//!    `.response.json`) and read the request without following links;
//! 3. parse it ([`HelperRequest::from_json`]: closed allowlist, unknown fields refused);
//! 4. require `request.root` to be the helper's own root;
//! 5. read TLDs and parked directories from **that** root's `etc/forgebox.toml` and
//!    validate the operation ([`HelperOp::validate`]);
//! 6. canonicalize every path of the operation (junctions resolved) and validate again
//!    against the canonical root;
//! 7. without `--dry-run`, require an elevated token, then act ([`ops`]); with it, only
//!    read the current state and answer the plan;
//! 8. write the [`HelperResponse`] and one `helper.log` line.

pub mod args;
pub mod exchange;
pub mod log;
pub mod messages;
pub mod ops;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use forge_core::config::ForgeConfig;
use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::helper::{HelperOp, HelperPolicy, HelperRequest, HelperResponse};
use forge_core::ids::RequestId;
use forge_core::paths::{ForgeRoot, plain_path};
use forge_core::platform::{self, CaseSensitiveFs, CodeSign, PrivilegedOps};
use serde_json::Value;

use crate::args::{Command, Input, RunArgs};
use crate::log::{HelperLog, LogEntry};

/// Binary name.
pub const BIN_NAME: &str = "forgebox-helper";

/// Exit code: the operation ran (or was planned) and the response says `ok`.
pub const EXIT_OK: u8 = 0;
/// Exit code: refused or failed; the response says why.
pub const EXIT_REFUSED: u8 = 1;
/// Exit code: bad command line or exchange files; no response written.
pub const EXIT_USAGE: u8 = 2;
/// Exit code: the response could not be written.
pub const EXIT_NO_RESPONSE: u8 = 3;

/// `forgebox-helper <version>`.
pub fn banner() -> String {
    format!("{BIN_NAME} {}", env!("CARGO_PKG_VERSION"))
}

/// The helper of one root, with the system it acts on.
pub struct Helper {
    root: PathBuf,
    sys: Arc<dyn PrivilegedOps>,
    case_fs: Arc<dyn CaseSensitiveFs>,
    code_sign: Arc<dyn CodeSign>,
}

impl Helper {
    /// The helper of `root` acting on this machine.
    pub fn for_os(root: impl Into<PathBuf>) -> Self {
        let platform = platform::current();
        Self::new(
            root,
            platform::privileged_ops(),
            Arc::new(CaseFs(platform.clone())),
            platform.code_sign().clone(),
        )
    }

    /// The helper of `root` with explicit system parts (fakes in tests).
    pub fn new(
        root: impl Into<PathBuf>,
        sys: Arc<dyn PrivilegedOps>,
        case_fs: Arc<dyn CaseSensitiveFs>,
        code_sign: Arc<dyn CodeSign>,
    ) -> Self {
        Self {
            root: root.into(),
            sys,
            case_fs,
            code_sign,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Handles the request `text`. `file_id` is the id of the request file name (file
    /// mode): it answers a request that cannot be parsed, and the parsed request must
    /// carry the same id. Returns the response and the operation name, when known.
    pub fn handle(
        &self,
        text: &str,
        file_id: Option<RequestId>,
        dry_run: bool,
    ) -> (HelperResponse, Option<&'static str>) {
        let request = match HelperRequest::from_json(text) {
            Ok(request) => request,
            Err(err) => {
                let id = file_id.or_else(|| id_in(text)).unwrap_or_default();
                return (HelperResponse::failure(id, &err), None);
            }
        };
        let name = request.op.name();
        if let Some(expected) = file_id {
            if expected != request.request_id {
                let err = ForgeError::Forbidden(format!(
                    "request id {} does not match the request file of {expected}",
                    request.request_id
                ));
                return (HelperResponse::failure(expected, &err), Some(name));
            }
        }
        let response = match self.execute(&request, dry_run) {
            Ok(outcome) => {
                let mut details = outcome.details;
                if let Value::Object(map) = &mut details {
                    map.insert("dry_run".to_owned(), Value::Bool(dry_run));
                }
                HelperResponse::success(&request, outcome.message).with_details(details)
            }
            Err(err) => HelperResponse::failure(request.request_id, &err),
        };
        (response, Some(name))
    }

    fn execute(&self, request: &HelperRequest, dry_run: bool) -> Result<ops::Outcome> {
        if !same_dir(&request.root, &self.root)? {
            return Err(ForgeError::Forbidden(format!(
                "request is about {}, but this helper belongs to {}",
                request.root.display(),
                self.root.display()
            )));
        }
        // Policy from the helper's own root, never from the request (ADR-0022).
        let config_path = self.root.join("etc").join("forgebox.toml");
        let config = ForgeConfig::load_or_default(&config_path)?;
        let lexical = HelperPolicy::from_config(&ForgeRoot::new(&request.root)?, &config)?;
        request.op.validate(&lexical)?;

        let canonical_root = plain_path(&canonicalize(&self.root)?);
        let canonical_policy = HelperPolicy::new(&canonical_root, lexical.tlds().to_vec())?
            .with_parked_dirs(
                lexical
                    .parked_dirs()
                    .iter()
                    .map(|dir| canonicalize(dir).map_or_else(|_| dir.clone(), |p| plain_path(&p))),
            )?;
        let op = canonical_op(&request.op)?;
        op.validate(&canonical_policy)?;

        if !dry_run && !self.sys.is_elevated()? {
            return Err(ForgeError::NeedsElevation(
                "forgebox-helper must run elevated (started through the UAC prompt)".to_owned(),
            ));
        }
        let env = ops::Env {
            root: &canonical_root,
            sys: self.sys.as_ref(),
            case_fs: self.case_fs.as_ref(),
            code_sign: self.code_sign.as_ref(),
            request_id: request.request_id,
            dry_run,
        };
        ops::run(&env, &op)
    }
}

/// [`CaseSensitiveFs`] of a [`platform::Platform`] (it only lends a reference).
struct CaseFs(platform::Platform);

impl CaseSensitiveFs for CaseFs {
    fn support(&self, dir: &Path) -> platform::CaseSensitivitySupport {
        self.0.case_sensitive_fs().support(dir)
    }
    fn get(&self, dir: &Path) -> Result<bool> {
        self.0.case_sensitive_fs().get(dir)
    }
    fn set(&self, dir: &Path, enabled: bool) -> Result<()> {
        self.0.case_sensitive_fs().set(dir, enabled)
    }
}

fn canonicalize(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path).at("resolving", path)
}

/// Whether `a` and `b` are the same existing directory (junctions, case and short names
/// resolved).
fn same_dir(a: &Path, b: &Path) -> Result<bool> {
    let a = canonicalize(a).map_err(|e| match e {
        ForgeError::NotFound(msg) => ForgeError::Forbidden(format!("request root {msg}")),
        other => other,
    })?;
    let b = canonicalize(b)?;
    Ok(if cfg!(windows) {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    } else {
        a == b
    })
}

/// `op` with every path replaced by its canonical form (must exist).
fn canonical_op(op: &HelperOp) -> Result<HelperOp> {
    let c = |p: &PathBuf| canonicalize(p).map(|p| plain_path(&p));
    Ok(match op {
        HelperOp::FirewallAdd { name, port, exe } => HelperOp::FirewallAdd {
            name: name.clone(),
            port: *port,
            exe: c(exe)?,
        },
        HelperOp::DefenderExclude { path } => HelperOp::DefenderExclude { path: c(path)? },
        HelperOp::DefenderRemove { path } => HelperOp::DefenderRemove { path: c(path)? },
        HelperOp::VcredistInstall { path, sha256 } => HelperOp::VcredistInstall {
            path: c(path)?,
            sha256: sha256.clone(),
        },
        HelperOp::FsSetCaseSensitive { path } => HelperOp::FsSetCaseSensitive { path: c(path)? },
        other => other.clone(),
    })
}

/// The `request_id` of a request that failed to parse, when readable.
fn id_in(text: &str) -> Option<RequestId> {
    serde_json::from_str::<Value>(text)
        .ok()?
        .get("request_id")?
        .as_str()?
        .parse()
        .ok()
}

/// Entry point of the binary.
pub fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let command = match args::parse(args) {
        Ok(command) => command,
        Err(message) => {
            eprintln!(
                "{}: {message}\n\n{}",
                messages::USAGE_ERROR,
                messages::USAGE
            );
            return ExitCode::from(EXIT_USAGE);
        }
    };
    match command {
        Command::Help => {
            println!("{}", messages::USAGE);
            ExitCode::from(EXIT_OK)
        }
        Command::Version => {
            println!("{}", banner());
            ExitCode::from(EXIT_OK)
        }
        Command::Run(run_args) => {
            let root = match std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf))
            {
                Some(root) => root,
                None => {
                    eprintln!("{BIN_NAME}: cannot locate its own folder");
                    return ExitCode::from(EXIT_USAGE);
                }
            };
            ExitCode::from(run(&Helper::for_os(root), &run_args))
        }
    }
}

/// Handles one request as described by `args`; the process exit code.
pub fn run(helper: &Helper, args: &RunArgs) -> u8 {
    let mut log = HelperLog::open(helper.root());

    let (file_id, text) = match &args.request {
        Input::Stdin => (
            None,
            exchange::read_limited(std::io::stdin().lock(), Path::new("<stdin>")),
        ),
        Input::File(path) => {
            match exchange::check_paths(helper.root(), path, args.response.as_deref()) {
                Ok(id) => (Some(id), exchange::read_request(path)),
                Err(err) => {
                    eprintln!("{}: {err}", messages::USAGE_ERROR);
                    if let Ok(log) = &mut log {
                        let _ = log.append(&LogEntry::now(
                            None,
                            None,
                            args.dry_run,
                            false,
                            Some(err.code().to_owned()),
                            err.to_string(),
                        ));
                    }
                    return EXIT_USAGE;
                }
            }
        }
    };

    let (response, op) = match (&mut log, text) {
        (Err(err), _) => {
            let err = ForgeError::Forbidden(format!(
                "logs/helper.log cannot be used, so nothing is done: {err}"
            ));
            let id = file_id.unwrap_or_default();
            (HelperResponse::failure(id, &err), None)
        }
        (Ok(_), Err(err)) => (
            HelperResponse::failure(file_id.unwrap_or_default(), &err),
            None,
        ),
        (Ok(_), Ok(text)) => helper.handle(&text, file_id, args.dry_run),
    };

    if let Ok(log) = &mut log {
        let entry = LogEntry::now(
            Some(response.request_id.to_string()),
            op.map(str::to_owned),
            args.dry_run,
            response.ok,
            response.code.clone(),
            response.message.clone(),
        );
        if let Err(err) = log.append(&entry) {
            eprintln!("{BIN_NAME}: {err}");
        }
    } else if let Err(err) = &log {
        eprintln!("{BIN_NAME}: {err}");
    }

    let written = response.to_json().and_then(|json| match &args.response {
        Some(path) => exchange::write_response(path, &json),
        None => {
            let mut out = std::io::stdout().lock();
            out.write_all(json.as_bytes())
                .and_then(|()| out.write_all(b"\n"))
                .and_then(|()| out.flush())
                .map_err(|e| ForgeError::io("writing the response to stdout", e))
        }
    });
    match written {
        Err(err) => {
            eprintln!("{}: {err}", messages::RESPONSE_NOT_WRITTEN);
            EXIT_NO_RESPONSE
        }
        Ok(()) if response.ok => EXIT_OK,
        Ok(()) => EXIT_REFUSED,
    }
}

#[cfg(test)]
mod tests;
