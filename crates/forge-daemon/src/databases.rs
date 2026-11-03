//! The database engine of the daemon (docs/04 §1.8, docs/09 §db, M2-E3-T5): lists, creates
//! and drops databases, builds connection strings and the shell command, all from the
//! `spec.database` block of the manifest of the instance, so there is no code per tool.
//!
//! * **Instances** are the entries whose manifest declares `spec.database`; `db.list`
//!   reports all of them (also the stopped ones) in `instances`.
//! * **Names** are validated by `forge_manifest::database` (`^[A-Za-z0-9_]{1,64}$`,
//!   charset `^[A-Za-z0-9_]{1,32}$`) before any substitution; every method renders the
//!   command first and only then looks at the instance state or spawns anything, so an
//!   invalid name never reaches a client. Names are **case-sensitive** (docs/15): `Loja`
//!   and `loja` are two databases, and the system database filter compares exactly.
//! * **Clients** run through the one-shot probe runner (own Job Object, 5 s, no window)
//!   inside `spawn_blocking` (ADR-0011) and print TAB separated rows.
//! * **Sizes** of the data folders come from a cached walk (TTL 60 s) done on a blocking
//!   thread: `None` until the first walk ends, so `db.list` never waits for a disk scan.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::error::{ForgeError, Result};
use forge_core::ids::InstanceId;
use forge_core::platform::{PROBE_TIMEOUT, ProbeCommand, ProbeOutput, run_probe};
use forge_core::runtime::spawn_blocking_timeout;
use forge_core::sync::{LockLevel, OrderedMutex};
use forge_ipc::api::common::InstanceKey;
use forge_ipc::api::{
    Ack, DatabaseDto, DatabaseListDto, DbCapabilitiesDto, DbCreateParams, DbDropParams,
    DbInstanceDto, DbListParams, DbShellParams, DbUrlDto, DbUrlParams, EnvVarsDto, InstanceRef,
    ServiceState, ShellCommandDto,
};
use forge_manifest::database::{DbQuery, DbVars, RenderError, RenderedCommand};
use forge_manifest::schema::tool::{DatabaseProtocol, DatabaseSpec};
use forge_supervisor::{InstanceState, QUICK_ASK_TIMEOUT};

use crate::api::convert;
use crate::daemon::Daemon;
use crate::services::{Entry, tail};

/// Time budget of one client invocation.
pub const CLIENT_TIMEOUT: Duration = PROBE_TIMEOUT;
/// How long a measured data folder size is reused.
pub const SIZE_TTL: Duration = Duration::from_secs(60);
/// A walk that takes longer gives up (the previous value stays).
const WALK_BUDGET: Duration = Duration::from_secs(30);

// ------------------------------------------------------------------ size cache

#[derive(Debug, Default)]
struct Slot {
    size: Option<u64>,
    measured: Option<Instant>,
    walking: bool,
}

/// Sizes of the data folders of the database instances, measured off the request path.
#[derive(Debug, Clone)]
pub struct SizeCache {
    slots: Arc<OrderedMutex<HashMap<InstanceId, Slot>>>,
}

impl Default for SizeCache {
    fn default() -> Self {
        Self {
            slots: Arc::new(OrderedMutex::new(LockLevel::DAEMON_TABLES, HashMap::new())),
        }
    }
}

impl SizeCache {
    /// The last measured size of `dir`, `None` before the first walk ends. A missing or
    /// stale (older than [`SIZE_TTL`]) value starts a walk on a blocking thread and the
    /// caller gets the old value at once. Needs a tokio runtime.
    pub fn get(&self, id: InstanceId, dir: Option<PathBuf>) -> Option<u64> {
        let dir = dir?;
        let known = {
            let mut slots = self.slots.lock();
            let slot = slots.entry(id).or_default();
            let fresh = slot.measured.is_some_and(|t| t.elapsed() < SIZE_TTL);
            if fresh || slot.walking {
                return slot.size;
            }
            slot.walking = true;
            slot.size
        };
        let slots = Arc::clone(&self.slots);
        tokio::task::spawn_blocking(move || {
            let size = walk_size(&dir, WALK_BUDGET);
            let mut slots = slots.lock();
            let slot = slots.entry(id).or_default();
            if size.is_some() {
                slot.size = size;
            }
            slot.measured = Some(Instant::now());
            slot.walking = false;
        });
        known
    }
}

/// Total size of the files under `dir` (links are not followed); `None` when the folder
/// is unreadable or the walk passes `budget`.
fn walk_size(dir: &Path, budget: Duration) -> Option<u64> {
    let started = Instant::now();
    let mut total = 0u64;
    let mut pending = vec![dir.to_path_buf()];
    let mut first = true;
    while let Some(current) = pending.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) if first => return None,
            Err(_) => continue,
        };
        first = false;
        for entry in entries.flatten() {
            if started.elapsed() > budget {
                return None;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                total += entry.metadata().map_or(0, |m| m.len());
            }
        }
    }
    Some(total)
}

// ------------------------------------------------------------------ resolution

fn render_error(err: RenderError) -> ForgeError {
    ForgeError::InvalidInput(err.to_string())
}

/// `(entry, spec)` of every instance whose manifest declares `spec.database`.
pub fn database_entries(daemon: &Daemon) -> Vec<(Entry, DatabaseSpec)> {
    daemon
        .services
        .entries()
        .into_iter()
        .filter_map(|entry| {
            let spec = entry.manifest.as_ref()?.spec.database.clone()?;
            Some((entry, spec))
        })
        .collect()
}

/// The database instance a call designates: by ULID, `tool/name`, `tool` (its `default`
/// instance or its only one), or, without a reference, the only/`default` database
/// instance.
pub fn resolve(daemon: &Daemon, reference: Option<&InstanceRef>) -> Result<(Entry, DatabaseSpec)> {
    let all = database_entries(daemon);
    let describe = |items: &[(Entry, DatabaseSpec)]| {
        items
            .iter()
            .map(|(e, _)| e.label())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let candidates: Vec<(Entry, DatabaseSpec)> = match reference.map(InstanceRef::key) {
        None => all,
        Some(InstanceKey::Id(id)) => {
            let Some(entry) = daemon.services.get(id) else {
                return Err(ForgeError::NotFound(format!("instance {id}")));
            };
            if !all.iter().any(|(e, _)| e.row.id == id) {
                return Err(ForgeError::InvalidInput(format!(
                    "{} is not a database instance",
                    entry.label()
                )));
            }
            all.into_iter().filter(|(e, _)| e.row.id == id).collect()
        }
        Some(InstanceKey::Name { tool, name }) => {
            let of_tool: Vec<_> = all
                .into_iter()
                .filter(|(e, _)| {
                    e.row.tool_id.as_str() == tool && name.as_ref().is_none_or(|n| *n == e.row.name)
                })
                .collect();
            if of_tool.is_empty() {
                let known = daemon
                    .services
                    .resolve(&reference.map_or_else(String::new, |r| r.as_str().to_owned()));
                return Err(match known {
                    Ok(_) => ForgeError::InvalidInput(format!(
                        "{} is not a database instance",
                        reference.map_or("", InstanceRef::as_str)
                    )),
                    Err(err) => err,
                });
            }
            of_tool
        }
    };
    match candidates.len() {
        0 => Err(ForgeError::NotFound(
            "no database instance (install one: forge tool install mysql)".into(),
        )),
        1 => Ok(candidates.into_iter().next().expect("one candidate")),
        _ => {
            let defaults: Vec<_> = candidates
                .iter()
                .filter(|(e, _)| e.row.name == "default")
                .collect();
            if defaults.len() == 1 {
                return Ok(defaults[0].clone());
            }
            Err(ForgeError::InvalidInput(format!(
                "several database instances ({}): say which with tool/name",
                describe(&candidates)
            )))
        }
    }
}

async fn state_of(entry: &Entry) -> ServiceState {
    match &entry.handle {
        Some(handle) => match handle.status(QUICK_ASK_TIMEOUT).await {
            Ok(status) => convert::service_state(&status.state),
            Err(_) => ServiceState::Failed,
        },
        None => ServiceState::Failed,
    }
}

/// Everything one database call needs about an instance, with no daemon in it: the
/// manifest block, the facts of the instance and whether it runs. The daemon builds it
/// from an [`Entry`] ([`Target::from_entry`]); tests build it directly.
#[derive(Debug, Clone)]
pub struct Target {
    pub id: InstanceId,
    /// `mysql/default`.
    pub label: String,
    /// Folder of the installed version (clients are relative to it).
    pub version_dir: Option<PathBuf>,
    /// Reserved ports by name (`{{ instance.ports.main }}`).
    pub ports: BTreeMap<String, u16>,
    pub running: bool,
    pub spec: DatabaseSpec,
}

impl Target {
    pub async fn from_entry(entry: &Entry, spec: &DatabaseSpec) -> Self {
        let running = match &entry.handle {
            Some(handle) => handle
                .status(QUICK_ASK_TIMEOUT)
                .await
                .is_ok_and(|s| matches!(s.state, InstanceState::Running)),
            None => false,
        };
        Self {
            id: entry.row.id,
            label: entry.label(),
            version_dir: entry
                .version
                .as_ref()
                .map(|v| PathBuf::from(v.path.replace('/', "\\"))),
            ports: entry.ports.clone(),
            running,
            spec: spec.clone(),
        }
    }

    /// `conflict` with the command that fixes it unless the instance runs.
    fn require_running(&self) -> Result<()> {
        if self.running {
            Ok(())
        } else {
            let label = &self.label;
            Err(ForgeError::Conflict(format!(
                "{label} is not running; start it with `forge start {label}`"
            )))
        }
    }

    fn vars(&self, database: Option<&str>, charset: Option<&str>) -> DbVars {
        let mut vars = DbVars::new();
        for (name, port) in &self.ports {
            vars = vars.port(name, *port);
        }
        if let Some(database) = database {
            vars = vars.database(database);
        }
        if let Some(charset) = charset {
            vars = vars.charset(charset);
        }
        vars
    }

    fn version_dir(&self) -> Result<&Path> {
        self.version_dir
            .as_deref()
            .ok_or_else(|| ForgeError::NotFound(format!("{} has no installed version", self.label)))
    }

    /// Runs a rendered client command and returns its stdout.
    async fn run_client(&self, command: RenderedCommand) -> Result<String> {
        let dir = self.version_dir()?.to_path_buf();
        let probe = ProbeCommand {
            exe: dir.join(command.exe.replace('/', "\\")),
            args: command.args.into_iter().map(OsString::from).collect(),
            cwd: dir,
        };
        let out = spawn_blocking_timeout(
            "running the database client",
            CLIENT_TIMEOUT + Duration::from_secs(3),
            move || run_probe(&probe, CLIENT_TIMEOUT),
        )
        .await?;
        if out.exit_code == Some(0) {
            Ok(out.stdout)
        } else {
            Err(client_failure(&self.label, &out))
        }
    }

    fn dto(&self, name: String, charset: Option<String>) -> DatabaseDto {
        DatabaseDto {
            instance_id: self.id,
            instance: self.label.clone(),
            name,
            charset,
            collation: None,
            size_bytes: None,
            tables: None,
        }
    }

    /// The databases of a running instance (system databases left out).
    pub async fn list(&self) -> Result<Vec<DatabaseDto>> {
        let command = self
            .spec
            .render_query(DbQuery::List, &self.vars(None, None))
            .map_err(render_error)?;
        self.require_running()?;
        let stdout = self.run_client(command).await?;
        Ok(parse_names(&stdout, &self.spec.system_databases)
            .into_iter()
            .map(|name| self.dto(name, None))
            .collect())
    }

    /// Refuses the databases the server owns (`mysql`, `postgres`...): they are hidden
    /// from the list and must never be created or dropped from here.
    fn refuse_system(&self, name: &str) -> Result<()> {
        if self.spec.system_databases.iter().any(|s| s == name) {
            return Err(ForgeError::InvalidInput(format!(
                "{name} is a system database"
            )));
        }
        Ok(())
    }

    /// Creates `name`. Everything is validated and rendered before the state is looked at
    /// and before any process starts.
    pub async fn create(&self, name: &str, charset: Option<&str>) -> Result<DatabaseDto> {
        let command = self
            .spec
            .render_query(DbQuery::Create, &self.vars(Some(name), charset))
            .map_err(render_error)?;
        self.refuse_system(name)?;
        self.require_running()?;
        self.run_client(command).await?;
        Ok(self.dto(
            name.to_owned(),
            Some(charset.map_or_else(|| self.spec.default_charset.clone(), str::to_owned)),
        ))
    }

    /// Drops `name`; `confirm` must be `true`.
    pub async fn drop_database(&self, name: &str, confirm: bool) -> Result<Ack> {
        if !confirm {
            return Err(ForgeError::InvalidInput(format!(
                "dropping {name} destroys its data: confirm it (confirm: true, forge db drop --yes)"
            )));
        }
        let command = self
            .spec
            .render_query(DbQuery::Drop, &self.vars(Some(name), None))
            .map_err(render_error)?;
        self.refuse_system(name)?;
        self.require_running()?;
        self.run_client(command).await?;
        Ok(Ack::with_message(format!("{name} dropped")))
    }

    /// DSN and `.env` variables (pure rendering; the instance may be stopped).
    pub fn url(&self, database: &str) -> Result<DbUrlDto> {
        let vars = self.vars(Some(database), None);
        Ok(DbUrlDto {
            url: self.spec.render_url(&vars).map_err(render_error)?,
            env: EnvVarsDto {
                vars: self.spec.render_env(&vars).map_err(render_error)?,
            },
        })
    }

    /// The interactive client as a command for the caller to run.
    pub fn shell(&self, database: Option<&str>) -> Result<ShellCommandDto> {
        let spec = &self.spec;
        let shell = spec.shell.as_ref().ok_or_else(|| {
            ForgeError::InvalidInput(format!("{} declares no interactive client", self.label))
        })?;
        let mut rendered = spec
            .render_command(shell, &self.vars(database, None))
            .map_err(render_error)?;
        let templated = shell
            .args
            .iter()
            .any(|a| a.replace(' ', "").contains("{{database}}"));
        if let (Some(database), false) = (database, templated) {
            select_database(spec.protocol, &mut rendered.args, database);
        }
        let dir = self.version_dir()?;
        Ok(ShellCommandDto {
            program: dir
                .join(rendered.exe.replace('/', "\\"))
                .to_string_lossy()
                .into_owned(),
            args: rendered.args,
            cwd: Some(dir.to_string_lossy().into_owned()),
            env: BTreeMap::new(),
        })
    }
}

// ------------------------------------------------------------------ client output

/// Maps a failed client run to an error that says what happened.
fn client_failure(label: &str, out: &ProbeOutput) -> ForgeError {
    let text = if out.stderr.trim().is_empty() {
        &out.stdout
    } else {
        &out.stderr
    };
    let lower = text.to_ascii_lowercase();
    let detail = tail(text, 400);
    if lower.contains("already exists") || lower.contains("database exists") {
        ForgeError::Conflict(format!("{label}: {detail}"))
    } else if lower.contains("unknown database")
        || lower.contains("doesn't exist")
        || lower.contains("does not exist")
    {
        ForgeError::NotFound(format!("{label}: {detail}"))
    } else {
        ForgeError::Conflict(format!(
            "{label}: the database client failed (exit {}): {detail}",
            out.exit_code.map_or_else(|| "?".into(), |c| c.to_string())
        ))
    }
}

/// First column of every row of a TAB separated output, system databases left out
/// (exact, case-sensitive comparison).
pub fn parse_names(stdout: &str, system: &[String]) -> Vec<String> {
    let mut names: Vec<String> = stdout
        .lines()
        .filter_map(|line| {
            let first = line.trim_end_matches('\r').split('\t').next()?.trim();
            (!first.is_empty() && !system.iter().any(|s| s == first)).then(|| first.to_owned())
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Selects `database` in a rendered shell command that does not mention
/// `{{ database }}`: the MySQL family takes it as the last argument, PostgreSQL replaces
/// the value of `-d` (or adds `-d <db>`).
fn select_database(protocol: DatabaseProtocol, args: &mut Vec<String>, database: &str) {
    match protocol {
        DatabaseProtocol::Mysql => args.push(database.to_owned()),
        DatabaseProtocol::Postgresql => {
            match args
                .iter()
                .position(|a| a == "-d")
                .filter(|i| i + 1 < args.len())
            {
                Some(i) => args[i + 1] = database.to_owned(),
                None => {
                    args.push("-d".into());
                    args.push(database.to_owned());
                }
            }
        }
    }
}

// ------------------------------------------------------------------ daemon methods

fn data_dir(daemon: &Daemon, entry: &Entry) -> Option<PathBuf> {
    let dir = PathBuf::from(entry.row.data_dir.as_deref()?.replace('/', "\\"));
    Some(if dir.is_absolute() {
        dir
    } else {
        daemon.root.path().join(dir)
    })
}

/// `db.list`: the databases of the running database instances (or of the one asked for,
/// which must run) and every database instance.
pub async fn list(daemon: &Daemon, params: DbListParams) -> Result<DatabaseListDto> {
    let all = database_entries(daemon);
    let explicit = params.instance.is_some();
    let chosen = match &params.instance {
        Some(reference) => Some(resolve(daemon, Some(reference))?.0.row.id),
        None => None,
    };
    let targets =
        futures::future::join_all(all.iter().map(|(e, s)| Target::from_entry(e, s))).await;
    let states = futures::future::join_all(all.iter().map(|(e, _)| state_of(e))).await;
    let mut databases = Vec::new();
    let runs = targets
        .iter()
        .filter(|t| chosen.is_none_or(|id| id == t.id))
        .filter(|t| explicit || t.running)
        .map(|t| async move { (t, t.list().await) });
    for (target, result) in futures::future::join_all(runs).await {
        match result {
            Ok(found) => databases.extend(found),
            Err(err) if explicit => return Err(err),
            Err(err) => {
                tracing::warn!(instance = %target.label, %err, "listing databases failed");
            }
        }
    }
    databases.sort_by(|a, b| (&a.instance, &a.name).cmp(&(&b.instance, &b.name)));
    let instances = all
        .iter()
        .zip(states)
        .map(|((entry, spec), state)| instance_dto(daemon, entry, spec, state))
        .collect();
    Ok(DatabaseListDto {
        databases,
        instances,
    })
}

fn instance_dto(
    daemon: &Daemon,
    entry: &Entry,
    spec: &DatabaseSpec,
    state: ServiceState,
) -> DbInstanceDto {
    DbInstanceDto {
        instance_id: entry.row.id,
        instance: entry.label(),
        tool: entry.row.tool_id.clone(),
        version: entry
            .version
            .as_ref()
            .map(|v| v.version.clone())
            .unwrap_or_default(),
        protocol: match spec.protocol {
            DatabaseProtocol::Mysql => "mysql",
            DatabaseProtocol::Postgresql => "postgresql",
        }
        .to_owned(),
        state,
        port: entry.ports.get(&spec.port).copied(),
        data_size_bytes: daemon.db_sizes.get(entry.row.id, data_dir(daemon, entry)),
        capabilities: DbCapabilitiesDto {
            create: true,
            drop: true,
            export: spec.dump.is_some(),
            import: spec.restore.is_some(),
            shell: spec.shell.is_some(),
            url: true,
            open_with: Vec::new(),
        },
    }
}

async fn target(daemon: &Daemon, reference: Option<&InstanceRef>) -> Result<Target> {
    let (entry, spec) = resolve(daemon, reference)?;
    Ok(Target::from_entry(&entry, &spec).await)
}

/// `db.create`.
pub async fn create(daemon: &Daemon, params: DbCreateParams) -> Result<DatabaseDto> {
    target(daemon, params.instance.as_ref())
        .await?
        .create(&params.name, params.charset.as_deref())
        .await
}

/// `db.drop`.
pub async fn drop_database(daemon: &Daemon, params: DbDropParams) -> Result<Ack> {
    target(daemon, params.instance.as_ref())
        .await?
        .drop_database(&params.name, params.confirm)
        .await
}

/// `db.url`.
pub async fn url(daemon: &Daemon, params: DbUrlParams) -> Result<DbUrlDto> {
    target(daemon, Some(&params.instance))
        .await?
        .url(&params.database)
}

/// `db.shell`.
pub async fn shell(daemon: &Daemon, params: DbShellParams) -> Result<ShellCommandDto> {
    target(daemon, Some(&params.instance))
        .await?
        .shell(params.database.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_the_first_column_without_system_databases() {
        let system = vec!["mysql".to_owned(), "sys".to_owned()];
        let out = "loja\t1\r\nmysql\nSys\n\nBlog\n";
        // Exact comparison: `Sys` is not `sys`.
        assert_eq!(parse_names(out, &system), ["Blog", "Sys", "loja"]);
    }

    #[test]
    fn postgres_shell_replaces_the_default_database() {
        let mut args = vec![
            "-U".into(),
            "postgres".into(),
            "-d".into(),
            "postgres".into(),
        ];
        select_database(DatabaseProtocol::Postgresql, &mut args, "loja");
        assert_eq!(args, ["-U", "postgres", "-d", "loja"]);
        let mut args = vec!["-U".to_owned()];
        select_database(DatabaseProtocol::Postgresql, &mut args, "loja");
        assert_eq!(args, ["-U", "-d", "loja"]);
        let mut args = vec!["--user=root".to_owned()];
        select_database(DatabaseProtocol::Mysql, &mut args, "loja");
        assert_eq!(args, ["--user=root", "loja"]);
    }

    #[test]
    fn failures_are_classified() {
        let out = |stderr: &str| ProbeOutput {
            exit_code: Some(1),
            stdout: String::new(),
            stderr: stderr.to_owned(),
        };
        assert_eq!(
            client_failure(
                "mysql/default",
                &out("ERROR 1007: Can't create database 'a'; database exists")
            )
            .code(),
            "conflict"
        );
        assert_eq!(
            client_failure(
                "mysql/default",
                &out("ERROR 1008: Can't drop database 'a'; database doesn't exist")
            )
            .code(),
            "not_found"
        );
    }

    #[test]
    fn walk_sums_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a"), [0u8; 10]).unwrap();
        std::fs::write(dir.path().join("sub").join("b"), [0u8; 5]).unwrap();
        assert_eq!(walk_size(dir.path(), Duration::from_secs(5)), Some(15));
        assert_eq!(
            walk_size(&dir.path().join("missing"), Duration::from_secs(5)),
            None
        );
    }

    #[tokio::test]
    async fn size_is_none_until_the_first_walk_then_cached() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), [0u8; 7]).unwrap();
        let cache = SizeCache::default();
        let id = InstanceId::new();
        assert_eq!(cache.get(id, Some(dir.path().to_path_buf())), None);
        let deadline = Instant::now() + Duration::from_secs(5);
        let size = loop {
            if let Some(size) = cache.get(id, Some(dir.path().to_path_buf())) {
                break size;
            }
            assert!(Instant::now() < deadline, "walk never finished");
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(size, 7);
        // Fresh: a later change is not seen until the TTL passes.
        std::fs::write(dir.path().join("b"), [0u8; 3]).unwrap();
        assert_eq!(cache.get(id, Some(dir.path().to_path_buf())), Some(7));
    }
}
