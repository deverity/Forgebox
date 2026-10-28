//! The state shared by the commands of one run: global flags, output, the root and the
//! (lazily connected) API client.

use std::io::{BufRead, IsTerminal};

use forge_ipc::api::{ApiMethod, JobDto};
use forge_ipc::base::paths::ForgeRoot;
use forge_ipc::{CallOptions, Client};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::OnceCell;

use crate::cli::GlobalArgs;
use crate::connect;
use crate::error::{CliError, CliResult};
use crate::messages::Msg;
use crate::output::Output;
use crate::remotes::Remotes;

/// Everything a command needs.
pub struct Context {
    pub global: GlobalArgs,
    pub out: Output,
    pub root: ForgeRoot,
    client: OnceCell<Client>,
}

impl Context {
    pub fn new(global: GlobalArgs, out: Output) -> CliResult<Self> {
        let (root, _source) = ForgeRoot::discover(global.root.clone())
            .map_err(|e| CliError::new(e.code(), e.to_string()))?;
        Ok(Self {
            global,
            out,
            root,
            client: OnceCell::new(),
        })
    }

    /// The saved profile in effect, if any is not the local daemon.
    fn remote_profile(&self) -> CliResult<Option<String>> {
        let remotes = Remotes::load()?;
        let Some(name) = remotes.effective(self.global.context.as_deref()) else {
            return Ok(None);
        };
        if !remotes.profiles.contains_key(name) {
            return Err(CliError::new(
                "not_found",
                self.out.tf(Msg::RemoteProfileMissing, &[&name]),
            ));
        }
        Ok(Some(name.to_owned()))
    }

    /// The API client, starting the daemon when needed and allowed.
    pub async fn client(&self) -> CliResult<&Client> {
        self.client
            .get_or_try_init(|| async {
                if let Some(name) = self.remote_profile()? {
                    return Err(CliError::not_implemented(
                        self.out.tf(Msg::RemoteNotAvailable, &[&name]),
                    ));
                }
                connect::connect_or_start(&self.root, self.global.no_daemon, &self.out).await
            })
            .await
    }

    /// The API client only if the daemon is already running (never starts it).
    pub async fn existing_client(&self) -> CliResult<Option<Client>> {
        if let Some(name) = self.remote_profile()? {
            return Err(CliError::not_implemented(
                self.out.tf(Msg::RemoteNotAvailable, &[&name]),
            ));
        }
        match connect::try_connect(&self.root).await {
            Ok(client) => Ok(Some(client)),
            Err(connect::Failure::Down(_)) => Ok(None),
            Err(connect::Failure::Other(err)) => Err(err),
        }
    }

    /// Calls a method of kind `call`.
    pub async fn call<M: ApiMethod>(&self, params: M::Params) -> CliResult<M::Result> {
        Ok(self.client().await?.call::<M>(params).await?)
    }

    /// Calls a method with a longer deadline (answers that read from the network or disk).
    pub async fn call_slow<M: ApiMethod>(&self, params: M::Params) -> CliResult<M::Result> {
        let client = self.client().await?;
        let options = CallOptions {
            timeout: Some(std::time::Duration::from_secs(60)),
            ..CallOptions::default()
        };
        Ok(client.call_with::<M>(params, options).await?)
    }

    /// Starts a job method and waits for it (progress on a terminal), returning the job's
    /// result.
    pub async fn job<M, R>(&self, params: M::Params) -> CliResult<R>
    where
        M: ApiMethod<Result = JobDto>,
        R: DeserializeOwned + Serialize,
    {
        crate::jobs::run::<M, R>(self, params).await
    }

    /// Asks for confirmation of a destructive action: `--yes`, or a yes at the prompt
    /// when there is a terminal; otherwise a usage error (exit 2, docs/09).
    pub fn confirm(&self, question: &str) -> CliResult<()> {
        if self.global.yes {
            return Ok(());
        }
        if !std::io::stdin().is_terminal() {
            return Err(CliError::usage(
                self.out.tf(Msg::ConfirmNeedsYes, &[&question]),
            ));
        }
        eprint!("{question} {} ", self.out.t(Msg::ConfirmPrompt));
        let mut answer = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut answer)
            .map_err(CliError::from)?;
        match answer.trim().to_lowercase().as_str() {
            "s" | "sim" | "y" | "yes" => Ok(()),
            _ => Err(CliError::new("cancelled", self.out.t(Msg::Cancelled))),
        }
    }

    /// Announces the UAC prompt of an action that needs the elevated helper, or refuses
    /// it with `--no-elevate` (exit 5, docs/09).
    pub fn elevation(&self, action: &str) -> CliResult<()> {
        if self.global.no_elevate {
            return Err(CliError::new(
                "needs_elevation",
                self.out.tf(Msg::NeedsElevation, &[&action]),
            ));
        }
        self.out.note(self.out.t(Msg::UacNotice));
        Ok(())
    }

    /// The current directory as the daemon wants paths (absolute).
    pub fn cwd(&self) -> CliResult<String> {
        let dir = std::env::current_dir().map_err(CliError::from)?;
        crate::parse::absolute(&dir)
    }

    /// `dir` or the current directory, absolute.
    pub fn dir_or_cwd(&self, dir: Option<&std::path::Path>) -> CliResult<String> {
        match dir {
            Some(dir) => crate::parse::absolute(dir),
            None => self.cwd(),
        }
    }
}
