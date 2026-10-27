//! Forgebox CLI (`forge`): a thin client of the daemon API with human and `--json` output
//! (docs/09). It depends on `forge-ipc` only: every command is a call of the typed
//! client, followed by a rendering.
//!
//! * [`cli`]: the clap tree with every group and command of docs/09 and the global flags;
//! * [`commands`]: one module per group of commands;
//! * [`connect`]: connection to the local daemon, with autostart;
//! * [`jobs`]: following long operations (progress bars on a terminal);
//! * [`output`] / [`render`]: tables, colors (`NO_COLOR`), JSON; [`messages`]: user texts
//!   (pt-BR, en-US with `FORGE_LANG=en`);
//! * [`error`]: stable error codes mapped to the exit codes of docs/09.

pub mod cli;
pub mod commands;
pub mod connect;
pub mod context;
pub mod error;
pub mod jobs;
pub mod messages;
pub mod open;
pub mod output;
pub mod parse;
pub mod remotes;
pub mod render;

use std::ffi::OsString;

use clap::Parser;

use crate::cli::Cli;
use crate::context::Context;
use crate::error::CliError;
use crate::messages::Lang;
use crate::output::Output;

/// Runs `forge` with `args` (the first one is the program name) and returns the process
/// exit code.
pub fn run<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(err) => return usage_error(&args, &err),
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("forge")
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("forge: {err}");
            return 1;
        }
    };
    runtime.block_on(execute(cli))
}

/// Help, version and parse errors. With `--json` a parse error is the usual
/// `{"error": ...}` document (code `invalid_input`, exit 2).
fn usage_error(args: &[OsString], err: &clap::Error) -> i32 {
    let wants_json = args.iter().any(|a| a == "--json");
    if err.use_stderr() && wants_json {
        let error = CliError::usage(err.render().to_string().trim().to_owned());
        Output::new(true, false, Lang::from_env()).error(&error);
        return error.exit;
    }
    let _ = err.print();
    err.exit_code()
}

async fn execute(cli: Cli) -> i32 {
    let out = Output::new(cli.global.json, cli.global.quiet, Lang::from_env());
    let ctx = match Context::new(cli.global, out) {
        Ok(ctx) => ctx,
        Err(err) => {
            out.error(&err);
            return err.exit;
        }
    };
    match commands::run(&ctx, cli.command).await {
        Ok(()) => 0,
        Err(err) => {
            out.error(&err);
            err.exit
        }
    }
}
