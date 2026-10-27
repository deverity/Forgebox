//! `AppSpec`: one process of a site, ready to supervise (docs/06 §4.2, Procfile 2.0).
//!
//! Building one is two steps because the port is allocated between them (asynchronously,
//! by the port allocator):
//!
//! 1. [`AppPlan::build`] (blocking: reads folders): validates the process, resolves its
//!    runtime ([`crate::apps::runtime`]), the deterministic `PATH`, the executable and the
//!    log file, and says what port it wants ([`PortRequest`]);
//! 2. [`AppPlan::finish`] (pure): given the port, substitutes `$PORT` in the command and
//!    the environment, sets `PORT` and `FORGEBOX_SITE` and produces the [`AppSpec`].
//!
//! The environment of the process is, in increasing priority: the variables the runtime
//! tool injects (`config.env` of its manifest), `env` of the site, `env` of the process,
//! then `PORT`, `FORGEBOX_SITE` (and `FORGEBOX_COMMAND` for shell-launched commands). The
//! user's own environment is never inherited, and neither is their `PATH`: the process
//! gets the runtime folders, `extra_path` and the system folder (docs/03 §1.2).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::ids::{InstanceId, SiteId, ToolId};
use forge_core::model::port::{OwnerKind, ReservationOwner};
use forge_core::paths::ForgeRoot;
use forge_core::store::sites::Site;
use forge_manifest::schema::common::RestartOn;
use forge_manifest::schema::project::{ProcessAutostart, ProcessPort, ProcessSpec, ProjectRuntime};
use forge_supervisor::{
    Backoff, EnvMap, HealthSpec, InstanceSpec, LogOptions, PrecheckSpec, Probe, RestartPolicy,
    SpawnSpec, StopPlan,
};

use super::command::{
    LaunchKind, SHELL_COMMAND_VAR, launch_args, plan_launch, references_port, split_args,
    substitute_port,
};
use super::runtime::{ResolvedRuntime, RuntimeCatalog, parse_runtime_ref};
use crate::messages;

/// Port range of `forgebox.toml` that apps get their `$PORT` from (docs/03 §4).
pub const APPS_RANGE: &str = "apps";

/// Tool id the instances of apps are published under (`service.*` events name them
/// `app/<host>.<process>`).
pub const APP_TOOL: &str = "app";

/// Folder of `logs/` with one subfolder per app host (`logs/apps/<host>/<name>.log`).
pub const APPS_LOG_DIR: &str = "apps";

/// Time a dev server gets to listen on its port after it starts. Generous: the first
/// compilation of a Next.js or Vite project is slow on a cold disk cache.
pub const APP_START_TIMEOUT: Duration = Duration::from_secs(120);

/// First graceful stop step of an app; dev servers exit on `CTRL_BREAK` at once.
pub const APP_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// The processes of `Site::processes` (an object name → [`ProcessSpec`]; the empty array
/// of a site without processes is accepted too).
pub fn processes_of(value: &serde_json::Value) -> Result<BTreeMap<String, ProcessSpec>> {
    match value {
        serde_json::Value::Null => Ok(BTreeMap::new()),
        serde_json::Value::Array(items) if items.is_empty() => Ok(BTreeMap::new()),
        serde_json::Value::Object(_) => serde_json::from_value(value.clone())
            .map_err(|e| ForgeError::ConfigInvalid(format!("site processes: {e}"))),
        _ => Err(ForgeError::ConfigInvalid(
            "site processes must be an object of process specs".to_owned(),
        )),
    }
}

/// The JSON stored in `Site::processes` (the inverse of [`processes_of`]).
pub fn processes_to_json(processes: &BTreeMap<String, ProcessSpec>) -> serde_json::Value {
    if processes.is_empty() {
        return serde_json::Value::Array(Vec::new());
    }
    serde_json::to_value(processes).unwrap_or(serde_json::Value::Null)
}

fn valid_process_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Checks names and that at most one process proxies the whole host.
pub fn validate_processes(host: &str, processes: &BTreeMap<String, ProcessSpec>) -> Result<()> {
    let mut proxied: Option<&str> = None;
    for (name, spec) in processes {
        if !valid_process_name(name) {
            return Err(ForgeError::InvalidInput(
                messages::process_name_invalid(name).1,
            ));
        }
        if spec.cmd.trim().is_empty() {
            return Err(ForgeError::InvalidInput(format!(
                "o processo '{name}' não tem comando (cmd)"
            )));
        }
        if spec.proxy {
            if let Some(first) = proxied {
                return Err(ForgeError::ConfigInvalid(
                    messages::process_proxy_twice(host, first, name).1,
                ));
            }
            proxied = Some(name);
        }
    }
    Ok(())
}

/// The process that receives the whole host (`proxy: true`) of an app site.
pub fn proxied_process<'a>(
    host: &str,
    processes: &'a BTreeMap<String, ProcessSpec>,
) -> Result<(&'a String, &'a ProcessSpec)> {
    validate_processes(host, processes)?;
    processes
        .iter()
        .find(|(_, spec)| spec.proxy)
        .ok_or_else(|| ForgeError::ConfigInvalid(messages::app_without_proxy(host).1))
}

/// A site's processes and where they run.
#[derive(Debug, Clone, PartialEq)]
pub struct AppSite {
    pub id: SiteId,
    /// Primary host (`app.test`): names the log folder and `FORGEBOX_SITE`.
    pub host: String,
    pub aliases: Vec<String>,
    /// Project folder: base of every process `cwd` and limit of version-file lookups.
    pub dir: PathBuf,
    /// `env` of the site, below the `env` of each process.
    pub env: BTreeMap<String, String>,
    pub processes: BTreeMap<String, ProcessSpec>,
    /// `spec.runtimes[]` of `forgebox.yaml`.
    pub runtimes: Vec<ProjectRuntime>,
    /// Folders appended to the `PATH` after the runtime's (dependencies such as
    /// `bin/vcredist`).
    pub extra_path: Vec<PathBuf>,
}

impl AppSite {
    /// The processes stored in `site`, running in the project folder `dir`.
    pub fn from_site(site: &Site, dir: PathBuf, runtimes: Vec<ProjectRuntime>) -> Result<Self> {
        let processes = processes_of(&site.processes)?;
        validate_processes(&site.host, &processes)?;
        Ok(Self {
            id: site.id,
            host: site.host.clone(),
            aliases: site.aliases.clone(),
            dir,
            env: site.env.clone(),
            processes,
            runtimes,
            extra_path: Vec::new(),
        })
    }

    /// Every host the site answers to: the primary first, then the aliases.
    pub fn hosts(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.host.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

/// What a plan needs from the machine.
#[derive(Debug, Clone)]
pub struct AppEnv {
    pub root: ForgeRoot,
    pub catalog: RuntimeCatalog,
    /// System folders appended to the `PATH` (`%SystemRoot%\System32`...), from
    /// `ProcessControl::system_path`.
    pub system_path: Vec<PathBuf>,
}

/// The port a process asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortRequest {
    /// Nothing listens (a queue worker).
    None,
    /// The next free port of the `apps` range.
    Auto,
    /// This port, reserved for the process.
    Fixed(u16),
}

/// The port a process asks for: its `port:` (`auto` or a number), else `auto` when it
/// proxies the host or mentions `$PORT` in its command or environment, else none.
pub fn port_request_for(site_env: &BTreeMap<String, String>, spec: &ProcessSpec) -> PortRequest {
    match spec.port {
        Some(ProcessPort::Fixed(port)) => PortRequest::Fixed(port),
        Some(ProcessPort::Auto) => PortRequest::Auto,
        None => {
            let mentioned = references_port(&spec.cmd)
                || site_env
                    .values()
                    .chain(spec.env.values())
                    .any(|value| references_port(value));
            if spec.proxy || mentioned {
                PortRequest::Auto
            } else {
                PortRequest::None
            }
        }
    }
}

/// A validated process waiting for its port.
#[derive(Debug, Clone, PartialEq)]
pub struct AppPlan {
    pub host: String,
    pub site_id: SiteId,
    pub name: String,
    command: String,
    cwd: PathBuf,
    runtime: Option<ResolvedRuntime>,
    path: Vec<PathBuf>,
    launch: LaunchKind,
    env: Vec<(String, String)>,
    port: PortRequest,
    proxy: bool,
    proxy_path: Option<String>,
    restart: RestartOn,
    autostart: ProcessAutostart,
    log_file: PathBuf,
}

/// `cwd` of a process: relative to the project, inside it.
fn process_cwd(site: &AppSite, name: &str, cwd: Option<&str>) -> Result<PathBuf> {
    let Some(cwd) = cwd.filter(|c| !c.is_empty() && *c != ".") else {
        return Ok(site.dir.clone());
    };
    let rel = Path::new(cwd);
    let escapes = rel.is_absolute()
        || rel.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        });
    if escapes {
        return Err(ForgeError::InvalidInput(
            messages::process_cwd_outside(name, cwd).1,
        ));
    }
    Ok(site.dir.join(rel))
}

impl AppPlan {
    /// Validates process `name` of `site` and resolves everything that does not depend
    /// on its port. Blocking.
    pub fn build(site: &AppSite, name: &str, spec: &ProcessSpec, env: &AppEnv) -> Result<Self> {
        validate_processes(
            &site.host,
            &BTreeMap::from([(name.to_owned(), spec.clone())]),
        )?;
        let cwd = process_cwd(site, name, spec.cwd.as_deref())?;
        if !cwd.is_dir() {
            return Err(ForgeError::NotFound(format!(
                "pasta do processo '{name}': {}",
                cwd.display()
            )));
        }

        // The runtime: declared, or implied by the first word of the command.
        let (tool, declared) = match &spec.runtime {
            Some(text) => {
                let (tool, selector) = parse_runtime_ref(text)?;
                (Some(tool), Some(selector))
            }
            None => (implied_tool(&env.catalog, &spec.cmd), None),
        };
        let runtime = match tool {
            Some(tool) => {
                let project_selector = site
                    .runtimes
                    .iter()
                    .find(|r| r.tool == tool)
                    .and_then(|r| r.version.as_deref().or(r.series.as_deref()));
                Some(env.catalog.resolve(
                    &tool,
                    declared.as_deref(),
                    project_selector,
                    &cwd,
                    &site.dir,
                )?)
            }
            None => None,
        };

        // PATH: the runtime, then the dependencies; the system folder is appended by the
        // supervisor. The user's PATH never takes part.
        let mut path: Vec<PathBuf> = runtime.iter().flat_map(|r| r.path.clone()).collect();
        path.extend(site.extra_path.iter().cloned());
        let launch = plan_launch(&spec.cmd, &cwd, &path, &env.system_path)?;

        // Environment templates, lowest priority first.
        let mut vars: Vec<(String, String)> = Vec::new();
        let mut put = |name: &str, value: &str| match vars
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(name))
        {
            Some(i) => vars[i].1 = value.to_owned(),
            None => vars.push((name.to_owned(), value.to_owned())),
        };
        for (n, v) in runtime.iter().flat_map(|r| r.env.iter()) {
            put(n, v);
        }
        for (n, v) in &site.env {
            put(n, v);
        }
        for (n, v) in &spec.env {
            put(n, v);
        }

        let port = port_request_for(&site.env, spec);

        Ok(Self {
            host: site.host.clone(),
            site_id: site.id,
            name: name.to_owned(),
            command: spec.cmd.trim().to_owned(),
            cwd,
            runtime,
            path,
            launch,
            env: vars,
            port,
            proxy: spec.proxy,
            proxy_path: spec.proxy_path.clone(),
            restart: spec.restart,
            autostart: spec.autostart,
            log_file: env
                .root
                .logs()
                .join(APPS_LOG_DIR)
                .join(&site.host)
                .join(format!("{name}.log")),
        })
    }

    /// The port the process asks for.
    pub fn port_request(&self) -> PortRequest {
        self.port
    }

    /// The reservation owner of the process' port.
    pub fn port_owner(&self) -> Result<ReservationOwner> {
        ReservationOwner::new(OwnerKind::App, self.site_id.to_string(), self.name.clone())
    }

    /// The final spec, with `port` substituted (`None` only when the plan asked for
    /// [`PortRequest::None`]).
    pub fn finish(self, port: Option<u16>) -> Result<AppSpec> {
        if port.is_none() && self.port != PortRequest::None {
            return Err(ForgeError::Internal(format!(
                "process {} asked for a port but none was given",
                self.name
            )));
        }
        let command = substitute_port(&self.command, port)?;
        let args = launch_args(&self.launch, &command)?;
        let (exe, shell) = match &self.launch {
            LaunchKind::Direct { exe } => (exe.clone(), false),
            LaunchKind::Shell { cmd_exe } => (cmd_exe.clone(), true),
        };
        let mut env = EnvMap::new();
        for (name, value) in &self.env {
            env.set(name, substitute_port(value, port)?);
        }
        if let Some(port) = port {
            env.set("PORT", port.to_string());
        }
        env.set("FORGEBOX_SITE", &self.host);
        if shell {
            env.set(SHELL_COMMAND_VAR, &command);
        }
        Ok(AppSpec {
            host: self.host,
            site_id: self.site_id,
            name: self.name,
            command,
            exe,
            args,
            cwd: self.cwd,
            env,
            path: self.path,
            port,
            proxy: self.proxy,
            proxy_path: self.proxy_path,
            runtime: self.runtime,
            shell,
            restart: self.restart,
            autostart: self.autostart,
            log_file: self.log_file,
        })
    }
}

/// The tool implied by the first word of a command (`npm run dev` → the tool that ships
/// `npm`), from the manifests' `install.shims`.
fn implied_tool(catalog: &RuntimeCatalog, command: &str) -> Option<ToolId> {
    // A quoting error is reported by the launch planning, not here.
    let first = split_args(command).ok()?.into_iter().next()?;
    let word = first.rsplit(['/', '\\']).next().unwrap_or("");
    catalog.tool_for_command(word).map(|tool| tool.id.clone())
}

/// A process of a site, ready to hand to the supervisor.
#[derive(Debug, Clone, PartialEq)]
pub struct AppSpec {
    pub host: String,
    pub site_id: SiteId,
    pub name: String,
    /// The command line with `$PORT` substituted.
    pub command: String,
    /// Executable started: the resolved program or `cmd.exe` ([`AppSpec::shell`]).
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Variables of the process (the supervisor adds the minimal system environment).
    pub env: EnvMap,
    /// `PATH` entries before the system folder: the runtime's, then the dependencies.
    pub path: Vec<PathBuf>,
    pub port: Option<u16>,
    /// The site host proxies to this process.
    pub proxy: bool,
    pub proxy_path: Option<String>,
    pub runtime: Option<ResolvedRuntime>,
    /// Started through `cmd.exe /d /c %FORGEBOX_COMMAND%`.
    pub shell: bool,
    pub restart: RestartOn,
    pub autostart: ProcessAutostart,
    /// `logs/apps/<host>/<name>.log`.
    pub log_file: PathBuf,
}

impl AppSpec {
    /// Name of the instance (`app.test.web`), unique per host and process.
    pub fn instance_name(&self) -> String {
        format!("{}.{}", self.host, self.name)
    }

    /// What the supervisor spawns.
    pub fn spawn_spec(&self) -> SpawnSpec {
        let mut spec = SpawnSpec::new(&self.exe, &self.cwd);
        spec.args = self.args.clone();
        spec.env = self.env.clone();
        spec.path = self.path.clone();
        spec
    }

    /// The supervisor spec: logs to `logs/apps/<host>/<name>.log`, the restart policy of
    /// the process, a TCP health check on its port (so `Running` means "listening"), the
    /// port pre-check and a short graceful stop.
    pub fn instance_spec(&self) -> Result<InstanceSpec> {
        let tool = ToolId::new(APP_TOOL)?;
        let mut spec = InstanceSpec::new(
            InstanceId::new(),
            tool,
            self.instance_name(),
            self.spawn_spec(),
        );
        spec.logs = LogOptions {
            file: Some(self.log_file.clone()),
            ..LogOptions::default()
        };
        spec.restart = match self.restart {
            RestartOn::Never => RestartPolicy::Never,
            RestartOn::Failure => RestartPolicy::default(),
            RestartOn::Always => RestartPolicy::Always {
                backoff: Backoff::default(),
            },
        };
        spec.stop = StopPlan {
            timeout: APP_STOP_TIMEOUT,
            ..StopPlan::default()
        };
        if let Some(port) = self.port {
            let mut health = HealthSpec::new(Probe::Tcp {
                addr: format!("127.0.0.1:{port}"),
            });
            health.start_timeout = APP_START_TIMEOUT;
            spec.health = Some(health);
            spec.precheck = PrecheckSpec {
                ports: vec![port],
                ..PrecheckSpec::default()
            };
        }
        Ok(spec)
    }

    /// `http://127.0.0.1:<port>` when the process listens.
    pub fn upstream(&self) -> Option<String> {
        self.port.map(crate::proxy::loopback_upstream)
    }

    /// `logs/apps/<host>`: the folder with the logs of every process of the host.
    pub fn log_dir(&self) -> Option<&Path> {
        self.log_file.parent()
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::apps::runtime::RuntimeTool;

    fn node_tool() -> RuntimeTool {
        RuntimeTool {
            id: ToolId::new("nodejs").unwrap(),
            commands: ["node", "npm", "npx"].map(String::from).to_vec(),
            version_files: vec![".nvmrc".into()],
            path: vec![".".into()],
            env: [
                (
                    "NODE_EXTRA_CA_CERTS".to_owned(),
                    "{{ paths.certs }}/ca/forgebox-ca.crt".to_owned(),
                ),
                (
                    "npm_config_prefix".to_owned(),
                    "{{ paths.data }}/node/{{ version }}/npm-global".to_owned(),
                ),
            ]
            .into(),
            series_key: "{major}".into(),
        }
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        env: AppEnv,
        site: AppSite,
        node: PathBuf,
        system: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(tmp.path().join("fb")).unwrap();
        let node = root.bin().join("nodejs").join("22.23.4");
        let older = root.bin().join("nodejs").join("20.12.2");
        let system = tmp.path().join("System32");
        let project = root.www().join("app");
        for dir in [&node, &older, &system, &project, &project.join("web")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for (dir, file) in [
            (&node, "node.exe"),
            (&node, "npm.cmd"),
            (&older, "node.exe"),
            (&older, "npm.cmd"),
            (&system, "cmd.exe"),
        ] {
            std::fs::write(dir.join(file), b"").unwrap();
        }
        let catalog = RuntimeCatalog::new(root.clone(), [node_tool()]);
        let site = AppSite {
            id: SiteId::new(),
            host: "app.test".to_owned(),
            aliases: vec![],
            dir: project,
            env: BTreeMap::from([("APP_ENV".to_owned(), "local".to_owned())]),
            processes: BTreeMap::new(),
            runtimes: vec![],
            extra_path: vec![],
        };
        Fixture {
            env: AppEnv {
                root,
                catalog,
                system_path: vec![system.clone()],
            },
            site,
            node,
            system,
            _tmp: tmp,
        }
    }

    fn process(yaml: &str) -> ProcessSpec {
        serde_yaml_ng::from_str(yaml).unwrap()
    }

    #[test]
    fn procfile_entry_becomes_a_spec_with_port_substituted() {
        let f = fixture();
        let spec = process(
            "{ cmd: 'npm run dev -- --port $PORT', port: auto, proxy: true, runtime: 'nodejs@22', env: { NEXT_PUBLIC_URL: 'https://app.test', INTERNAL: 'http://127.0.0.1:$PORT' } }",
        );
        let plan = AppPlan::build(&f.site, "web", &spec, &f.env).unwrap();
        assert_eq!(plan.port_request(), PortRequest::Auto);
        let app = plan.finish(Some(3042)).unwrap();
        assert_eq!(app.command, "npm run dev -- --port 3042");
        assert!(app.shell, "npm is a .cmd: it goes through cmd.exe");
        assert_eq!(app.exe, f.system.join("cmd.exe"));
        assert_eq!(app.args, ["/d", "/c", "%FORGEBOX_COMMAND%"]);
        assert_eq!(
            app.env.get("FORGEBOX_COMMAND"),
            Some("npm run dev -- --port 3042")
        );
        assert_eq!(app.env.get("PORT"), Some("3042"));
        assert_eq!(app.env.get("FORGEBOX_SITE"), Some("app.test"));
        assert_eq!(app.env.get("INTERNAL"), Some("http://127.0.0.1:3042"));
        assert_eq!(app.env.get("APP_ENV"), Some("local"));
        assert_eq!(app.env.get("NEXT_PUBLIC_URL"), Some("https://app.test"));
        let root = f.env.root.to_posix_string();
        assert_eq!(
            app.env.get("NODE_EXTRA_CA_CERTS"),
            Some(format!("{root}/certs/ca/forgebox-ca.crt").as_str())
        );
        assert_eq!(
            app.env.get("npm_config_prefix"),
            Some(format!("{root}/data/node/22.23.4/npm-global").as_str())
        );
        assert_eq!(app.port, Some(3042));
        assert!(app.proxy);
        assert_eq!(app.upstream().as_deref(), Some("http://127.0.0.1:3042"));
        assert_eq!(app.runtime.as_ref().unwrap().folder, "22.23.4");
        assert_eq!(
            app.log_file,
            f.env
                .root
                .logs()
                .join("apps")
                .join("app.test")
                .join("web.log")
        );
    }

    #[test]
    fn path_holds_only_the_runtime_the_dependencies_and_the_system_folder() {
        let mut f = fixture();
        let vcredist = f.env.root.bin().join("vcredist");
        f.site.extra_path.push(vcredist.clone());
        let spec = process("{ cmd: 'node server.js $PORT', runtime: 'nodejs@20' }");
        let app = AppPlan::build(&f.site, "api", &spec, &f.env)
            .unwrap()
            .finish(Some(3001))
            .unwrap();
        // node.exe is found in the runtime folder: direct launch, no cmd.exe.
        assert!(!app.shell);
        assert_eq!(
            app.exe,
            f.env
                .root
                .bin()
                .join("nodejs")
                .join("20.12.2")
                .join("node.exe")
        );
        assert_eq!(app.args, ["server.js", "3001"]);
        let sep = if cfg!(windows) { ";" } else { ":" };
        let base = vec![
            (
                std::ffi::OsString::from("SystemRoot"),
                std::ffi::OsString::from(r"C:\Windows"),
            ),
            (
                std::ffi::OsString::from("Path"),
                std::ffi::OsString::from(r"C:\xampp\php;C:\Users\me\bin"),
            ),
        ];
        let env = app
            .spawn_spec()
            .environment(base, &[f.system.clone()])
            .unwrap();
        let path = env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.to_string_lossy().into_owned())
            .unwrap();
        let expected = [
            f.env.root.bin().join("nodejs").join("20.12.2"),
            vcredist,
            f.system.clone(),
        ]
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(sep);
        assert_eq!(path, expected);
        assert!(!path.contains("xampp") && !path.contains(r"C:\Users\me"));
    }

    #[test]
    fn runtime_is_implied_by_the_command_and_read_from_version_files() {
        let f = fixture();
        std::fs::write(f.site.dir.join(".nvmrc"), "20\n").unwrap();
        let spec = process("{ cmd: 'npm start' }");
        let app = AppPlan::build(&f.site, "web", &spec, &f.env)
            .unwrap()
            .finish(None)
            .unwrap();
        let runtime = app.runtime.unwrap();
        assert_eq!(runtime.folder, "20.12.2");
        assert_eq!(runtime.tool.as_str(), "nodejs");
        assert_eq!(app.path, [f.env.root.bin().join("nodejs").join("20.12.2")]);
        // No $PORT, no proxy: nothing listens, nothing is allocated.
        assert_eq!(app.port, None);
        assert_eq!(app.env.get("PORT"), None);
    }

    #[test]
    fn port_requests() {
        let f = fixture();
        let request = |yaml: &str| {
            AppPlan::build(&f.site, "p", &process(yaml), &f.env)
                .unwrap()
                .port_request()
        };
        assert_eq!(
            request("{ cmd: 'node a.js', runtime: 'nodejs@22' }"),
            PortRequest::None
        );
        assert_eq!(
            request("{ cmd: 'node a.js --port $PORT', runtime: 'nodejs@22' }"),
            PortRequest::Auto
        );
        assert_eq!(
            request("{ cmd: 'node a.js', proxy: true, runtime: 'nodejs@22' }"),
            PortRequest::Auto
        );
        assert_eq!(
            request("{ cmd: 'node a.js', port: 5173, runtime: 'nodejs@22' }"),
            PortRequest::Fixed(5173)
        );
        assert_eq!(
            request("{ cmd: 'node a.js', port: auto, runtime: 'nodejs@22' }"),
            PortRequest::Auto
        );
        // Using $PORT without a port is caught when the spec is finished.
        let plan = AppPlan::build(
            &f.site,
            "p",
            &process("{ cmd: 'node a.js $PORT', runtime: 'nodejs@22' }"),
            &f.env,
        )
        .unwrap();
        assert!(plan.clone().finish(None).is_err());
        assert!(plan.finish(Some(3000)).is_ok());
    }

    #[test]
    fn invalid_processes_are_refused() {
        let f = fixture();
        let build = |name: &str, yaml: &str| {
            AppPlan::build(&f.site, name, &process(yaml), &f.env).unwrap_err()
        };
        assert_eq!(
            build("Web", "{ cmd: 'node a.js', runtime: 'nodejs@22' }").code(),
            "invalid_input"
        );
        assert_eq!(build("web", "{ cmd: '  ' }").code(), "invalid_input");
        assert_eq!(
            build(
                "web",
                "{ cmd: 'node a.js', cwd: '../other', runtime: 'nodejs@22' }"
            )
            .code(),
            "invalid_input"
        );
        assert_eq!(
            build(
                "web",
                "{ cmd: 'node a.js', cwd: 'missing', runtime: 'nodejs@22' }"
            )
            .code(),
            "not_found"
        );
        assert_eq!(
            build("web", "{ cmd: 'node a.js', runtime: 'nodejs@18' }").code(),
            "not_found"
        );
        assert_eq!(
            build("web", "{ cmd: 'node a.js', runtime: 'nodejs' }").code(),
            "invalid_input"
        );
        // A program that is nowhere in the deterministic PATH.
        assert_eq!(
            build("web", "{ cmd: 'php artisan queue:work' }").code(),
            "not_found"
        );
        let mut twice = BTreeMap::new();
        twice.insert("a".to_owned(), process("{ cmd: 'x', proxy: true }"));
        twice.insert("b".to_owned(), process("{ cmd: 'y', proxy: true }"));
        assert_eq!(
            validate_processes("app.test", &twice).unwrap_err().code(),
            "config_invalid"
        );
        assert_eq!(
            proxied_process("app.test", &BTreeMap::new())
                .unwrap_err()
                .code(),
            "config_invalid"
        );
    }

    #[test]
    fn working_folder_is_relative_to_the_project() {
        let f = fixture();
        let spec = process("{ cmd: 'node a.js', cwd: web, runtime: 'nodejs@22' }");
        let app = AppPlan::build(&f.site, "web", &spec, &f.env)
            .unwrap()
            .finish(None)
            .unwrap();
        assert_eq!(app.cwd, f.site.dir.join("web"));
    }

    #[test]
    fn instance_spec_listens_checks_and_restarts_as_declared() {
        let f = fixture();
        let spec = process(
            "{ cmd: 'node a.js $PORT', proxy: true, restart: always, runtime: 'nodejs@22' }",
        );
        let app = AppPlan::build(&f.site, "web", &spec, &f.env)
            .unwrap()
            .finish(Some(3100))
            .unwrap();
        let instance = app.instance_spec().unwrap();
        assert_eq!(instance.tool.as_str(), "app");
        assert_eq!(instance.name, "app.test.web");
        assert_eq!(instance.logs.file.as_deref(), Some(app.log_file.as_path()));
        assert!(matches!(instance.restart, RestartPolicy::Always { .. }));
        assert_eq!(instance.precheck.ports, [3100]);
        let health = instance.health.unwrap();
        assert_eq!(
            health.probe,
            Probe::Tcp {
                addr: "127.0.0.1:3100".to_owned()
            }
        );
        assert_eq!(health.start_timeout, APP_START_TIMEOUT);
        assert_eq!(instance.stop.timeout, APP_STOP_TIMEOUT);

        let worker = process("{ cmd: 'node q.js', restart: never, runtime: 'nodejs@22' }");
        let app = AppPlan::build(&f.site, "queue", &worker, &f.env)
            .unwrap()
            .finish(None)
            .unwrap();
        let instance = app.instance_spec().unwrap();
        assert_eq!(instance.restart, RestartPolicy::Never);
        assert!(instance.health.is_none() && instance.precheck.is_empty());
        assert_eq!(f.node, f.env.root.bin().join("nodejs").join("22.23.4"));
    }

    #[test]
    fn processes_round_trip_through_the_site_json() {
        let mut map = BTreeMap::new();
        map.insert(
            "web".to_owned(),
            process("{ cmd: 'npm run dev', port: auto, proxy: true, autostart: on-visit, restart: always }"),
        );
        let json = processes_to_json(&map);
        assert_eq!(processes_of(&json).unwrap(), map);
        assert_eq!(processes_to_json(&BTreeMap::new()), serde_json::json!([]));
        assert!(processes_of(&serde_json::json!([])).unwrap().is_empty());
        assert!(processes_of(&serde_json::json!(null)).unwrap().is_empty());
        assert!(processes_of(&serde_json::json!("x")).is_err());
        assert!(processes_of(&serde_json::json!({"web": {"nope": 1}})).is_err());
        let (name, spec) = proxied_process("app.test", &map).unwrap();
        assert_eq!(name, "web");
        assert_eq!(spec.autostart, ProcessAutostart::OnVisit);
    }
}
